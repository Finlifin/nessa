//! Checked runtime type predicates and explicit conversions.

use gc::ObjectHeader;
use nsbc::Instruction;
use runtime::{Number, TaggedValue, TaskId};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::collections::write_u64_atomic;
use crate::{DispatchResult, Vm, VmError, a_fields};

impl Vm {
    fn closure_type(&self, payload: *const u8, words: usize) -> Result<Option<TypeIndex>, VmError> {
        if words < 2 {
            return Err(VmError::TypeError);
        }
        // SAFETY: callers hold an execution operation and root the live closure;
        // the validated header guarantees two initialized metadata words.
        let (id, captures) = unsafe {
            (
                payload.cast::<u64>().read(),
                payload.cast::<u64>().add(1).read(),
            )
        };
        let id = u32::try_from(id).map_err(|_| VmError::TypeError)?;
        let captures = usize::try_from(captures).map_err(|_| VmError::TypeError)?;
        if captures != words - 2 {
            return Err(VmError::TypeError);
        }
        let function = self
            .shared
            .bytecode
            .try_get_function(nsbc::FuncId(id))
            .ok_or(VmError::InvalidFunction(nsbc::FuncId(id)))?;
        self.check_capture_abi(nsbc::FuncId(id), captures)?;
        if function.function_type == TypeIndex::INVALID {
            return Ok(None);
        }
        let ty = self.canonical_type(function.function_type)?;
        let param_len = {
            let tp = self.type_pool_read();
            let TypeKind::Function { params, .. } = &tp.get(ty).kind else {
                return Err(VmError::InvalidType(ty));
            };
            params.len()
        };
        if param_len != function.abi.logical_parameter_count() {
            return Err(VmError::TypeError);
        }
        Ok(Some(ty))
    }

    /// Construct a pool-local descriptor, checking metadata and transparent aliases.
    pub(crate) fn type_value(&self, ty: TypeIndex) -> Result<TaggedValue, VmError> {
        let ty = self.canonical_type(ty)?;
        TaggedValue::from_type(ty).ok_or(VmError::InvalidType(ty))
    }

    pub(crate) fn reflected_type(&self, value: TaggedValue) -> Result<TypeIndex, VmError> {
        let ty = match self.value_type(value)? {
            Some(ty) => ty,
            None => self.type_pool_read().null_type(),
        };
        self.canonical_type(ty)
    }

    pub(super) fn canonical_type(&self, ty: TypeIndex) -> Result<TypeIndex, VmError> {
        let raw = ty.as_u32() as usize;
        if let Some(Some(cached)) = self.sync_meta_cache().canonical_types.get(raw).copied() {
            return Ok(cached);
        }
        let canonical = {
            let tp = self.type_pool_read();
            let canonical = tp.canonical_type(ty).ok_or(VmError::InvalidType(ty))?;
            // Declarations can hold symbolic associated types, but runtime values
            // and conversions require their concrete implementation bindings.
            if tp.contains_associated_type(canonical) {
                return Err(VmError::InvalidType(ty));
            }
            canonical
        };
        if raw < 1_048_576 {
            let mut cache = self.sync_meta_cache();
            if cache.canonical_types.len() <= raw {
                cache.canonical_types.resize(raw + 1, None);
            }
            cache.canonical_types[raw] = Some(canonical);
        }
        Ok(canonical)
    }

    pub(crate) fn cached_heap_type_meta(
        &self,
        heap_type: TypeIndex,
    ) -> Result<crate::CachedHeapTypeMeta, VmError> {
        let raw = heap_type.as_u32() as usize;
        if let Some(Some(meta)) = self.sync_meta_cache().heap_type_meta.get(raw).copied() {
            return Ok(meta);
        }
        let meta = {
            let tp = self.type_pool_read();
            if tp.as_intrinsic(heap_type).is_some() {
                crate::CachedHeapTypeMeta::Slow
            } else {
                match &tp.get(heap_type).kind {
                    TypeKind::Struct { fields, .. } => {
                        match tp
                            .checked_collection_role(heap_type)
                            .map_err(|_| VmError::TypeError)?
                        {
                            Some(
                                role @ (type_pool::CollectionRole::List
                                | type_pool::CollectionRole::Map),
                            ) => crate::CachedHeapTypeMeta::Collection(role),
                            Some(_) => crate::CachedHeapTypeMeta::Slow,
                            None => match u16::try_from(fields.len()) {
                                Ok(expected_words) => {
                                    crate::CachedHeapTypeMeta::PlainStruct { expected_words }
                                }
                                Err(_) => crate::CachedHeapTypeMeta::Slow,
                            },
                        }
                    }
                    TypeKind::Newtype { .. } | TypeKind::Future { .. } => {
                        crate::CachedHeapTypeMeta::SingleWord
                    }
                    _ => crate::CachedHeapTypeMeta::Slow,
                }
            }
        };
        if raw < 1_048_576 {
            let mut cache = self.sync_meta_cache();
            if cache.heap_type_meta.len() <= raw {
                cache.heap_type_meta.resize(raw + 1, None);
            }
            cache.heap_type_meta[raw] = Some(meta);
        }
        Ok(meta)
    }

    /// Only called with rooted VM values inside the execution operation.
    pub(super) fn value_type(&self, value: TaggedValue) -> Result<Option<TypeIndex>, VmError> {
        self.value_type_depth(value, 0)
    }

    pub(super) fn value_type_depth(
        &self,
        value: TaggedValue,
        depth: usize,
    ) -> Result<Option<TypeIndex>, VmError> {
        if depth >= crate::error_values::MAX_VALUE_DEPTH {
            return Err(VmError::TypeError);
        }
        if let Some(layout) = self.error_layout(value, depth)? {
            return Ok(Some(layout.ty));
        }
        let ty = if value.as_i64().is_some() {
            Intrinsic::I64
        } else if value.as_u64().is_some() {
            Intrinsic::U64
        } else if value.as_f64().is_some() {
            Intrinsic::F64
        } else if value.as_bool().is_some() {
            Intrinsic::Bool
        } else if value.as_char().is_some() {
            Intrinsic::Char
        } else if let Some(ty) = value.as_type() {
            self.canonical_type(ty)?;
            Intrinsic::Type
        } else if value.as_enum().is_some() {
            return self
                .enum_layout(value)
                .map(|layout| layout.map(|layout| layout.ty));
        } else if value.is_unit() {
            Intrinsic::Unit
        } else if value.is_null() {
            return Ok(None);
        } else if let Some(pointer) = value.as_heap_ptr() {
            // SAFETY: the caller holds the operation and the source register
            // roots this live object. No collection occurs during inspection.
            let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
            if !header.is_ordinary() {
                return Err(VmError::TypeError);
            }
            let heap_type = self.canonical_type(header.type_index)?;
            match self.cached_heap_type_meta(heap_type)? {
                crate::CachedHeapTypeMeta::Collection(_) => return Ok(Some(heap_type)),
                crate::CachedHeapTypeMeta::PlainStruct { expected_words } => {
                    return if header.payload_words() == expected_words as usize {
                        Ok(Some(heap_type))
                    } else {
                        Err(VmError::TypeError)
                    };
                }
                crate::CachedHeapTypeMeta::SingleWord => {
                    return if header.payload_words() == 1 {
                        Ok(Some(heap_type))
                    } else {
                        Err(VmError::TypeError)
                    };
                }
                crate::CachedHeapTypeMeta::Slow => {}
            }
            let (intrinsic_kind, is_enum, collection_role) = {
                let tp = self.type_pool_read();
                let intrinsic_kind = tp.as_intrinsic(heap_type);
                let collection_role = tp
                    .checked_collection_role(heap_type)
                    .map_err(|_| VmError::TypeError)?;
                if let TypeKind::Struct { fields, .. } = &tp.get(heap_type).kind
                    && collection_role.is_none()
                    && header.payload_words() != fields.len()
                {
                    return Err(VmError::TypeError);
                }
                if matches!(
                    tp.get(heap_type).kind,
                    TypeKind::Newtype { .. } | TypeKind::Future { .. }
                ) && header.payload_words() != 1
                {
                    return Err(VmError::TypeError);
                }
                let is_enum = matches!(tp.get(heap_type).kind, TypeKind::Enum { .. });
                (intrinsic_kind, is_enum, collection_role)
            };
            match collection_role {
                Some(type_pool::CollectionRole::Bytes) => {
                    self.bytes_layout(value)?;
                }
                Some(
                    type_pool::CollectionRole::Buffer
                    | type_pool::CollectionRole::MapBuffer
                    | type_pool::CollectionRole::BytesBuffer,
                ) => {
                    return Err(VmError::TypeError);
                }
                _ => {}
            }
            if let Some(kind) = intrinsic_kind {
                if kind.is_numeric() {
                    // SAFETY: live ordinary object, retained in caller roots.
                    if unsafe { Number::from_tagged(value) }.is_none() {
                        return Err(VmError::TypeError);
                    }
                } else if kind == Intrinsic::Str {
                    if crate::builtin_ctx::heap_string_to_owned(value).is_none() {
                        return Err(VmError::TypeError);
                    }
                } else if kind == Intrinsic::Continuation {
                    if header.payload_words() != 1 {
                        return Err(VmError::TypeError);
                    }
                } else if kind != Intrinsic::Closure {
                    return Err(VmError::TypeError);
                }
            }
            if header.type_index == Intrinsic::Closure.type_index() {
                return self
                    .closure_type(pointer, header.payload_words())
                    .map(|signature| Some(signature.unwrap_or(Intrinsic::Closure.type_index())));
            }
            if is_enum {
                return self
                    .enum_layout(value)
                    .map(|layout| layout.map(|layout| layout.ty));
            }
            return Ok(Some(heap_type));
        } else {
            return Err(VmError::TypeError);
        };
        Ok(Some(ty.type_index()))
    }

    pub(super) fn value_matches_type(
        &self,
        value: TaggedValue,
        target: TypeIndex,
        depth: usize,
    ) -> Result<bool, VmError> {
        let pool_len = self.type_pool_read().len();
        if depth >= pool_len || depth >= crate::error_values::MAX_VALUE_DEPTH {
            return Err(VmError::InvalidType(target));
        }
        let target = self.canonical_type(target)?;
        let envelope = self.error_layout(value, depth)?;
        let target_kind = self.type_pool_read().get(target).kind.clone();
        match target_kind {
            TypeKind::Intrinsic(Intrinsic::Any) => {
                self.value_type_depth(value, depth + 1)?;
                Ok(true)
            }
            TypeKind::Intrinsic(Intrinsic::NoReturn) => Ok(false),
            TypeKind::Optional { inner } => {
                if value.is_null() {
                    Ok(true)
                } else {
                    self.value_matches_type(value, inner, depth + 1)
                }
            }
            TypeKind::EffectQualified { inner, .. } => {
                self.value_matches_type(value, inner, depth + 1)
            }
            TypeKind::ErrorQualified { .. } => {
                let Some(layout) = envelope else {
                    return Ok(false);
                };
                let (inner, members) = self.error_shape(target)?;
                if layout.tag == type_pool::TypeId::ZERO {
                    self.value_matches_type(layout.payload, inner, depth + 1)
                } else {
                    let concrete = self
                        .type_pool_read()
                        .lookup_by_id(layout.tag)
                        .ok_or(VmError::TypeError)?;
                    self.error_member(concrete, &members)
                }
            }
            TypeKind::Trait { .. } => {
                let Some(actual) = self.value_type(value)? else {
                    return Ok(false);
                };
                if let Some(scope) = self.worker.type_query_scope {
                    return self
                        .type_pool_read()
                        .has_trait_impl_scoped(actual, target, scope)
                        .map_err(VmError::TraitLookup);
                }
                // Legacy bytecode and host queries have no lexical authority.
                // Do not silently reinterpret a scoped proof as a global one.
                let actual = self.canonical_type(actual)?;
                let tp = self.type_pool_read();
                if tp.trait_impls_snapshot().iter().any(|record| {
                    record.visible_scope.is_some()
                        && tp.canonical_type(record.implementor) == Some(actual)
                        && tp.canonical_type(record.trait_type) == Some(target)
                }) {
                    return Err(VmError::MissingTraitContext);
                }
                Ok(tp.has_trait_impl(actual, target))
            }
            TypeKind::Function { .. } => {
                Ok(self
                    .value_type_depth(value, depth + 1)?
                    .is_some_and(|actual| {
                        self.type_pool_read()
                            .is_gradually_consistent(actual, target)
                    }))
            }
            TypeKind::Future { .. } => {
                Ok(self
                    .value_type_depth(value, depth + 1)?
                    .is_some_and(|actual| {
                        let tp = self.type_pool_read();
                        matches!(tp.get(actual).kind, TypeKind::Future { .. })
                            && (tp.is_subtype(actual, target)
                                || tp.is_gradually_consistent(actual, target))
                    }))
            }
            TypeKind::Tuple { elements } => {
                let values = match self.tuple_values(value) {
                    Ok(values) => values,
                    Err(VmError::TypeError) => return Ok(false),
                    Err(error) => return Err(error),
                };
                if values.len() != elements.len() {
                    return Ok(false);
                }
                for (&value, &expected) in values.iter().zip(&elements) {
                    if !self.value_matches_type(value, expected, depth + 1)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => Ok(self
                .value_type_depth(value, depth + 1)?
                .is_some_and(|actual| {
                    actual == target
                        || (actual.as_u32() <= Intrinsic::F64.type_index().as_u32()
                            && target.as_u32() <= Intrinsic::F64.type_index().as_u32()
                            && self.type_pool_read().is_subtype(actual, target))
                })),
        }
    }

    /// Check a gradual boundary and preserve the requested representation.
    pub(super) fn assert_value_type(
        &mut self,
        value: TaggedValue,
        target: TypeIndex,
    ) -> Result<TaggedValue, VmError> {
        self.assert_value_type_depth(value, target, 0)
    }

    #[inline]
    pub(crate) fn value_is_exact_intrinsic_fast(value: TaggedValue, target: TypeIndex) -> bool {
        if target == Intrinsic::I64.type_index() {
            return value.as_i64().is_some();
        }
        if target == Intrinsic::U64.type_index() {
            return value.as_u64().is_some();
        }
        if target == Intrinsic::F64.type_index() {
            return value.as_f64().is_some();
        }
        if target == Intrinsic::Bool.type_index() {
            return value.as_bool().is_some();
        }
        if target == Intrinsic::Char.type_index() {
            return value.as_char().is_some();
        }
        if target == Intrinsic::Unit.type_index() {
            return value.is_unit();
        }
        if target == Intrinsic::Str.type_index()
            && let Some(ptr) = value.as_heap_ptr()
        {
            // SAFETY: caller holds a mutator operation and `value` is a live VM value.
            let header = unsafe { ObjectHeader::from_payload_ptr(ptr) };
            return header.is_ordinary() && header.type_index == Intrinsic::Str.type_index();
        }
        false
    }

    fn assert_value_type_depth(
        &mut self,
        value: TaggedValue,
        target: TypeIndex,
        depth: usize,
    ) -> Result<TaggedValue, VmError> {
        if depth >= crate::error_values::MAX_VALUE_DEPTH {
            return Err(VmError::TypeError);
        }
        if Self::value_is_exact_intrinsic_fast(value, target) {
            return Ok(value);
        }
        let canonical = self.canonical_type(target)?;
        let kind = self.type_pool_read().get(canonical).kind.clone();
        if matches!(kind, TypeKind::ErrorQualified { .. }) {
            return self.assert_error(value, canonical, depth);
        }
        match kind {
            TypeKind::Optional { inner } if !value.is_null() => {
                return self.assert_value_type_depth(value, inner, depth + 1);
            }
            TypeKind::EffectQualified { inner, .. } => {
                return self.assert_value_type_depth(value, inner, depth + 1);
            }
            _ => {}
        }
        if !self.value_matches_type(value, target, depth)? {
            return Err(VmError::TypeError);
        }
        self.cast_value(value, target, depth)
    }

    pub(super) fn cast_value(
        &mut self,
        value: TaggedValue,
        target: TypeIndex,
        depth: usize,
    ) -> Result<TaggedValue, VmError> {
        if depth < crate::error_values::MAX_VALUE_DEPTH
            && Self::value_is_exact_intrinsic_fast(value, target)
        {
            return Ok(value);
        }
        let pool_len = self.type_pool_read().len();
        if depth >= pool_len || depth >= crate::error_values::MAX_VALUE_DEPTH {
            return Err(VmError::InvalidType(target));
        }
        let target = self.canonical_type(target)?;
        let target_kind = self.type_pool_read().get(target).kind.clone();
        if matches!(target_kind, TypeKind::ErrorQualified { .. }) {
            return self.convert_error(value, target, depth);
        }
        match &target_kind {
            TypeKind::Optional { inner } if !value.is_null() => {
                return self.cast_value(value, *inner, depth + 1);
            }
            TypeKind::EffectQualified { inner, .. } => {
                return self.cast_value(value, *inner, depth + 1);
            }
            _ => {}
        }
        if let Some(layout) = self.error_layout(value, depth)? {
            if target == Intrinsic::Any.type_index() {
                return Ok(value);
            }
            if layout.tag != type_pool::TypeId::ZERO {
                return Err(VmError::TypeError);
            }
            return self.cast_value(layout.payload, target, depth + 1);
        }
        if target != Intrinsic::Any.type_index()
            && let Some(actual) = self.value_type_depth(value, depth + 1)?
            && actual != target
        {
            let maybe_newtype_inner = match self.type_pool_read().get(actual).kind {
                TypeKind::Newtype { inner, .. } => Some(inner),
                _ => None,
            };
            if let Some(inner) = maybe_newtype_inner
                && self.canonical_type(inner)? == target
            {
                return self.load_field_value(value, 0);
            }
        }
        match target_kind {
            TypeKind::Intrinsic(kind) if kind.is_numeric() => {
                // SAFETY: the source register keeps value rooted until this
                // scalar snapshot has been read. Allocation follows the read.
                let number = unsafe { Number::from_tagged(value) }.ok_or(VmError::TypeError)?;
                self.typed_number_value(number.cast_to(kind)?, kind)
            }
            TypeKind::Newtype { inner, .. } => {
                if self.value_matches_type(value, target, depth)? {
                    return Ok(value);
                }
                if !self.value_matches_type(value, inner, depth + 1)? {
                    return Err(VmError::TypeError);
                }
                let root = self.worker_roots().temporary_values.borrow().len();
                self.with_temporary_roots(&[value], |vm| {
                    let value = vm.worker_roots().temporary_values.borrow()[root];
                    let inner_val = vm.assert_value_type_depth(value, inner, depth + 1)?;
                    let inner_root = vm.worker_roots().temporary_values.borrow().len();
                    vm.with_temporary_roots(&[inner_val], |vm| {
                        // SAFETY: execution operation is active and inner_val is rooted in
                        // temporary_values across alloc_object. Payload is initialized before return.
                        let ptr = unsafe { vm.worker.heap.alloc_object(target, 1) }
                            .ok_or(VmError::OutOfMemory)?;
                        let rooted_inner = vm.worker_roots().temporary_values.borrow()[inner_root];
                        write_u64_atomic(ptr.as_ptr().cast::<u64>(), 0, rooted_inner.raw());
                        // SAFETY: `ptr` points to a fresh, fully initialized 1-word heap object.
                        Ok(unsafe { TaggedValue::from_heap_ptr(ptr.as_ptr()) })
                    })
                })
            }
            TypeKind::Tuple { elements } => self.cast_tuple(value, target, &elements, depth + 1),
            TypeKind::Optional { inner } if !value.is_null() => {
                self.cast_value(value, inner, depth + 1)
            }
            TypeKind::EffectQualified { inner, .. } => self.cast_value(value, inner, depth + 1),
            _ if self.value_matches_type(value, target, depth)? => Ok(value),
            _ => Err(VmError::TypeError),
        }
    }

    /// Numeric casts preserve the concrete target type even for small values.
    /// Narrow types currently require a box because immediate subtags erase width.
    pub(crate) fn typed_number_value(
        &mut self,
        number: Number,
        kind: Intrinsic,
    ) -> Result<TaggedValue, VmError> {
        if matches!(
            kind,
            Intrinsic::I64 | Intrinsic::U64 | Intrinsic::I128 | Intrinsic::U128 | Intrinsic::F64
        ) {
            return self.number_value(number);
        }
        let bits = match number {
            Number::I64(value) => value as u64,
            Number::U64(value) => value,
            Number::F64(value) => value.to_bits(),
            _ => return Err(VmError::TypeError),
        };
        // SAFETY: the execution operation is active and number is an owned
        // scalar. Initialize and publish without a second allocation/safepoint.
        let payload = unsafe { self.worker.heap.alloc_object(kind.type_index(), 1) }
            .ok_or(VmError::OutOfMemory)?;
        unsafe {
            payload.as_ptr().cast::<u64>().write(bits);
            Ok(TaggedValue::from_heap_ptr(payload.as_ptr()))
        }
    }

    pub(crate) fn dispatch_type_operation(
        &mut self,
        task: TaskId,
        instruction: Instruction,
    ) -> DispatchResult {
        let (destination, source, index) = a_fields(&instruction);
        let Some(task_ref) = self.task_ref(task) else {
            return DispatchResult::Error(VmError::InvalidTask);
        };
        let value = task_ref.registers.get(source);
        let target = TypeIndex::from_raw(index as u32);
        let result = if instruction.opcode == nsbc::Opcode::TypeCheck {
            self.value_matches_type(value, target, 0)
                .map(TaggedValue::from_bool)
        } else if instruction.opcode == nsbc::Opcode::TypeAssert {
            self.assert_value_type(value, target)
        } else {
            // Validate metadata independently: safe casts suppress value
            // mismatches, never malformed targets or allocation failures.
            if let Err(error) = self.canonical_type(target) {
                return DispatchResult::Error(error);
            }
            match self.cast_value(value, target, 0) {
                Err(VmError::TypeError | VmError::NumericOverflow)
                    if instruction.opcode == nsbc::Opcode::TypeCastSafe =>
                {
                    Ok(TaggedValue::NULL)
                }
                result => result,
            }
        };
        match result {
            Ok(value) => {
                if let Some(t) = self.task_mut(task) {
                    t.registers.set(destination, value);
                    DispatchResult::Continue
                } else {
                    DispatchResult::Error(VmError::InvalidTask)
                }
            }
            Err(error) => DispatchResult::Error(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use nsbc::{AddrMode, Constant, Opcode, Reg};
    use runtime::Number;

    use super::*;

    fn apply(
        opcode: Opcode,
        constant: Constant,
        target: TypeIndex,
    ) -> (crate::tests::TestVm, TaskId, DispatchResult) {
        let mut vm = crate::tests::make_vm();
        let index = vm.push_constant(&constant).unwrap();
        let task = vm.spawn_root(nsbc::FuncId(0));
        let value = vm.roots().constants.read(0).get(index);
        vm.task_mut(task).unwrap().registers.set(Reg(0), value);
        let result = vm.dispatch_type_operation(
            task,
            Instruction::a_type(
                opcode,
                AddrMode::Imm,
                Reg(1),
                Reg(0),
                target.as_u32() as u16,
            ),
        );
        (vm, task, result)
    }

    #[test]
    fn type_checks_use_actual_value_categories() {
        for (constant, target, expected) in [
            (Constant::Int(42), Intrinsic::Bool, false),
            (Constant::Int(42), Intrinsic::I64, true),
            (Constant::Str("a".into()), Intrinsic::Str, true),
            (Constant::Str("a".into()), Intrinsic::I64, false),
            (Constant::UInt128(u128::MAX), Intrinsic::U128, true),
            (Constant::UInt128(u128::MAX), Intrinsic::I64, false),
            (Constant::Float(0.1), Intrinsic::F64, true),
            (Constant::Int(42), Intrinsic::Any, true),
            (Constant::Int(42), Intrinsic::NoReturn, false),
        ] {
            let (vm, task, result) = apply(Opcode::TypeCheck, constant, target.type_index());
            assert!(matches!(result, DispatchResult::Continue));
            assert_eq!(
                vm.task_ref(task).unwrap().registers.get(Reg(1)).as_bool(),
                Some(expected)
            );
        }
    }

    #[test]
    fn closure_type_checks_use_function_metadata_and_validate_capture_layout() {
        let mut vm = crate::tests::make_vm();
        let signature = vm.type_pool_write().intern_structural(TypeKind::Function {
            params: vec![Intrinsic::I64.type_index()],
            ret: Intrinsic::I64.type_index(),
        });
        let wrong = vm.type_pool_write().intern_structural(TypeKind::Function {
            params: vec![Intrinsic::Bool.type_index()],
            ret: Intrinsic::I64.type_index(),
        });
        vm.add_function(runtime::FunctionCode {
            display_owner: None,
            abi: nsbc::FunctionAbi::value_only(1),
            func_id: nsbc::FuncId(0),
            instructions: vec![],
            register_count: 32,
            param_count: 1,
            is_closure: false,
            function_type: signature,
        });
        let task = vm.spawn_root(nsbc::FuncId(0));
        // SAFETY: TestVm owns an operation; initialize and publish this exact
        // two-word closure before any further managed allocation or safepoint.
        let payload = unsafe {
            vm.heap_mut()
                .alloc_object(Intrinsic::Closure.type_index(), 2)
        }
        .unwrap();
        unsafe {
            runtime::ClosureEnv::write(payload.as_ptr(), nsbc::FuncId(0), &[]);
        }
        let value = unsafe { TaggedValue::from_heap_ptr(payload.as_ptr()) };
        vm.task_mut(task).unwrap().registers.set(Reg(0), value);
        assert!(vm.value_matches_type(value, signature, 0).unwrap());
        assert!(!vm.value_matches_type(value, wrong, 0).unwrap());
        assert_eq!(vm.reflected_type(value).unwrap(), signature);
        // SAFETY: exclusively borrowed fixture VM owns the live payload. A
        // count of one cannot fit this object's two metadata-only words.
        unsafe {
            payload.as_ptr().cast::<u64>().add(1).write(1);
        }
        assert!(matches!(vm.reflected_type(value), Err(VmError::TypeError)));
    }

    #[test]
    fn type_descriptors_validate_pool_bounds_and_format_names() {
        let mut vm = crate::tests::make_vm();
        assert!(matches!(
            vm.push_constant(&Constant::Type(TypeIndex::from_raw(4095))),
            Err(VmError::InvalidType(_))
        ));
        assert!(matches!(
            vm.type_value(TypeIndex::INVALID),
            Err(VmError::InvalidType(_))
        ));
        let value = vm.type_value(Intrinsic::I64.type_index()).unwrap();
        assert_eq!(
            vm.reflected_type(value).unwrap(),
            Intrinsic::Type.type_index()
        );
        let task = vm.spawn_root(nsbc::FuncId(0));
        let ctx = crate::BuiltinCtx::new(&mut vm, task, 0);
        assert_eq!(ctx.format_value(value).unwrap(), "i64");
        let null_ty = ctx.vm.type_pool_read().null_type();
        let value = ctx.vm.type_value(null_ty).unwrap();
        assert_eq!(ctx.format_value(value).unwrap(), "?NoReturn");
        drop(ctx);
        *vm.type_pool_write() = type_pool::TypePool::new();
        assert!(matches!(
            vm.reflected_type(TaggedValue::NULL),
            Err(VmError::InvalidType(_))
        ));
    }

    #[test]
    fn implicit_assertions_allow_widening_but_never_truncate() {
        let (_, _, result) = apply(
            Opcode::TypeAssert,
            Constant::Float(1.5),
            Intrinsic::I64.type_index(),
        );
        assert!(matches!(result, DispatchResult::Error(VmError::TypeError)));
        let (vm, task, result) = apply(
            Opcode::TypeAssert,
            Constant::Int(2),
            Intrinsic::F64.type_index(),
        );
        assert!(matches!(result, DispatchResult::Continue));
        let value = vm.task_ref(task).unwrap().registers.get(Reg(1));
        // SAFETY: the fixture VM owns the live value and no allocation occurs.
        assert_eq!(
            unsafe { Number::from_tagged(value) }.unwrap(),
            Number::F64(2.0)
        );
    }

    #[test]
    fn safe_casts_return_null_on_value_failure_but_reject_bad_metadata() {
        for constant in [Constant::Int(300), Constant::Str("a".into())] {
            let (vm, task, result) =
                apply(Opcode::TypeCastSafe, constant, Intrinsic::I8.type_index());
            assert!(matches!(result, DispatchResult::Continue));
            assert!(vm.task_ref(task).unwrap().registers.get(Reg(1)).is_null());
        }
        let (_, _, result) = apply(
            Opcode::TypeCastSafe,
            Constant::Int(42),
            TypeIndex::from_raw(4095),
        );
        assert!(matches!(
            result,
            DispatchResult::Error(VmError::InvalidType(_))
        ));
    }

    #[test]
    fn narrow_numeric_casts_preserve_target_identity() {
        let (vm, task, result) = apply(
            Opcode::TypeCast,
            Constant::Int(42),
            Intrinsic::I8.type_index(),
        );
        assert!(matches!(result, DispatchResult::Continue));
        let value = vm.task_ref(task).unwrap().registers.get(Reg(1));
        assert!(value.is_heap());
        assert_eq!(
            vm.value_type(value).unwrap(),
            Some(Intrinsic::I8.type_index())
        );
        assert!(
            vm.value_matches_type(value, Intrinsic::I8.type_index(), 0)
                .unwrap()
        );
        assert!(
            !vm.value_matches_type(value, Intrinsic::U8.type_index(), 0)
                .unwrap()
        );
        // SAFETY: TestVm owns an operation, with value rooted in the register.
        assert_eq!(unsafe { Number::from_tagged(value) }, Some(Number::I64(42)));
    }

    #[test]
    fn optional_types_and_aliases_test_null_and_inner_values() {
        let vm = crate::tests::make_vm();
        let optional = vm.type_pool_write().register(type_pool::TypeInfo {
            kind: TypeKind::Optional {
                inner: Intrinsic::Bool.type_index(),
            },
            type_id: type_pool::TypeId::ZERO,
            size: 8,
            align: 8,
        });
        let alias = vm.type_pool_write().register(type_pool::TypeInfo {
            kind: TypeKind::Typealias {
                name: str_interner::intern("MaybeBool"),
                target: optional,
            },
            type_id: type_pool::TypeId::ZERO,
            size: 8,
            align: 8,
        });
        for target in [optional, alias] {
            assert!(vm.value_matches_type(TaggedValue::NULL, target, 0).unwrap());
            assert!(vm.value_matches_type(TaggedValue::TRUE, target, 0).unwrap());
            assert!(
                !vm.value_matches_type(TaggedValue::from_i64(1), target, 0)
                    .unwrap()
            );
        }
        assert!(
            !vm.value_matches_type(TaggedValue::NULL, Intrinsic::Unit.type_index(), 0)
                .unwrap()
        );
    }

    #[test]
    fn malformed_alias_cycles_produce_an_error_without_recursing() {
        let vm = crate::tests::make_vm();
        let index = TypeIndex::from_raw(vm.type_pool_read().len() as u32);
        let alias = vm.type_pool_write().register(type_pool::TypeInfo {
            kind: TypeKind::Typealias {
                name: str_interner::intern("Cycle"),
                target: index,
            },
            type_id: type_pool::TypeId::ZERO,
            size: 8,
            align: 8,
        });
        assert_eq!(alias, index);
        assert!(matches!(
            vm.canonical_type(alias),
            Err(VmError::InvalidType(_))
        ));
    }

    #[test]
    fn unresolved_associated_types_are_not_runtime_values_even_under_optional() {
        let mut vm = crate::tests::make_vm();
        let owner = vm.type_pool_read().well_known.display;
        let symbolic = vm.type_pool_write().register(type_pool::TypeInfo {
            kind: TypeKind::AssociatedType {
                trait_owner: owner,
                name: str_interner::intern("Item"),
            },
            type_id: type_pool::TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let optional = vm
            .type_pool_write()
            .intern_structural(TypeKind::Optional { inner: symbolic });
        let function = vm.type_pool_write().intern_structural(TypeKind::Function {
            params: vec![],
            ret: optional,
        });
        for target in [symbolic, optional, function] {
            assert!(matches!(
                vm.type_value(target),
                Err(VmError::InvalidType(_))
            ));
            assert!(matches!(
                vm.value_matches_type(TaggedValue::NULL, target, 0),
                Err(VmError::InvalidType(_))
            ));
            assert!(matches!(
                vm.cast_value(TaggedValue::NULL, target, 0),
                Err(VmError::InvalidType(_))
            ));
            // Host-created descriptors also undergo the same runtime check.
            assert!(matches!(
                vm.reflected_type(TaggedValue::from_type(target).unwrap()),
                Err(VmError::InvalidType(_))
            ));
        }
        let concrete = vm.type_pool_write().intern_structural(TypeKind::Optional {
            inner: Intrinsic::Str.type_index(),
        });
        assert!(
            vm.value_matches_type(TaggedValue::NULL, concrete, 0)
                .unwrap()
        );
    }

    #[test]
    fn trait_queries_need_lexical_authority_and_never_borrow_a_previous_query() {
        let mut vm = crate::tests::make_vm();
        let mut pool = type_pool::TypePool::with_intrinsics();
        pool.install_scopes(vec![
            type_pool::ScopeContext {
                parent: None,
                package: 0,
                assoc_type: None,
            },
            type_pool::ScopeContext {
                parent: Some(0),
                package: 0,
                assoc_type: None,
            },
        ])
        .unwrap();
        let marker = pool.well_known.eq;
        pool.add_trait_impl(type_pool::TraitImplRecord {
            implementor: Intrinsic::I64.type_index(),
            trait_type: marker,
            visible_scope: Some(1),
            methods: vec![],
        });
        vm.install_type_pool(pool);
        let value = TaggedValue::from_i64(42);
        assert!(matches!(
            vm.value_matches_type(value, marker, 0),
            Err(VmError::MissingTraitContext)
        ));
        vm.worker.type_query_scope = Some(1);
        assert!(vm.value_matches_type(value, marker, 0).unwrap());
        vm.worker.type_query_scope = Some(0);
        assert!(!vm.value_matches_type(value, marker, 0).unwrap());
        vm.worker.type_query_scope = None;
        assert!(matches!(
            vm.value_matches_type(value, marker, 0),
            Err(VmError::MissingTraitContext)
        ));
        vm.install_type_pool(type_pool::TypePool::with_intrinsics());
        assert!(vm.worker.type_query_scope.is_none());
    }
}
