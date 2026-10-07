//! Dynamic handlers, typed continuation values and delimiter entry/resumption.

use nsbc::{FuncId, Instruction, Opcode, Reg};
use runtime::{ClosureEnv, ContinuationId, EffectHandler, PromptId, TaggedValue, TaskId};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::continuation_roots::ContinuationInputContract;

use crate::{DispatchResult, ObjectHeader, Vm, VmError, e_payload};

impl Vm {
    pub(super) fn dispatch_push_handler_closure(
        &mut self,
        task_id: TaskId,
        instruction: &Instruction,
    ) -> DispatchResult {
        let payload = e_payload(instruction);
        let register = Reg((payload >> 17) as u8);
        let (effect_type, continuation_param) =
            if instruction.opcode == Opcode::PushCapturingHandler {
                let metadata = self.roots.constants.get(payload & 0x1FFFF);
                let Some(packed) = metadata.as_u64() else {
                    return DispatchResult::Error(VmError::TypeError);
                };
                let position = packed >> 32;
                if position >= 32 {
                    return DispatchResult::Error(VmError::TypeError);
                }
                (TypeIndex::from_raw(packed as u32), Some(position as u8))
            } else {
                (TypeIndex::from_raw(payload & 0x1FFFF), None)
            };
        let task = self
            .roots
            .scheduler
            .get_task(task_id)
            .expect("dispatched task exists");
        let closure = task.registers.get(register);
        let layout = match Self::closure_layout(closure) {
            Ok(layout) => layout,
            Err(error) => return DispatchResult::Error(error),
        };
        let handler_func = layout.function;
        if let Err(error) = self.check_entry_environment(handler_func, Some(closure)) {
            return DispatchResult::Error(error);
        }
        let task = self
            .roots
            .scheduler
            .get_task_mut(task_id)
            .expect("dispatched task exists");
        task.handler_stack.push(EffectHandler {
            effect_type,
            handler_func,
            is_async: false,
            closure_env: Some(closure),
            continuation_param,
        });
        DispatchResult::Continue
    }

    pub(super) fn dispatch_effect(
        &mut self,
        task_id: TaskId,
        instruction: &Instruction,
    ) -> DispatchResult {
        let payload = e_payload(instruction);
        let arg_count = (payload >> 17) as u8;
        let effect = TypeIndex::from_raw(payload & 0x1FFFF);
        if arg_count as usize > runtime::GP_REGISTER_COUNT {
            return DispatchResult::Error(VmError::TypeError);
        }
        let task = self
            .roots
            .scheduler
            .get_task(task_id)
            .expect("dispatched task exists");
        let Some(handler) = task.stacks.find_handler(effect).cloned() else {
            return DispatchResult::Error(VmError::UnhandledEffect(effect));
        };
        let code = match self.checked_function(handler.handler_func) {
            Ok(code) => code,
            Err(error) => return DispatchResult::Error(error),
        };
        // Legacy EffectCallDyn carries logical raw values and has no lexical
        // acquisition context. A future physical effect opcode must preserve
        // proofs for bare trait arguments; never guess handler authority here.
        if code.abi.as_ref().is_some_and(|abi| {
            abi.parameters.iter().any(|parameter| {
                matches!(
                    parameter,
                    nsbc::ParameterAbi::Trait { .. } | nsbc::ParameterAbi::TraitSelf { .. }
                )
            })
        }) {
            return DispatchResult::Error(VmError::UnsupportedFunctionAbi);
        }
        let Some(effect_type) = self.state.type_pool.canonical_type(effect) else {
            return DispatchResult::Error(VmError::InvalidType(effect));
        };
        let TypeKind::Effect {
            ret: input_type, ..
        } = self.state.type_pool.get(effect_type).kind
        else {
            return DispatchResult::Error(VmError::InvalidType(effect));
        };
        let args: Vec<TaggedValue> = task.registers.regs[..arg_count as usize].to_vec();
        let argument_count = args.len();
        let has_environment = handler.closure_env.is_some();
        let mut values = args;
        values.extend(handler.closure_env);
        let base = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&values, |vm| {
            if let Some(position) = handler.continuation_param {
                if position as usize > argument_count {
                    return Err(VmError::TypeError);
                }
                let task = vm
                    .roots
                    .scheduler
                    .get_task_mut(task_id)
                    .expect("dispatched task exists");
                let handle = match task.stacks.capture(PromptId(effect.as_u32()), Reg(0)) {
                    Ok(handle) => handle,
                    Err(error) => return Err(VmError::Stack(error)),
                };
                let continuation = match vm.alloc_effect_continuation(task_id, handle, input_type) {
                    Some(continuation) => continuation,
                    None => {
                        vm.roots
                            .scheduler
                            .get_task_mut(task_id)
                            .expect("dispatched task exists")
                            .stacks
                            .discard(handle)
                            .expect("unpublished capture remains pending");
                        return Err(VmError::OutOfMemory);
                    }
                };
                let roots = vm.roots.temporary_values.borrow();
                let mut args = roots[base..base + argument_count].to_vec();
                let environment = has_environment.then(|| roots[base + argument_count]);
                drop(roots);
                args.insert(position as usize, continuation);
                return Ok(vm.invoke_closure(task_id, handler.handler_func, environment, &args));
            }
            let roots = vm.roots.temporary_values.borrow();
            let args = roots[base..base + argument_count].to_vec();
            let environment = has_environment.then(|| roots[base + argument_count]);
            drop(roots);
            Ok(vm.invoke_closure(task_id, handler.handler_func, environment, &args))
        })
        .unwrap_or_else(DispatchResult::Error)
    }

    #[cfg(test)]
    pub(super) fn alloc_continuation(
        &mut self,
        task: TaskId,
        handle: ContinuationId,
    ) -> Option<TaggedValue> {
        self.alloc_effect_continuation(task, handle, Intrinsic::Any.type_index())
    }

    pub(super) fn alloc_effect_continuation(
        &mut self,
        task: TaskId,
        handle: ContinuationId,
        input: TypeIndex,
    ) -> Option<TaggedValue> {
        let contract = ContinuationInputContract {
            input,
            scope: self.state.type_query_scope,
        };
        self.alloc_continuation_with_contract(task, handle, contract)
    }

    fn alloc_continuation_with_contract(
        &mut self,
        task: TaskId,
        handle: ContinuationId,
        contract: ContinuationInputContract,
    ) -> Option<TaggedValue> {
        let value = self.alloc_continuation_wrapper(handle)?;
        self.own_continuation(task, handle, value, contract);
        Some(value)
    }

    fn alloc_continuation_wrapper(&mut self, handle: ContinuationId) -> Option<TaggedValue> {
        // SAFETY: the caller holds an operation and publishes the initialized
        // wrapper before the next safe point or operation exit.
        let pointer = unsafe {
            self.state
                .heap
                .alloc_object(Intrinsic::Continuation.type_index(), 1)?
        };
        // The generic GC scanner treats payload words as TaggedValues. A raw
        // handle divisible by eight would look like an invalid heap pointer.
        let payload = TaggedValue::from_u64(handle.as_u64()).raw();
        // SAFETY: the freshly allocated, 8-byte aligned payload has one u64 word.
        unsafe {
            pointer.as_ptr().cast::<u64>().write(payload);
            Some(TaggedValue::from_heap_ptr(pointer.as_ptr()))
        }
    }

    pub(super) fn dispatch_reset_closure(
        &mut self,
        task_id: TaskId,
        instruction: &Instruction,
    ) -> DispatchResult {
        let payload = e_payload(instruction);
        if payload & 0xFFF != 0 {
            return DispatchResult::Error(VmError::InvalidInstruction(instruction.encode()));
        }
        let (body, count) = Instruction::e_control_regs(payload);
        let task = self
            .roots
            .scheduler
            .get_task(task_id)
            .expect("dispatched task exists");
        let environment = task.registers.get(body);
        let layout = match Self::closure_layout(environment) {
            Ok(layout) => layout,
            Err(error) => return DispatchResult::Error(error),
        };
        let function = layout.function;
        if let Err(error) = self.check_entry_environment(function, Some(environment)) {
            return DispatchResult::Error(error);
        }
        if let Err(error) = self.check_user_argument_abi(function, 0) {
            return DispatchResult::Error(error);
        }
        let capture_count = layout.capture_count;
        if capture_count as usize > runtime::GP_REGISTER_COUNT {
            return DispatchResult::Error(VmError::TypeError);
        }
        let Some(code) = self.state.bytecode.try_get_function(function) else {
            return DispatchResult::Error(VmError::InvalidFunction(function));
        };
        if code.param_count as u32 != capture_count {
            return DispatchResult::Error(VmError::TypeError);
        }
        let Some(start) = task.handler_stack.len().checked_sub(count.0 as usize) else {
            return DispatchResult::Error(VmError::TypeError);
        };
        let prompts: Vec<_> = task.handler_stack[start..]
            .iter()
            .filter(|handler| handler.continuation_param.is_some())
            .map(|handler| PromptId(handler.effect_type.as_u32()))
            .collect();
        let handlers = task.stacks.effective_handlers();
        let captures: Vec<_> = (0..capture_count)
            .map(|index| {
                // SAFETY: index is bounded by the checked closure capture count.
                unsafe { ClosureEnv::get_capture(layout.pointer, index) }
            })
            .collect();
        let task = self
            .roots
            .scheduler
            .get_task_mut(task_id)
            .expect("dispatched task exists");
        match task.stacks.enter_delimiters(&prompts, function, &captures) {
            Ok(()) => {
                task.stacks.active_mut().entry_closure_env = Some(environment);
                task.handler_stack = handlers;
                DispatchResult::Continue
            }
            Err(error) => DispatchResult::Error(VmError::Stack(error)),
        }
    }

    pub(super) fn dispatch_language_resume(
        &mut self,
        task_id: TaskId,
        instruction: &Instruction,
    ) -> DispatchResult {
        let payload = e_payload(instruction);
        if payload & 0xFFF != 0 {
            return DispatchResult::Error(VmError::InvalidInstruction(instruction.encode()));
        }
        let (continuation, value) = Instruction::e_control_regs(payload);
        let task = self
            .roots
            .scheduler
            .get_task(task_id)
            .expect("dispatched task exists");
        self.resume_language_continuation(
            task_id,
            task.registers.get(continuation),
            task.registers.get(value),
            instruction.opcode == Opcode::ResumeContinuationOnce,
        )
    }

    fn language_continuation_handle(continuation: TaggedValue) -> Result<ContinuationId, VmError> {
        let Some(pointer) = continuation.as_heap_ptr() else {
            return Err(VmError::TypeError);
        };
        // SAFETY: validate the live heap value before accessing continuation payload.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        if !header.is_ordinary() {
            return Err(VmError::TypeError);
        }
        if header.type_index != Intrinsic::Continuation.type_index() || header.payload_words() != 1
        {
            return Err(VmError::TypeError);
        }
        // SAFETY: Continuation objects initialize exactly one aligned handle word.
        let payload = TaggedValue::from_raw(unsafe { pointer.cast::<u64>().read() });
        payload
            .as_u64()
            .map(ContinuationId::from_u64)
            .ok_or(VmError::TypeError)
    }

    pub(super) fn resume_language_value(
        &mut self,
        task_id: TaskId,
        continuation: TaggedValue,
        value: TaggedValue,
    ) -> DispatchResult {
        self.resume_language_continuation(task_id, continuation, value, false)
    }

    fn resume_language_continuation(
        &mut self,
        task_id: TaskId,
        continuation: TaggedValue,
        value: TaggedValue,
        once: bool,
    ) -> DispatchResult {
        let result = self.with_temporary_roots(&[continuation, value], |vm| {
            let handle = Self::language_continuation_handle(continuation)?;
            let contract = vm.continuation_input_contract(task_id, handle)?;
            let previous_scope = vm.state.type_query_scope;
            vm.state.type_query_scope = contract.scope;
            let checked = vm.assert_value_type(value, contract.input);
            vm.state.type_query_scope = previous_scope;
            let checked = checked?;
            // Conversion may allocate; the owner and source remain rooted until
            // the converted value is installed in the resumed context below.
            let task = vm
                .roots
                .scheduler
                .get_task_mut(task_id)
                .expect("validated capture's task exists");
            if once {
                task.stacks
                    .resume(handle, checked)
                    .map_err(VmError::Stack)?;
                vm.roots.continuations.remove(&handle);
            } else {
                // Validate before branching so a failed input cannot grow the pool
                // or consume the preserved multi-shot template.
                let branch = task.stacks.fork(handle).map_err(VmError::Stack)?;
                task.stacks
                    .resume(branch, checked)
                    .map_err(VmError::Stack)?;
            }
            Ok(())
        });
        match result {
            Ok(()) => DispatchResult::Continue,
            Err(error) => DispatchResult::Error(error),
        }
    }

    pub(super) fn clone_language_continuation(
        &mut self,
        task_id: TaskId,
        continuation: TaggedValue,
    ) -> DispatchResult {
        let handle = match Self::language_continuation_handle(continuation) {
            Ok(handle) => handle,
            Err(error) => return DispatchResult::Error(error),
        };
        let contract = match self.continuation_input_contract(task_id, handle) {
            Ok(contract) => contract,
            Err(error) => return DispatchResult::Error(error),
        };
        let task = self
            .roots
            .scheduler
            .get_task_mut(task_id)
            .expect("dispatched task exists");
        let branch = match task.stacks.fork(handle) {
            Ok(branch) => branch,
            Err(error) => return DispatchResult::Error(VmError::Stack(error)),
        };
        match self.alloc_continuation_with_contract(task_id, branch, contract) {
            Some(value) => {
                self.roots
                    .scheduler
                    .get_task_mut(task_id)
                    .expect("dispatched task exists")
                    .registers
                    .set(Reg(0), value);
                DispatchResult::Continue
            }
            None => {
                // No language value owns the new branch if allocation fails.
                let task = self
                    .roots
                    .scheduler
                    .get_task_mut(task_id)
                    .expect("dispatched task exists");
                let _ = task.stacks.discard(branch);
                DispatchResult::Error(VmError::OutOfMemory)
            }
        }
    }

    pub(super) fn invoke_closure(
        &mut self,
        task_id: TaskId,
        function: FuncId,
        environment: Option<TaggedValue>,
        args: &[TaggedValue],
    ) -> DispatchResult {
        let mut roots = args.to_vec();
        roots.extend(environment);
        let base = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&roots, |vm| {
            let physical = vm.expand_logical_arguments(function, args)?;
            let environment =
                environment.map(|_| vm.roots.temporary_values.borrow()[base + args.len()]);
            Ok(vm.invoke_closure_physical(task_id, function, environment, &physical))
        })
        .unwrap_or_else(DispatchResult::Error)
    }

    pub(super) fn invoke_closure_physical(
        &mut self,
        task_id: TaskId,
        function: FuncId,
        environment: Option<TaggedValue>,
        args: &[TaggedValue],
    ) -> DispatchResult {
        if args.len() > runtime::GP_REGISTER_COUNT {
            return DispatchResult::Error(VmError::TypeError);
        }
        if let Err(error) = self.check_entry_environment(function, environment) {
            return DispatchResult::Error(error);
        }
        if let Err(error) = self.check_physical_arguments(function, args) {
            return DispatchResult::Error(error);
        }
        let mut parameters = Vec::new();
        if let Some(environment) = environment {
            let layout = match Self::closure_layout(environment) {
                Ok(layout) => layout,
                Err(error) => return DispatchResult::Error(error),
            };
            let count = layout.capture_count;
            if count as usize + args.len() > runtime::GP_REGISTER_COUNT {
                return DispatchResult::Error(VmError::TypeError);
            }
            for index in 0..count {
                // SAFETY: index is within the initialized capture array.
                parameters.push(unsafe { ClosureEnv::get_capture(layout.pointer, index) });
            }
        }
        parameters.extend_from_slice(args);
        let Some(code) = self.state.bytecode.try_get_function(function) else {
            return DispatchResult::Error(VmError::InvalidFunction(function));
        };
        if code.param_count as usize != parameters.len() {
            return DispatchResult::Error(VmError::ArityError {
                expected: code.param_count,
                got: parameters.len() as u8,
            });
        }
        // Save the caller before installing closure parameters, including captures.
        let outcome = self.push_call_frame(task_id, function, environment, &parameters);
        if !matches!(outcome, DispatchResult::Continue) {
            return outcome;
        }
        let task = self
            .roots
            .scheduler
            .get_task_mut(task_id)
            .expect("dispatched task exists");
        task.registers.regs[..parameters.len()].copy_from_slice(&parameters);
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continuation_handle_payload_cannot_be_scanned_as_a_heap_pointer() {
        let mut vm = crate::tests::make_vm();
        for handle in [0, 8, 16, 24, (1 << 57) - 1] {
            let id = ContinuationId::from_u64(handle);
            let continuation = vm.alloc_continuation_wrapper(id).unwrap();
            let pointer = continuation.as_heap_ptr().unwrap();
            // SAFETY: alloc_continuation initialized this live one-word payload.
            let payload = unsafe { pointer.cast::<u64>().read() };
            // GC's generic payload slot scanner recognizes aligned, nonzero
            // words as pointers; the handle must retain an immediate tag.
            assert_ne!(payload & 0b111, 0);
            assert_eq!(Vm::language_continuation_handle(continuation).unwrap(), id);
        }
    }

    #[test]
    fn continuation_payload_rejects_a_non_unsigned_value() {
        let mut vm = crate::tests::make_vm();
        let continuation = vm
            .alloc_continuation_wrapper(ContinuationId::from_u64(8))
            .unwrap();
        let pointer = continuation.as_heap_ptr().unwrap();
        // SAFETY: this test owns the live allocation and its initialized word.
        unsafe {
            pointer
                .cast_mut()
                .cast::<u64>()
                .write(TaggedValue::TRUE.raw());
        }
        assert!(matches!(
            Vm::language_continuation_handle(continuation),
            Err(VmError::TypeError)
        ));
    }
}
