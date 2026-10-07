//! Checked field access against both the allocation and its declared layout.

use gc::ObjectHeader;
use runtime::TaggedValue;
use type_pool::{TypeIndex, TypeKind};

use crate::{Vm, VmError};

impl Vm {
    fn field_layout(
        &self,
        object: TaggedValue,
        index: u32,
    ) -> Result<(*const u8, TypeIndex), VmError> {
        let pointer = object.as_heap_ptr().ok_or(VmError::TypeError)?;
        // SAFETY: live heap-tagged operands are rooted for the VM operation.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        if !header.is_ordinary() {
            return Err(VmError::TypeError);
        }
        let ty = self
            .state
            .type_pool
            .canonical_type(header.type_index)
            .ok_or(VmError::TypeError)?;
        if self.state.type_pool.is_reserved_collection_role(ty) {
            return Err(VmError::TypeError);
        }
        let field = match &self.state.type_pool.get(ty).kind {
            TypeKind::Struct { fields, .. } if header.payload_words() == fields.len() => {
                fields.get(index as usize).map(|field| field.ty)
            }
            TypeKind::Tuple { elements } if header.payload_words() == elements.len() => {
                elements.get(index as usize).copied()
            }
            _ => return Err(VmError::TypeError),
        }
        .ok_or(VmError::InvalidField(index))?;
        if index as usize >= header.payload_words() {
            return Err(VmError::InvalidField(index));
        }
        Ok((pointer, field))
    }

    pub(super) fn load_field_value(
        &mut self,
        object: TaggedValue,
        index: u32,
    ) -> Result<TaggedValue, VmError> {
        let (pointer, ty) = self.field_layout(object, index)?;
        // SAFETY: field_layout checked allocation bounds and payload alignment.
        let value = unsafe { pointer.cast::<u64>().add(index as usize).read() };
        self.assert_value_type(TaggedValue::from_raw(value), ty)
    }

    pub(super) fn store_field_value(
        &mut self,
        object: TaggedValue,
        index: u32,
        value: TaggedValue,
    ) -> Result<(), VmError> {
        let (_, ty) = self.field_layout(object, index)?;
        let root = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&[object, value], |vm| {
            let value = vm.roots.temporary_values.borrow()[root + 1];
            let value = vm.assert_value_type(value, ty)?;
            let object = vm.roots.temporary_values.borrow()[root];
            let (pointer, _) = vm.field_layout(object, index)?;
            // SAFETY: receiver reloaded after conversion and exact field bounds
            // rechecked. No allocation occurs before publishing the fresh value.
            unsafe {
                pointer
                    .cast_mut()
                    .cast::<u64>()
                    .add(index as usize)
                    .write(value.raw());
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use nsbc::{FuncId, Instruction, Reg};
    use runtime::FunctionCode;
    use type_pool::{Intrinsic, TypeId, TypeInfo};

    use super::*;
    use crate::{VmResult, tests::make_vm};

    #[test]
    fn field_stores_reject_wrong_types_and_reads_reject_uninitialized_values() {
        for store in [false, true] {
            let mut vm = make_vm();
            let ty = vm.state.type_pool.register(TypeInfo {
                kind: TypeKind::Tuple {
                    elements: vec![Intrinsic::I64.type_index()],
                },
                type_id: TypeId(99, 100),
                size: 8,
                align: 8,
            });
            let access = if store {
                Instruction::store_field(Reg(1), 0, Reg(0))
            } else {
                Instruction::load_field(Reg(0), Reg(1), 0)
            };
            vm.add_function(FunctionCode {
                display_owner: None,
                abi: None,
                func_id: FuncId(0),
                instructions: [
                    Instruction::new_object(Reg(1), ty),
                    Instruction::load_true(Reg(0)),
                    access,
                ]
                .into_iter()
                .map(Instruction::encode)
                .collect(),
                register_count: 32,
                param_count: 0,
                is_closure: false,
                function_type: TypeIndex::INVALID,
            });
            vm.spawn_root(FuncId(0));
            assert!(matches!(vm.run(), VmResult::Error(VmError::TypeError)));
            assert_eq!(vm.active_stack_count(), 0);
        }
    }
}
