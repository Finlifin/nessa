//! Structural tuple checks and conversions preserve the source object's identity.

use gc::{MAX_LARGE_TAGGED_WORDS, ObjectHeader};
use runtime::TaggedValue;
use type_pool::{TypeId, TypeIndex, TypeKind};

use crate::collections::{read_u64_atomic, write_u64_atomic};
use crate::{Vm, VmError};

impl Vm {
    pub(super) fn alloc_tuple(
        &mut self,
        elements: &[TypeIndex],
        values: &[TaggedValue],
    ) -> Result<TaggedValue, VmError> {
        if elements.len() != values.len() {
            return Err(VmError::TypeError);
        }
        let words = values.len();
        if words > MAX_LARGE_TAGGED_WORDS {
            return Err(VmError::ObjectTooLarge);
        }
        let fast_ty = {
            let tp = self.type_pool_read();
            let existing = (0..tp.len())
                .map(|index| TypeIndex::from_raw(index as u32))
                .find(|&index| {
                    matches!(
                        &tp.get(index).kind,
                        TypeKind::Tuple { elements: found } if found.as_slice() == elements
                    )
                });
            existing.and_then(|index| {
                let info = tp.get(index);
                if info.type_id != TypeId::ZERO || info.size != 0 || info.align != 0 {
                    Some(index)
                } else {
                    None
                }
            })
        };
        let ty = match fast_ty {
            Some(index) => index,
            None => {
                let mut tp = self.type_pool_write();
                let existing = (0..tp.len())
                    .map(|index| TypeIndex::from_raw(index as u32))
                    .find(|&index| {
                        matches!(
                            &tp.get(index).kind,
                            TypeKind::Tuple { elements: found } if found.as_slice() == elements
                        )
                    });
                match existing {
                    Some(index) => {
                        let info = tp.get(index);
                        if info.type_id == TypeId::ZERO && info.size == 0 {
                            let info_mut = tp.get_mut(index);
                            info_mut.size =
                                u32::try_from(words.saturating_mul(8)).unwrap_or(u32::MAX);
                            info_mut.align = 8;
                        }
                        index
                    }
                    None => {
                        let index = tp.intern_structural(TypeKind::Tuple {
                            elements: elements.to_vec(),
                        });
                        let info_mut = tp.get_mut(index);
                        info_mut.size = u32::try_from(words.saturating_mul(8)).unwrap_or(u32::MAX);
                        info_mut.align = 8;
                        if let Some(input) = tp.identity_input().cloned() {
                            tp.finalize_type_identities(input)
                                .map_err(|_| VmError::InvalidType(index))?;
                        }
                        index
                    }
                }
            }
        };
        let root = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(values, |vm| {
            // SAFETY: payload length fits MAX_LARGE_TAGGED_WORDS; every element is rooted for
            // the allocation and written before another safepoint, with no lock held.
            let pointer = unsafe { vm.worker.heap.alloc_tagged_words(ty, words) }
                .ok_or(VmError::OutOfMemory)?;
            let base = pointer.as_ptr().cast::<u64>();
            for index in 0..values.len() {
                let value = vm.worker_roots().temporary_values.borrow()[root + index];
                write_u64_atomic(base, index, value.raw());
            }
            // SAFETY: the tuple payload is fully initialized.
            Ok(unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) })
        })
    }

    pub(super) fn tuple_values(&self, value: TaggedValue) -> Result<Vec<TaggedValue>, VmError> {
        let pointer = value.as_heap_ptr().ok_or(VmError::TypeError)?;
        // SAFETY: source is a live value rooted for the enclosing VM operation.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        if !header.is_ordinary() {
            return Err(VmError::TypeError);
        }
        let elem_len = {
            let tp = self.type_pool_read();
            let ty = tp
                .canonical_type(header.type_index)
                .ok_or(VmError::TypeError)?;
            let TypeKind::Tuple { elements } = &tp.get(ty).kind else {
                return Err(VmError::TypeError);
            };
            elements.len()
        };
        if header.payload_words() != elem_len {
            return Err(VmError::TypeError);
        }
        let base = pointer as *const u64;
        Ok((0..elem_len)
            .map(|index| TaggedValue::from_raw(read_u64_atomic(base, index)))
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
        let base = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&values, |vm| {
            for (index, &expected) in elements.iter().enumerate() {
                let element = vm.worker_roots().temporary_values.borrow()[base + index];
                let element = vm.cast_value(element, expected, depth)?;
                vm.worker_roots().temporary_values.borrow_mut()[base + index] = element;
            }
            let words = elements.len();
            if words > MAX_LARGE_TAGGED_WORDS {
                return Err(VmError::ObjectTooLarge);
            }
            // SAFETY: every converted value is rooted. Reload those roots after
            // allocation and initialize fully before another safepoint.
            let pointer = unsafe { vm.worker.heap.alloc_tagged_words(target, words) }
                .ok_or(VmError::OutOfMemory)?;
            let raw_base = pointer.as_ptr().cast::<u64>();
            for index in 0..elements.len() {
                let element = vm.worker_roots().temporary_values.borrow()[base + index];
                write_u64_atomic(raw_base, index, element.raw());
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
        let root = vm.worker_roots().temporary_values.borrow().len();
        vm.with_temporary_roots(values, |vm| {
            // SAFETY: fixture values are rooted, and payload length fits u16.
            let pointer = unsafe { vm.worker.heap.alloc_object(ty, values.len() as u16) }.unwrap();
            let roots = vm.worker_roots().temporary_values.borrow();
            let base = pointer.as_ptr().cast::<u64>();
            for index in 0..values.len() {
                let value = roots[root + index];
                write_u64_atomic(base, index, value.raw());
            }
            // SAFETY: initialized live managed allocation.
            Ok(unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) })
        })
        .unwrap()
    }

    #[test]
    fn gradual_tuple_assertions_check_elements_and_preserve_source_layout() {
        let mut vm = make_vm();
        let source_ty = vm.type_pool_write().intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::Any.type_index(), Intrinsic::Any.type_index()],
        });
        let target = vm.type_pool_write().intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::F64.type_index(), Intrinsic::Bool.type_index()],
        });
        let narrow = vm.type_pool_write().intern_structural(TypeKind::Tuple {
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
            vm.worker_roots()
                .temporary_values
                .borrow_mut()
                .push(converted);
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
            vm.worker_roots()
                .temporary_values
                .borrow_mut()
                .push(narrowed);
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
        let singleton = vm.type_pool_write().intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::Any.type_index()],
        });
        let empty = vm
            .type_pool_write()
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
        let any = vm.type_pool_write().intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::Any.type_index()],
        });
        let integer = vm.type_pool_write().intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::I64.type_index()],
        });
        let nested = vm.type_pool_write().intern_structural(TypeKind::Tuple {
            elements: vec![integer],
        });
        let pair = vm.type_pool_write().intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::Any.type_index(); 2],
        });
        let inner = tuple(&mut vm, any, &[TaggedValue::TRUE]);
        vm.with_temporary_roots(&[inner], |vm| {
            let outer = tuple(vm, any, &[inner]);
            vm.worker_roots().temporary_values.borrow_mut().push(outer);
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

    #[test]
    fn alloc_tuple_preserves_finalized_type_identities_for_error_checks() {
        let mut vm = make_vm();
        let err_ty = vm
            .type_pool_write()
            .intern_structural(TypeKind::ErrorQualified {
                errors: vec![Intrinsic::Str.type_index()],
                inner: Intrinsic::I64.type_index(),
            });
        {
            let mut tp = vm.type_pool_write();
            let info = tp.get_mut(err_ty);
            info.size = 24;
            info.align = 8;
            tp.finalize_type_identities(type_pool::TypeIdentityInput {
                schema: 1,
                packages: Vec::new(),
                declarations: Vec::new(),
            })
            .unwrap();
        }

        let msg = vm.alloc_string("unseen tuple shape").unwrap();
        let tup = vm
            .alloc_tuple(
                &[Intrinsic::Bool.type_index(), Intrinsic::Str.type_index()],
                &[TaggedValue::FALSE, msg],
            )
            .unwrap();
        vm.with_temporary_roots(&[tup], |vm| {
            let id = vm
                .type_pool_read()
                .stable_type_id(err_ty)
                .expect("stable_type_id must remain valid after dynamic alloc_tuple");
            assert_ne!(id, TypeId::ZERO);
            Ok(())
        })
        .unwrap();
    }
}
