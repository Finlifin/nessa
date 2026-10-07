//! List identity is a fixed wrapper; growth replaces only its tagged backing buffer.

use std::cell::RefCell;
use std::ptr::NonNull;

use gc::ObjectHeader;
use runtime::{Number, TaggedValue};
use type_pool::CollectionRole;

use crate::{DispatchResult, Vm, VmError};
use nsbc::Reg;
use runtime::TaskId;
use str_interner::StrId;

const MAX_CAPACITY: usize = u16::MAX as usize;

pub(super) struct ListLayout {
    wrapper: *mut u64,
    pub(super) buffer: *mut u64,
    pub(super) length: usize,
    capacity: usize,
}

/// The independently boxed RootCell remains stable throughout the private VM
/// operations using this guard. No callback can replace or drop that root domain.
struct TemporaryRootScope {
    roots: NonNull<RefCell<Vec<TaggedValue>>>,
    base: usize,
}

impl Drop for TemporaryRootScope {
    fn drop(&mut self) {
        // SAFETY: with_temporary_roots creates this guard from the stable RootCell,
        // then calls only internal List operations before dropping the guard.
        // No RefCell borrow spans an allocation, callback, or this cleanup; the
        // enclosing mutator operation also excludes concurrent root scanning.
        unsafe { self.roots.as_ref() }
            .borrow_mut()
            .truncate(self.base);
    }
}

impl Vm {
    /// Collection apply/update use their checked index/key ABI before native
    /// method dispatch, including when the receiver's static type is Any.
    pub(super) fn exec_collection_call(
        &mut self,
        task: TaskId,
        receiver: TaggedValue,
        ty: type_pool::TypeIndex,
        method: StrId,
        argument_count: u8,
    ) -> Option<DispatchResult> {
        match self.state.type_pool.checked_collection_role(ty) {
            Ok(Some(CollectionRole::List | CollectionRole::Map)) => {}
            Ok(_) => return None,
            Err(_) => return Some(DispatchResult::Error(VmError::TypeError)),
        }
        let method = match str_interner::try_get(method) {
            Some(method) => method,
            None => return Some(DispatchResult::Error(VmError::MethodNotFound)),
        };
        let expected = match method.as_str() {
            "apply" => 1,
            "update" => 2,
            _ => return None,
        };
        if argument_count != expected {
            return Some(DispatchResult::Error(VmError::ArityError {
                expected,
                got: argument_count,
            }));
        }
        let Some(registers) = self
            .roots
            .scheduler
            .get_task(task)
            .map(|task| &task.registers)
        else {
            return Some(DispatchResult::Error(VmError::InvalidTask));
        };
        let index = registers.get(Reg(0));
        let value = registers.get(Reg(1));
        let result = if expected == 1 {
            self.collection_get(receiver, index)
        } else {
            self.collection_set(receiver, index, value)
                .map(|()| TaggedValue::UNIT)
        };
        Some(match result {
            Ok(value) => {
                if let Some(task) = self.roots.scheduler.get_task_mut(task) {
                    task.registers.set(Reg(0), value);
                    DispatchResult::Continue
                } else {
                    DispatchResult::Error(VmError::InvalidTask)
                }
            }
            Err(error) => DispatchResult::Error(error),
        })
    }

    fn collection_role(&self, value: TaggedValue) -> Result<CollectionRole, VmError> {
        let pointer = value.as_heap_ptr().ok_or(VmError::TypeError)?;
        // SAFETY: caller holds this live operand in the active operation's roots.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        if !header.is_ordinary() {
            return Err(VmError::TypeError);
        }
        self.state
            .type_pool
            .checked_collection_role(header.type_index)
            .map_err(|_| VmError::TypeError)?
            .ok_or(VmError::TypeError)
    }

    pub(super) fn collection_get(
        &self,
        collection: TaggedValue,
        index: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        match self.collection_role(collection)? {
            CollectionRole::List => self.list_get(collection, index),
            CollectionRole::Map => self.map_get(collection, index),
            _ => Err(VmError::TypeError),
        }
    }

    pub(super) fn collection_set(
        &mut self,
        collection: TaggedValue,
        index: TaggedValue,
        value: TaggedValue,
    ) -> Result<(), VmError> {
        match self.collection_role(collection)? {
            CollectionRole::List => self.list_set(collection, index, value),
            CollectionRole::Map => self.map_set(collection, index, value),
            _ => Err(VmError::TypeError),
        }
    }

    pub(super) fn with_temporary_roots<T>(
        &mut self,
        values: &[TaggedValue],
        action: impl FnOnce(&mut Self) -> Result<T, VmError>,
    ) -> Result<T, VmError> {
        let base = self.roots.temporary_values.borrow().len();
        let _scope = TemporaryRootScope {
            roots: NonNull::from(&self.roots.temporary_values),
            base,
        };
        self.roots
            .temporary_values
            .borrow_mut()
            .extend_from_slice(values);
        action(self)
    }

    pub(super) fn list_layout(&self, value: TaggedValue) -> Result<ListLayout, VmError> {
        let pointer = value.as_heap_ptr().ok_or(VmError::TypeError)?;
        // SAFETY: operands are live registered VM roots within an execution operation.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        if !header.is_ordinary() {
            return Err(VmError::TypeError);
        }
        if self
            .state
            .type_pool
            .checked_collection_role(header.type_index)
            .map_err(|_| VmError::TypeError)?
            != Some(CollectionRole::List)
            || header.payload_words() != 3
        {
            return Err(VmError::TypeError);
        }
        let wrapper = pointer.cast_mut().cast::<u64>();
        // SAFETY: the checked header provides exactly three aligned payload words.
        let (length, capacity, buffer) = unsafe {
            (
                TaggedValue::from_raw(wrapper.read()),
                TaggedValue::from_raw(wrapper.add(1).read()),
                TaggedValue::from_raw(wrapper.add(2).read()),
            )
        };
        let length = length
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(VmError::TypeError)?;
        let capacity = capacity
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(VmError::TypeError)?;
        if length > capacity || capacity > MAX_CAPACITY {
            return Err(VmError::TypeError);
        }
        if capacity == 0 {
            if !buffer.is_null() {
                return Err(VmError::TypeError);
            }
            return Ok(ListLayout {
                wrapper,
                buffer: std::ptr::null_mut(),
                length,
                capacity,
            });
        }
        let buffer = buffer.as_heap_ptr().ok_or(VmError::TypeError)?;
        // SAFETY: this managed reference came from the rooted wrapper's tagged slot.
        let header = unsafe { ObjectHeader::from_payload_ptr(buffer) };
        if !header.is_ordinary() {
            return Err(VmError::TypeError);
        }
        if self
            .state
            .type_pool
            .checked_collection_role(header.type_index)
            .map_err(|_| VmError::TypeError)?
            != Some(CollectionRole::Buffer)
            || header.payload_words() != capacity
        {
            return Err(VmError::TypeError);
        }
        Ok(ListLayout {
            wrapper,
            buffer: buffer.cast_mut().cast(),
            length,
            capacity,
        })
    }

    fn list_buffer(&mut self, capacity: usize) -> Result<TaggedValue, VmError> {
        let words = u16::try_from(capacity).map_err(|_| VmError::ObjectTooLarge)?;
        let ty = self
            .state
            .type_pool
            .list_buffer_type()
            .ok_or(VmError::TypeError)?;
        // SAFETY: execution owns the mutator; initialize without another allocation.
        let pointer =
            unsafe { self.state.heap.alloc_object(ty, words) }.ok_or(VmError::OutOfMemory)?;
        for index in 0..capacity {
            // SAFETY: index is within the newly allocated aligned payload.
            unsafe {
                pointer
                    .as_ptr()
                    .cast::<u64>()
                    .add(index)
                    .write(TaggedValue::UNIT.raw());
            }
        }
        // SAFETY: this allocation is aligned and live, with all slots initialized.
        Ok(unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) })
    }

    pub(super) fn new_list(&mut self, length: usize) -> Result<TaggedValue, VmError> {
        if length > MAX_CAPACITY {
            return Err(VmError::ObjectTooLarge);
        }
        let ty = self.state.type_pool.list_type().ok_or(VmError::TypeError)?;
        self.state
            .type_pool
            .list_buffer_type()
            .ok_or(VmError::TypeError)?;
        // SAFETY: initialize this fresh wrapper before another allocation or safepoint.
        let pointer = unsafe { self.state.heap.alloc_object(ty, 3) }.ok_or(VmError::OutOfMemory)?;
        // SAFETY: exactly three aligned words were allocated above.
        unsafe {
            pointer
                .as_ptr()
                .cast::<u64>()
                .write(TaggedValue::from_u64(0).raw());
            pointer
                .as_ptr()
                .cast::<u64>()
                .add(1)
                .write(TaggedValue::from_u64(0).raw());
            pointer
                .as_ptr()
                .cast::<u64>()
                .add(2)
                .write(TaggedValue::NULL.raw());
        }
        // SAFETY: wrapper is live and initialized; the next allocation roots it.
        let value = unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) };
        let root = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&[value], |vm| {
            if length != 0 {
                let buffer = vm.list_buffer(length)?;
                vm.roots.temporary_values.borrow_mut().push(buffer);
                let value = vm.roots.temporary_values.borrow()[root];
                let layout = vm.list_layout(value)?;
                // SAFETY: no allocation follows, and the rooted wrapper has three words.
                unsafe {
                    layout
                        .wrapper
                        .write(TaggedValue::from_u64(length as u64).raw());
                    layout
                        .wrapper
                        .add(1)
                        .write(TaggedValue::from_u64(length as u64).raw());
                    layout.wrapper.add(2).write(buffer.raw());
                }
            }
            Ok(vm.roots.temporary_values.borrow()[root])
        })
    }

    fn list_index(index: TaggedValue) -> Result<usize, VmError> {
        // SAFETY: dispatch/native callers retain the live numeric index as a root;
        // scalar decoding does not allocate or stop the mutator.
        let number = unsafe { Number::from_tagged(index) }.ok_or(VmError::TypeError)?;
        let integer = match number {
            Number::I64(n) => u128::try_from(n).ok(),
            Number::U64(n) => Some(n as u128),
            Number::I128(n) => u128::try_from(n).ok(),
            Number::U128(n) => Some(n),
            Number::F64(_) => return Err(VmError::TypeError),
        }
        .and_then(|n| usize::try_from(n).ok())
        .ok_or(VmError::InvalidIndex)?;
        Ok(integer)
    }

    pub(super) fn list_len(&self, list: TaggedValue) -> Result<usize, VmError> {
        Ok(self.list_layout(list)?.length)
    }

    pub(super) fn list_get(
        &self,
        list: TaggedValue,
        index: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let index = Self::list_index(index)?;
        let layout = self.list_layout(list)?;
        if index >= layout.length {
            return Err(VmError::InvalidIndex);
        }
        // SAFETY: validated buffer bounds, with no safepoint during this read.
        Ok(TaggedValue::from_raw(unsafe {
            layout.buffer.add(index).read()
        }))
    }

    pub(super) fn list_set(
        &mut self,
        list: TaggedValue,
        index: TaggedValue,
        value: TaggedValue,
    ) -> Result<(), VmError> {
        let index = Self::list_index(index)?;
        let layout = self.list_layout(list)?;
        if index >= layout.length {
            return Err(VmError::InvalidIndex);
        }
        // SAFETY: validated slot in rooted buffer, no allocation during mutation.
        unsafe {
            layout.buffer.add(index).write(value.raw());
        }
        Ok(())
    }

    pub(super) fn list_push(
        &mut self,
        list: TaggedValue,
        value: TaggedValue,
    ) -> Result<(), VmError> {
        let root = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&[list, value], |vm| {
            let list = vm.roots.temporary_values.borrow()[root];
            let layout = vm.list_layout(list)?;
            if layout.length == MAX_CAPACITY {
                return Err(VmError::ObjectTooLarge);
            }
            if layout.length == layout.capacity {
                let capacity = layout
                    .capacity
                    .checked_mul(2)
                    .ok_or(VmError::ObjectTooLarge)?
                    .clamp(4, MAX_CAPACITY);
                let buffer = vm.list_buffer(capacity)?;
                vm.roots.temporary_values.borrow_mut().push(buffer);
                // Re-read after allocation: no raw pointer crosses a GC boundary.
                let list = vm.roots.temporary_values.borrow()[root];
                let old = vm.list_layout(list)?;
                let pointer = buffer
                    .as_heap_ptr()
                    .ok_or(VmError::TypeError)?
                    .cast_mut()
                    .cast::<u64>();
                // SAFETY: fresh buffer and old buffer are disjoint; lengths fit both.
                // No allocation occurs before the replacement is published.
                unsafe {
                    if old.length != 0 {
                        std::ptr::copy_nonoverlapping(old.buffer, pointer, old.length);
                    }
                    old.wrapper
                        .add(1)
                        .write(TaggedValue::from_u64(capacity as u64).raw());
                    old.wrapper.add(2).write(buffer.raw());
                }
            }
            let list = vm.roots.temporary_values.borrow()[root];
            let value = vm.roots.temporary_values.borrow()[root + 1];
            let layout = vm.list_layout(list)?;
            // SAFETY: spare capacity was established and no allocation intervenes.
            unsafe {
                layout.buffer.add(layout.length).write(value.raw());
                layout
                    .wrapper
                    .write(TaggedValue::from_u64((layout.length + 1) as u64).raw());
            }
            Ok(())
        })
    }

    pub(super) fn list_pop(&mut self, list: TaggedValue) -> Result<TaggedValue, VmError> {
        let layout = self.list_layout(list)?;
        if layout.length == 0 {
            return Ok(TaggedValue::NULL);
        }
        let index = layout.length - 1;
        // SAFETY: the last occupied slot is in bounds; clear it so GC does not
        // retain removed objects through unused buffer capacity.
        unsafe {
            let value = TaggedValue::from_raw(layout.buffer.add(index).read());
            layout.buffer.add(index).write(TaggedValue::UNIT.raw());
            layout
                .wrapper
                .write(TaggedValue::from_u64(index as u64).raw());
            Ok(value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::make_vm;

    #[test]
    fn lists_preserve_alias_identity_and_unit_elements_during_growth() {
        let mut vm = make_vm();
        let list = vm.new_list(2).unwrap();
        let root = vm.roots.temporary_values.borrow().len();
        vm.with_temporary_roots(&[list], |vm| {
            let list = vm.roots.temporary_values.borrow()[root];
            assert!(vm.list_get(list, TaggedValue::from_u64(0))?.is_unit());
            vm.list_set(list, TaggedValue::from_u64(1), TaggedValue::TRUE)?;
            for _ in 0..20 {
                let list = vm.roots.temporary_values.borrow()[root];
                vm.list_push(list, TaggedValue::FALSE)?;
            }
            let list = vm.roots.temporary_values.borrow()[root];
            assert_eq!(vm.list_len(list)?, 22);
            assert_eq!(
                vm.list_get(list, TaggedValue::from_u64(1))?,
                TaggedValue::TRUE
            );
            for _ in 0..20 {
                assert_eq!(vm.list_pop(list)?, TaggedValue::FALSE);
            }
            assert_eq!(vm.list_pop(list)?, TaggedValue::TRUE);
            assert!(vm.list_pop(list)?.is_unit());
            assert!(vm.list_pop(list)?.is_null());
            assert_eq!(vm.list_layout(list)?.length, 0);
            Ok(())
        })
        .unwrap();
        assert!(vm.roots.temporary_values.borrow().is_empty());
    }

    #[test]
    fn integer_indices_and_capacity_limits_are_checked_without_mutation() {
        let mut vm = make_vm();
        let list = vm.new_list(MAX_CAPACITY).unwrap();
        vm.with_temporary_roots(&[list], |vm| {
            assert!(matches!(
                vm.list_push(list, TaggedValue::TRUE),
                Err(VmError::ObjectTooLarge)
            ));
            assert_eq!(vm.list_len(list)?, MAX_CAPACITY);
            assert!(matches!(
                vm.list_get(list, TaggedValue::from_i64(-1)),
                Err(VmError::InvalidIndex)
            ));
            assert!(matches!(
                vm.list_get(list, TaggedValue::from_u64(MAX_CAPACITY as u64)),
                Err(VmError::InvalidIndex)
            ));
            let float = vm.number_value(Number::F64(0.0))?;
            assert!(matches!(vm.list_get(list, float), Err(VmError::TypeError)));
            let large = vm.number_value(Number::U128(u128::MAX))?;
            assert!(matches!(
                vm.list_get(list, large),
                Err(VmError::InvalidIndex)
            ));
            assert!(vm.list_get(list, TaggedValue::from_u64(0))?.is_unit());
            Ok(())
        })
        .unwrap();
        assert!(vm.roots.temporary_values.borrow().is_empty());
        assert!(matches!(
            vm.new_list(MAX_CAPACITY + 1),
            Err(VmError::ObjectTooLarge)
        ));
    }
    #[test]
    fn malformed_list_header_is_rejected_before_buffer_access() {
        let mut vm = make_vm();
        let list = vm.new_list(0).unwrap();
        vm.with_temporary_roots(&[list], |vm| {
            let pointer = list.as_heap_ptr().unwrap().cast_mut().cast::<u64>();
            // SAFETY: test intentionally corrupts one word of its live three-word wrapper.
            unsafe {
                pointer.write(TaggedValue::from_u64(1).raw());
            }
            assert!(matches!(vm.list_len(list), Err(VmError::TypeError)));
            // SAFETY: same validated test allocation; restore len then corrupt its unsigned tag.
            unsafe {
                pointer.write(TaggedValue::from_i64(0).raw());
            }
            assert!(matches!(vm.list_len(list), Err(VmError::TypeError)));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn collection_roles_and_index_registers_cannot_be_forged_by_ordinary_instructions() {
        use crate::VmResult;
        use nsbc::{AddrMode, FuncId, Instruction, Opcode, Reg};
        use runtime::FunctionCode;
        use type_pool::TypeIndex;
        for opcode in [
            Opcode::NewObject,
            Opcode::LoadField,
            Opcode::StoreField,
            Opcode::LoadIndex,
        ] {
            let mut vm = make_vm();
            let ty = vm.state.type_pool.list_type().unwrap();
            let mut instructions = vec![Instruction::a_type(
                Opcode::NewList,
                AddrMode::Imm,
                Reg(1),
                Reg(0),
                0,
            )];
            instructions.push(match opcode {
                Opcode::NewObject => Instruction::new_object(Reg(0), ty),
                Opcode::LoadField => Instruction::load_field(Reg(0), Reg(1), 0),
                Opcode::StoreField => Instruction::store_field(Reg(1), 0, Reg(0)),
                Opcode::LoadIndex => Instruction::a_type(opcode, AddrMode::Imm, Reg(0), Reg(1), 32),
                _ => Instruction::a_type(opcode, AddrMode::Imm, Reg(0), Reg(0), 0),
            });
            vm.add_function(FunctionCode {
                display_owner: None,
                abi: None,
                func_id: FuncId(0),
                instructions: instructions.into_iter().map(Instruction::encode).collect(),
                register_count: 32,
                param_count: 0,
                is_closure: false,
                function_type: TypeIndex::INVALID,
            });
            vm.spawn_root(FuncId(0));
            assert!(matches!(
                vm.run(),
                VmResult::Error(VmError::TypeError | VmError::InvalidIndex)
            ));
        }
    }
    #[test]
    fn dynamic_list_apply_and_update_use_integer_collection_abi() {
        use crate::VmResult;
        use nsbc::{AddrMode, FuncId, Instruction, Opcode};
        use runtime::FunctionCode;
        use type_pool::TypeIndex;
        let mut vm = make_vm();
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: None,
            func_id: FuncId(0),
            instructions: [
                Instruction::a_type(Opcode::NewList, AddrMode::Imm, Reg(10), Reg(0), 1),
                Instruction::load_imm(Reg(0), 0),
                Instruction::load_imm(Reg(1), 42),
                Instruction::call_method(Reg(10), str_interner::intern("update").as_u32(), 2),
                Instruction::load_imm(Reg(0), 0),
                Instruction::call_indirect(Reg(10), 1),
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
        let task = vm.spawn_root(FuncId(0));
        assert!(matches!(vm.run(), VmResult::Finished));
        assert_eq!(vm.task_result_i64(task).unwrap(), 42);
    }

    #[test]
    fn dynamic_list_calls_reject_arity_and_noninteger_indices() {
        use crate::VmResult;
        use nsbc::{AddrMode, FuncId, Instruction, Opcode};
        use runtime::FunctionCode;
        use type_pool::TypeIndex;
        for (arity, bad_index) in [(0, false), (2, false), (1, true)] {
            let mut vm = make_vm();
            vm.add_function(FunctionCode {
                display_owner: None,
                abi: None,
                func_id: FuncId(0),
                instructions: [
                    Instruction::a_type(Opcode::NewList, AddrMode::Imm, Reg(10), Reg(0), 1),
                    if bad_index {
                        Instruction::load_true(Reg(0))
                    } else {
                        Instruction::load_imm(Reg(0), 0)
                    },
                    Instruction::call_indirect(Reg(10), arity),
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
            vm.spawn_root(FuncId(0));
            assert!(matches!(
                vm.run(),
                VmResult::Error(VmError::TypeError | VmError::ArityError { .. })
            ));
        }
    }
    #[test]
    fn list_display_formats_nested_values_and_shared_subgraphs() {
        let mut vm = make_vm();
        let list = vm.new_list(0).unwrap();
        vm.with_temporary_roots(&[list], |vm| {
            let nested = vm.new_list(0)?;
            vm.list_push(list, nested)?;
            vm.list_push(list, nested)?;
            let string = vm.alloc_string("a\nb")?;
            vm.list_push(nested, string)?;
            vm.list_push(
                nested,
                TaggedValue::from_type(type_pool::Intrinsic::I64.type_index()).unwrap(),
            )?;
            vm.list_push(nested, TaggedValue::TRUE)?;
            vm.list_push(nested, TaggedValue::UNIT)?;
            vm.list_push(nested, TaggedValue::NULL)?;
            let number = vm.number_value(Number::F64(1.5))?;
            vm.list_push(nested, number)?;
            let expected =
                r#"[["a\nb", i64, true, (), null, 1.5], ["a\nb", i64, true, (), null, 1.5]]"#;
            assert_eq!(vm.format_aggregate(list)?, Some(expected.into()));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn list_display_checks_cycles_depth_malformed_payloads_and_private_buffers() {
        let mut vm = make_vm();
        let list = vm.new_list(0).unwrap();
        vm.with_temporary_roots(&[list], |vm| {
            assert_eq!(vm.format_aggregate(list)?, Some("[]".into()));
            vm.list_push(list, list)?;
            assert_eq!(vm.format_aggregate(list)?, Some("[<cycle>]".into()));
            vm.list_pop(list)?;
            let mut cursor = list;
            for _ in 0..127 {
                let child = vm.new_list(0)?;
                vm.list_push(cursor, child)?;
                cursor = child;
            }
            assert!(vm.format_aggregate(list)?.is_some());
            let child = vm.new_list(0)?;
            vm.list_push(cursor, child)?;
            assert!(matches!(
                vm.format_aggregate(list),
                Err(VmError::DisplayDepthExceeded)
            ));
            let pointer = list.as_heap_ptr().unwrap().cast_mut().cast::<u64>();
            // SAFETY: corrupt the owned live wrapper to exercise checked display bounds.
            unsafe {
                pointer.write(TaggedValue::from_u64(65535).raw());
            }
            assert!(matches!(vm.format_aggregate(list), Err(VmError::TypeError)));
            // SAFETY: restore its sole occupied slot before testing internal buffer visibility.
            unsafe {
                pointer.write(TaggedValue::from_u64(1).raw());
            }
            let buffer = TaggedValue::from_raw(unsafe { pointer.add(2).read() });
            assert!(matches!(
                vm.format_aggregate(buffer),
                Err(VmError::TypeError)
            ));
            vm.list_set(list, TaggedValue::from_u64(0), buffer)?;
            assert!(matches!(vm.format_aggregate(list), Err(VmError::TypeError)));
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn list_display_bounds_exponential_expansion_of_shared_values() {
        let mut vm = make_vm();
        let list = vm.new_list(0).unwrap();
        vm.with_temporary_roots(&[list], |vm| {
            let mut cursor = list;
            for _ in 0..20 {
                let child = vm.new_list(0)?;
                vm.list_push(cursor, child)?;
                vm.list_push(cursor, child)?;
                cursor = child;
            }
            assert!(matches!(
                vm.format_aggregate(list),
                Err(VmError::DisplaySizeExceeded)
            ));
            Ok(())
        })
        .unwrap();
    }
    #[test]
    fn list_root_scope_restores_existing_roots_on_errors_and_unwind() {
        let mut vm = make_vm();
        vm.roots
            .temporary_values
            .borrow_mut()
            .push(TaggedValue::TRUE);
        let baseline = vm.roots.temporary_values.borrow().clone();
        let result: Result<(), VmError> = vm.with_temporary_roots(&[TaggedValue::UNIT], |vm| {
            vm.roots
                .temporary_values
                .borrow_mut()
                .push(TaggedValue::FALSE);
            Err(VmError::TypeError)
        });
        assert!(matches!(result, Err(VmError::TypeError)));
        assert_eq!(*vm.roots.temporary_values.borrow(), baseline);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            vm.with_temporary_roots(&[TaggedValue::UNIT], |vm| -> Result<(), VmError> {
                vm.roots
                    .temporary_values
                    .borrow_mut()
                    .push(TaggedValue::FALSE);
                panic!("intentional List action unwind");
            })
        }));
        assert!(result.is_err());
        assert_eq!(*vm.roots.temporary_values.borrow(), baseline);
    }
}
