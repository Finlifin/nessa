//! Checked physical entry layouts and trait evidence boundaries.

use nsbc::FuncId;
use runtime::{FunctionCode, TaggedValue};

use crate::{Vm, VmError};

impl Vm {
    pub(super) fn checked_function(&self, id: FuncId) -> Result<&FunctionCode, VmError> {
        let function = self
            .state
            .bytecode
            .try_get_function(id)
            .ok_or(VmError::InvalidFunction(id))?;
        if let Some(owner) = function.display_owner {
            nsbc::validate_display_owner(
                &self.state.type_pool,
                id,
                owner,
                function.function_type,
                function.abi.as_ref(),
                function.param_count,
                function.is_closure,
            )
            .map_err(|_| VmError::TypeError)?;
        }
        if let Some(abi) = &function.abi {
            for parameter in &abi.parameters {
                if let nsbc::ParameterAbi::Trait { view } = parameter
                    && self
                        .state
                        .type_pool
                        .trait_has_associated_types(*view)
                        .map_err(|_| VmError::InvalidTraitProof)?
                {
                    return Err(VmError::UnsupportedFunctionAbi);
                }
            }
            for view in abi
                .captures
                .iter()
                .filter_map(|capture| match capture {
                    nsbc::CaptureAbi::TraitProof { view } => Some(view),
                    _ => None,
                })
                .chain(
                    abi.parameters
                        .iter()
                        .filter_map(|parameter| match parameter {
                            nsbc::ParameterAbi::Trait { view }
                            | nsbc::ParameterAbi::TraitSelf { view } => Some(view),
                            _ => None,
                        }),
                )
            {
                let view = self
                    .state
                    .type_pool
                    .canonical_type(*view)
                    .ok_or(VmError::InvalidTraitProof)?;
                if !matches!(
                    self.state.type_pool.get(view).kind,
                    type_pool::TypeKind::Trait { .. }
                ) {
                    return Err(VmError::InvalidTraitProof);
                }
            }
            if function.function_type != type_pool::TypeIndex::INVALID {
                let ty = self
                    .state
                    .type_pool
                    .canonical_type(function.function_type)
                    .ok_or(VmError::TypeError)?;
                let type_pool::TypeKind::Function { params, .. } =
                    &self.state.type_pool.get(ty).kind
                else {
                    return Err(VmError::TypeError);
                };
                for (parameter, &ty) in abi.parameters.iter().zip(params) {
                    if let nsbc::ParameterAbi::Trait { view } = parameter
                        && self.state.type_pool.canonical_type(*view)
                            != self.state.type_pool.canonical_type(ty)
                    {
                        return Err(VmError::InvalidTraitProof);
                    }
                }
                for (parameter, &ty) in abi.parameters.iter().zip(params) {
                    if matches!(parameter, nsbc::ParameterAbi::TraitSelf { .. }) {
                        let concrete = self
                            .state
                            .type_pool
                            .canonical_type(ty)
                            .ok_or(VmError::TypeError)?;
                        if matches!(
                            self.state.type_pool.get(concrete).kind,
                            type_pool::TypeKind::Trait { .. }
                        ) {
                            return Err(VmError::TypeError);
                        }
                    }
                }
                if params.len() != abi.logical_parameter_count() {
                    return Err(VmError::TypeError);
                }
            }
            if function.function_type == type_pool::TypeIndex::INVALID
                && abi
                    .parameters
                    .iter()
                    .any(|parameter| matches!(parameter, nsbc::ParameterAbi::TraitSelf { .. }))
            {
                return Err(VmError::TypeError);
            }
            if abi.capture_count() > runtime::GP_REGISTER_COUNT
                || abi.logical_parameter_count() > runtime::GP_REGISTER_COUNT
                || abi.physical_parameter_count() != function.param_count as usize
                || function.param_count as usize > runtime::GP_REGISTER_COUNT
                || function.param_count > function.register_count
                || (!function.is_closure && abi.capture_count() != 0)
            {
                return Err(VmError::TypeError);
            }
        }
        Ok(function)
    }

    /// Only explicit descriptors assert a capture count. Legacy callers retain
    /// their existing physical-count/signature interpretation.
    pub(super) fn check_capture_abi(&self, id: FuncId, count: usize) -> Result<(), VmError> {
        let function = self.checked_function(id)?;
        if function
            .abi
            .as_ref()
            .is_some_and(|abi| abi.capture_count() != count)
        {
            return Err(VmError::TypeError);
        }
        Ok(())
    }

    pub(super) fn check_entry_environment(
        &self,
        id: FuncId,
        environment: Option<TaggedValue>,
    ) -> Result<(), VmError> {
        let count = match environment {
            Some(environment) => {
                let layout = Self::closure_layout(environment)?;
                if layout.function != id {
                    return Err(VmError::TypeError);
                }
                layout.capture_count as usize
            }
            None => 0,
        };
        self.check_capture_abi(id, count)?;
        if let Some(environment) = environment {
            let layout = Self::closure_layout(environment)?;
            let values: Vec<_> = (0..layout.capture_count)
                .map(|index| {
                    // SAFETY: closure layout proves the initialized capture range.
                    unsafe { runtime::ClosureEnv::get_capture(layout.pointer, index) }
                })
                .collect();
            self.check_capture_proofs(id, &values)?;
        }
        Ok(())
    }

    pub(super) fn check_user_argument_abi(&self, id: FuncId, count: usize) -> Result<(), VmError> {
        let function = self.checked_function(id)?;
        if let Some(abi) = &function.abi
            && abi.logical_parameter_count() != count
        {
            return Err(VmError::ArityError {
                expected: abi.logical_parameter_count() as u8,
                got: u8::try_from(count).unwrap_or(u8::MAX),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use nsbc::{CaptureAbi, FunctionAbi, Instruction, ParameterAbi, Reg};
    use runtime::{ClosureEnv, TaggedValue};
    use type_pool::{Intrinsic, TypeIndex, TypeKind};

    use super::*;
    use crate::{DispatchResult, VmResult};

    fn descriptor(captures: usize, parameters: usize) -> FunctionAbi {
        FunctionAbi {
            captures: vec![CaptureAbi::Value; captures],
            parameters: vec![ParameterAbi::Value; parameters],
        }
    }

    fn add(vm: &mut Vm, id: u32, instructions: &[Instruction], captures: usize, parameters: usize) {
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: Some(descriptor(captures, parameters)),
            func_id: FuncId(id),
            instructions: instructions
                .iter()
                .map(|instruction| instruction.encode())
                .collect(),
            register_count: 32,
            param_count: (captures + parameters) as u8,
            is_closure: captures != 0,
            function_type: TypeIndex::INVALID,
        });
    }

    fn environment(
        vm: &mut Vm,
        task: runtime::TaskId,
        function: FuncId,
        captures: &[TaggedValue],
    ) -> TaggedValue {
        // SAFETY: TestVm owns an operation. The fresh closure is initialized and
        // rooted in its task register before any further managed allocation.
        let pointer = unsafe {
            vm.state
                .heap
                .alloc_object(
                    Intrinsic::Closure.type_index(),
                    ClosureEnv::payload_words(captures.len() as u16),
                )
                .unwrap()
        };
        let value = unsafe {
            ClosureEnv::write(pointer.as_ptr(), function, captures);
            TaggedValue::from_heap_ptr(pointer.as_ptr())
        };
        vm.roots
            .scheduler
            .get_task_mut(task)
            .unwrap()
            .registers
            .set(Reg(20), value);
        value
    }

    #[test]
    fn explicit_closure_capture_count_is_checked_at_creation_and_invocation() {
        let mut vm = crate::tests::make_vm();
        add(
            &mut vm,
            0,
            &[
                Instruction::new_closure(Reg(1), 1, 0),
                Instruction::ret(Reg(0)),
            ],
            0,
            0,
        );
        add(&mut vm, 1, &[Instruction::ret(Reg(0))], 1, 0);
        vm.spawn_root(FuncId(0));
        assert!(matches!(vm.run(), VmResult::Error(VmError::TypeError)));

        let mut vm = crate::tests::make_vm();
        add(&mut vm, 0, &[], 0, 0);
        add(&mut vm, 1, &[Instruction::ret(Reg(0))], 1, 0);
        let task = vm.spawn_root(FuncId(0));
        let wrong = environment(&mut vm, task, FuncId(1), &[]);
        assert!(matches!(
            vm.invoke_closure(task, FuncId(1), Some(wrong), &[]),
            DispatchResult::Error(VmError::TypeError)
        ));
        assert!(
            vm.roots
                .scheduler
                .get_task(task)
                .unwrap()
                .call_stack
                .is_empty()
        );
    }

    #[test]
    fn closure_user_arity_is_separate_from_capture_count() {
        let mut vm = crate::tests::make_vm();
        add(&mut vm, 0, &[], 0, 0);
        add(&mut vm, 1, &[Instruction::ret(Reg(0))], 1, 1);
        let task = vm.spawn_root(FuncId(0));
        let closure = environment(&mut vm, task, FuncId(1), &[TaggedValue::from_i64(42)]);
        assert!(matches!(
            vm.invoke_closure(task, FuncId(1), Some(closure), &[]),
            DispatchResult::Error(VmError::ArityError {
                expected: 1,
                got: 0
            })
        ));
        assert!(matches!(
            vm.invoke_closure(
                task,
                FuncId(1),
                Some(closure),
                &[TaggedValue::UNIT, TaggedValue::UNIT]
            ),
            DispatchResult::Error(VmError::ArityError {
                expected: 1,
                got: 2
            })
        ));
        assert!(
            vm.roots
                .scheduler
                .get_task(task)
                .unwrap()
                .call_stack
                .is_empty()
        );
    }

    #[test]
    fn delimiter_and_effect_handler_entries_check_capture_layouts() {
        let mut vm = crate::tests::make_vm();
        add(&mut vm, 0, &[], 0, 0);
        add(&mut vm, 1, &[Instruction::ret(Reg(0))], 1, 0);
        let task = vm.spawn_root(FuncId(0));
        environment(&mut vm, task, FuncId(1), &[]);
        assert!(matches!(
            vm.dispatch_push_handler_closure(
                task,
                &Instruction::push_handler_closure(Intrinsic::I64.type_index(), Reg(20))
            ),
            DispatchResult::Error(VmError::TypeError)
        ));
        assert!(matches!(
            vm.dispatch_reset_closure(task, &Instruction::reset_closure(Reg(20), 0)),
            DispatchResult::Error(VmError::TypeError)
        ));
        let task = vm.roots.scheduler.get_task(task).unwrap();
        assert!(task.handler_stack.is_empty());
        assert!(task.call_stack.is_empty());
    }

    #[test]
    fn missing_proof_values_and_signature_gaps_are_explicit_errors() {
        for captures in [false, true] {
            let mut vm = crate::tests::make_vm();
            let abi = if captures {
                FunctionAbi {
                    captures: vec![CaptureAbi::TraitProof {
                        view: vm.type_pool().well_known.eq,
                    }],
                    parameters: vec![],
                }
            } else {
                FunctionAbi {
                    captures: vec![],
                    parameters: vec![ParameterAbi::Trait {
                        view: vm.type_pool().well_known.eq,
                    }],
                }
            };
            vm.add_function(FunctionCode {
                display_owner: None,
                abi: Some(abi),
                func_id: FuncId(0),
                instructions: vec![Instruction::ret(Reg(0)).encode()],
                register_count: 32,
                param_count: if captures { 1 } else { 2 },
                is_closure: captures,
                function_type: TypeIndex::INVALID,
            });
            vm.spawn_root(FuncId(0));
            assert!(matches!(
                vm.run(),
                VmResult::Error(
                    VmError::TypeError | VmError::InvalidTraitProof | VmError::ArityError { .. }
                )
            ));
        }
        let mut vm = crate::tests::make_vm();
        let function_type = vm.state.type_pool.intern_structural(TypeKind::Function {
            params: vec![Intrinsic::I64.type_index()],
            ret: Intrinsic::I64.type_index(),
        });
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: Some(descriptor(0, 0)),
            func_id: FuncId(0),
            instructions: vec![],
            register_count: 32,
            param_count: 0,
            is_closure: false,
            function_type,
        });
        vm.spawn_root(FuncId(0));
        assert!(matches!(vm.run(), VmResult::Error(VmError::TypeError)));
    }
    #[test]
    fn direct_calls_and_root_entries_check_explicit_user_arity() {
        for far in [false, true] {
            let mut vm = crate::tests::make_vm();
            let call = if far {
                let index = vm.push_constant(&nsbc::Constant::UInt(1)).unwrap();
                Instruction::call_far(index.try_into().unwrap(), 0)
            } else {
                Instruction::call(1, 0)
            };
            add(&mut vm, 0, &[call, Instruction::ret(Reg(0))], 0, 0);
            add(&mut vm, 1, &[Instruction::ret(Reg(0))], 0, 1);
            vm.spawn_root(FuncId(0));
            assert!(matches!(
                vm.run(),
                VmResult::Error(VmError::ArityError {
                    expected: 1,
                    got: 0
                })
            ));
        }
        let mut vm = crate::tests::make_vm();
        add(&mut vm, 0, &[Instruction::ret(Reg(0))], 0, 1);
        vm.spawn_root(FuncId(0));
        assert!(matches!(
            vm.run(),
            VmResult::Error(VmError::ArityError {
                expected: 1,
                got: 0
            })
        ));
    }
}
