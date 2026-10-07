//! Structural tuple checks and conversions preserve the source object's identity.

use gc::ObjectHeader;
use runtime::TaggedValue;
use type_pool::{TypeIndex, TypeKind};

use crate::{Vm, VmError};

impl Vm {
    pub(super) fn tuple_values(&self, value: TaggedValue) -> Result<Vec<TaggedValue>, VmError> {
        let pointer = value.as_heap_ptr().ok_or(VmError::TypeError)?;
        // SAFETY: source is a live value rooted for the enclosing VM operation.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        if !header.is_ordinary() {
            return Err(VmError::TypeError);
        }
        let ty = self
            .state
            .type_pool
            .canonical_type(header.type_index)
            .ok_or(VmError::TypeError)?;
        let TypeKind::Tuple { elements } = &self.state.type_pool.get(ty).kind else {
            return Err(VmError::TypeError);
        };
        if header.payload_words() != elements.len() {
            return Err(VmError::TypeError);
        }
        // SAFETY: exact payload length checked; this snapshot uses host allocation
        // only, so the live source remains rooted without a collection safepoint.
        Ok((0..elements.len())
            .map(|index| TaggedValue::from_raw(unsafe { pointer.cast::<u64>().add(index).read() }))
            .collect())
    }

    pub(super) fn cast_tuple(
        &mut self,
        value: TaggedValue,
        target: TypeIndex,
        elements: &[TypeIndex],
        depth: usize,
    ) -> Result<TaggedValue, VmError> {
        let values = self.tuple_values(value)?;
        if values.len() != elements.len() {
            return Err(VmError::TypeError);
        }
        if self.reflected_type(value)? == target && self.value_matches_type(value, target, 0)? {
            return Ok(value);
        }
        // Snapshot values into updateable roots; both inputs and converted
        // outputs may move during element conversion or final allocation.
        let base = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&values, |vm| {
            for (index, &expected) in elements.iter().enumerate() {
                let element = vm.roots.temporary_values.borrow()[base + index];
                let element = vm.cast_value(element, expected, depth)?;
                vm.roots.temporary_values.borrow_mut()[base + index] = element;
            }
            let words = u16::try_from(elements.len()).map_err(|_| VmError::ObjectTooLarge)?;
            // SAFETY: every converted value is rooted. Reload those roots after
            // allocation and initialize fully before another safepoint.
            let pointer =
                unsafe { vm.state.heap.alloc_object(target, words) }.ok_or(VmError::OutOfMemory)?;
            for index in 0..elements.len() {
                let element = vm.roots.temporary_values.borrow()[base + index];
                // SAFETY: index is in bounds of the fresh aligned tuple payload.
                unsafe {
                    pointer
                        .as_ptr()
                        .cast::<u64>()
                        .add(index)
                        .write(element.raw());
                }
            }
            // SAFETY: complete initialized managed allocation, no further safepoint.
            Ok(unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::make_vm;
    use type_pool::Intrinsic;

    fn tuple(vm: &mut Vm, ty: TypeIndex, values: &[TaggedValue]) -> TaggedValue {
        let root = vm.roots.temporary_values.borrow().len();
        vm.with_temporary_roots(values, |vm| {
            // SAFETY: fixture values are rooted, and payload length fits u16.
            let pointer = unsafe { vm.state.heap.alloc_object(ty, values.len() as u16) }.unwrap();
            let roots = vm.roots.temporary_values.borrow();
            for index in 0..values.len() {
                let value = roots[root + index];
                // SAFETY: the fresh allocation contains this initialized slot.
                unsafe {
                    pointer.as_ptr().cast::<u64>().add(index).write(value.raw());
                }
            }
            // SAFETY: initialized live managed allocation.
            Ok(unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) })
        })
        .unwrap()
    }

    #[test]
    fn gradual_tuple_assertions_check_elements_and_preserve_source_layout() {
        let mut vm = make_vm();
        let source_ty = vm.state.type_pool.intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::Any.type_index(), Intrinsic::Any.type_index()],
        });
        let target = vm.state.type_pool.intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::F64.type_index(), Intrinsic::Bool.type_index()],
        });
        let narrow = vm.state.type_pool.intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::I8.type_index(), Intrinsic::Bool.type_index()],
        });
        let source = tuple(
            &mut vm,
            source_ty,
            &[TaggedValue::from_i64(42), TaggedValue::TRUE],
        );
        vm.with_temporary_roots(&[source], |vm| {
            assert!(vm.value_matches_type(source, target, 0)?);
            let converted = vm.assert_value_type(source, target)?;
            vm.roots.temporary_values.borrow_mut().push(converted);
            assert_ne!(converted, source);
            assert_eq!(vm.reflected_type(source)?, source_ty);
            assert_eq!(vm.reflected_type(converted)?, target);
            assert_eq!(vm.assert_value_type(converted, target)?, converted);
            assert_eq!(vm.cast_value(converted, target, 0)?, converted);
            vm.store_field_value(converted, 1, TaggedValue::FALSE)?;
            assert_eq!(vm.load_field_value(converted, 1)?, TaggedValue::FALSE);
            vm.store_field_value(converted, 1, TaggedValue::TRUE)?;
            assert_eq!(vm.tuple_values(converted)?[0].as_f64(), Some(42.0));
            assert!(matches!(
                vm.assert_value_type(source, narrow),
                Err(VmError::TypeError)
            ));
            let narrowed = vm.cast_value(source, narrow, 0)?;
            vm.roots.temporary_values.borrow_mut().push(narrowed);
            assert_eq!(
                vm.reflected_type(vm.tuple_values(narrowed)?[0])?,
                Intrinsic::I8.type_index()
            );
            assert!(matches!(
                vm.store_field_value(converted, 1, TaggedValue::from_i64(0)),
                Err(VmError::TypeError)
            ));
            assert_eq!(vm.load_field_value(converted, 1)?, TaggedValue::TRUE);
            assert!(matches!(
                vm.load_field_value(converted, 2),
                Err(VmError::InvalidField(2))
            ));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn tuple_display_handles_singletons_nesting_cycles_and_exact_payloads() {
        let mut vm = make_vm();
        let singleton = vm.state.type_pool.intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::Any.type_index()],
        });
        let empty = vm
            .state
            .type_pool
            .intern_structural(TypeKind::Tuple { elements: vec![] });
        let source = tuple(&mut vm, singleton, &[TaggedValue::NULL]);
        vm.with_temporary_roots(&[source], |vm| {
            assert_eq!(vm.format_aggregate(source)?, Some("(null,)".into()));
            let string = vm.alloc_string("quoted")?;
            vm.store_field_value(source, 0, string)?;
            assert_eq!(vm.format_aggregate(source)?, Some("(\"quoted\",)".into()));
            let list = vm.new_list(0)?;
            vm.store_field_value(source, 0, list)?;
            vm.list_push(list, source)?;
            assert_eq!(vm.format_aggregate(source)?, Some("([<cycle>],)".into()));
            let zero = tuple(vm, empty, &[]);
            assert_eq!(vm.format_aggregate(zero)?, Some("()".into()));
            let malformed = tuple(vm, singleton, &[]);
            assert!(matches!(
                vm.format_aggregate(malformed),
                Err(VmError::TypeError)
            ));
            assert!(matches!(
                vm.tuple_values(malformed),
                Err(VmError::TypeError)
            ));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn nested_tuple_casts_reject_bad_elements_and_arity() {
        let mut vm = make_vm();
        let any = vm.state.type_pool.intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::Any.type_index()],
        });
        let integer = vm.state.type_pool.intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::I64.type_index()],
        });
        let nested = vm.state.type_pool.intern_structural(TypeKind::Tuple {
            elements: vec![integer],
        });
        let pair = vm.state.type_pool.intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::Any.type_index(); 2],
        });
        let inner = tuple(&mut vm, any, &[TaggedValue::TRUE]);
        vm.with_temporary_roots(&[inner], |vm| {
            let outer = tuple(vm, any, &[inner]);
            vm.roots.temporary_values.borrow_mut().push(outer);
            assert!(!vm.value_matches_type(outer, nested, 0)?);
            assert!(matches!(
                vm.assert_value_type(outer, nested),
                Err(VmError::TypeError)
            ));
            assert!(matches!(
                vm.cast_value(outer, nested, 0),
                Err(VmError::TypeError)
            ));
            assert!(matches!(
                vm.cast_value(outer, pair, 0),
                Err(VmError::TypeError)
            ));
            Ok(())
        })
        .unwrap();
    }
}
