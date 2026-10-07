//! Checked runtime type predicates and explicit conversions.

use gc::ObjectHeader;
use nsbc::Instruction;
use runtime::{Number, TaggedValue, TaskId};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

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
            .state
            .bytecode
            .try_get_function(nsbc::FuncId(id))
            .ok_or(VmError::InvalidFunction(nsbc::FuncId(id)))?;
        self.check_capture_abi(nsbc::FuncId(id), captures)?;
        if function.function_type == TypeIndex::INVALID {
            return Ok(None);
        }
        let ty = self.canonical_type(function.function_type)?;
        let TypeKind::Function { params, .. } = &self.state.type_pool.get(ty).kind else {
            return Err(VmError::InvalidType(ty));
        };
        let matching_parameters = match &function.abi {
            Some(abi) => params.len() == abi.logical_parameter_count(),
            None => params.len() + captures == function.param_count as usize,
        };
        if !matching_parameters {
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
        let ty = self
            .value_type(value)?
            .unwrap_or_else(|| self.state.type_pool.null_type());
        self.canonical_type(ty)
    }

    pub(super) fn canonical_type(&self, ty: TypeIndex) -> Result<TypeIndex, VmError> {
        let canonical = self
            .state
            .type_pool
            .canonical_type(ty)
            .ok_or(VmError::InvalidType(ty))?;
        // Declarations can hold symbolic associated types, but runtime values
        // and conversions require their concrete implementation bindings.
        if self.state.type_pool.contains_associated_type(canonical) {
            return Err(VmError::InvalidType(ty));
        }
        Ok(canonical)
    }

    /// Only called with rooted VM values inside the execution operation.
    fn value_type(&self, value: TaggedValue) -> Result<Option<TypeIndex>, VmError> {
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
            let heap_type = self.canonical_type(header.type_index)?;
            if let Some(kind) = self.state.type_pool.as_intrinsic(heap_type) {
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
            if let TypeKind::Struct { fields, .. } = &self.state.type_pool.get(heap_type).kind
                && self
                    .state
                    .type_pool
                    .checked_collection_role(heap_type)
                    .map_err(|_| VmError::TypeError)?
                    .is_none()
                && header.payload_words() != fields.len()
            {
                return Err(VmError::TypeError);
            }
            if header.type_index == Intrinsic::Closure.type_index() {
                return self
                    .closure_type(pointer, header.payload_words())
                    .map(|signature| Some(signature.unwrap_or(Intrinsic::Closure.type_index())));
            }
            if matches!(
                self.state
                    .type_pool
                    .get(self.canonical_type(header.type_index)?)
                    .kind,
                TypeKind::Enum { .. }
            ) {
                return self
                    .enum_layout(value)
                    .map(|layout| layout.map(|layout| layout.ty));
            }
            return Ok(Some(self.canonical_type(header.type_index)?));
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
        if depth >= self.state.type_pool.len() || depth >= crate::error_values::MAX_VALUE_DEPTH {
            return Err(VmError::InvalidType(target));
        }
        let target = self.canonical_type(target)?;
        let envelope = self.error_layout(value, depth)?;
        match &self.state.type_pool.get(target).kind {
            TypeKind::Intrinsic(Intrinsic::Any) => {
                self.value_type_depth(value, depth + 1)?;
                Ok(true)
            }
            TypeKind::Intrinsic(Intrinsic::NoReturn) => Ok(false),
            TypeKind::Optional { inner } => {
                if value.is_null() {
                    Ok(true)
                } else {
                    self.value_matches_type(value, *inner, depth + 1)
                }
            }
            TypeKind::EffectQualified { inner, .. } => {
                self.value_matches_type(value, *inner, depth + 1)
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
                        .state
                        .type_pool
                        .lookup_by_id(layout.tag)
                        .ok_or(VmError::TypeError)?;
                    self.error_member(concrete, &members)
                }
            }
            TypeKind::Trait { .. } => {
                let Some(actual) = self.value_type(value)? else {
                    return Ok(false);
                };
                if let Some(scope) = self.state.type_query_scope {
                    return self
                        .state
                        .type_pool
                        .has_trait_impl_scoped(actual, target, scope)
                        .map_err(VmError::TraitLookup);
                }
                // Legacy bytecode and host queries have no lexical authority.
                // Do not silently reinterpret a scoped proof as a global one.
                let actual = self.canonical_type(actual)?;
                if self
                    .state
                    .type_pool
                    .trait_impls_snapshot()
                    .iter()
                    .any(|record| {
                        record.visible_scope.is_some()
                            && self.state.type_pool.canonical_type(record.implementor)
                                == Some(actual)
                            && self.state.type_pool.canonical_type(record.trait_type)
                                == Some(target)
                    })
                {
                    return Err(VmError::MissingTraitContext);
                }
                Ok(self.state.type_pool.has_trait_impl(actual, target))
            }
            TypeKind::Function { .. } => {
                Ok(self
                    .value_type_depth(value, depth + 1)?
                    .is_some_and(|actual| {
                        self.state.type_pool.is_gradually_consistent(actual, target)
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
                for (&value, &expected) in values.iter().zip(elements) {
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
                            && self.state.type_pool.is_subtype(actual, target))
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

    fn assert_value_type_depth(
        &mut self,
        value: TaggedValue,
        target: TypeIndex,
        depth: usize,
    ) -> Result<TaggedValue, VmError> {
        if depth >= crate::error_values::MAX_VALUE_DEPTH {
            return Err(VmError::TypeError);
        }
        if matches!(
            self.state.type_pool.get(self.canonical_type(target)?).kind,
            TypeKind::ErrorQualified { .. }
        ) {
            return self.assert_error(value, self.canonical_type(target)?, depth);
        }
        match self.state.type_pool.get(self.canonical_type(target)?).kind {
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
        if depth >= self.state.type_pool.len() || depth >= crate::error_values::MAX_VALUE_DEPTH {
            return Err(VmError::InvalidType(target));
        }
        let target = self.canonical_type(target)?;
        if matches!(
            self.state.type_pool.get(target).kind,
            TypeKind::ErrorQualified { .. }
        ) {
            return self.convert_error(value, target, depth);
        }
        match self.state.type_pool.get(target).kind {
            TypeKind::Optional { inner } if !value.is_null() => {
                return self.cast_value(value, inner, depth + 1);
            }
            TypeKind::EffectQualified { inner, .. } => {
                return self.cast_value(value, inner, depth + 1);
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
        match self.state.type_pool.get(target).kind.clone() {
            TypeKind::Intrinsic(kind) if kind.is_numeric() => {
                // SAFETY: the source register keeps value rooted until this
                // scalar snapshot has been read. Allocation follows the read.
                let number = unsafe { Number::from_tagged(value) }.ok_or(VmError::TypeError)?;
                self.typed_number_value(number.cast_to(kind)?, kind)
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
    fn typed_number_value(
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
        let payload = unsafe { self.state.heap.alloc_object(kind.type_index(), 1) }
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
        let value = self
            .roots
            .scheduler
            .get_task(task)
            .unwrap()
            .registers
            .get(source);
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
                self.roots
                    .scheduler
                    .get_task_mut(task)
                    .unwrap()
                    .registers
                    .set(destination, value);
                DispatchResult::Continue
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
        let value = vm.roots.constants.get(index);
        vm.roots
            .scheduler
            .get_task_mut(task)
            .unwrap()
            .registers
            .set(Reg(0), value);
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
                vm.roots
                    .scheduler
                    .get_task(task)
                    .unwrap()
                    .registers
                    .get(Reg(1))
                    .as_bool(),
                Some(expected)
            );
        }
    }

    #[test]
    fn closure_type_checks_use_function_metadata_and_validate_capture_layout() {
        let mut vm = crate::tests::make_vm();
        let signature = vm.state.type_pool.intern_structural(TypeKind::Function {
            params: vec![Intrinsic::I64.type_index()],
            ret: Intrinsic::I64.type_index(),
        });
        let wrong = vm.state.type_pool.intern_structural(TypeKind::Function {
            params: vec![Intrinsic::Bool.type_index()],
            ret: Intrinsic::I64.type_index(),
        });
        vm.add_function(runtime::FunctionCode {
            display_owner: None,
            abi: None,
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
            vm.state
                .heap
                .alloc_object(Intrinsic::Closure.type_index(), 2)
        }
        .unwrap();
        unsafe {
            runtime::ClosureEnv::write(payload.as_ptr(), nsbc::FuncId(0), &[]);
        }
        let value = unsafe { TaggedValue::from_heap_ptr(payload.as_ptr()) };
        vm.roots
            .scheduler
            .get_task_mut(task)
            .unwrap()
            .registers
            .set(Reg(0), value);
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
        let value = ctx
            .vm
            .type_value(ctx.vm.state.type_pool.null_type())
            .unwrap();
        assert_eq!(ctx.format_value(value).unwrap(), "?NoReturn");
        drop(ctx);
        vm.state.type_pool = type_pool::TypePool::new();
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
        let value = vm
            .roots
            .scheduler
            .get_task(task)
            .unwrap()
            .registers
            .get(Reg(1));
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
            assert!(
                vm.roots
                    .scheduler
                    .get_task(task)
                    .unwrap()
                    .registers
                    .get(Reg(1))
                    .is_null()
            );
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
        let value = vm
            .roots
            .scheduler
            .get_task(task)
            .unwrap()
            .registers
            .get(Reg(1));
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
        let mut vm = crate::tests::make_vm();
        let optional = vm.state.type_pool.register(type_pool::TypeInfo {
            kind: TypeKind::Optional {
                inner: Intrinsic::Bool.type_index(),
            },
            type_id: type_pool::TypeId::ZERO,
            size: 8,
            align: 8,
        });
        let alias = vm.state.type_pool.register(type_pool::TypeInfo {
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
        let mut vm = crate::tests::make_vm();
        let index = TypeIndex::from_raw(vm.state.type_pool.len() as u32);
        let alias = vm.state.type_pool.register(type_pool::TypeInfo {
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
        let owner = vm.state.type_pool.well_known.display;
        let symbolic = vm.state.type_pool.register(type_pool::TypeInfo {
            kind: TypeKind::AssociatedType {
                trait_owner: owner,
                name: str_interner::intern("Item"),
            },
            type_id: type_pool::TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let optional = vm
            .state
            .type_pool
            .intern_structural(TypeKind::Optional { inner: symbolic });
        let function = vm.state.type_pool.intern_structural(TypeKind::Function {
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
        let concrete = vm.state.type_pool.intern_structural(TypeKind::Optional {
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
        vm.state.type_query_scope = Some(1);
        assert!(vm.value_matches_type(value, marker, 0).unwrap());
        vm.state.type_query_scope = Some(0);
        assert!(!vm.value_matches_type(value, marker, 0).unwrap());
        vm.state.type_query_scope = None;
        assert!(matches!(
            vm.value_matches_type(value, marker, 0),
            Err(VmError::MissingTraitContext)
        ));
        vm.install_type_pool(type_pool::TypePool::with_intrinsics());
        assert!(vm.state.type_query_scope.is_none());
    }
}
