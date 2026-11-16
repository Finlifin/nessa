//! Immutable proof selection survives ordinary frames and delimited stacks.
//! Registry entries contain metadata only; receiver data stays in GC root slots.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use nsbc::{CaptureAbi, FuncId, ParameterAbi};
use runtime::{TaggedValue, TaskId};
use type_pool::{MethodSlot, TraitMethodDescriptor, TraitMethodKey, TypeIndex, TypeKind};

use crate::{DispatchResult, Vm, VmError};

static NEXT_PROOF: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub(crate) struct FrozenTraitProof {
    pub root_table: usize,
    pub concrete: TypeIndex,
    pub view: TypeIndex,
    pub descriptors: Vec<TraitMethodDescriptor>,
}

impl Vm {
    fn checked_trait_view(&self, view: TypeIndex) -> Result<TypeIndex, VmError> {
        let tp = self.type_pool_read();
        let view = tp.canonical_type(view).ok_or(VmError::InvalidType(view))?;
        if !matches!(tp.get(view).kind, TypeKind::Trait { .. }) {
            return Err(VmError::InvalidType(view));
        }
        Ok(view)
    }

    /// Internal concrete adapters may carry associated views only when the
    /// selected root implementation has every concrete binding already fixed.
    fn check_associated_proof_bindings(&self, table_index: usize) -> Result<(), VmError> {
        let tp = self.type_pool_read();
        let table = tp
            .vtables_snapshot()
            .get(table_index)
            .ok_or(VmError::InvalidTraitProof)?;
        tp.check_associated_implementation_bindings(
            table.implementor,
            table.trait_type,
            table.visible_scope,
        )
        .map_err(|_| VmError::UnsupportedFunctionAbi)
    }

    fn save_proof(&self, proof: FrozenTraitProof) -> Result<TaggedValue, VmError> {
        let key = (proof.root_table, proof.view);
        if let Some(&value) = self.trait_registry_read().cache.get(&key) {
            return Ok(value);
        }
        let mut reg = self.trait_registry_write();
        if let Some(&value) = reg.cache.get(&key) {
            return Ok(value);
        }
        let id = NEXT_PROOF
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
                (id < (1 << 57)).then_some(id + 1)
            })
            .map_err(|_| VmError::TraitProofLimit)?;
        let value = TaggedValue::from_trait_proof(id).ok_or(VmError::TraitProofLimit)?;
        reg.proofs.insert(id, Arc::new(proof));
        reg.cache.insert(key, value);
        Ok(value)
    }

    fn proof(&self, value: TaggedValue) -> Result<Arc<FrozenTraitProof>, VmError> {
        let id = value.as_trait_proof().ok_or(VmError::InvalidTraitProof)?;
        self.trait_registry_read()
            .proofs
            .get(&id)
            .cloned()
            .ok_or(VmError::InvalidTraitProof)
    }

    pub(super) fn acquire_trait_proof(
        &mut self,
        data: TaggedValue,
        view: TypeIndex,
    ) -> Result<TaggedValue, VmError> {
        let view = self.checked_trait_view(view)?;
        let concrete = self.reflected_type(data)?;
        let scope = self
            .worker
            .type_query_scope
            .ok_or(VmError::MissingTraitContext)?;
        let (index, descriptors) = {
            let tp = self.type_pool_read();
            let table = tp
                .find_vtable_scoped(concrete, view, scope)
                .map_err(VmError::TraitLookup)?
                .ok_or(VmError::TypeError)?;
            let index = tp
                .vtables_snapshot()
                .iter()
                .position(|candidate| std::ptr::eq(candidate, table))
                .ok_or(VmError::InvalidTraitProof)?;
            let descriptors = tp
                .checked_vtable_descriptors(index)
                .map_err(VmError::TraitProofMetadata)?;
            (index, descriptors)
        };
        self.check_associated_proof_bindings(index)?;
        for descriptor in &descriptors {
            if descriptor.signature.is_none() {
                return Err(VmError::UnsupportedFunctionAbi);
            }
            let method = MethodSlot {
                name: descriptor.name,
                func_id: descriptor.func_id,
                trait_impl: Some(descriptor.implementation_trait),
                visible_scope: descriptor.implementation_scope,
                access: descriptor.access,
            };
            if !self
                .type_pool_read()
                .method_accessible(&method, scope)
                .map_err(VmError::MethodAccess)?
            {
                return Err(VmError::MethodAccessDenied);
            }
            let code = self.checked_function(FuncId(descriptor.func_id))?;
            let key = TraitMethodKey {
                trait_owner: descriptor.trait_owner,
                name: descriptor.name,
                signature: descriptor.signature.clone(),
            };
            // Abstract declaration bodies are not executable concrete targets.
            // Adapters must publish a concrete signature and an explicit ABI.
            if key.signature.is_none() {
                return Err(VmError::UnsupportedFunctionAbi);
            }
            self.type_pool_read()
                .check_trait_method_signature_in_impl(
                    &key,
                    concrete,
                    code.function_type,
                    descriptor.implementation_trait,
                    descriptor.implementation_scope,
                )
                .map_err(|_| VmError::UnsupportedFunctionAbi)?;
        }
        self.save_proof(FrozenTraitProof {
            root_table: index,
            concrete,
            view,
            descriptors,
        })
    }

    pub(super) fn project_trait_proof(
        &mut self,
        value: TaggedValue,
        view: TypeIndex,
    ) -> Result<TaggedValue, VmError> {
        let view = self.checked_trait_view(view)?;
        let original = self.proof(value)?;
        if original.view == view {
            return Ok(value);
        }
        let mut pending = vec![(original.view, 0usize)];
        let mut visited = std::collections::HashSet::new();
        let mut ancestor = false;
        while let Some((current, depth)) = pending.pop() {
            if depth >= 256 {
                return Err(VmError::InvalidTraitProof);
            }
            if !visited.insert(current) {
                continue;
            }
            if current == view {
                ancestor = true;
                break;
            }
            let parents = {
                let tp = self.type_pool_read();
                match &tp.get(current).kind {
                    TypeKind::Trait { parents, .. } => parents.clone(),
                    _ => Vec::new(),
                }
            };
            for parent in parents {
                let parent = self.checked_trait_view(parent)?;
                pending.push((parent, depth + 1));
            }
        }
        if !ancestor {
            return Err(VmError::InvalidTraitProof);
        }
        let descriptors = {
            let tp = self.type_pool_read();
            let schema = tp
                .trait_schema(view)
                .ok_or(VmError::UnsupportedFunctionAbi)?;
            let mut descriptors = Vec::with_capacity(schema.slots.len());
            for key in &schema.slots {
                let descriptor = original
                    .descriptors
                    .iter()
                    .find(|descriptor| descriptor.name == key.name)
                    .ok_or(VmError::InvalidTraitProof)?;
                let root_key = TraitMethodKey {
                    trait_owner: descriptor.trait_owner,
                    name: descriptor.name,
                    signature: descriptor.signature.clone(),
                };
                tp.check_trait_method_redeclaration(&root_key, key)
                    .map_err(|_| VmError::InvalidTraitProof)?;
                let mut projected = descriptor.clone();
                projected.trait_owner = key.trait_owner;
                projected.signature = key.signature.clone();
                descriptors.push(projected);
            }
            descriptors
        };
        self.save_proof(FrozenTraitProof {
            root_table: original.root_table,
            concrete: original.concrete,
            view,
            descriptors,
        })
    }

    pub(super) fn assert_trait_pair(
        &self,
        proof: TaggedValue,
        data: TaggedValue,
        view: TypeIndex,
    ) -> Result<(), VmError> {
        let proof = self.proof(proof)?;
        if proof.view != self.checked_trait_view(view)?
            || self.reflected_type(data)? != proof.concrete
        {
            return Err(VmError::InvalidTraitProof);
        }
        Ok(())
    }

    pub(super) fn check_capture_proofs(
        &self,
        id: FuncId,
        values: &[TaggedValue],
    ) -> Result<(), VmError> {
        let code = self.checked_function(id)?;
        for (index, capture) in code.abi.captures.iter().enumerate() {
            if let CaptureAbi::TraitProof { view } = capture {
                let value = *values.get(index).ok_or(VmError::InvalidTraitProof)?;
                if index == 0 || code.abi.captures[index - 1] != CaptureAbi::Value {
                    return Err(VmError::InvalidTraitProof);
                }
                self.assert_trait_pair(value, values[index - 1], *view)?;
            }
        }
        Ok(())
    }

    pub(super) fn check_physical_arguments(
        &self,
        id: FuncId,
        values: &[TaggedValue],
    ) -> Result<(), VmError> {
        let code = self.checked_function(id)?;
        let abi = &code.abi;
        let expected = abi.physical_parameter_count() - abi.capture_count();
        if values.len() != expected {
            return Err(VmError::ArityError {
                expected: expected as u8,
                got: u8::try_from(values.len()).unwrap_or(u8::MAX),
            });
        }
        if abi.parameters.iter().all(|p| *p == ParameterAbi::Value) {
            return Ok(());
        }
        // Synthesized handler entries can carry an ABI without a source
        // function signature. Only a real descriptor supplies ordinary
        // parameter contracts; trait proof checks remain mandatory.
        let params: Option<Vec<TypeIndex>> = if code.function_type == TypeIndex::INVALID {
            None
        } else {
            let tp = self.type_pool_read();
            let declaration = tp
                .canonical_type(code.function_type)
                .ok_or(VmError::TypeError)?;
            let TypeKind::Function { params, .. } = &tp.get(declaration).kind else {
                return Err(VmError::TypeError);
            };
            Some(params.clone())
        };
        let mut offset = 0;
        for (index, parameter) in abi.parameters.iter().enumerate() {
            match parameter {
                ParameterAbi::Value => offset += 1,
                ParameterAbi::Trait { view } => {
                    self.assert_trait_pair(values[offset], values[offset + 1], *view)?;
                    offset += 2;
                }
                ParameterAbi::TraitSelf { view } => {
                    self.assert_trait_pair(values[offset], values[offset + 1], *view)?;
                    let params = params.as_ref().ok_or(VmError::TypeError)?;
                    let concrete = self
                        .type_pool_read()
                        .canonical_type(params[index])
                        .ok_or(VmError::TypeError)?;
                    if self.proof(values[offset])?.concrete != concrete {
                        return Err(VmError::InvalidTraitProof);
                    }
                    offset += 2;
                }
            }
        }
        Ok(())
    }

    /// Dynamic calls have a lexical instruction context and must establish the
    /// actual callable's value contracts, including numeric representation.
    /// Direct source calls already check their arguments during lowering.
    pub(super) fn prepare_indirect_arguments(
        &mut self,
        id: FuncId,
        values: &mut [TaggedValue],
    ) -> Result<(), VmError> {
        self.prepare_indirect_arguments_with(id, values, |_| Ok(()))
    }

    fn prepare_indirect_arguments_with(
        &mut self,
        id: FuncId,
        values: &mut [TaggedValue],
        mut after_conversion: impl FnMut(&mut Self) -> Result<(), VmError>,
    ) -> Result<(), VmError> {
        self.check_physical_arguments(id, values)?;
        let code = self.checked_function(id)?;
        if code.function_type == TypeIndex::INVALID {
            return Ok(());
        }
        let cached = self.sync_meta_cache().indirect_contracts.get(&id).cloned();
        let contracts: std::sync::Arc<[(usize, TypeIndex)]> = match cached {
            Some(arc) => arc,
            None => {
                let built = {
                    let abi = &code.abi;
                    let tp = self.type_pool_read();
                    let declaration = tp
                        .canonical_type(code.function_type)
                        .ok_or(VmError::TypeError)?;
                    let TypeKind::Function { params, .. } = &tp.get(declaration).kind else {
                        return Err(VmError::TypeError);
                    };
                    let mut contracts = Vec::with_capacity(abi.parameters.len());
                    let mut offset = 0;
                    for (index, parameter) in abi.parameters.iter().enumerate() {
                        match parameter {
                            ParameterAbi::Value => {
                                let pty = *params.get(index).ok_or(VmError::TypeError)?;
                                let canon = tp.canonical_type(pty).unwrap_or(pty);
                                contracts.push((offset, canon));
                                offset += 1;
                            }
                            ParameterAbi::Trait { .. } | ParameterAbi::TraitSelf { .. } => {
                                offset += 2
                            }
                        }
                    }
                    std::sync::Arc::<[(usize, TypeIndex)]>::from(contracts)
                };
                self.sync_meta_cache()
                    .indirect_contracts
                    .insert(id, std::sync::Arc::clone(&built));
                built
            }
        };
        if contracts.iter().all(|&(offset, expected)| {
            Self::value_is_exact_intrinsic_fast(values[offset], expected)
        }) {
            for _ in contracts.iter() {
                after_conversion(self)?;
            }
            return Ok(());
        }
        let base = self.worker_roots().temporary_values.borrow().len();
        let val_len = values.len();
        let mut out_buf = [TaggedValue::UNIT; runtime::GP_REGISTER_COUNT];
        self.with_temporary_roots(values, |vm| {
            for &(offset, expected) in contracts.iter() {
                // Read from registered slots: a preceding allocation may have
                // relocated either an original argument or a converted value.
                let source = vm.worker_roots().temporary_values.borrow()[base + offset];
                let converted = vm.assert_value_type(source, expected)?;
                vm.worker_roots().temporary_values.borrow_mut()[base + offset] = converted;
                after_conversion(vm)?;
            }
            out_buf[..val_len].copy_from_slice(
                &vm.worker_roots().temporary_values.borrow()[base..base + val_len],
            );
            Ok(())
        })?;
        values.copy_from_slice(&out_buf[..val_len]);
        Ok(())
        // The caller publishes these values into the callee's registers before
        // any further Nessa allocation; stack/frame installation is host memory.
    }

    pub(super) fn expand_logical_arguments(
        &mut self,
        id: FuncId,
        values: &[TaggedValue],
    ) -> Result<Vec<TaggedValue>, VmError> {
        self.check_user_argument_abi(id, values.len())?;
        if self
            .checked_function(id)?
            .abi
            .parameters
            .iter()
            .all(|p| *p == ParameterAbi::Value)
        {
            return Ok(values.to_vec());
        }
        let abi = self.checked_function(id)?.abi.clone();
        let mut physical = Vec::new();
        for (parameter, &data) in abi.parameters.iter().zip(values) {
            if let ParameterAbi::Trait { view } | ParameterAbi::TraitSelf { view } = parameter {
                physical.push(self.acquire_trait_proof(data, *view)?);
            }
            physical.push(data);
        }
        Ok(physical)
    }

    pub(super) fn dispatch_trait_call(
        &mut self,
        task: TaskId,
        proof_value: TaggedValue,
        slot: usize,
        physical: &[TaggedValue],
    ) -> DispatchResult {
        match self.prepare_trait_call(proof_value, slot, physical) {
            Ok((function, arguments)) => {
                self.invoke_closure_physical(task, function, None, &arguments)
            }
            Err(error) => DispatchResult::Error(error),
        }
    }

    fn prepare_trait_call(
        &mut self,
        proof_value: TaggedValue,
        slot: usize,
        physical: &[TaggedValue],
    ) -> Result<(FuncId, Vec<TaggedValue>), VmError> {
        let proof = self.proof(proof_value)?;
        let descriptor = proof
            .descriptors
            .get(slot)
            .ok_or(VmError::InvalidTraitProof)?;
        let receiver = *physical.first().ok_or(VmError::TypeError)?;
        self.assert_trait_pair(proof_value, receiver, proof.view)?;
        let key = TraitMethodKey {
            trait_owner: descriptor.trait_owner,
            name: descriptor.name,
            signature: descriptor.signature.clone(),
        };
        let signature = key
            .signature
            .as_ref()
            .ok_or(VmError::UnsupportedFunctionAbi)?;
        if signature.parameter_kinds.first() != Some(&type_pool::TraitParameterKind::Receiver) {
            return Err(VmError::UnsupportedFunctionAbi);
        }
        let params = {
            let mut tp = self.type_pool_write();
            let interface = tp
                .instantiate_trait_method_signature_in_impl(
                    &key,
                    proof.concrete,
                    descriptor.implementation_trait,
                    descriptor.implementation_scope,
                )
                .map_err(|_| VmError::TypeError)?;
            let TypeKind::Function { params, .. } = tp.get(interface).kind.clone() else {
                return Err(VmError::TypeError);
            };
            params
        };
        let function = FuncId(descriptor.func_id);
        let target = self.checked_function(function)?;
        let abi = target.abi.clone();
        if abi.capture_count() != 0 || abi.parameters.len() != params.len() {
            return Err(VmError::UnsupportedFunctionAbi);
        }
        self.type_pool_read()
            .check_trait_method_signature_in_impl(
                &key,
                proof.concrete,
                target.function_type,
                descriptor.implementation_trait,
                descriptor.implementation_scope,
            )
            .map_err(|_| VmError::UnsupportedFunctionAbi)?;
        let mut arguments = match abi.parameters.first() {
            Some(ParameterAbi::Value) => vec![receiver],
            Some(ParameterAbi::TraitSelf { view }) => {
                let receiver_proof = self.project_trait_proof(proof_value, *view)?;
                self.assert_trait_pair(receiver_proof, receiver, *view)?;
                vec![receiver_proof, receiver]
            }
            _ => return Err(VmError::UnsupportedFunctionAbi),
        };
        let mut offset = 1;
        for (index, &parameter) in params.iter().enumerate().skip(1) {
            let (parameter, is_trait_param) = {
                let tp = self.type_pool_read();
                let canonical = tp
                    .canonical_type(parameter)
                    .ok_or(VmError::InvalidType(parameter))?;
                let is_trait = matches!(tp.get(canonical).kind, TypeKind::Trait { .. });
                (canonical, is_trait)
            };
            let direct_self = signature
                .self_paths
                .contains(&vec![type_pool::TraitTypeStep::Parameter(index as u32)]);
            if direct_self || is_trait_param {
                let parameter_view = if direct_self { proof.view } else { parameter };
                let incoming = *physical.get(offset).ok_or(VmError::TypeError)?;
                let data = *physical.get(offset + 1).ok_or(VmError::TypeError)?;
                let projected = self.project_trait_proof(incoming, parameter_view)?;
                self.assert_trait_pair(projected, data, parameter_view)?;
                if direct_self {
                    if self.proof(incoming)?.concrete != proof.concrete {
                        return Err(VmError::InvalidTraitProof);
                    }
                    match abi.parameters[index] {
                        ParameterAbi::Value => {}
                        ParameterAbi::TraitSelf { view } => {
                            let carried = self.project_trait_proof(incoming, view)?;
                            self.assert_trait_pair(carried, data, view)?;
                            arguments.push(carried);
                        }
                        _ => return Err(VmError::UnsupportedFunctionAbi),
                    }
                } else {
                    if !matches!(
                        abi.parameters[index],
                        ParameterAbi::Trait { view }
                            if self.type_pool_read().canonical_type(view) == Some(parameter)
                    ) {
                        return Err(VmError::UnsupportedFunctionAbi);
                    }
                    arguments.push(projected);
                }
                arguments.push(data);
                offset += 2;
            } else {
                if abi.parameters[index] != ParameterAbi::Value {
                    return Err(VmError::UnsupportedFunctionAbi);
                }
                arguments.push(*physical.get(offset).ok_or(VmError::TypeError)?);
                offset += 1;
            }
        }
        if offset != physical.len() {
            return Err(VmError::TypeError);
        }
        Ok((function, arguments))
    }
}

#[cfg(test)]
mod tests {
    use nsbc::{FunctionAbi, Instruction, ParameterAbi, Reg};
    use runtime::FunctionCode;
    use type_pool::{
        MethodAccess, ScopeContext, TraitDispatchSchema, TraitImplRecord, TraitMethodSignature,
        TraitParameterKind, TraitTypeStep, TypeId, TypeInfo, VTable,
    };

    use super::*;

    #[test]
    fn indirect_parameter_conversions_remain_rooted_between_allocations() {
        let mut vm = crate::tests::make_vm();
        let target = type_pool::Intrinsic::I128.type_index();
        let signature = vm.type_pool_write().intern_structural(TypeKind::Function {
            params: vec![target, target],
            ret: target,
        });
        vm.add_function(FunctionCode {
            display_owner: None,
            func_id: FuncId(0),
            instructions: vec![Instruction::ret(Reg(0)).encode()],
            register_count: 2,
            param_count: 2,
            is_closure: false,
            function_type: signature,
            abi: FunctionAbi {
                captures: vec![],
                parameters: vec![ParameterAbi::Value; 2],
            },
        });
        let base = vm.worker_roots().temporary_values.borrow().len();
        let before = vm.completed_collections();
        let mut converted = 0;
        let mut arguments = [TaggedValue::from_i64(40), TaggedValue::from_i64(2)];
        vm.prepare_indirect_arguments_with(FuncId(0), &mut arguments, |vm| {
            converted += 1;
            assert_eq!(vm.worker_roots().temporary_values.borrow().len(), base + 2);
            assert!(vm.collect_garbage()?);
            let values = vm.worker_roots().temporary_values.borrow();
            assert!(values[base].as_heap_ptr().is_some());
            // SAFETY: values are registered roots even across the completed collection.
            assert_eq!(
                unsafe { runtime::Number::from_tagged(values[base]) },
                Some(runtime::Number::I128(40))
            );
            if converted == 2 {
                assert!(values[base + 1].as_heap_ptr().is_some());
                // SAFETY: this converted value is in its registered argument slot.
                assert_eq!(
                    unsafe { runtime::Number::from_tagged(values[base + 1]) },
                    Some(runtime::Number::I128(2))
                );
            } else {
                assert_eq!(values[base + 1].as_i64(), Some(2));
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(converted, 2);
        assert!(vm.completed_collections() >= before + 2);
        // SAFETY: no allocation occurs between preparation and these scalar reads.
        assert_eq!(
            unsafe { runtime::Number::from_tagged(arguments[0]) },
            Some(runtime::Number::I128(40))
        );
        // SAFETY: same publication interval, before any further allocation.
        assert_eq!(
            unsafe { runtime::Number::from_tagged(arguments[1]) },
            Some(runtime::Number::I128(2))
        );
        assert_eq!(vm.worker_roots().temporary_values.borrow().len(), base);
        let mut invalid = [TaggedValue::from_i64(40), TaggedValue::TRUE];
        assert!(matches!(
            vm.prepare_indirect_arguments(FuncId(0), &mut invalid),
            Err(VmError::TypeError)
        ));
        assert_eq!(vm.worker_roots().temporary_values.borrow().len(), base);
    }

    fn fixture() -> (crate::tests::TestVm, TypeIndex, TypeIndex) {
        let mut vm = crate::tests::make_vm();
        let mut pool = type_pool::TypePool::with_intrinsics();
        pool.install_scopes(vec![
            ScopeContext {
                parent: None,
                package: 0,
                assoc_type: None,
            },
            ScopeContext {
                parent: Some(0),
                package: 0,
                assoc_type: None,
            },
        ])
        .unwrap();
        let view = pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("ProofRead"),
                parents: vec![],
                assoc_types: vec![],
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let other = pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("Unrelated"),
                parents: vec![],
                assoc_types: vec![],
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let concrete = type_pool::Intrinsic::I64.type_index();
        let declaration = pool.intern_structural(TypeKind::Function {
            params: vec![view],
            ret: concrete,
        });
        let function_type = pool.intern_structural(TypeKind::Function {
            params: vec![concrete],
            ret: concrete,
        });
        let name = str_interner::intern("value");
        pool.register_trait_schema(TraitDispatchSchema {
            trait_type: view,
            slots: vec![TraitMethodKey {
                trait_owner: view,
                name,
                signature: Some(TraitMethodSignature {
                    associated_paths: Vec::new(),
                    declaration,
                    self_paths: vec![vec![TraitTypeStep::Parameter(0)]],
                    parameter_kinds: vec![TraitParameterKind::Receiver],
                }),
            }],
        })
        .unwrap();
        pool.register_trait_schema(TraitDispatchSchema {
            trait_type: other,
            slots: vec![],
        })
        .unwrap();
        let method = MethodSlot {
            name,
            func_id: 1,
            trait_impl: Some(view),
            visible_scope: Some(1),
            access: MethodAccess::Public,
        };
        pool.add_method(concrete, method.clone());
        pool.add_trait_impl(TraitImplRecord {
            implementor: concrete,
            trait_type: view,
            visible_scope: Some(1),
            methods: vec![method],
        });
        pool.add_vtable(VTable {
            implementor: concrete,
            trait_type: view,
            visible_scope: Some(1),
            entries: vec![1],
        });
        // Implementing an unrelated interface is not parent inheritance.
        pool.add_trait_impl(TraitImplRecord {
            implementor: view,
            trait_type: other,
            visible_scope: None,
            methods: vec![],
        });
        vm.install_type_pool(pool);
        for id in 0..2 {
            vm.add_function(FunctionCode {
                display_owner: None,
                abi: FunctionAbi {
                    captures: vec![],
                    parameters: if id == 0 {
                        vec![]
                    } else {
                        vec![ParameterAbi::Value]
                    },
                },
                func_id: FuncId(id),
                instructions: vec![Instruction::ret(Reg(0)).encode()],
                register_count: 32,
                param_count: id as u8,
                is_closure: false,
                function_type: if id == 0 {
                    TypeIndex::INVALID
                } else {
                    function_type
                },
            });
        }
        vm.worker.type_query_scope = Some(1);
        (vm, view, other)
    }

    #[test]
    fn associated_proofs_freeze_exact_bindings_but_reject_bare_parameter_abis() {
        let (mut vm, view, _) = fixture();
        let mut snapshot = vm.type_pool_read().snapshot();
        let name = str_interner::intern("Item");
        let TypeKind::Trait { assoc_types, .. } = &mut snapshot.types[view.as_u32() as usize].kind
        else {
            panic!("fixture is a trait");
        };
        assoc_types.push((name, type_pool::Intrinsic::I64.type_index()));
        let schema = snapshot
            .trait_schemas
            .iter_mut()
            .find(|schema| schema.trait_type == view)
            .unwrap();
        schema.slots[0]
            .signature
            .as_mut()
            .unwrap()
            .associated_paths
            .push(type_pool::TraitAssociatedPath {
                trait_owner: view,
                name,
                path: vec![TraitTypeStep::Return],
            });
        snapshot
            .associated_bindings
            .push(type_pool::AssociatedTypeBinding {
                implementor: type_pool::Intrinsic::I64.type_index(),
                trait_type: view,
                visible_scope: Some(1),
                trait_owner: view,
                name,
                value: type_pool::Intrinsic::I64.type_index(),
            });
        vm.install_type_pool(type_pool::TypePool::restore(snapshot).unwrap());
        vm.worker.type_query_scope = Some(1);
        let data = TaggedValue::from_i64(42);
        let proof = vm.acquire_trait_proof(data, view).unwrap();
        vm.assert_trait_pair(proof, data, view).unwrap();
        let (function, arguments) = vm.prepare_trait_call(proof, 0, &[data]).unwrap();
        assert_eq!(function, FuncId(1));
        assert_eq!(arguments, [data]);
        assert_eq!(vm.project_trait_proof(proof, view).unwrap(), proof);
        let signature = vm.type_pool_write().intern_structural(TypeKind::Function {
            params: vec![view],
            ret: type_pool::Intrinsic::I64.type_index(),
        });
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi {
                captures: vec![],
                parameters: vec![ParameterAbi::Trait { view }],
            },
            func_id: FuncId(2),
            instructions: vec![Instruction::ret(Reg(1)).encode()],
            register_count: 32,
            param_count: 2,
            is_closure: false,
            function_type: signature,
        });
        assert!(matches!(
            vm.checked_function(FuncId(2)),
            Err(VmError::UnsupportedFunctionAbi)
        ));
    }

    #[test]
    fn associated_item_equal_to_self_still_uses_one_value_argument() {
        let (mut vm, view, _) = fixture();
        let concrete = type_pool::Intrinsic::I64.type_index();
        let declaration = vm.type_pool_write().intern_structural(TypeKind::Function {
            params: vec![view, concrete],
            ret: concrete,
        });
        let function_type = vm.type_pool_write().intern_structural(TypeKind::Function {
            params: vec![concrete, concrete],
            ret: concrete,
        });
        let name = str_interner::intern("Item");
        let mut snapshot = vm.type_pool_read().snapshot();
        let TypeKind::Trait { assoc_types, .. } = &mut snapshot.types[view.as_u32() as usize].kind
        else {
            panic!("fixture trait");
        };
        assoc_types.push((name, concrete));
        let signature = snapshot
            .trait_schemas
            .iter_mut()
            .find(|schema| schema.trait_type == view)
            .unwrap()
            .slots[0]
            .signature
            .as_mut()
            .unwrap();
        signature.declaration = declaration;
        signature.parameter_kinds.push(TraitParameterKind::Required);
        signature
            .associated_paths
            .push(type_pool::TraitAssociatedPath {
                trait_owner: view,
                name,
                path: vec![TraitTypeStep::Parameter(1)],
            });
        snapshot
            .associated_bindings
            .push(type_pool::AssociatedTypeBinding {
                implementor: concrete,
                trait_type: view,
                visible_scope: Some(1),
                trait_owner: view,
                name,
                value: concrete,
            });
        for method in &mut snapshot.methods[concrete.as_u32() as usize] {
            if method.func_id == 1 {
                method.func_id = 2;
            }
        }
        for record in &mut snapshot.trait_impls {
            for method in &mut record.methods {
                if method.func_id == 1 {
                    method.func_id = 2;
                }
            }
        }
        snapshot.vtables[0].entries[0] = 2;
        vm.install_type_pool(type_pool::TypePool::restore(snapshot).unwrap());
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi {
                captures: vec![],
                parameters: vec![ParameterAbi::TraitSelf { view }, ParameterAbi::Value],
            },
            func_id: FuncId(2),
            instructions: vec![Instruction::ret(Reg(2)).encode()],
            register_count: 32,
            param_count: 3,
            is_closure: false,
            function_type,
        });
        vm.worker.type_query_scope = Some(1);
        let receiver = TaggedValue::from_i64(40);
        let item = TaggedValue::from_i64(2);
        let proof = vm.acquire_trait_proof(receiver, view).unwrap();
        let (function, arguments) = vm.prepare_trait_call(proof, 0, &[receiver, item]).unwrap();
        assert_eq!(function, FuncId(2));
        assert_eq!(arguments, [proof, receiver, item]);
        vm.check_physical_arguments(function, &arguments).unwrap();
        assert!(matches!(
            vm.prepare_trait_call(proof, 0, &[receiver, proof, item]),
            Err(VmError::TypeError)
        ));

        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi {
                captures: vec![CaptureAbi::Value, CaptureAbi::TraitProof { view }],
                parameters: vec![],
            },
            func_id: FuncId(3),
            instructions: vec![Instruction::ret(Reg(0)).encode()],
            register_count: 32,
            param_count: 2,
            is_closure: true,
            function_type: TypeIndex::INVALID,
        });
        vm.check_capture_proofs(FuncId(3), &[receiver, proof])
            .unwrap();
        assert!(matches!(
            vm.check_capture_proofs(FuncId(3), &[TaggedValue::TRUE, proof]),
            Err(VmError::InvalidTraitProof)
        ));
    }

    #[test]
    fn declaration_only_associated_views_and_their_children_are_not_bare_proofs() {
        for inherited in [false, true] {
            let (mut vm, view, parent) = fixture();
            let mut snapshot = vm.type_pool_read().snapshot();
            let owner = if inherited { parent } else { view };
            let TypeKind::Trait { assoc_types, .. } =
                &mut snapshot.types[owner.as_u32() as usize].kind
            else {
                panic!("fixture is a trait");
            };
            assoc_types.push((
                str_interner::intern("UnusedItem"),
                type_pool::Intrinsic::Any.type_index(),
            ));
            if inherited {
                let TypeKind::Trait { parents, .. } =
                    &mut snapshot.types[view.as_u32() as usize].kind
                else {
                    panic!("fixture is a trait");
                };
                parents.push(parent);
            }
            vm.install_type_pool(type_pool::TypePool::restore(snapshot).unwrap());
            vm.worker.type_query_scope = Some(1);
            assert!(matches!(
                vm.acquire_trait_proof(TaggedValue::from_i64(42), view),
                Err(VmError::UnsupportedFunctionAbi)
            ));
            assert!(matches!(
                vm.project_trait_proof(TaggedValue::NULL, view),
                Err(VmError::InvalidTraitProof)
            ));
            assert!(vm.shared.trait_registry.read(0).proofs.is_empty());
        }
    }

    #[test]
    fn handles_are_pool_and_vm_bound_and_projection_requires_ancestry() {
        let (mut first, view, other) = fixture();
        let data = TaggedValue::from_i64(42);
        let proof = first.acquire_trait_proof(data, view).unwrap();
        assert!(first.assert_trait_pair(proof, data, view).is_ok());
        for _ in 0..20 {
            assert_eq!(first.acquire_trait_proof(data, view).unwrap(), proof);
        }
        assert_eq!(first.shared.trait_registry.read(0).proofs.len(), 1);
        assert!(matches!(
            first.assert_trait_pair(proof, TaggedValue::TRUE, view),
            Err(VmError::InvalidTraitProof)
        ));
        assert!(matches!(
            first.project_trait_proof(proof, other),
            Err(VmError::InvalidTraitProof)
        ));
        let (mut second, second_view, _) = fixture();
        let second_proof = second.acquire_trait_proof(data, second_view).unwrap();
        assert_ne!(proof, second_proof);
        assert!(matches!(
            second.assert_trait_pair(proof, data, second_view),
            Err(VmError::InvalidTraitProof)
        ));
        let pool = first.type_pool_read().snapshot();
        first.install_type_pool(type_pool::TypePool::restore(pool).unwrap());
        assert!(matches!(
            first.assert_trait_pair(proof, data, view),
            Err(VmError::InvalidTraitProof)
        ));
    }

    #[test]
    fn concrete_self_entries_keep_frozen_proof_and_validate_concrete_signature() {
        let (mut vm, view, _) = fixture();
        let signature = vm.shared.bytecode.get_function(FuncId(1)).function_type;
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi {
                captures: vec![],
                parameters: vec![ParameterAbi::TraitSelf { view }],
            },
            func_id: FuncId(2),
            instructions: vec![Instruction::ret(Reg(1)).encode()],
            register_count: 32,
            param_count: 2,
            is_closure: false,
            function_type: signature,
        });
        let mut metadata = vm.type_pool_read().snapshot();
        for methods in &mut metadata.methods {
            for method in methods {
                if method.func_id == 1 {
                    method.func_id = 2;
                }
            }
        }
        for implementation in &mut metadata.trait_impls {
            for method in &mut implementation.methods {
                if method.func_id == 1 {
                    method.func_id = 2;
                }
            }
        }
        metadata.vtables[0].entries[0] = 2;
        vm.install_type_pool(type_pool::TypePool::restore(metadata).unwrap());
        vm.worker.type_query_scope = Some(1);
        let data = TaggedValue::from_i64(42);
        let proof = vm.acquire_trait_proof(data, view).unwrap();
        vm.worker.type_query_scope = None;
        let (function, arguments) = vm.prepare_trait_call(proof, 0, &[data]).unwrap();
        assert_eq!(function, FuncId(2));
        assert_eq!(arguments, vec![proof, data]);
        assert_eq!(vm.shared.trait_registry.read(0).proofs.len(), 1);
        assert!(vm.check_physical_arguments(function, &arguments).is_ok());
        assert!(matches!(
            vm.check_physical_arguments(function, &[proof, TaggedValue::TRUE]),
            Err(VmError::InvalidTraitProof)
        ));
        let wrong_signature = vm.type_pool_write().intern_structural(TypeKind::Function {
            params: vec![type_pool::Intrinsic::Bool.type_index()],
            ret: type_pool::Intrinsic::Bool.type_index(),
        });
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi {
                captures: vec![],
                parameters: vec![ParameterAbi::TraitSelf { view }],
            },
            func_id: FuncId(3),
            instructions: vec![Instruction::return_unit().encode()],
            register_count: 32,
            param_count: 2,
            is_closure: false,
            function_type: wrong_signature,
        });
        assert!(matches!(
            vm.check_physical_arguments(FuncId(3), &[proof, data]),
            Err(VmError::InvalidTraitProof)
        ));
        let task = vm.spawn_root(FuncId(0));
        assert!(matches!(
            vm.invoke_closure_physical(task, function, None, &arguments),
            DispatchResult::Continue
        ));
    }

    #[test]
    fn physical_entries_validate_pairs_even_without_assert_opcodes() {
        let (mut vm, view, _) = fixture();
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi {
                captures: vec![],
                parameters: vec![ParameterAbi::Trait { view }],
            },
            func_id: FuncId(2),
            instructions: vec![Instruction::return_unit().encode()],
            register_count: 32,
            param_count: 2,
            is_closure: false,
            function_type: TypeIndex::INVALID,
        });
        let task = vm.spawn_root(FuncId(0));
        let data = TaggedValue::from_i64(42);
        let proof = vm.acquire_trait_proof(data, view).unwrap();
        assert!(matches!(
            vm.invoke_closure_physical(task, FuncId(2), None, &[proof, TaggedValue::TRUE]),
            DispatchResult::Error(VmError::InvalidTraitProof)
        ));
        assert!(vm.task_ref(task).unwrap().call_stack.is_empty());
        assert!(matches!(
            vm.invoke_closure_physical(
                task,
                FuncId(2),
                None,
                &[TaggedValue::from_trait_proof((1 << 57) - 1).unwrap(), data]
            ),
            DispatchResult::Error(VmError::InvalidTraitProof)
        ));
        assert!(matches!(
            vm.invoke_closure_physical(task, FuncId(2), None, &[proof, data]),
            DispatchResult::Continue
        ));
    }

    #[test]
    fn registry_caches_frozen_tables_and_parent_views_without_mixing_origins() {
        let (mut vm, view, parent) = fixture();
        if let TypeKind::Trait { parents, .. } = &mut vm.type_pool_write().get_mut(view).kind {
            parents.push(parent);
        }
        let concrete = type_pool::Intrinsic::I64.type_index();
        vm.type_pool_write().add_trait_impl(TraitImplRecord {
            implementor: concrete,
            trait_type: parent,
            visible_scope: None,
            methods: vec![],
        });
        vm.type_pool_write().add_vtable(VTable {
            implementor: concrete,
            trait_type: parent,
            visible_scope: None,
            entries: vec![],
        });
        let data = TaggedValue::from_i64(42);
        let proof = vm.acquire_trait_proof(data, view).unwrap();
        let projected = vm.project_trait_proof(proof, parent).unwrap();
        for _ in 0..20 {
            assert_eq!(vm.acquire_trait_proof(data, view).unwrap(), proof);
            assert_eq!(vm.project_trait_proof(proof, parent).unwrap(), projected);
        }
        assert_eq!(vm.shared.trait_registry.read(0).proofs.len(), 2);
        let independent = vm.acquire_trait_proof(data, parent).unwrap();
        assert_ne!(projected, independent);
        assert_eq!(vm.shared.trait_registry.read(0).proofs.len(), 3);
        assert!(vm.assert_trait_pair(projected, data, parent).is_ok());
        assert!(vm.assert_trait_pair(independent, data, parent).is_ok());
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
                    assoc_type: None,
                },
                ScopeContext {
                    parent: Some(0),
                    package: 0,
                    assoc_type: None,
                },
            ])
            .unwrap();
        let mut method = vm.type_pool_read().trait_impls_snapshot()[0].methods[0].clone();
        method.visible_scope = Some(2);
        vm.type_pool_write().add_method(concrete, method.clone());
        vm.type_pool_write().add_trait_impl(TraitImplRecord {
            implementor: concrete,
            trait_type: view,
            visible_scope: Some(2),
            methods: vec![method],
        });
        vm.type_pool_write().add_vtable(VTable {
            implementor: concrete,
            trait_type: view,
            visible_scope: Some(2),
            entries: vec![1],
        });
        vm.worker.type_query_scope = Some(2);
        let sibling = vm.acquire_trait_proof(data, view).unwrap();
        assert_ne!(sibling, proof);
        assert_eq!(vm.shared.trait_registry.read(0).proofs.len(), 4);
        assert_eq!(
            vm.proof(sibling).unwrap().descriptors[0].implementation_scope,
            Some(2)
        );
        assert_eq!(
            vm.proof(proof).unwrap().descriptors[0].implementation_scope,
            Some(1)
        );
    }

    #[test]
    fn captured_proofs_validate_their_data_before_creation_and_invocation() {
        let (mut vm, view, _) = fixture();
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi {
                captures: vec![CaptureAbi::Value, CaptureAbi::TraitProof { view }],
                parameters: vec![],
            },
            func_id: FuncId(2),
            instructions: vec![Instruction::return_unit().encode()],
            register_count: 32,
            param_count: 2,
            is_closure: true,
            function_type: TypeIndex::INVALID,
        });
        let task = vm.spawn_root(FuncId(0));
        let proof = vm
            .acquire_trait_proof(TaggedValue::from_i64(42), view)
            .unwrap();
        let state = vm.task_mut(task).unwrap();
        state.registers.set(Reg(0), TaggedValue::TRUE);
        state.registers.set(Reg(1), proof);
        assert!(matches!(
            vm.exec_new_closure(task, Reg(3), FuncId(2), 2),
            DispatchResult::Error(VmError::InvalidTraitProof)
        ));
        // SAFETY: TestVm owns the operation. The initialized closure is rooted
        // immediately, before any further allocation or collection can occur.
        let pointer = unsafe {
            vm.heap_mut()
                .alloc_object(
                    type_pool::Intrinsic::Closure.type_index(),
                    runtime::ClosureEnv::payload_words(2),
                )
                .unwrap()
        };
        let environment = unsafe {
            runtime::ClosureEnv::write(pointer.as_ptr(), FuncId(2), &[TaggedValue::TRUE, proof]);
            TaggedValue::from_heap_ptr(pointer.as_ptr())
        };
        vm.task_mut(task)
            .unwrap()
            .registers
            .set(Reg(20), environment);
        assert!(matches!(
            vm.invoke_closure_physical(task, FuncId(2), Some(environment), &[]),
            DispatchResult::Error(VmError::InvalidTraitProof)
        ));
        assert!(vm.task_ref(task).unwrap().call_stack.is_empty());
    }
}
