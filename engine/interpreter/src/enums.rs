//! Checked nominal enum descriptors and tagged variant payloads.

use gc::ObjectHeader;
use runtime::TaggedValue;
use str_interner::StrId;
use type_pool::{FieldInfo, TypeIndex, TypeKind};

use crate::collections::{read_u64_atomic, write_u64_atomic};
use crate::{Vm, VmError};

pub(super) struct EnumLayout {
    pub(super) ty: TypeIndex,
    pub(super) variant: u32,
    pub(super) name: StrId,
    pub(super) variant_name: StrId,
    pub(super) fields: Vec<FieldInfo>,
    pub(super) pointer: *const u64,
    words: usize,
}

impl Vm {
    fn enum_descriptor(&self, ty: TypeIndex, variant: u32) -> Result<EnumLayout, VmError> {
        let tp = self.type_pool_read();
        let ty = tp.canonical_type(ty).ok_or(VmError::InvalidType(ty))?;
        let TypeKind::Enum { name, variants } = &tp.get(ty).kind else {
            return Err(VmError::TypeError);
        };
        let selected = variants
            .iter()
            .find(|entry| entry.tag == variant)
            .ok_or(VmError::InvalidEnumVariant(variant))?;
        let words = variants
            .iter()
            .map(|entry| entry.fields.len())
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .filter(|&words| words <= u16::MAX as usize)
            .ok_or(VmError::ObjectTooLarge)?;
        Ok(EnumLayout {
            ty,
            variant,
            name: *name,
            variant_name: selected.name,
            fields: selected.fields.clone(),
            pointer: std::ptr::null(),
            words,
        })
    }

    pub(super) fn enum_constant(
        &self,
        ty: TypeIndex,
        variant: u32,
    ) -> Result<TaggedValue, VmError> {
        let descriptor = self.enum_descriptor(ty, variant)?;
        TaggedValue::from_enum(descriptor.ty, variant).ok_or(VmError::InvalidEnumVariant(variant))
    }

    pub(super) fn enum_layout(&self, value: TaggedValue) -> Result<Option<EnumLayout>, VmError> {
        if let Some((ty, variant)) = value.as_enum() {
            let descriptor = self.enum_descriptor(ty, variant)?;
            if !descriptor.fields.is_empty() {
                return Err(VmError::TypeError);
            }
            return Ok(Some(descriptor));
        }
        let Some(pointer) = value.as_heap_ptr() else {
            return Ok(None);
        };
        // SAFETY: callers retain a live managed value in roots during the operation.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        if !header.is_ordinary() {
            self.error_layout(value, 0)?;
            return Ok(None);
        }
        let ty = {
            let tp = self.type_pool_read();
            let ty = tp
                .canonical_type(header.type_index)
                .ok_or(VmError::TypeError)?;
            if !matches!(tp.get(ty).kind, TypeKind::Enum { .. }) {
                return Ok(None);
            }
            ty
        };
        if header.payload_words() == 0 {
            return Err(VmError::TypeError);
        }
        let (tag_type, tag) = TaggedValue::from_raw(read_u64_atomic(pointer as *const u64, 0))
            .as_enum()
            .ok_or(VmError::TypeError)?;
        let mut descriptor = self.enum_descriptor(tag_type, tag)?;
        if descriptor.ty != ty || descriptor.words != header.payload_words() {
            return Err(VmError::TypeError);
        }
        // SAFETY: the checked allocation contains the tag and all declared payload slots.
        descriptor.pointer = unsafe { pointer.cast::<u64>().add(1) };
        Ok(Some(descriptor))
    }

    pub(super) fn new_enum(
        &mut self,
        descriptor: TaggedValue,
        arguments: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let (ty, variant) = descriptor.as_enum().ok_or(VmError::TypeError)?;
        let descriptor = self.enum_descriptor(ty, variant)?;
        let values = if arguments.is_unit() {
            Vec::new()
        } else {
            self.tuple_values(arguments)?
        };
        if values.len() != descriptor.fields.len() {
            return Err(VmError::TypeError);
        }
        if values.is_empty() {
            return self.enum_constant(ty, variant);
        }
        let root = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&values, |vm| {
            for (index, field) in descriptor.fields.iter().enumerate() {
                let value = vm.worker_roots().temporary_values.borrow()[root + index];
                let converted = vm.assert_value_type(value, field.ty)?;
                vm.worker_roots().temporary_values.borrow_mut()[root + index] = converted;
            }
            let tag = vm.enum_constant(descriptor.ty, variant)?;
            // SAFETY: every converted field occupies an updateable root. The
            // complete fresh object is initialized before another allocation.
            let payload = unsafe {
                vm.worker
                    .heap
                    .alloc_object(descriptor.ty, descriptor.words as u16)
            }
            .ok_or(VmError::OutOfMemory)?;
            let roots = vm.worker_roots().temporary_values.borrow();
            let base = payload.as_ptr().cast::<u64>();
            write_u64_atomic(base, 0, tag.raw());
            for index in 1..descriptor.words {
                let value = if index <= descriptor.fields.len() {
                    roots[root + index - 1]
                } else {
                    TaggedValue::UNIT
                };
                write_u64_atomic(base, index, value.raw());
            }
            // SAFETY: `payload` is fully initialized.
            Ok(unsafe { TaggedValue::from_heap_ptr(payload.as_ptr()) })
        })
    }

    pub(super) fn enum_is(
        &self,
        value: TaggedValue,
        descriptor: TaggedValue,
    ) -> Result<bool, VmError> {
        let (ty, variant) = descriptor.as_enum().ok_or(VmError::TypeError)?;
        let expected = self.enum_descriptor(ty, variant)?;
        Ok(self
            .enum_layout(value)?
            .is_some_and(|actual| actual.ty == expected.ty && actual.variant == variant))
    }

    /// Test the tag of any concrete `IterationStep(T)`, whatever its item type.
    /// Nominal enums, templates and non-enum values are type errors.
    pub(super) fn iteration_step_done(&self, value: TaggedValue) -> Result<bool, VmError> {
        let layout = self.enum_layout(value)?.ok_or(VmError::TypeError)?;
        self.type_pool_read()
            .checked_iteration_step_item(layout.ty)
            .map_err(|_| VmError::TypeError)?
            .ok_or(VmError::TypeError)?;
        Ok(layout.variant == type_pool::ITERATION_DONE_TAG)
    }

    pub(super) fn enum_field(
        &mut self,
        value: TaggedValue,
        index: u32,
    ) -> Result<TaggedValue, VmError> {
        let layout = self.enum_layout(value)?.ok_or(VmError::TypeError)?;
        let field = layout
            .fields
            .get(index as usize)
            .ok_or(VmError::InvalidField(index))?;
        let value = TaggedValue::from_raw(read_u64_atomic(layout.pointer, index as usize));
        self.assert_value_type(value, field.ty)
    }

    pub(super) fn enum_equal(
        &self,
        a: TaggedValue,
        b: TaggedValue,
    ) -> Result<Option<bool>, VmError> {
        match (self.enum_layout(a)?, self.enum_layout(b)?) {
            (None, None) => Ok(None),
            (Some(a), Some(b)) if a.ty == b.ty && a.variant == b.variant => {
                if a.fields.is_empty() {
                    Ok(Some(true))
                } else {
                    Err(VmError::UnsupportedEnumEquality)
                }
            }
            _ => Ok(Some(false)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{VmResult, tests::make_vm};
    use nsbc::{Constant, FuncId, Instruction, Reg};
    use runtime::FunctionCode;
    use type_pool::{Intrinsic, TypeId, TypeInfo, VariantInfo};

    fn enum_type(vm: &mut Vm, id: u64) -> TypeIndex {
        let field = |name: &str, ty, offset| FieldInfo {
            name: str_interner::intern(name),
            ty,
            offset,
            has_default: false,
        };
        vm.type_pool_write().register(TypeInfo {
            kind: TypeKind::Enum {
                name: str_interner::intern("Choice"),
                variants: vec![
                    VariantInfo {
                        name: str_interner::intern("none"),
                        tag: 0,
                        fields: vec![],
                    },
                    VariantInfo {
                        name: str_interner::intern("text"),
                        tag: 1,
                        fields: vec![field("value", Intrinsic::Str.type_index(), 0)],
                    },
                    VariantInfo {
                        name: str_interner::intern("pair"),
                        tag: 2,
                        fields: vec![
                            field("x", Intrinsic::F64.type_index(), 0),
                            field("ok", Intrinsic::Bool.type_index(), 8),
                        ],
                    },
                ],
            },
            type_id: TypeId(111, id),
            size: 24,
            align: 8,
        })
    }

    fn arguments(vm: &mut Vm, values: &[TaggedValue]) -> TaggedValue {
        let ty = vm.type_pool_write().intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::Any.type_index(); values.len()],
        });
        let root = vm.worker_roots().temporary_values.borrow().len();
        vm.with_temporary_roots(values, |vm| {
            // SAFETY: source values are rooted and this fixture initializes its full payload.
            let payload = unsafe { vm.worker.heap.alloc_object(ty, values.len() as u16) }.unwrap();
            let roots = vm.worker_roots().temporary_values.borrow();
            let base = payload.as_ptr().cast::<u64>();
            for index in 0..values.len() {
                let value = roots[root + index];
                write_u64_atomic(base, index, value.raw());
            }
            // SAFETY: initialized live managed allocation.
            Ok(unsafe { TaggedValue::from_heap_ptr(payload.as_ptr()) })
        })
        .unwrap()
    }

    #[test]
    fn iteration_step_distinguishes_null_and_end_and_checks_item() {
        let mut vm = make_vm();
        let ty = vm
            .type_pool_write()
            .intern_iteration_step(Intrinsic::Any.type_index())
            .unwrap();
        let done = vm.enum_constant(ty, type_pool::ITERATION_DONE_TAG).unwrap();
        let descriptor = vm
            .enum_constant(ty, type_pool::ITERATION_YIELDED_TAG)
            .unwrap();
        let args = arguments(&mut vm, &[TaggedValue::NULL]);
        let yielded = vm.new_enum(descriptor, args).unwrap();
        assert_eq!(vm.reflected_type(yielded).unwrap(), ty);
        assert_eq!(vm.enum_field(yielded, 0).unwrap(), TaggedValue::NULL);
        assert!(!vm.enum_is(yielded, done).unwrap());
        assert!(vm.enum_is(yielded, descriptor).unwrap());
        assert!(vm.enum_is(done, done).unwrap());
        let typed = vm
            .type_pool_write()
            .intern_iteration_step(Intrinsic::I64.type_index())
            .unwrap();
        let descriptor = vm
            .enum_constant(typed, type_pool::ITERATION_YIELDED_TAG)
            .unwrap();
        let wrong = arguments(&mut vm, &[TaggedValue::TRUE]);
        assert!(matches!(
            vm.new_enum(descriptor, wrong),
            Err(VmError::TypeError)
        ));
        let valid = arguments(&mut vm, &[TaggedValue::from_i64(42)]);
        let value = vm.new_enum(descriptor, valid).unwrap();
        assert_eq!(vm.enum_field(value, 0).unwrap().as_i64(), Some(42));
        assert_eq!(vm.reflected_type(value).unwrap(), typed);
        // The dynamic done test accepts every concrete item type.
        assert!(!vm.iteration_step_done(value).unwrap());
        assert!(!vm.iteration_step_done(yielded).unwrap());
        assert!(vm.iteration_step_done(done).unwrap());
        let typed_done = vm
            .enum_constant(typed, type_pool::ITERATION_DONE_TAG)
            .unwrap();
        assert!(vm.iteration_step_done(typed_done).unwrap());
    }

    #[test]
    fn dynamic_iteration_done_test_rejects_values_that_are_not_steps() {
        let mut vm = make_vm();
        let nominal = enum_type(&mut vm, 1);
        let lookalike = vm.enum_constant(nominal, 0).unwrap();
        for value in [lookalike, TaggedValue::from_i64(0), TaggedValue::NULL] {
            assert!(matches!(
                vm.iteration_step_done(value),
                Err(VmError::TypeError)
            ));
        }
    }

    #[test]
    fn enum_construction_reflection_fields_display_and_equality_are_checked() {
        let mut vm = make_vm();
        let ty = enum_type(&mut vm, 1);
        let other = enum_type(&mut vm, 2);
        let nullary = vm.enum_constant(ty, 0).unwrap();
        assert_eq!(vm.reflected_type(nullary).unwrap(), ty);
        assert_eq!(
            vm.format_aggregate(nullary).unwrap(),
            Some("Choice.none".into())
        );
        assert_eq!(
            vm.enum_equal(nullary, vm.enum_constant(other, 0).unwrap())
                .unwrap(),
            Some(false)
        );
        assert_eq!(vm.enum_equal(nullary, nullary).unwrap(), Some(true));
        assert!(!vm.enum_is(TaggedValue::TRUE, nullary).unwrap());
        let args = arguments(&mut vm, &[TaggedValue::from_i64(42), TaggedValue::TRUE]);
        vm.with_temporary_roots(&[args], |vm| {
            let descriptor = vm.enum_constant(ty, 2)?;
            assert!(matches!(
                vm.enum_layout(descriptor),
                Err(VmError::TypeError)
            ));
            let value = vm.new_enum(descriptor, args)?;
            vm.worker_roots().temporary_values.borrow_mut().push(value);
            assert_eq!(vm.reflected_type(value)?, ty);
            assert!(vm.value_matches_type(value, ty, 0)?);
            assert_eq!(
                vm.format_aggregate(value)?,
                Some("Choice.pair(42, true)".into())
            );
            assert_eq!(vm.enum_field(value, 0)?.as_f64(), Some(42.0));
            assert_eq!(vm.tuple_values(args)?[0].as_i64(), Some(42));
            assert!(vm.enum_is(value, descriptor)?);
            assert!(!vm.enum_is(value, nullary)?);
            assert!(matches!(
                vm.enum_field(value, 2),
                Err(VmError::InvalidField(2))
            ));
            assert!(matches!(
                vm.enum_equal(value, value),
                Err(VmError::UnsupportedEnumEquality)
            ));
            assert!(matches!(
                vm.new_enum(descriptor, TaggedValue::UNIT),
                Err(VmError::TypeError)
            ));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn payload_descriptors_cannot_load_as_values_and_match_fail_is_explicit() {
        for payload in [true, false] {
            let mut vm = make_vm();
            let ty = enum_type(&mut vm, 1);
            let descriptor = vm
                .push_constant(&Constant::Enum {
                    type_index: ty,
                    variant: if payload { 1 } else { 0 },
                })
                .unwrap();
            vm.add_function(FunctionCode {
                display_owner: None,
                abi: nsbc::FunctionAbi::value_only(0),
                func_id: FuncId(0),
                instructions: vec![
                    Instruction::load_const(Reg(0), descriptor as u16).encode(),
                    Instruction::match_fail().encode(),
                ],
                register_count: 32,
                param_count: 0,
                is_closure: false,
                function_type: TypeIndex::INVALID,
            });
            vm.spawn_root(FuncId(0));
            assert!(matches!(
                (payload, vm.run()),
                (true, VmResult::Error(VmError::TypeError))
                    | (false, VmResult::Error(VmError::NoMatchingCase))
            ));
        }
    }

    #[test]
    fn enum_payload_tag_layout_must_match_its_nominal_header() {
        let mut vm = make_vm();
        let ty = enum_type(&mut vm, 1);
        let other = enum_type(&mut vm, 2);
        let args = arguments(&mut vm, &[TaggedValue::from_i64(2), TaggedValue::TRUE]);
        vm.with_temporary_roots(&[args], |vm| {
            let value = vm.new_enum(vm.enum_constant(ty, 2)?, args)?;
            vm.worker_roots().temporary_values.borrow_mut().push(value);
            let pointer = value.as_heap_ptr().unwrap().cast_mut().cast::<u64>();
            write_u64_atomic(pointer, 0, vm.enum_constant(other, 2)?.raw());
            assert!(matches!(vm.enum_layout(value), Err(VmError::TypeError)));
            assert!(matches!(vm.reflected_type(value), Err(VmError::TypeError)));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn enum_is_and_comparison_opcodes_preserve_nominal_discriminants() {
        use crate::DispatchResult;
        use nsbc::{AddrMode, Opcode};
        let mut vm = make_vm();
        let ty = enum_type(&mut vm, 1);
        let other = enum_type(&mut vm, 2);
        let own = vm
            .push_constant(&Constant::Enum {
                type_index: ty,
                variant: 0,
            })
            .unwrap();
        let foreign = vm
            .push_constant(&Constant::Enum {
                type_index: other,
                variant: 0,
            })
            .unwrap();
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: nsbc::FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: vec![],
            register_count: 32,
            param_count: 0,
            is_closure: false,
            function_type: TypeIndex::INVALID,
        });
        let task = vm.spawn_root(FuncId(0));
        let value = vm.enum_constant(ty, 0).unwrap();
        vm.task_mut(task).unwrap().registers.set(Reg(0), value);
        for (descriptor, expected) in [(own, true), (foreign, false)] {
            assert!(matches!(
                vm.dispatch(
                    task,
                    Instruction::enum_is(Reg(1), Reg(0), descriptor as u16)
                ),
                DispatchResult::Continue
            ));
            assert_eq!(
                vm.task_ref(task).unwrap().registers.get(Reg(1)).as_bool(),
                Some(expected)
            );
        }
        vm.task_mut(task).unwrap().registers.set(Reg(2), value);
        assert!(matches!(
            vm.dispatch(
                task,
                Instruction::r_type(Opcode::CmpEq, Reg(1), Reg(0), Reg(2))
            ),
            DispatchResult::Continue
        ));
        assert_eq!(
            vm.task_ref(task).unwrap().registers.get(Reg(1)).as_bool(),
            Some(true)
        );
        assert!(matches!(
            vm.dispatch(
                task,
                Instruction::a_type(
                    Opcode::TypeAssert,
                    AddrMode::Imm,
                    Reg(1),
                    Reg(0),
                    ty.as_u32() as u16
                )
            ),
            DispatchResult::Continue
        ));
        assert_eq!(vm.task_ref(task).unwrap().registers.get(Reg(1)), value);
    }

    #[test]
    fn registered_self_methods_dispatch_for_nullary_and_heap_enum_values() {
        use nsbc::{AddrMode, Opcode};
        for payload in [false, true] {
            let mut vm = make_vm();
            let ty = enum_type(&mut vm, 1);
            let method = str_interner::intern("value");
            vm.type_pool_write().add_method(
                ty,
                type_pool::MethodSlot {
                    name: method,
                    func_id: 1,
                    trait_impl: None,
                    visible_scope: None,
                    access: type_pool::MethodAccess::Public,
                },
            );
            vm.add_function(FunctionCode {
                display_owner: None,
                abi: nsbc::FunctionAbi::value_only(0),
                func_id: FuncId(0),
                instructions: [
                    Instruction::call_method(Reg(20), method.as_u32(), 0),
                    Instruction::ret(Reg(0)),
                ]
                .into_iter()
                .map(Instruction::encode)
                .collect(),
                register_count: 32,
                param_count: 0,
                is_closure: false,
                function_type: TypeIndex::INVALID,
            });
            vm.add_function(FunctionCode {
                display_owner: None,
                abi: nsbc::FunctionAbi::value_only(1),
                func_id: FuncId(1),
                instructions: [
                    Instruction::a_type(
                        Opcode::TypeAssert,
                        AddrMode::Imm,
                        Reg(0),
                        Reg(0),
                        ty.as_u32() as u16,
                    ),
                    Instruction::load_imm(Reg(0), 42),
                    Instruction::ret(Reg(0)),
                ]
                .into_iter()
                .map(Instruction::encode)
                .collect(),
                register_count: 32,
                param_count: 1,
                is_closure: false,
                function_type: TypeIndex::INVALID,
            });
            let task = vm.spawn_root(FuncId(0));
            let value = if payload {
                let args = arguments(&mut vm, &[TaggedValue::from_i64(42), TaggedValue::TRUE]);
                let descriptor = vm.enum_constant(ty, 2).unwrap();
                vm.new_enum(descriptor, args).unwrap()
            } else {
                vm.enum_constant(ty, 0).unwrap()
            };
            vm.task_mut(task).unwrap().registers.set(Reg(20), value);
            assert!(matches!(vm.run(), VmResult::Finished));
            assert_eq!(vm.task_result_i64(task).unwrap(), 42);
        }
    }
}
