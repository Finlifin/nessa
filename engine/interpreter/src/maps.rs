//! String-keyed maps retain identity while replacing a managed tagged bucket buffer.

use gc::ObjectHeader;
use runtime::TaggedValue;
use type_pool::CollectionRole;

use crate::{Vm, VmError};

const MAX_CAPACITY: usize = 8192;
const HASH_MASK: u64 = (1 << 57) - 1;
const EMPTY: u64 = 0;
const OCCUPIED: u64 = 1;
const DELETED: u64 = 2;

pub(super) struct MapLayout {
    wrapper: *mut u64,
    pub(super) buffer: *mut u64,
    pub(super) length: usize,
    pub(super) capacity: usize,
}

fn key_hash(key: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in key.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    hash & HASH_MASK
}

fn string_key(value: TaggedValue) -> Result<String, VmError> {
    crate::builtin_ctx::heap_string_to_owned(value).ok_or(VmError::TypeError)
}

impl MapLayout {
    fn slot(&self, bucket: usize, field: usize) -> TaggedValue {
        // SAFETY: MapLayout validates the four-word bucket region. Callers use
        // bucket < capacity and field < 4 while the rooted operation is active.
        TaggedValue::from_raw(unsafe { self.buffer.add(bucket * 4 + field).read() })
    }

    fn bucket_state(&self, bucket: usize) -> Result<u64, VmError> {
        let state = self.slot(bucket, 0).as_u64().ok_or(VmError::TypeError)?;
        let hash = self.slot(bucket, 1).as_u64().ok_or(VmError::TypeError)?;
        match state {
            EMPTY | DELETED
                if hash == 0
                    && self.slot(bucket, 2).is_unit()
                    && self.slot(bucket, 3).is_unit() =>
            {
                Ok(state)
            }
            OCCUPIED => {
                let pointer = self
                    .slot(bucket, 2)
                    .as_heap_ptr()
                    .ok_or(VmError::TypeError)?;
                // SAFETY: the bucket key is a live reference from this rooted
                // managed buffer; verify its string header before any byte read.
                let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
                if !header.is_ordinary() {
                    return Err(VmError::TypeError);
                }
                if header.type_index != type_pool::Intrinsic::Str.type_index()
                    || header.payload_words() == 0
                {
                    return Err(VmError::TypeError);
                }
                Ok(state)
            }
            _ => Err(VmError::TypeError),
        }
    }

    pub(super) fn occupied_buckets(&self) -> Result<Vec<usize>, VmError> {
        let mut buckets = Vec::with_capacity(self.length);
        for bucket in 0..self.capacity {
            if self.bucket_state(bucket)? == OCCUPIED {
                buckets.push(bucket);
            }
        }
        if buckets.len() != self.length {
            return Err(VmError::TypeError);
        }
        Ok(buckets)
    }

    fn find(&self, key: &str, hash: u64) -> Result<(Option<usize>, Option<usize>), VmError> {
        let mut available = None;
        for offset in 0..self.capacity {
            let bucket = (hash as usize).wrapping_add(offset) & (self.capacity - 1);
            match self.bucket_state(bucket)? {
                EMPTY => return Ok((None, available.or(Some(bucket)))),
                DELETED => {
                    available.get_or_insert(bucket);
                }
                OCCUPIED => {
                    if self.slot(bucket, 1).as_u64() == Some(hash)
                        && string_key(self.slot(bucket, 2))? == key
                    {
                        return Ok((Some(bucket), available));
                    }
                }
                _ => return Err(VmError::TypeError),
            }
        }
        Ok((None, available))
    }

    fn write_bucket(&self, bucket: usize, hash: u64, key: TaggedValue, value: TaggedValue) {
        // SAFETY: bucket is a checked occupied/vacant slot in this managed buffer;
        // both references are rooted and no allocation or safepoint intervenes.
        unsafe {
            let slot = self.buffer.add(bucket * 4);
            slot.write(TaggedValue::from_u64(OCCUPIED).raw());
            slot.add(1).write(TaggedValue::from_u64(hash).raw());
            slot.add(2).write(key.raw());
            slot.add(3).write(value.raw());
        }
    }
}

impl Vm {
    pub(super) fn map_layout(&self, value: TaggedValue) -> Result<MapLayout, VmError> {
        let pointer = value.as_heap_ptr().ok_or(VmError::TypeError)?;
        // SAFETY: operand is a live managed root in the current mutator operation.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        if !header.is_ordinary() {
            return Err(VmError::TypeError);
        }
        if self
            .state
            .type_pool
            .checked_collection_role(header.type_index)
            .map_err(|_| VmError::TypeError)?
            != Some(CollectionRole::Map)
            || header.payload_words() != 3
        {
            return Err(VmError::TypeError);
        }
        let wrapper = pointer.cast_mut().cast::<u64>();
        // SAFETY: the wrapper contains exactly three aligned tagged words.
        let (length, capacity, buffer) = unsafe {
            (
                TaggedValue::from_raw(wrapper.read()),
                TaggedValue::from_raw(wrapper.add(1).read()),
                TaggedValue::from_raw(wrapper.add(2).read()),
            )
        };
        let length = length
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(VmError::TypeError)?;
        let capacity = capacity
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .ok_or(VmError::TypeError)?;
        if length > capacity
            || capacity > MAX_CAPACITY
            || (capacity != 0 && !capacity.is_power_of_two())
        {
            return Err(VmError::TypeError);
        }
        if capacity == 0 {
            if !buffer.is_null() {
                return Err(VmError::TypeError);
            }
            return Ok(MapLayout {
                wrapper,
                buffer: std::ptr::null_mut(),
                length,
                capacity,
            });
        }
        let buffer = buffer.as_heap_ptr().ok_or(VmError::TypeError)?;
        // SAFETY: the reference is reachable from the rooted wrapper.
        let header = unsafe { ObjectHeader::from_payload_ptr(buffer) };
        if !header.is_ordinary() {
            return Err(VmError::TypeError);
        }
        if self
            .state
            .type_pool
            .checked_collection_role(header.type_index)
            .map_err(|_| VmError::TypeError)?
            != Some(CollectionRole::MapBuffer)
            || header.payload_words() != capacity * 4
        {
            return Err(VmError::TypeError);
        }
        let layout = MapLayout {
            wrapper,
            buffer: buffer.cast_mut().cast(),
            length,
            capacity,
        };
        // Collection roles cannot be mutated by ordinary field/object opcodes.
        // Check touched buckets in find; traversing the whole table here would
        // make a sequence of inserts quadratic even without hash collisions.
        Ok(layout)
    }

    fn map_buffer(&mut self, capacity: usize) -> Result<TaggedValue, VmError> {
        let words = u16::try_from(capacity.checked_mul(4).ok_or(VmError::ObjectTooLarge)?)
            .map_err(|_| VmError::ObjectTooLarge)?;
        let ty = self
            .state
            .type_pool
            .map_buffer_type()
            .ok_or(VmError::TypeError)?;
        // SAFETY: mutator owns this allocation; initialize every slot before publication.
        let pointer =
            unsafe { self.state.heap.alloc_object(ty, words) }.ok_or(VmError::OutOfMemory)?;
        for bucket in 0..capacity {
            // SAFETY: each four-word bucket lies in the fresh payload.
            unsafe {
                let slot = pointer.as_ptr().cast::<u64>().add(bucket * 4);
                slot.write(TaggedValue::from_u64(EMPTY).raw());
                slot.add(1).write(TaggedValue::from_u64(0).raw());
                slot.add(2).write(TaggedValue::UNIT.raw());
                slot.add(3).write(TaggedValue::UNIT.raw());
            }
        }
        // SAFETY: aligned, live, completely initialized managed allocation.
        Ok(unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) })
    }

    pub(super) fn new_map(&mut self, capacity: usize) -> Result<TaggedValue, VmError> {
        if capacity > MAX_CAPACITY {
            return Err(VmError::ObjectTooLarge);
        }
        let capacity = if capacity == 0 {
            0
        } else {
            capacity
                .checked_next_power_of_two()
                .ok_or(VmError::ObjectTooLarge)?
        };
        let ty = self.state.type_pool.map_type().ok_or(VmError::TypeError)?;
        self.state
            .type_pool
            .map_buffer_type()
            .ok_or(VmError::TypeError)?;
        // SAFETY: initialize the empty wrapper before any further allocation.
        let pointer = unsafe { self.state.heap.alloc_object(ty, 3) }.ok_or(VmError::OutOfMemory)?;
        // SAFETY: the new wrapper has exactly three aligned payload words.
        unsafe {
            let wrapper = pointer.as_ptr().cast::<u64>();
            wrapper.write(TaggedValue::from_u64(0).raw());
            wrapper.add(1).write(TaggedValue::from_u64(0).raw());
            wrapper.add(2).write(TaggedValue::NULL.raw());
        }
        // SAFETY: wrapper is live, aligned and initialized.
        let value = unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) };
        let root = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&[value], |vm| {
            if capacity != 0 {
                let buffer = vm.map_buffer(capacity)?;
                vm.roots.temporary_values.borrow_mut().push(buffer);
                let value = vm.roots.temporary_values.borrow()[root];
                let layout = vm.map_layout(value)?;
                // SAFETY: no safepoint follows; publish the completed buffer atomically
                // with respect to VM operations and collection root scanning.
                unsafe {
                    layout
                        .wrapper
                        .add(1)
                        .write(TaggedValue::from_u64(capacity as u64).raw());
                    layout.wrapper.add(2).write(buffer.raw());
                }
            }
            Ok(vm.roots.temporary_values.borrow()[root])
        })
    }

    pub(super) fn map_len(&self, map: TaggedValue) -> Result<usize, VmError> {
        Ok(self.map_layout(map)?.length)
    }

    pub(super) fn map_keys(&mut self, map: TaggedValue) -> Result<TaggedValue, VmError> {
        let root = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&[map], |vm| {
            // Validate all buckets and the occupied count before allocation.
            let layout = vm.map_layout(map)?;
            let buckets = layout.occupied_buckets()?;
            for &bucket in &buckets {
                string_key(layout.slot(bucket, 2))?;
            }
            let keys = vm.new_list(buckets.len())?;
            vm.with_temporary_roots(&[keys], |vm| {
                // Reacquire the layout after allocating the List. No managed
                // allocation or callback occurs while copying the live keys.
                let map = vm.roots.temporary_values.borrow()[root];
                let layout = vm.map_layout(map)?;
                for (index, bucket) in buckets.into_iter().enumerate() {
                    vm.list_set(
                        keys,
                        TaggedValue::from_u64(index as u64),
                        layout.slot(bucket, 2),
                    )?;
                }
                Ok(keys)
            })
        })
    }

    pub(super) fn map_get(
        &self,
        map: TaggedValue,
        key: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let key = string_key(key)?;
        let layout = self.map_layout(map)?;
        Ok(layout
            .find(&key, key_hash(&key))?
            .0
            .map_or(TaggedValue::NULL, |bucket| layout.slot(bucket, 3)))
    }

    pub(super) fn map_contains(&self, map: TaggedValue, key: TaggedValue) -> Result<bool, VmError> {
        let key = string_key(key)?;
        Ok(self
            .map_layout(map)?
            .find(&key, key_hash(&key))?
            .0
            .is_some())
    }

    pub(super) fn map_set(
        &mut self,
        map: TaggedValue,
        key: TaggedValue,
        value: TaggedValue,
    ) -> Result<(), VmError> {
        let text = string_key(key)?;
        let hash = key_hash(&text);
        let root = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&[map, key, value], |vm| {
            let old = vm.map_layout(map)?;
            let (existing, available) = old.find(&text, hash)?;
            if let Some(bucket) = existing {
                old.write_bucket(bucket, hash, key, value);
                return Ok(());
            }
            let length = old.length + 1;
            let grow =
                old.capacity == 0 || (length > old.capacity * 3 / 4 && old.capacity < MAX_CAPACITY);
            if !grow {
                let bucket = available.ok_or(VmError::ObjectTooLarge)?;
                old.write_bucket(bucket, hash, key, value);
                // SAFETY: wrapper is rooted and has three words, no allocation intervenes.
                unsafe {
                    old.wrapper
                        .write(TaggedValue::from_u64(length as u64).raw());
                }
                return Ok(());
            }
            let capacity = old
                .capacity
                .checked_mul(2)
                .ok_or(VmError::ObjectTooLarge)?
                .max(4);
            let buffer = vm.map_buffer(capacity)?;
            vm.roots.temporary_values.borrow_mut().push(buffer);
            // Reload every managed operand after buffer allocation.
            let map = vm.roots.temporary_values.borrow()[root];
            let key = vm.roots.temporary_values.borrow()[root + 1];
            let value = vm.roots.temporary_values.borrow()[root + 2];
            let old = vm.map_layout(map)?;
            let fresh = MapLayout {
                wrapper: old.wrapper,
                buffer: buffer
                    .as_heap_ptr()
                    .ok_or(VmError::TypeError)?
                    .cast_mut()
                    .cast(),
                length: 0,
                capacity,
            };
            let mut occupied = 0;
            for bucket in 0..old.capacity {
                if old.bucket_state(bucket)? == OCCUPIED {
                    occupied += 1;
                    let old_key = old.slot(bucket, 2);
                    let old_text = string_key(old_key)?;
                    let old_hash = key_hash(&old_text);
                    if old.slot(bucket, 1).as_u64() != Some(old_hash) {
                        return Err(VmError::TypeError);
                    }
                    let target = fresh
                        .find(&old_text, old_hash)?
                        .1
                        .ok_or(VmError::TypeError)?;
                    fresh.write_bucket(target, old_hash, old_key, old.slot(bucket, 3));
                }
            }
            if occupied != old.length {
                return Err(VmError::TypeError);
            }
            let bucket = fresh.find(&text, hash)?.1.ok_or(VmError::TypeError)?;
            fresh.write_bucket(bucket, hash, key, value);
            // SAFETY: fresh buffer is fully initialized and registered; commit
            // only after all fallible checks. No GC or user callback intervenes.
            unsafe {
                old.wrapper
                    .write(TaggedValue::from_u64(length as u64).raw());
                old.wrapper
                    .add(1)
                    .write(TaggedValue::from_u64(capacity as u64).raw());
                old.wrapper.add(2).write(buffer.raw());
            }
            Ok(())
        })
    }

    pub(super) fn map_remove(
        &mut self,
        map: TaggedValue,
        key: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let key = string_key(key)?;
        let layout = self.map_layout(map)?;
        let Some(bucket) = layout.find(&key, key_hash(&key))?.0 else {
            return Ok(TaggedValue::NULL);
        };
        let value = layout.slot(bucket, 3);
        let length = layout.length.checked_sub(1).ok_or(VmError::TypeError)?;
        // SAFETY: checked bucket and wrapper remain live without allocations.
        // Clear both references so removed keys and values can be collected.
        unsafe {
            let slot = layout.buffer.add(bucket * 4);
            slot.write(TaggedValue::from_u64(DELETED).raw());
            slot.add(1).write(TaggedValue::from_u64(0).raw());
            slot.add(2).write(TaggedValue::UNIT.raw());
            slot.add(3).write(TaggedValue::UNIT.raw());
            layout
                .wrapper
                .write(TaggedValue::from_u64(length as u64).raw());
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::make_vm;

    #[test]
    fn keys_compare_by_content_and_null_is_distinct_from_absence() {
        let mut vm = make_vm();
        let map = vm.new_map(0).unwrap();
        let root = vm.roots.temporary_values.borrow().len();
        vm.with_temporary_roots(&[map], |vm| {
            let key = vm.alloc_string("same")?;
            vm.roots.temporary_values.borrow_mut().push(key);
            let equivalent = vm.alloc_string("same")?;
            vm.roots.temporary_values.borrow_mut().push(equivalent);
            let map = vm.roots.temporary_values.borrow()[root];
            let key = vm.roots.temporary_values.borrow()[root + 1];
            let equivalent = vm.roots.temporary_values.borrow()[root + 2];
            assert_ne!(key, equivalent);
            vm.map_set(map, key, TaggedValue::NULL)?;
            let map = vm.roots.temporary_values.borrow()[root];
            let key = vm.roots.temporary_values.borrow()[root + 1];
            let equivalent = vm.roots.temporary_values.borrow()[root + 2];
            assert!(vm.map_contains(map, equivalent)?);
            assert!(vm.map_get(map, equivalent)?.is_null());
            vm.map_set(map, equivalent, TaggedValue::UNIT)?;
            assert_eq!(vm.map_len(map)?, 1);
            assert!(vm.map_get(map, key)?.is_unit());
            assert!(vm.map_remove(map, equivalent)?.is_unit());
            assert!(!vm.map_contains(map, key)?);
            assert!(vm.map_remove(map, key)?.is_null());
            assert_eq!(vm.map_len(map)?, 0);
            assert!(matches!(
                vm.map_set(map, TaggedValue::TRUE, TaggedValue::FALSE),
                Err(VmError::TypeError)
            ));
            assert_eq!(vm.map_len(map)?, 0);
            Ok(())
        })
        .unwrap();
        assert!(vm.roots.temporary_values.borrow().is_empty());
    }

    #[test]
    fn colliding_keys_survive_deletion_and_reuse_tombstones() {
        let keys: Vec<_> = (0..100)
            .map(|n| format!("key{n}"))
            .filter(|key| key_hash(key) & 3 == 0)
            .take(4)
            .collect();
        assert_eq!(keys.len(), 4);
        let mut vm = make_vm();
        let map = vm.new_map(4).unwrap();
        vm.with_temporary_roots(&[map], |vm| {
            let mut values = Vec::new();
            for key in &keys {
                let key = vm.alloc_string(key)?;
                vm.roots.temporary_values.borrow_mut().push(key);
                values.push(key);
            }
            for (index, &key) in values.iter().take(3).enumerate() {
                vm.map_set(map, key, TaggedValue::from_i64(index as i64))?;
            }
            assert_eq!(vm.map_remove(map, values[0])?.as_i64(), Some(0));
            assert_eq!(vm.map_get(map, values[2])?.as_i64(), Some(2));
            vm.map_set(map, values[3], TaggedValue::TRUE)?;
            assert_eq!(vm.map_layout(map)?.capacity, 4);
            assert_eq!(vm.map_get(map, values[3])?, TaggedValue::TRUE);
            assert_eq!(vm.map_len(map)?, 3);
            let snapshot = vm.map_keys(map)?;
            vm.roots.temporary_values.borrow_mut().push(snapshot);
            assert_eq!(vm.list_len(snapshot)?, 3);
            let copied: Vec<_> = (0..3)
                .map(|index| vm.list_get(snapshot, TaggedValue::from_i64(index)))
                .collect::<Result<_, _>>()?;
            assert!(!copied.contains(&values[0]));
            assert!(values[1..].iter().all(|key| copied.contains(key)));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn growth_and_full_capacity_fail_without_changing_existing_entries() {
        let mut vm = make_vm();
        let map = vm.new_map(0).unwrap();
        vm.with_temporary_roots(&[map], |vm| {
            for index in 0..MAX_CAPACITY {
                let key = vm.alloc_string(&format!("entry{index}"))?;
                vm.map_set(map, key, TaggedValue::from_i64(index as i64))?;
            }
            assert_eq!(vm.map_layout(map)?.capacity, MAX_CAPACITY);
            assert_eq!(vm.map_len(map)?, MAX_CAPACITY);
            let snapshot = vm.map_keys(map)?;
            vm.roots.temporary_values.borrow_mut().push(snapshot);
            assert_eq!(vm.list_len(snapshot)?, MAX_CAPACITY);
            let mut seen = std::collections::HashSet::new();
            for index in 0..MAX_CAPACITY {
                let key = vm.list_get(snapshot, TaggedValue::from_u64(index as u64))?;
                assert!(seen.insert(string_key(key)?));
                assert!(vm.map_contains(map, key)?);
            }
            let missing = vm.alloc_string("overflow")?;
            assert!(matches!(
                vm.map_set(map, missing, TaggedValue::TRUE),
                Err(VmError::ObjectTooLarge)
            ));
            assert_eq!(vm.map_len(map)?, MAX_CAPACITY);
            let existing = vm.alloc_string("entry4096")?;
            vm.roots.temporary_values.borrow_mut().push(existing);
            vm.map_set(map, existing, TaggedValue::TRUE)?;
            assert_eq!(vm.map_get(map, existing)?, TaggedValue::TRUE);
            assert_eq!(vm.map_remove(map, existing)?, TaggedValue::TRUE);
            let replacement = vm.alloc_string("replacement")?;
            vm.map_set(map, replacement, TaggedValue::FALSE)?;
            assert_eq!(vm.map_get(map, replacement)?, TaggedValue::FALSE);
            assert_eq!(vm.map_len(map)?, MAX_CAPACITY);
            Ok(())
        })
        .unwrap();
        assert!(matches!(
            vm.new_map(MAX_CAPACITY + 1),
            Err(VmError::ObjectTooLarge)
        ));
    }

    #[test]
    fn display_shares_nested_collection_traversal_and_rejects_bad_buckets() {
        let mut vm = make_vm();
        let map = vm.new_map(4).unwrap();
        vm.with_temporary_roots(&[map], |vm| {
            let key = vm.alloc_string("self")?;
            vm.roots.temporary_values.borrow_mut().push(key);
            vm.map_set(map, key, map)?;
            assert_eq!(
                vm.format_aggregate(map)?.as_deref(),
                Some("{\"self\": <cycle>}")
            );
            let list = vm.new_list(1)?;
            vm.roots.temporary_values.borrow_mut().push(list);
            let text = vm.alloc_string("line\n")?;
            vm.list_set(list, TaggedValue::from_i64(0), text)?;
            vm.map_set(map, key, list)?;
            assert_eq!(
                vm.format_aggregate(map)?.as_deref(),
                Some("{\"self\": [\"line\\n\"]}")
            );
            let layout = vm.map_layout(map)?;
            let bucket = layout.find("self", key_hash("self"))?.0.unwrap();
            // SAFETY: intentionally damage the state of a live four-word test bucket.
            unsafe {
                layout
                    .buffer
                    .add(bucket * 4)
                    .write(TaggedValue::from_u64(7).raw());
            }
            assert!(matches!(vm.map_get(map, key), Err(VmError::TypeError)));
            assert!(matches!(vm.format_aggregate(map), Err(VmError::TypeError)));
            assert!(matches!(vm.map_keys(map), Err(VmError::TypeError)));
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn key_snapshots_reject_corrupt_string_contents_and_occupied_counts() {
        let mut vm = make_vm();
        let map = vm.new_map(0).unwrap();
        vm.with_temporary_roots(&[map], |vm| {
            let empty = vm.map_keys(map)?;
            vm.roots.temporary_values.borrow_mut().push(empty);
            assert_eq!(vm.list_len(empty)?, 0);
            let key = vm.alloc_string("key")?;
            vm.roots.temporary_values.borrow_mut().push(key);
            vm.map_set(map, key, TaggedValue::NULL)?;
            let layout = vm.map_layout(map)?;
            // SAFETY: intentionally corrupt the tagged length of the rooted test wrapper.
            unsafe { layout.wrapper.write(TaggedValue::from_u64(2).raw()) };
            assert!(matches!(vm.map_keys(map), Err(VmError::TypeError)));
            // SAFETY: restore wrapper length and corrupt only the live String's length word.
            let pointer = key.as_heap_ptr().unwrap().cast_mut().cast::<u64>();
            unsafe {
                layout.wrapper.write(TaggedValue::from_u64(1).raw());
                pointer.write(u64::MAX);
            }
            assert!(matches!(vm.map_keys(map), Err(VmError::TypeError)));
            // SAFETY: restore the three-byte String then give it invalid UTF-8.
            unsafe {
                pointer.write(3);
                pointer.cast::<u8>().add(8).write(0xff);
            }
            assert!(matches!(vm.map_keys(map), Err(VmError::TypeError)));
            Ok(())
        })
        .unwrap();
        assert!(vm.roots.temporary_values.borrow().is_empty());
    }

    #[test]
    fn new_map_index_and_dynamic_call_opcodes_use_checked_string_keys() {
        use nsbc::{AddrMode, Constant, FuncId, Instruction, Opcode, Reg};
        use runtime::FunctionCode;
        let mut vm = make_vm();
        vm.push_constant(&Constant::Str("key".into())).unwrap();
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: None,
            func_id: FuncId(0),
            instructions: [
                Instruction::a_type(Opcode::NewMap, AddrMode::Imm, Reg(10), Reg(0), 3),
                Instruction::load_const(Reg(1), 0),
                Instruction::load_imm(Reg(0), 41),
                Instruction::a_type(Opcode::StoreIndex, AddrMode::Imm, Reg(0), Reg(10), 1),
                Instruction::a_type(Opcode::LoadIndex, AddrMode::Imm, Reg(2), Reg(10), 1),
                Instruction::mov(Reg(0), Reg(1)),
                Instruction::load_imm(Reg(1), 42),
                Instruction::call_method(Reg(10), str_interner::intern("update").as_u32(), 2),
                Instruction::load_const(Reg(0), 0),
                Instruction::call_indirect(Reg(10), 1),
                Instruction::ret(Reg(0)),
            ]
            .into_iter()
            .map(Instruction::encode)
            .collect(),
            register_count: 32,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        let task = vm.spawn_root(FuncId(0));
        assert!(matches!(vm.run(), crate::VmResult::Finished));
        assert_eq!(vm.task_result_i64(task).unwrap(), 42);
    }
}
