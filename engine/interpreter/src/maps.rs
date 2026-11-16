//! String-keyed maps retain identity while replacing a managed tagged bucket buffer.

use gc::ObjectHeader;
use runtime::TaggedValue;
use type_pool::CollectionRole;

use crate::collections::{read_u64_atomic, write_u64_atomic};
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
    pub(super) fn slot(&self, bucket: usize, field: usize) -> TaggedValue {
        TaggedValue::from_raw(read_u64_atomic(self.buffer, bucket * 4 + field))
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

    pub(super) fn snapshot_entries(&self) -> Result<Vec<(String, TaggedValue)>, VmError> {
        let buckets = self.occupied_buckets()?;
        let mut out = Vec::with_capacity(buckets.len());
        for bucket in buckets {
            let k = string_key(self.slot(bucket, 2))?;
            let v = self.slot(bucket, 3);
            out.push((k, v));
        }
        Ok(out)
    }

    fn find_key(
        &self,
        query_key: Option<TaggedValue>,
        key: &str,
        hash: u64,
    ) -> Result<(Option<usize>, Option<usize>), VmError> {
        let mut available = None;
        for offset in 0..self.capacity {
            let bucket = (hash as usize).wrapping_add(offset) & (self.capacity - 1);
            match self.bucket_state(bucket)? {
                EMPTY => return Ok((None, available.or(Some(bucket)))),
                DELETED => {
                    available.get_or_insert(bucket);
                }
                OCCUPIED => {
                    if self.slot(bucket, 1).as_u64() == Some(hash) {
                        let stored_key = self.slot(bucket, 2);
                        if query_key == Some(stored_key) {
                            // Verify valid UTF-8 string invariant without allocating.
                            if crate::builtin_ctx::with_heap_string_str(stored_key, |_| ())
                                .is_none()
                            {
                                return Err(VmError::TypeError);
                            }
                            return Ok((Some(bucket), available));
                        }
                        let matched =
                            crate::builtin_ctx::with_heap_string_str(stored_key, |s| s == key)
                                .ok_or(VmError::TypeError)?;
                        if matched {
                            return Ok((Some(bucket), available));
                        }
                    }
                }
                _ => return Err(VmError::TypeError),
            }
        }
        Ok((None, available))
    }

    fn find(&self, key: &str, hash: u64) -> Result<(Option<usize>, Option<usize>), VmError> {
        self.find_key(None, key, hash)
    }

    fn write_bucket(&self, bucket: usize, hash: u64, key: TaggedValue, value: TaggedValue) {
        let base = bucket * 4;
        write_u64_atomic(self.buffer, base, TaggedValue::from_u64(OCCUPIED).raw());
        write_u64_atomic(self.buffer, base + 1, TaggedValue::from_u64(hash).raw());
        write_u64_atomic(self.buffer, base + 2, key.raw());
        write_u64_atomic(self.buffer, base + 3, value.raw());
    }
}

impl Vm {
    #[inline]
    fn cached_map_roles(&self) -> Option<(type_pool::TypeIndex, type_pool::TypeIndex)> {
        {
            let cache = self.sync_meta_cache();
            if let Some(cached) = cache.map_roles {
                return cached;
            }
        }
        let pair = {
            let tp = self.type_pool_read();
            tp.map_type().zip(tp.map_buffer_type())
        };
        self.sync_meta_cache().map_roles = Some(pair);
        pair
    }

    pub(super) fn map_layout_unlocked(&self, value: TaggedValue) -> Result<MapLayout, VmError> {
        let pointer = value.as_heap_ptr().ok_or(VmError::TypeError)?;
        // SAFETY: operand is a live managed root in the current mutator operation.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        if !header.is_ordinary() || header.payload_words() != 3 {
            return Err(VmError::TypeError);
        }
        let fast_roles = self.cached_map_roles();
        let is_map = if fast_roles.is_some_and(|(mt, _)| header.type_index == mt) {
            true
        } else {
            self.type_pool_read()
                .checked_collection_role(header.type_index)
                .map_err(|_| VmError::TypeError)?
                == Some(CollectionRole::Map)
        };
        if !is_map {
            return Err(VmError::TypeError);
        }
        let wrapper = pointer.cast_mut().cast::<u64>();
        let length = TaggedValue::from_raw(read_u64_atomic(wrapper, 0));
        let capacity = TaggedValue::from_raw(read_u64_atomic(wrapper, 1));
        let buffer = TaggedValue::from_raw(read_u64_atomic(wrapper, 2));
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
        if !header.is_ordinary() || header.payload_words() != capacity * 4 {
            return Err(VmError::TypeError);
        }
        let is_buf = if fast_roles.is_some_and(|(_, bt)| header.type_index == bt) {
            true
        } else {
            self.type_pool_read()
                .checked_collection_role(header.type_index)
                .map_err(|_| VmError::TypeError)?
                == Some(CollectionRole::MapBuffer)
        };
        if !is_buf {
            return Err(VmError::TypeError);
        }
        Ok(MapLayout {
            wrapper,
            buffer: buffer.cast_mut().cast(),
            length,
            capacity,
        })
    }

    #[cfg(test)]
    pub(super) fn map_layout(&self, value: TaggedValue) -> Result<MapLayout, VmError> {
        let _guard = self.stripe_read(value)?;
        self.map_layout_unlocked(value)
    }

    fn snapshot_map_pairs(
        &self,
        map: TaggedValue,
    ) -> Result<Vec<(TaggedValue, TaggedValue)>, VmError> {
        let _guard = self.stripe_read(map)?;
        let layout = self.map_layout_unlocked(map)?;
        let buckets = layout.occupied_buckets()?;
        let mut out = Vec::with_capacity(buckets.len());
        for bucket in buckets {
            let key = layout.slot(bucket, 2);
            let _ = string_key(key)?;
            out.push((key, layout.slot(bucket, 3)));
        }
        Ok(out)
    }

    fn map_buffer(&mut self, capacity: usize) -> Result<TaggedValue, VmError> {
        let words = u16::try_from(capacity.checked_mul(4).ok_or(VmError::ObjectTooLarge)?)
            .map_err(|_| VmError::ObjectTooLarge)?;
        let ty = self
            .type_pool_read()
            .map_buffer_type()
            .ok_or(VmError::TypeError)?;
        // SAFETY: mutator owns this allocation with no lock held across `alloc_object`.
        let pointer =
            unsafe { self.worker.heap.alloc_object(ty, words) }.ok_or(VmError::OutOfMemory)?;
        let base = pointer.as_ptr().cast::<u64>();
        for bucket in 0..capacity {
            let slot = bucket * 4;
            write_u64_atomic(base, slot, TaggedValue::from_u64(EMPTY).raw());
            write_u64_atomic(base, slot + 1, TaggedValue::from_u64(0).raw());
            write_u64_atomic(base, slot + 2, TaggedValue::UNIT.raw());
            write_u64_atomic(base, slot + 3, TaggedValue::UNIT.raw());
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
        let ty = {
            let tp = self.type_pool_read();
            let map_ty = tp.map_type().ok_or(VmError::TypeError)?;
            tp.map_buffer_type().ok_or(VmError::TypeError)?;
            map_ty
        };
        // SAFETY: initialize the empty wrapper before any further allocation.
        let pointer =
            unsafe { self.worker.heap.alloc_object(ty, 3) }.ok_or(VmError::OutOfMemory)?;
        let wrapper = pointer.as_ptr().cast::<u64>();
        write_u64_atomic(wrapper, 0, TaggedValue::from_u64(0).raw());
        write_u64_atomic(wrapper, 1, TaggedValue::from_u64(0).raw());
        write_u64_atomic(wrapper, 2, TaggedValue::NULL.raw());
        // SAFETY: wrapper is live, aligned and initialized.
        let value = unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) };
        let root = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&[value], |vm| {
            if capacity != 0 {
                let buffer = vm.map_buffer(capacity)?;
                vm.worker_roots().temporary_values.borrow_mut().push(buffer);
                let value = vm.worker_roots().temporary_values.borrow()[root];
                let layout = vm.map_layout_unlocked(value)?;
                write_u64_atomic(
                    layout.wrapper,
                    1,
                    TaggedValue::from_u64(capacity as u64).raw(),
                );
                write_u64_atomic(layout.wrapper, 2, buffer.raw());
            }
            Ok(vm.worker_roots().temporary_values.borrow()[root])
        })
    }

    pub(super) fn map_len(&self, map: TaggedValue) -> Result<usize, VmError> {
        let _guard = self.stripe_read(map)?;
        Ok(self.map_layout_unlocked(map)?.length)
    }

    fn map_column(&mut self, map: TaggedValue, select_key: bool) -> Result<TaggedValue, VmError> {
        let pairs = self.snapshot_map_pairs(map)?;
        let selected: Vec<TaggedValue> = pairs
            .into_iter()
            .map(|(k, v)| if select_key { k } else { v })
            .collect();
        let base = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&selected, |vm| {
            let column = vm.new_list(selected.len())?;
            let layout = vm.list_layout_unlocked(column)?;
            let roots = vm.worker_roots().temporary_values.borrow();
            for index in 0..selected.len() {
                write_u64_atomic(layout.buffer, index, roots[base + index].raw());
            }
            Ok(column)
        })
    }

    pub(super) fn map_keys(&mut self, map: TaggedValue) -> Result<TaggedValue, VmError> {
        self.map_column(map, true)
    }

    pub(super) fn map_values(&mut self, map: TaggedValue) -> Result<TaggedValue, VmError> {
        self.map_column(map, false)
    }

    pub(super) fn map_entries(&mut self, map: TaggedValue) -> Result<TaggedValue, VmError> {
        let pairs = self.snapshot_map_pairs(map)?;
        let count = pairs.len();
        let mut flat = Vec::with_capacity(count * 2);
        for (k, v) in pairs {
            flat.push(k);
            flat.push(v);
        }
        let base = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&flat, |vm| {
            let column = vm.new_list(count)?;
            let col_slot = vm.worker_roots().temporary_values.borrow().len();
            vm.worker_roots().temporary_values.borrow_mut().push(column);
            let str_ty = type_pool::Intrinsic::Str.type_index();
            let any_ty = type_pool::Intrinsic::Any.type_index();
            for index in 0..count {
                let (key, value) = {
                    let roots = vm.worker_roots().temporary_values.borrow();
                    (roots[base + index * 2], roots[base + index * 2 + 1])
                };
                let pair = vm.alloc_tuple(&[str_ty, any_ty], &[key, value])?;
                let column = vm.worker_roots().temporary_values.borrow()[col_slot];
                vm.list_set(column, TaggedValue::from_u64(index as u64), pair)?;
            }
            Ok(vm.worker_roots().temporary_values.borrow()[col_slot])
        })
    }

    pub(super) fn map_copy(&mut self, map: TaggedValue) -> Result<TaggedValue, VmError> {
        let dest = self.new_map(0)?;
        let root = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&[map, dest], |vm| {
            let map = vm.worker_roots().temporary_values.borrow()[root];
            let dest = vm.worker_roots().temporary_values.borrow()[root + 1];
            vm.map_merge(dest, map)?;
            Ok(vm.worker_roots().temporary_values.borrow()[root + 1])
        })
    }

    pub(super) fn map_clear(&mut self, map: TaggedValue) -> Result<(), VmError> {
        let _guard = self.stripe_write(map)?;
        let layout = self.map_layout_unlocked(map)?;
        if layout.capacity == 0 {
            return Ok(());
        }
        let _ = layout.occupied_buckets()?;
        for bucket in 0..layout.capacity {
            let slot = bucket * 4;
            write_u64_atomic(layout.buffer, slot, TaggedValue::from_u64(EMPTY).raw());
            write_u64_atomic(layout.buffer, slot + 1, TaggedValue::from_u64(0).raw());
            write_u64_atomic(layout.buffer, slot + 2, TaggedValue::UNIT.raw());
            write_u64_atomic(layout.buffer, slot + 3, TaggedValue::UNIT.raw());
        }
        write_u64_atomic(layout.wrapper, 0, TaggedValue::from_u64(0).raw());
        Ok(())
    }

    pub(super) fn map_merge(
        &mut self,
        map: TaggedValue,
        other: TaggedValue,
    ) -> Result<(), VmError> {
        let root = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&[map, other], |vm| {
            let other = vm.worker_roots().temporary_values.borrow()[root + 1];
            let entries = vm.snapshot_map_pairs(other)?;
            let count = entries.len();
            let entry_base = vm.worker_roots().temporary_values.borrow().len();
            {
                let mut roots = vm.worker_roots().temporary_values.borrow_mut();
                for &(key, value) in &entries {
                    roots.push(key);
                    roots.push(value);
                }
            }
            for idx in 0..count {
                let (map, key, value) = {
                    let roots = vm.worker_roots().temporary_values.borrow();
                    (
                        roots[root],
                        roots[entry_base + idx * 2],
                        roots[entry_base + idx * 2 + 1],
                    )
                };
                vm.map_set(map, key, value)?;
            }
            Ok(())
        })
    }

    pub(super) fn map_get_or_insert(
        &mut self,
        map: TaggedValue,
        key: TaggedValue,
        value: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        Ok(self.map_set_internal(map, key, value, false)?.0)
    }

    pub(super) fn map_insert_absent(
        &mut self,
        map: TaggedValue,
        key: TaggedValue,
        value: TaggedValue,
    ) -> Result<bool, VmError> {
        Ok(self.map_set_internal(map, key, value, false)?.1)
    }

    pub(super) fn map_get(
        &self,
        map: TaggedValue,
        key: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let _guard = self.stripe_read(map)?;
        let layout = self.map_layout_unlocked(map)?;
        crate::builtin_ctx::with_heap_string_str(key, |k_str| {
            layout
                .find_key(Some(key), k_str, key_hash(k_str))
                .map(|(b, _)| b.map_or(TaggedValue::NULL, |bucket| layout.slot(bucket, 3)))
        })
        .ok_or(VmError::TypeError)?
    }

    pub(super) fn map_contains(&self, map: TaggedValue, key: TaggedValue) -> Result<bool, VmError> {
        let _guard = self.stripe_read(map)?;
        let layout = self.map_layout_unlocked(map)?;
        crate::builtin_ctx::with_heap_string_str(key, |k_str| {
            layout
                .find_key(Some(key), k_str, key_hash(k_str))
                .map(|(b, _)| b.is_some())
        })
        .ok_or(VmError::TypeError)?
    }

    pub(super) fn map_set(
        &mut self,
        map: TaggedValue,
        key: TaggedValue,
        value: TaggedValue,
    ) -> Result<(), VmError> {
        let _ = self.map_set_internal(map, key, value, true)?;
        Ok(())
    }

    fn map_set_internal(
        &mut self,
        map: TaggedValue,
        key: TaggedValue,
        value: TaggedValue,
        overwrite: bool,
    ) -> Result<(TaggedValue, bool), VmError> {
        let hash =
            crate::builtin_ctx::with_heap_string_str(key, key_hash).ok_or(VmError::TypeError)?;
        // Fast path: in-place update or non-growing insert without allocating a Rust `String`.
        {
            let _guard = self.stripe_write(map)?;
            let old = self.map_layout_unlocked(map)?;
            if old.capacity > 0 {
                let found = crate::builtin_ctx::with_heap_string_str(key, |k_str| {
                    old.find_key(Some(key), k_str, hash)
                })
                .ok_or(VmError::TypeError)??;
                if let Some(bucket) = found.0 {
                    if overwrite {
                        old.write_bucket(bucket, hash, key, value);
                        return Ok((value, false));
                    }
                    return Ok((old.slot(bucket, 3), false));
                }
                let length = old.length + 1;
                let grow = length > old.capacity * 3 / 4 && old.capacity < MAX_CAPACITY;
                if !grow && let Some(bucket) = found.1 {
                    old.write_bucket(bucket, hash, key, value);
                    write_u64_atomic(old.wrapper, 0, TaggedValue::from_u64(length as u64).raw());
                    return Ok((value, true));
                }
            }
        }
        let text = string_key(key)?;
        let root = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&[map, key, value], |vm| {
            loop {
                let needed_cap = {
                    let map = vm.worker_roots().temporary_values.borrow()[root];
                    let key = vm.worker_roots().temporary_values.borrow()[root + 1];
                    let value = vm.worker_roots().temporary_values.borrow()[root + 2];
                    let _guard = vm.stripe_write(map)?;
                    let old = vm.map_layout_unlocked(map)?;
                    let (existing, available) = old.find(&text, hash)?;
                    if let Some(bucket) = existing {
                        if overwrite {
                            old.write_bucket(bucket, hash, key, value);
                            return Ok((value, false));
                        }
                        return Ok((old.slot(bucket, 3), false));
                    }
                    let length = old.length + 1;
                    let grow = old.capacity == 0
                        || (length > old.capacity * 3 / 4 && old.capacity < MAX_CAPACITY);
                    if !grow {
                        let bucket = available.ok_or(VmError::ObjectTooLarge)?;
                        old.write_bucket(bucket, hash, key, value);
                        write_u64_atomic(
                            old.wrapper,
                            0,
                            TaggedValue::from_u64(length as u64).raw(),
                        );
                        return Ok((value, true));
                    }
                    old.capacity
                        .checked_mul(2)
                        .ok_or(VmError::ObjectTooLarge)?
                        .max(4)
                };

                // Allocate rehash buffer outside the stripe lock.
                let buffer = vm.map_buffer(needed_cap)?;
                let buf_slot = vm.worker_roots().temporary_values.borrow().len();
                vm.worker_roots().temporary_values.borrow_mut().push(buffer);

                let map = vm.worker_roots().temporary_values.borrow()[root];
                let key = vm.worker_roots().temporary_values.borrow()[root + 1];
                let value = vm.worker_roots().temporary_values.borrow()[root + 2];
                let buffer = vm.worker_roots().temporary_values.borrow()[buf_slot];
                let _guard = vm.stripe_write(map)?;
                let old = vm.map_layout_unlocked(map)?;
                let (existing, available) = old.find(&text, hash)?;
                if let Some(bucket) = existing {
                    if overwrite {
                        old.write_bucket(bucket, hash, key, value);
                        return Ok((value, false));
                    }
                    return Ok((old.slot(bucket, 3), false));
                }
                let length = old.length + 1;
                let grow = old.capacity == 0
                    || (length > old.capacity * 3 / 4 && old.capacity < MAX_CAPACITY);
                if !grow {
                    let bucket = available.ok_or(VmError::ObjectTooLarge)?;
                    old.write_bucket(bucket, hash, key, value);
                    write_u64_atomic(old.wrapper, 0, TaggedValue::from_u64(length as u64).raw());
                    return Ok((value, true));
                }
                if old.capacity >= needed_cap {
                    continue;
                }

                let fresh = MapLayout {
                    wrapper: old.wrapper,
                    buffer: buffer
                        .as_heap_ptr()
                        .ok_or(VmError::TypeError)?
                        .cast_mut()
                        .cast(),
                    length: 0,
                    capacity: needed_cap,
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
                write_u64_atomic(old.wrapper, 0, TaggedValue::from_u64(length as u64).raw());
                write_u64_atomic(
                    old.wrapper,
                    1,
                    TaggedValue::from_u64(needed_cap as u64).raw(),
                );
                write_u64_atomic(old.wrapper, 2, buffer.raw());
                return Ok((value, true));
            }
        })
    }

    pub(super) fn map_remove(
        &mut self,
        map: TaggedValue,
        key: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let _guard = self.stripe_write(map)?;
        let layout = self.map_layout_unlocked(map)?;
        let found = crate::builtin_ctx::with_heap_string_str(key, |k_str| {
            layout.find_key(Some(key), k_str, key_hash(k_str))
        })
        .ok_or(VmError::TypeError)??;
        let Some(bucket) = found.0 else {
            return Ok(TaggedValue::NULL);
        };
        let value = layout.slot(bucket, 3);
        let length = layout.length.checked_sub(1).ok_or(VmError::TypeError)?;
        let slot = bucket * 4;
        write_u64_atomic(layout.buffer, slot, TaggedValue::from_u64(DELETED).raw());
        write_u64_atomic(layout.buffer, slot + 1, TaggedValue::from_u64(0).raw());
        write_u64_atomic(layout.buffer, slot + 2, TaggedValue::UNIT.raw());
        write_u64_atomic(layout.buffer, slot + 3, TaggedValue::UNIT.raw());
        write_u64_atomic(
            layout.wrapper,
            0,
            TaggedValue::from_u64(length as u64).raw(),
        );
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
        let root = vm.worker_roots().temporary_values.borrow().len();
        vm.with_temporary_roots(&[map], |vm| {
            let key = vm.alloc_string("same")?;
            vm.worker_roots().temporary_values.borrow_mut().push(key);
            let equivalent = vm.alloc_string("same")?;
            vm.worker_roots()
                .temporary_values
                .borrow_mut()
                .push(equivalent);
            let map = vm.worker_roots().temporary_values.borrow()[root];
            let key = vm.worker_roots().temporary_values.borrow()[root + 1];
            let equivalent = vm.worker_roots().temporary_values.borrow()[root + 2];
            assert_ne!(key, equivalent);
            vm.map_set(map, key, TaggedValue::NULL)?;
            let map = vm.worker_roots().temporary_values.borrow()[root];
            let key = vm.worker_roots().temporary_values.borrow()[root + 1];
            let equivalent = vm.worker_roots().temporary_values.borrow()[root + 2];
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
        assert!(vm.worker_roots().temporary_values.borrow().is_empty());
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
                vm.worker_roots().temporary_values.borrow_mut().push(key);
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
            vm.worker_roots()
                .temporary_values
                .borrow_mut()
                .push(snapshot);
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
            vm.worker_roots()
                .temporary_values
                .borrow_mut()
                .push(snapshot);
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
            vm.worker_roots()
                .temporary_values
                .borrow_mut()
                .push(existing);
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
            vm.worker_roots().temporary_values.borrow_mut().push(key);
            vm.map_set(map, key, map)?;
            assert_eq!(
                vm.format_aggregate(map)?.as_deref(),
                Some("{\"self\": <cycle>}")
            );
            let list = vm.new_list(1)?;
            vm.worker_roots().temporary_values.borrow_mut().push(list);
            let text = vm.alloc_string("line\n")?;
            vm.list_set(list, TaggedValue::from_i64(0), text)?;
            vm.map_set(map, key, list)?;
            assert_eq!(
                vm.format_aggregate(map)?.as_deref(),
                Some("{\"self\": [\"line\\n\"]}")
            );
            let layout = vm.map_layout(map)?;
            let bucket = layout.find("self", key_hash("self"))?.0.unwrap();
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
            vm.worker_roots().temporary_values.borrow_mut().push(empty);
            assert_eq!(vm.list_len(empty)?, 0);
            let key = vm.alloc_string("key")?;
            vm.worker_roots().temporary_values.borrow_mut().push(key);
            vm.map_set(map, key, TaggedValue::NULL)?;
            let layout = vm.map_layout(map)?;
            unsafe { layout.wrapper.write(TaggedValue::from_u64(2).raw()) };
            assert!(matches!(vm.map_keys(map), Err(VmError::TypeError)));
            let pointer = key.as_heap_ptr().unwrap().cast_mut().cast::<u64>();
            unsafe {
                layout.wrapper.write(TaggedValue::from_u64(1).raw());
                pointer.write(u64::MAX);
            }
            assert!(matches!(vm.map_keys(map), Err(VmError::TypeError)));
            unsafe {
                pointer.write(3);
                pointer.cast::<u8>().add(8).write(0xff);
            }
            assert!(matches!(vm.map_keys(map), Err(VmError::TypeError)));
            Ok(())
        })
        .unwrap();
        assert!(vm.worker_roots().temporary_values.borrow().is_empty());
    }

    #[test]
    fn new_map_index_and_dynamic_call_opcodes_use_checked_string_keys() {
        use nsbc::{AddrMode, Constant, FuncId, Instruction, Opcode, Reg};
        use runtime::FunctionCode;
        let mut vm = make_vm();
        vm.push_constant(&Constant::Str("key".into())).unwrap();
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: nsbc::FunctionAbi::value_only(0),
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
