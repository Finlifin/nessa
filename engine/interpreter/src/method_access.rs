//! Dynamic method selection uses the lexical context of the executing call.

use nsbc::{Instruction, MethodCallScope};
use runtime::{FastHashMap, TaskId};
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
        let mut scopes = FastHashMap::default();
        scopes.reserve(contexts.len());
        {
            let tp = self.type_pool_read();
            for context in contexts {
                if tp.scope_context(context.scope).is_none() {
                    return Err(VmError::MethodAccess(MethodAccessError::InvalidCaller(
                        context.scope,
                    )));
                }
                let function = self
                    .shared
                    .bytecode
                    .try_get_function(context.func_id)
                    .ok_or(VmError::InvalidFunction(context.func_id))?;
                function
                    .instructions
                    .get(context.pc as usize)
                    .and_then(|&word| Instruction::decode(word))
                    .filter(|instruction| nsbc::instruction_requires_scope(instruction.opcode))
                    .ok_or(VmError::MissingMethodContext)?;
                if scopes
                    .insert((context.func_id, context.pc as usize), context.scope)
                    .is_some()
                {
                    return Err(VmError::MissingMethodContext);
                }
            }
            for index in 0..self.shared.bytecode.function_count() {
                let func_id = nsbc::FuncId(index as u32);
                let function = self.shared.bytecode.get_function(func_id);
                for (pc, &word) in function.instructions.iter().enumerate() {
                    if Instruction::decode(word).is_some_and(|instruction| {
                        nsbc::instruction_requires_scope(instruction.opcode)
                    }) && !scopes.contains_key(&(func_id, pc))
                    {
                        return Err(VmError::MissingMethodContext);
                    }
                }
            }
        }
        self.with_exclusive_shared_mut(|shared| {
            shared.method_call_scopes = scopes;
        });
        self.bump_metadata_epoch();
        self.invalidate_jit();
        Ok(())
    }

    /// Incrementally validate and register `MethodCallScope` entries for newly loaded functions
    /// (`new_func_range`) without discarding previously installed module call contexts.
    pub fn append_method_call_scopes(
        &mut self,
        contexts: Vec<MethodCallScope>,
        new_func_range: std::ops::Range<u32>,
    ) -> Result<(), VmError> {
        let _operation = self.operation();
        let mut batch = FastHashMap::default();
        batch.reserve(contexts.len());
        {
            let tp = self.type_pool_read();
            for context in contexts {
                if !new_func_range.contains(&context.func_id.0) {
                    return Err(VmError::InvalidFunction(context.func_id));
                }
                if tp.scope_context(context.scope).is_none() {
                    return Err(VmError::MethodAccess(MethodAccessError::InvalidCaller(
                        context.scope,
                    )));
                }
                let function = self
                    .shared
                    .bytecode
                    .try_get_function(context.func_id)
                    .ok_or(VmError::InvalidFunction(context.func_id))?;
                function
                    .instructions
                    .get(context.pc as usize)
                    .and_then(|&word| Instruction::decode(word))
                    .filter(|instruction| nsbc::instruction_requires_scope(instruction.opcode))
                    .ok_or(VmError::MissingMethodContext)?;
                if batch
                    .insert((context.func_id, context.pc as usize), context.scope)
                    .is_some()
                {
                    return Err(VmError::MissingMethodContext);
                }
            }
            for raw_fid in new_func_range {
                let func_id = nsbc::FuncId(raw_fid);
                let function = self
                    .shared
                    .bytecode
                    .try_get_function(func_id)
                    .ok_or(VmError::InvalidFunction(func_id))?;
                for (pc, &word) in function.instructions.iter().enumerate() {
                    if Instruction::decode(word).is_some_and(|instruction| {
                        nsbc::instruction_requires_scope(instruction.opcode)
                    }) && !batch.contains_key(&(func_id, pc))
                    {
                        return Err(VmError::MissingMethodContext);
                    }
                }
            }
        }
        self.with_exclusive_shared_mut(|shared| {
            shared.method_call_scopes.extend(batch);
        });
        self.bump_metadata_epoch();
        self.invalidate_jit();
        Ok(())
    }

    pub(crate) fn selected_method(
        &self,
        task_id: TaskId,
        receiver: TypeIndex,
        name: StrId,
    ) -> Result<Option<type_pool::MethodSlot>, VmError> {
        let task = self.task_ref(task_id).ok_or(VmError::InvalidTask)?;
        let caller_func = task.current_func;
        let caller_pc = task.pc;
        let cache_key = (caller_func, caller_pc, receiver, name);
        {
            let cache = self.sync_meta_cache();
            if let Some(cached) = cache.method_ic.get(&cache_key) {
                return Ok(cached.clone());
            }
        }
        // Dispatch advances PC before executing the instruction. Restored
        // continuations retain their own function and PC, not handler authority.
        let caller_scope = caller_pc.checked_sub(1).and_then(|pc| {
            self.shared
                .method_call_scopes
                .get(&(caller_func, pc as usize))
                .copied()
        });
        let tp = self.type_pool_read();
        let mut selected = None;
        let mut found = false;
        for method in tp
            .methods_of(receiver)
            .iter()
            .filter(|method| method.name == name)
        {
            found = true;
            let accessible = match caller_scope {
                Some(scope) => tp
                    .method_accessible(method, scope)
                    .map_err(VmError::MethodAccess)?,
                // Explicit public host methods on un-scoped intrinsic pools need
                // no lexical authority; any pool with lexical scopes requires
                // an installed instruction scope.
                None if method.access == MethodAccess::Public
                    && method.visible_scope.is_none()
                    && tp.scopes().is_empty() =>
                {
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
        drop(tp);
        self.sync_meta_cache()
            .method_ic
            .insert(cache_key, selected.clone());
        Ok(selected)
    }
}

#[cfg(test)]
mod tests {
    use nsbc::{FuncId, FunctionAbi, Reg};
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
        vm.type_pool_write()
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
        vm.type_pool_write().add_method(
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
                abi: FunctionAbi::value_only(parameters as usize),
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
    fn missing_and_ambiguous_authority_are_not_silently_selected() {
        for access in [MethodAccess::Private(1), MethodAccess::Public] {
            let mut missing = vm(access, None, None);
            missing.spawn_root(FuncId(0));
            assert!(matches!(
                missing.run(),
                VmResult::Error(VmError::MissingMethodContext)
            ));
        }
        let mut incomplete = vm(MethodAccess::Public, None, None);
        assert!(matches!(
            incomplete.install_method_call_scopes(vec![]),
            Err(VmError::MissingMethodContext)
        ));
        drop(incomplete);
        let mut ambiguous = vm(MethodAccess::Public, None, Some(0));
        ambiguous.type_pool_write().add_method(
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
        let replacement = type_pool::TypePool::restore(vm.type_pool_read().snapshot()).unwrap();
        vm.install_type_pool(replacement);
        vm.spawn_root(FuncId(0));
        assert!(matches!(
            vm.run(),
            VmResult::Error(VmError::MissingMethodContext)
        ));
    }
}
