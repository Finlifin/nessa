//! Dynamic method selection uses the lexical context of the executing call.

use std::collections::HashMap;

use nsbc::{Instruction, MethodCallScope};
use runtime::TaskId;
use str_interner::StrId;
use type_pool::{MethodAccess, MethodAccessError, TypeIndex};

use crate::{Vm, VmError};

impl Vm {
    /// Install checked call contexts after loading functions and the type pool.
    /// A failed installation preserves the previous table.
    pub fn install_method_call_scopes(
        &mut self,
        contexts: Vec<MethodCallScope>,
    ) -> Result<(), VmError> {
        let _operation = self.operation();
        let mut scopes = HashMap::with_capacity(contexts.len());
        for context in contexts {
            if self.state.type_pool.scope_context(context.scope).is_none() {
                return Err(VmError::MethodAccess(MethodAccessError::InvalidCaller(
                    context.scope,
                )));
            }
            let function = self
                .state
                .bytecode
                .try_get_function(context.func_id)
                .ok_or(VmError::InvalidFunction(context.func_id))?;
            function
                .instructions
                .get(context.pc as usize)
                .and_then(|&word| Instruction::decode(word))
                .filter(|instruction| nsbc::ScopeCoverage::CallsAndTypes.covers(instruction.opcode))
                .ok_or(VmError::MissingMethodContext)?;
            if scopes
                .insert((context.func_id, context.pc as usize), context.scope)
                .is_some()
            {
                return Err(VmError::MissingMethodContext);
            }
        }
        self.state.method_call_scopes = scopes;
        Ok(())
    }

    pub(crate) fn selected_method(
        &self,
        task_id: TaskId,
        receiver: TypeIndex,
        name: StrId,
    ) -> Result<Option<type_pool::MethodSlot>, VmError> {
        let task = self
            .roots
            .scheduler
            .get_task(task_id)
            .ok_or(VmError::InvalidTask)?;
        // Dispatch advances PC before executing the instruction. Restored
        // continuations retain their own function and PC, not handler authority.
        let caller_scope = task.pc.checked_sub(1).and_then(|pc| {
            self.state
                .method_call_scopes
                .get(&(task.current_func, pc as usize))
                .copied()
        });
        let mut selected = None;
        let mut found = false;
        for method in self
            .state
            .type_pool
            .methods_of(receiver)
            .iter()
            .filter(|method| method.name == name)
        {
            found = true;
            let accessible = match caller_scope {
                Some(scope) => self
                    .state
                    .type_pool
                    .method_accessible(method, scope)
                    .map_err(VmError::MethodAccess)?,
                None if method.access == MethodAccess::LegacyUnknown => {
                    return Err(VmError::MethodAccess(MethodAccessError::UnknownAccess));
                }
                // Explicit public host methods need no lexical authority. An
                // artifact with a complete table is validated before loading.
                None if method.access == MethodAccess::Public && method.visible_scope.is_none() => {
                    true
                }
                None => return Err(VmError::MissingMethodContext),
            };
            if !accessible {
                continue;
            }
            if selected.replace(method.clone()).is_some() {
                return Err(VmError::AmbiguousMethod);
            }
        }
        if found && selected.is_none() {
            return Err(VmError::MethodAccessDenied);
        }
        Ok(selected)
    }
}

#[cfg(test)]
mod tests {
    use nsbc::{FuncId, Reg};
    use runtime::FunctionCode;
    use type_pool::{Intrinsic, MethodSlot, ScopeContext};

    use super::*;
    use crate::VmResult;

    fn vm(
        access: MethodAccess,
        visible_scope: Option<u32>,
        caller: Option<u32>,
    ) -> crate::tests::TestVm {
        let mut vm = crate::tests::make_vm();
        let ty = Intrinsic::Bool.type_index();
        vm.state
            .type_pool
            .install_scopes(vec![
                ScopeContext {
                    parent: None,
                    package: 0,
                    assoc_type: None,
                },
                ScopeContext {
                    parent: Some(0),
                    package: 0,
                    assoc_type: Some(ty),
                },
                ScopeContext {
                    parent: Some(0),
                    package: 0,
                    assoc_type: Some(ty),
                },
                ScopeContext {
                    parent: Some(0),
                    package: 1,
                    assoc_type: Some(ty),
                },
                ScopeContext {
                    parent: Some(1),
                    package: 0,
                    assoc_type: None,
                },
            ])
            .unwrap();
        let name = str_interner::intern("permission_probe");
        vm.state.type_pool.add_method(
            ty,
            MethodSlot {
                name,
                func_id: 1,
                trait_impl: None,
                visible_scope,
                access,
            },
        );
        for (id, instructions, parameters) in [
            (
                0,
                vec![
                    Instruction::load_true(Reg(5)),
                    Instruction::call_method(Reg(5), name.as_u32(), 0),
                    Instruction::ret(Reg(0)),
                ],
                0,
            ),
            (
                1,
                vec![Instruction::load_imm(Reg(0), 42), Instruction::ret(Reg(0))],
                1,
            ),
        ] {
            vm.add_function(FunctionCode {
                display_owner: None,
                abi: None,
                func_id: FuncId(id),
                instructions: instructions.into_iter().map(Instruction::encode).collect(),
                register_count: 6,
                param_count: parameters,
                is_closure: false,
                function_type: TypeIndex::INVALID,
            });
        }
        if let Some(scope) = caller {
            vm.install_method_call_scopes(vec![MethodCallScope {
                func_id: FuncId(0),
                pc: 1,
                scope,
            }])
            .unwrap();
        }
        vm
    }

    #[test]
    fn dispatch_checks_private_package_and_extend_before_invoking_method() {
        for (access, visible, scope, allowed) in [
            (MethodAccess::Private(1), None, 2, true),
            (MethodAccess::Private(1), None, 0, false),
            (MethodAccess::Private(1), None, 3, false),
            (MethodAccess::Package(0), None, 2, true),
            (MethodAccess::Package(0), None, 3, false),
            (MethodAccess::Public, Some(1), 4, true),
            (MethodAccess::Public, Some(1), 2, false),
        ] {
            let mut vm = vm(access, visible, Some(scope));
            let task = vm.spawn_root(FuncId(0));
            let result = vm.run();
            if allowed {
                assert!(matches!(result, VmResult::Finished), "{result:?}");
                assert_eq!(
                    vm.task_result_number(task).unwrap().to_i64_checked(),
                    Some(42)
                );
            } else {
                assert!(
                    matches!(result, VmResult::Error(VmError::MethodAccessDenied)),
                    "{result:?}"
                );
            }
        }
    }

    #[test]
    fn unknown_missing_and_ambiguous_authority_are_not_silently_selected() {
        let mut legacy = vm(MethodAccess::LegacyUnknown, None, Some(0));
        legacy.spawn_root(FuncId(0));
        assert!(matches!(
            legacy.run(),
            VmResult::Error(VmError::MethodAccess(MethodAccessError::UnknownAccess))
        ));
        drop(legacy);
        let mut missing = vm(MethodAccess::Private(1), None, None);
        missing.spawn_root(FuncId(0));
        assert!(matches!(
            missing.run(),
            VmResult::Error(VmError::MissingMethodContext)
        ));
        drop(missing);
        let mut ambiguous = vm(MethodAccess::Public, None, Some(0));
        ambiguous.state.type_pool.add_method(
            Intrinsic::Bool.type_index(),
            MethodSlot {
                name: str_interner::intern("permission_probe"),
                func_id: 1,
                trait_impl: None,
                visible_scope: None,
                access: MethodAccess::Public,
            },
        );
        ambiguous.spawn_root(FuncId(0));
        assert!(matches!(
            ambiguous.run(),
            VmResult::Error(VmError::AmbiguousMethod)
        ));
    }

    #[test]
    fn failed_context_installation_preserves_authority_and_replacing_pool_revokes_it() {
        let mut vm = vm(MethodAccess::Private(1), None, Some(2));
        let duplicate = MethodCallScope {
            func_id: FuncId(0),
            pc: 1,
            scope: 0,
        };
        assert!(
            vm.install_method_call_scopes(vec![duplicate, duplicate])
                .is_err()
        );
        let task = vm.spawn_root(FuncId(0));
        assert!(matches!(vm.run(), VmResult::Finished));
        assert_eq!(
            vm.task_result_number(task).unwrap().to_i64_checked(),
            Some(42)
        );
        let replacement = type_pool::TypePool::restore(vm.state.type_pool.snapshot()).unwrap();
        vm.install_type_pool(replacement);
        vm.spawn_root(FuncId(0));
        assert!(matches!(
            vm.run(),
            VmResult::Error(VmError::MissingMethodContext)
        ));
    }
}
