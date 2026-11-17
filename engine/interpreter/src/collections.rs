//! List identity is a fixed wrapper; growth replaces only its tagged backing buffer.

use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};

use gc::{MAX_LARGE_TAGGED_WORDS, ObjectHeader};
use nsbc::Reg;
use runtime::{Number, TaggedValue, TaskId};
use str_interner::StrId;
use type_pool::CollectionRole;

use crate::{DispatchResult, Vm, VmError};

pub(crate) const MAX_LIST_CAPACITY: usize = MAX_LARGE_TAGGED_WORDS;
const MAX_CAPACITY: usize = MAX_LIST_CAPACITY;

#[inline]
pub(super) fn read_u64_atomic(base: *const u64, index: usize) -> u64 {
    // SAFETY: Caller verified `base.add(index)` points to an aligned `u64` payload slot
    // within a live managed object under an active mutator operation.
    unsafe { (&*base.cast::<AtomicU64>().add(index)).load(Ordering::Acquire) }
}

#[inline]
pub(super) fn write_u64_atomic(base: *mut u64, index: usize, raw: u64) {
    // SAFETY: Caller verified `base.add(index)` points to an aligned `u64` payload slot
    // within a live managed object under exclusive ownership or stripe write lock.
    unsafe { (&*base.cast::<AtomicU64>().add(index)).store(raw, Ordering::Release) }
}

pub(super) struct ListLayout {
    wrapper: *mut u64,
    pub(super) buffer: *mut u64,
    pub(super) length: usize,
    capacity: usize,
}

/// Guard restoring this worker's `temporary_values` root stack on normal exit, error, or unwind.
struct TemporaryRootScope {
    roots: NonNull<RefCell<Vec<TaggedValue>>>,
    base: usize,
}

impl Drop for TemporaryRootScope {
    fn drop(&mut self) {
        // SAFETY: `roots` points to the current worker's `WorkerRootSlot` inside the stable
        // `RootCell` allocation, accessed exclusively by this worker thread during execution.
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
        static APPLY_UPDATE_IDS: std::sync::OnceLock<(StrId, StrId)> = std::sync::OnceLock::new();
        let &(apply_id, update_id) = APPLY_UPDATE_IDS.get_or_init(|| {
            (
                str_interner::intern("apply"),
                str_interner::intern("update"),
            )
        });
        let expected = if method == apply_id {
            1
        } else if method == update_id {
            2
        } else {
            return None;
        };
        match self.cached_heap_type_meta(ty) {
            Ok(crate::CachedHeapTypeMeta::Collection(
                CollectionRole::List | CollectionRole::Map,
            )) => {}
            Ok(_) => match self.type_pool_read().checked_collection_role(ty) {
                Ok(Some(CollectionRole::List | CollectionRole::Map)) => {}
                Ok(_) => return None,
                Err(_) => return Some(DispatchResult::Error(VmError::TypeError)),
            },
            Err(error) => return Some(DispatchResult::Error(error)),
        }
        if argument_count != expected {
            return Some(DispatchResult::Error(VmError::ArityError {
                expected,
                got: argument_count,
            }));
        }
        let Some((index, value)) = self
            .task_ref(task)
            .map(|t| (t.registers.get(Reg(0)), t.registers.get(Reg(1))))
        else {
            return Some(DispatchResult::Error(VmError::InvalidTask));
        };
        let result = if expected == 1 {
            self.collection_get(receiver, index)
        } else {
            self.collection_set(receiver, index, value)
                .map(|()| TaggedValue::UNIT)
        };
        Some(match result {
            Ok(value) => {
                if let Some(task) = self.task_mut(task) {
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
        if let Ok(crate::CachedHeapTypeMeta::Collection(role)) =
            self.cached_heap_type_meta(header.type_index)
        {
            return Ok(role);
        }
        self.type_pool_read()
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
        let roots_ref = &self.worker_roots().temporary_values;
        let base = roots_ref.borrow().len();
        let _scope = TemporaryRootScope {
            roots: NonNull::from(roots_ref),
            base,
        };
        roots_ref.borrow_mut().extend_from_slice(values);
        action(self)
    }

    #[inline]
    fn cached_list_roles(&self) -> Option<(type_pool::TypeIndex, type_pool::TypeIndex)> {
        {
            let cache = self.sync_meta_cache();
            if let Some(cached) = cache.list_roles {
                return cached;
            }
        }
        let pair = {
            let tp = self.type_pool_read();
            tp.list_type().zip(tp.list_buffer_type())
        };
        self.sync_meta_cache().list_roles = Some(pair);
        pair
    }

    /// Inspect `ListLayout` when caller already holds `stripe_read`/`stripe_write` (or owns a fresh list).
    pub(super) fn list_layout_unlocked(&self, value: TaggedValue) -> Result<ListLayout, VmError> {
        let pointer = value.as_heap_ptr().ok_or(VmError::TypeError)?;
        // SAFETY: operands are live registered VM roots within an execution operation.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        if !header.is_ordinary() || header.payload_words() != 3 {
            return Err(VmError::TypeError);
        }
        let fast_roles = self.cached_list_roles();
        let is_list = if fast_roles.is_some_and(|(lt, _)| header.type_index == lt) {
            true
        } else {
            self.type_pool_read()
                .checked_collection_role(header.type_index)
                .map_err(|_| VmError::TypeError)?
                == Some(CollectionRole::List)
        };
        if !is_list {
            return Err(VmError::TypeError);
        }
        let wrapper = pointer.cast_mut().cast::<u64>();
        let length = TaggedValue::from_raw(read_u64_atomic(wrapper, 0));
        let capacity = TaggedValue::from_raw(read_u64_atomic(wrapper, 1));
        let buffer = TaggedValue::from_raw(read_u64_atomic(wrapper, 2));
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
        if !header.is_ordinary() || header.payload_words() != capacity {
            return Err(VmError::TypeError);
        }
        let is_buf = if fast_roles.is_some_and(|(_, bt)| header.type_index == bt) {
            true
        } else {
            self.type_pool_read()
                .checked_collection_role(header.type_index)
                .map_err(|_| VmError::TypeError)?
                == Some(CollectionRole::Buffer)
        };
        if !is_buf {
            return Err(VmError::TypeError);
        }
        Ok(ListLayout {
            wrapper,
            buffer: buffer.cast_mut().cast(),
            length,
            capacity,
        })
    }

    #[cfg(test)]
    pub(super) fn list_layout(&self, value: TaggedValue) -> Result<ListLayout, VmError> {
        let _guard = self.stripe_read(value)?;
        self.list_layout_unlocked(value)
    }

    pub(super) fn snapshot_list_elements(
        &self,
        list: TaggedValue,
    ) -> Result<Vec<TaggedValue>, VmError> {
        let _guard = self.stripe_read(list)?;
        let layout = self.list_layout_unlocked(list)?;
        let mut out = Vec::with_capacity(layout.length);
        for index in 0..layout.length {
            out.push(TaggedValue::from_raw(read_u64_atomic(layout.buffer, index)));
        }
        Ok(out)
    }

    fn list_buffer(&mut self, capacity: usize) -> Result<TaggedValue, VmError> {
        if capacity > MAX_CAPACITY {
            return Err(VmError::ObjectTooLarge);
        }
        let ty = self
            .type_pool_read()
            .list_buffer_type()
            .ok_or(VmError::TypeError)?;
        // SAFETY: execution owns the mutator and holds no lock across allocation.
        let pointer = unsafe { self.worker.heap.alloc_tagged_words(ty, capacity) }
            .ok_or(VmError::OutOfMemory)?;
        let base = pointer.as_ptr().cast::<u64>();
        for index in 0..capacity {
            write_u64_atomic(base, index, TaggedValue::UNIT.raw());
        }
        // SAFETY: this allocation is aligned and live, with all slots initialized.
        Ok(unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) })
    }

    pub(super) fn new_list(&mut self, length: usize) -> Result<TaggedValue, VmError> {
        if length > MAX_CAPACITY {
            return Err(VmError::ObjectTooLarge);
        }
        let ty = {
            let tp = self.type_pool_read();
            let list_ty = tp.list_type().ok_or(VmError::TypeError)?;
            tp.list_buffer_type().ok_or(VmError::TypeError)?;
            list_ty
        };
        // SAFETY: initialize this fresh wrapper before another allocation or safepoint.
        let pointer =
            unsafe { self.worker.heap.alloc_object(ty, 3) }.ok_or(VmError::OutOfMemory)?;
        let wrapper_init = pointer.as_ptr().cast::<u64>();
        write_u64_atomic(wrapper_init, 0, TaggedValue::from_u64(0).raw());
        write_u64_atomic(wrapper_init, 1, TaggedValue::from_u64(0).raw());
        write_u64_atomic(wrapper_init, 2, TaggedValue::NULL.raw());
        // SAFETY: wrapper is live and initialized; the next allocation roots it.
        let value = unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) };
        let root = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&[value], |vm| {
            if length != 0 {
                let buffer = vm.list_buffer(length)?;
                vm.worker_roots().temporary_values.borrow_mut().push(buffer);
                let value = vm.worker_roots().temporary_values.borrow()[root];
                let layout = vm.list_layout_unlocked(value)?;
                write_u64_atomic(
                    layout.wrapper,
                    0,
                    TaggedValue::from_u64(length as u64).raw(),
                );
                write_u64_atomic(
                    layout.wrapper,
                    1,
                    TaggedValue::from_u64(length as u64).raw(),
                );
                write_u64_atomic(layout.wrapper, 2, buffer.raw());
            }
            Ok(vm.worker_roots().temporary_values.borrow()[root])
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
        let _guard = self.stripe_read(list)?;
        Ok(self.list_layout_unlocked(list)?.length)
    }

    pub(super) fn list_get(
        &self,
        list: TaggedValue,
        index: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let index = Self::list_index(index)?;
        let _guard = self.stripe_read(list)?;
        let layout = self.list_layout_unlocked(list)?;
        if index >= layout.length {
            return Err(VmError::InvalidIndex);
        }
        Ok(TaggedValue::from_raw(read_u64_atomic(layout.buffer, index)))
    }

    pub(super) fn list_set(
        &mut self,
        list: TaggedValue,
        index: TaggedValue,
        value: TaggedValue,
    ) -> Result<(), VmError> {
        let index = Self::list_index(index)?;
        let _guard = self.stripe_write(list)?;
        let layout = self.list_layout_unlocked(list)?;
        if index >= layout.length {
            return Err(VmError::InvalidIndex);
        }
        write_u64_atomic(layout.buffer, index, value.raw());
        Ok(())
    }

    pub(super) fn list_push(
        &mut self,
        list: TaggedValue,
        value: TaggedValue,
    ) -> Result<(), VmError> {
        let root = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&[list, value], |vm| {
            loop {
                let needed_cap = {
                    let list = vm.worker_roots().temporary_values.borrow()[root];
                    let value = vm.worker_roots().temporary_values.borrow()[root + 1];
                    let _guard = vm.stripe_write(list)?;
                    let layout = vm.list_layout_unlocked(list)?;
                    if layout.length == MAX_CAPACITY {
                        return Err(VmError::ObjectTooLarge);
                    }
                    if layout.length < layout.capacity {
                        write_u64_atomic(layout.buffer, layout.length, value.raw());
                        write_u64_atomic(
                            layout.wrapper,
                            0,
                            TaggedValue::from_u64((layout.length + 1) as u64).raw(),
                        );
                        return Ok(());
                    }
                    layout
                        .capacity
                        .checked_mul(2)
                        .ok_or(VmError::ObjectTooLarge)?
                        .clamp(4, MAX_CAPACITY)
                };

                // Allocate outside the stripe lock so GC safepoints never wait on stripe locks.
                let buffer = vm.list_buffer(needed_cap)?;
                let buf_slot = vm.worker_roots().temporary_values.borrow().len();
                vm.worker_roots().temporary_values.borrow_mut().push(buffer);

                let list = vm.worker_roots().temporary_values.borrow()[root];
                let value = vm.worker_roots().temporary_values.borrow()[root + 1];
                let buffer = vm.worker_roots().temporary_values.borrow()[buf_slot];
                let _guard = vm.stripe_write(list)?;
                let old = vm.list_layout_unlocked(list)?;
                if old.length == MAX_CAPACITY {
                    return Err(VmError::ObjectTooLarge);
                }
                if old.length < old.capacity {
                    write_u64_atomic(old.buffer, old.length, value.raw());
                    write_u64_atomic(
                        old.wrapper,
                        0,
                        TaggedValue::from_u64((old.length + 1) as u64).raw(),
                    );
                    return Ok(());
                }
                if old.length >= needed_cap {
                    continue;
                }
                let pointer = buffer
                    .as_heap_ptr()
                    .ok_or(VmError::TypeError)?
                    .cast_mut()
                    .cast::<u64>();
                for i in 0..old.length {
                    write_u64_atomic(pointer, i, read_u64_atomic(old.buffer, i));
                }
                write_u64_atomic(pointer, old.length, value.raw());
                write_u64_atomic(
                    old.wrapper,
                    0,
                    TaggedValue::from_u64((old.length + 1) as u64).raw(),
                );
                write_u64_atomic(
                    old.wrapper,
                    1,
                    TaggedValue::from_u64(needed_cap as u64).raw(),
                );
                write_u64_atomic(old.wrapper, 2, buffer.raw());
                return Ok(());
            }
        })
    }

    pub(super) fn list_pop(&mut self, list: TaggedValue) -> Result<TaggedValue, VmError> {
        let _guard = self.stripe_write(list)?;
        let layout = self.list_layout_unlocked(list)?;
        if layout.length == 0 {
            return Ok(TaggedValue::NULL);
        }
        let index = layout.length - 1;
        let value = TaggedValue::from_raw(read_u64_atomic(layout.buffer, index));
        write_u64_atomic(layout.buffer, index, TaggedValue::UNIT.raw());
        write_u64_atomic(layout.wrapper, 0, TaggedValue::from_u64(index as u64).raw());
        Ok(value)
    }

    fn new_list_from_rooted_slice(
        &mut self,
        root_base: usize,
        start: usize,
        end: usize,
    ) -> Result<TaggedValue, VmError> {
        if start > end {
            return Err(VmError::InvalidIndex);
        }
        let length = end - start;
        let copy = self.new_list(length)?;
        if length == 0 {
            return Ok(copy);
        }
        let layout = self.list_layout_unlocked(copy)?;
        let roots = self.worker_roots().temporary_values.borrow();
        for offset in 0..length {
            write_u64_atomic(
                layout.buffer,
                offset,
                roots[root_base + start + offset].raw(),
            );
        }
        Ok(copy)
    }

    fn list_of_ranges_from_snapshot(
        &mut self,
        elems: &[TaggedValue],
        ranges: &[(usize, usize)],
    ) -> Result<TaggedValue, VmError> {
        let elem_base = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(elems, |vm| {
            let outer = vm.new_list(ranges.len())?;
            let outer_slot = vm.worker_roots().temporary_values.borrow().len();
            vm.worker_roots().temporary_values.borrow_mut().push(outer);
            for (index, &(start, end)) in ranges.iter().enumerate() {
                let piece = vm.new_list_from_rooted_slice(elem_base, start, end)?;
                let outer = vm.worker_roots().temporary_values.borrow()[outer_slot];
                vm.list_set(outer, TaggedValue::from_u64(index as u64), piece)?;
            }
            Ok(vm.worker_roots().temporary_values.borrow()[outer_slot])
        })
    }

    fn require_chunk_size(size: usize) -> Result<(), VmError> {
        if size == 0 {
            Err(VmError::InvalidArgument("chunk size must not be zero"))
        } else {
            Ok(())
        }
    }

    pub(super) fn list_windows(
        &mut self,
        list: TaggedValue,
        size: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let size = Self::list_index(size)?;
        if size == 0 {
            return Err(VmError::InvalidArgument("window size must not be zero"));
        }
        let elems = self.snapshot_list_elements(list)?;
        let len = elems.len();
        let count = if size > len { 0 } else { len - size + 1 };
        let mut ranges = Vec::with_capacity(count);
        for start in 0..count {
            ranges.push((start, start + size));
        }
        self.list_of_ranges_from_snapshot(&elems, &ranges)
    }

    pub(super) fn list_chunks(
        &mut self,
        list: TaggedValue,
        size: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let size = Self::list_index(size)?;
        Self::require_chunk_size(size)?;
        let elems = self.snapshot_list_elements(list)?;
        let len = elems.len();
        let mut ranges = Vec::new();
        let mut start = 0;
        while start < len {
            let end = start.saturating_add(size).min(len);
            ranges.push((start, end));
            start = end;
        }
        self.list_of_ranges_from_snapshot(&elems, &ranges)
    }

    pub(super) fn list_chunks_exact(
        &mut self,
        list: TaggedValue,
        size: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let size = Self::list_index(size)?;
        Self::require_chunk_size(size)?;
        let elems = self.snapshot_list_elements(list)?;
        let count = elems.len() / size;
        let mut ranges = Vec::with_capacity(count);
        for index in 0..count {
            let start = index * size;
            ranges.push((start, start + size));
        }
        self.list_of_ranges_from_snapshot(&elems, &ranges)
    }

    pub(super) fn list_rchunks(
        &mut self,
        list: TaggedValue,
        size: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let size = Self::list_index(size)?;
        Self::require_chunk_size(size)?;
        let elems = self.snapshot_list_elements(list)?;
        let mut ranges = Vec::new();
        let mut end = elems.len();
        while end > 0 {
            let start = end.saturating_sub(size);
            ranges.push((start, end));
            end = start;
        }
        self.list_of_ranges_from_snapshot(&elems, &ranges)
    }

    pub(super) fn list_rchunks_exact(
        &mut self,
        list: TaggedValue,
        size: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let size = Self::list_index(size)?;
        Self::require_chunk_size(size)?;
        let elems = self.snapshot_list_elements(list)?;
        let len = elems.len();
        let remainder = len % size;
        let mut ranges = Vec::new();
        let mut end = len;
        while end > remainder {
            let start = end - size;
            ranges.push((start, end));
            end = start;
        }
        self.list_of_ranges_from_snapshot(&elems, &ranges)
    }

    pub(super) fn list_chunks_remainder(
        &mut self,
        list: TaggedValue,
        size: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let size = Self::list_index(size)?;
        Self::require_chunk_size(size)?;
        let elems = self.snapshot_list_elements(list)?;
        let len = elems.len();
        let start = len - (len % size);
        let tail = &elems[start..len];
        let base = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(tail, |vm| {
            vm.new_list_from_rooted_slice(base, 0, tail.len())
        })
    }

    pub(super) fn list_rchunks_remainder(
        &mut self,
        list: TaggedValue,
        size: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let size = Self::list_index(size)?;
        Self::require_chunk_size(size)?;
        let elems = self.snapshot_list_elements(list)?;
        let head = &elems[0..elems.len() % size];
        let base = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(head, |vm| {
            vm.new_list_from_rooted_slice(base, 0, head.len())
        })
    }

    pub(super) fn list_split_at(
        &mut self,
        list: TaggedValue,
        index: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let index = Self::list_index(index)?;
        let elems = self.snapshot_list_elements(list)?;
        let len = elems.len();
        if index > len {
            return Err(VmError::InvalidIndex);
        }
        self.list_of_ranges_from_snapshot(&elems, &[(0, index), (index, len)])
    }

    fn rotated_list(
        &mut self,
        list: TaggedValue,
        distance: usize,
        right: bool,
    ) -> Result<TaggedValue, VmError> {
        let elems = self.snapshot_list_elements(list)?;
        let len = elems.len();
        if distance > len {
            return Err(VmError::InvalidArgument("rotate distance is out of range"));
        }
        let start = if !right || distance == 0 {
            distance
        } else {
            len - distance
        };
        let mut rotated = Vec::with_capacity(len);
        if len > 0 {
            for idx in 0..len {
                rotated.push(elems[(start + idx) % len]);
            }
        }
        let base = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&rotated, |vm| {
            vm.new_list_from_rooted_slice(base, 0, rotated.len())
        })
    }

    pub(super) fn list_rotate_left(
        &mut self,
        list: TaggedValue,
        distance: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let distance = Self::list_index(distance)?;
        self.rotated_list(list, distance, false)
    }

    pub(super) fn list_rotate_right(
        &mut self,
        list: TaggedValue,
        distance: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let distance = Self::list_index(distance)?;
        self.rotated_list(list, distance, true)
    }

    pub(super) fn list_repeat(
        &mut self,
        list: TaggedValue,
        count: usize,
    ) -> Result<TaggedValue, VmError> {
        let elems = self.snapshot_list_elements(list)?;
        let len = elems.len();
        let total = len.checked_mul(count).ok_or(VmError::ObjectTooLarge)?;
        if total > MAX_CAPACITY {
            return Err(VmError::ObjectTooLarge);
        }
        let elem_base = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&elems, |vm| {
            let copy = vm.new_list(total)?;
            if total == 0 {
                return Ok(copy);
            }
            let layout = vm.list_layout_unlocked(copy)?;
            let roots = vm.worker_roots().temporary_values.borrow();
            for block in 0..count {
                for offset in 0..len {
                    write_u64_atomic(
                        layout.buffer,
                        block * len + offset,
                        roots[elem_base + offset].raw(),
                    );
                }
            }
            Ok(copy)
        })
    }

    pub(super) fn list_truncate(
        &mut self,
        list: TaggedValue,
        len: TaggedValue,
    ) -> Result<(), VmError> {
        let new_len = Self::list_index(len)?;
        let _guard = self.stripe_write(list)?;
        let layout = self.list_layout_unlocked(list)?;
        if new_len >= layout.length {
            return Ok(());
        }
        for index in new_len..layout.length {
            write_u64_atomic(layout.buffer, index, TaggedValue::UNIT.raw());
        }
        write_u64_atomic(
            layout.wrapper,
            0,
            TaggedValue::from_u64(new_len as u64).raw(),
        );
        Ok(())
    }

    pub(super) fn list_fill(
        &mut self,
        list: TaggedValue,
        value: TaggedValue,
    ) -> Result<(), VmError> {
        let _guard = self.stripe_write(list)?;
        let layout = self.list_layout_unlocked(list)?;
        for index in 0..layout.length {
            write_u64_atomic(layout.buffer, index, value.raw());
        }
        Ok(())
    }

    pub(super) fn list_swap_remove(
        &mut self,
        list: TaggedValue,
        index: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let index = Self::list_index(index)?;
        let _guard = self.stripe_write(list)?;
        let layout = self.list_layout_unlocked(list)?;
        if index >= layout.length {
            return Err(VmError::InvalidIndex);
        }
        let last_idx = layout.length - 1;
        let removed = TaggedValue::from_raw(read_u64_atomic(layout.buffer, index));
        if index != last_idx {
            let last_val = read_u64_atomic(layout.buffer, last_idx);
            write_u64_atomic(layout.buffer, index, last_val);
        }
        write_u64_atomic(layout.buffer, last_idx, TaggedValue::UNIT.raw());
        write_u64_atomic(
            layout.wrapper,
            0,
            TaggedValue::from_u64(last_idx as u64).raw(),
        );
        Ok(removed)
    }

    pub(super) fn list_split_off(
        &mut self,
        list: TaggedValue,
        index: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let index = Self::list_index(index)?;
        let root = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&[list], |vm| {
            let tail_elems = {
                let list = vm.worker_roots().temporary_values.borrow()[root];
                let _guard = vm.stripe_write(list)?;
                let layout = vm.list_layout_unlocked(list)?;
                if index > layout.length {
                    return Err(VmError::InvalidIndex);
                }
                let mut out = Vec::with_capacity(layout.length - index);
                for pos in index..layout.length {
                    out.push(TaggedValue::from_raw(read_u64_atomic(layout.buffer, pos)));
                    write_u64_atomic(layout.buffer, pos, TaggedValue::UNIT.raw());
                }
                write_u64_atomic(layout.wrapper, 0, TaggedValue::from_u64(index as u64).raw());
                out
            };
            let tail_base = vm.worker_roots().temporary_values.borrow().len();
            vm.with_temporary_roots(&tail_elems, |vm| {
                vm.new_list_from_rooted_slice(tail_base, 0, tail_elems.len())
            })
        })
    }

    pub(super) fn list_resize(
        &mut self,
        list: TaggedValue,
        len: TaggedValue,
        value: TaggedValue,
    ) -> Result<(), VmError> {
        let new_len = Self::list_index(len)?;
        if new_len > MAX_CAPACITY {
            return Err(VmError::ObjectTooLarge);
        }
        let root = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&[list, value], |vm| {
            let list = vm.worker_roots().temporary_values.borrow()[root];
            let current = vm.list_len(list)?;
            if new_len <= current {
                return vm.list_truncate(list, TaggedValue::from_u64(new_len as u64));
            }
            for _ in current..new_len {
                let list = vm.worker_roots().temporary_values.borrow()[root];
                let value = vm.worker_roots().temporary_values.borrow()[root + 1];
                vm.list_push(list, value)?;
            }
            Ok(())
        })
    }

    pub(super) fn list_copy(&mut self, list: TaggedValue) -> Result<TaggedValue, VmError> {
        let elems = self.snapshot_list_elements(list)?;
        let base = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&elems, |vm| {
            vm.new_list_from_rooted_slice(base, 0, elems.len())
        })
    }

    pub(super) fn list_reverse(&mut self, list: TaggedValue) -> Result<TaggedValue, VmError> {
        let mut elems = self.snapshot_list_elements(list)?;
        elems.reverse();
        let base = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&elems, |vm| {
            vm.new_list_from_rooted_slice(base, 0, elems.len())
        })
    }

    pub(super) fn list_concat(
        &mut self,
        left: TaggedValue,
        right: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let mut elems = self.snapshot_list_elements(left)?;
        let right_elems = self.snapshot_list_elements(right)?;
        let total = elems
            .len()
            .checked_add(right_elems.len())
            .ok_or(VmError::ObjectTooLarge)?;
        if total > MAX_CAPACITY {
            return Err(VmError::ObjectTooLarge);
        }
        elems.extend_from_slice(&right_elems);
        let base = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&elems, |vm| {
            vm.new_list_from_rooted_slice(base, 0, elems.len())
        })
    }

    pub(super) fn list_swap(
        &mut self,
        list: TaggedValue,
        left: TaggedValue,
        right: TaggedValue,
    ) -> Result<(), VmError> {
        let left = Self::list_index(left)?;
        let right = Self::list_index(right)?;
        let _guard = self.stripe_write(list)?;
        let layout = self.list_layout_unlocked(list)?;
        if left >= layout.length || right >= layout.length {
            return Err(VmError::InvalidIndex);
        }
        if left == right {
            return Ok(());
        }
        let first = read_u64_atomic(layout.buffer, left);
        let second = read_u64_atomic(layout.buffer, right);
        write_u64_atomic(layout.buffer, left, second);
        write_u64_atomic(layout.buffer, right, first);
        Ok(())
    }

    pub(super) fn list_clear(&mut self, list: TaggedValue) -> Result<(), VmError> {
        self.list_truncate(list, TaggedValue::from_u64(0))
    }

    pub(super) fn list_insert(
        &mut self,
        list: TaggedValue,
        index: TaggedValue,
        value: TaggedValue,
    ) -> Result<(), VmError> {
        let index = Self::list_index(index)?;
        let root = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&[list, value], |vm| {
            loop {
                let needed_cap = {
                    let list = vm.worker_roots().temporary_values.borrow()[root];
                    let value = vm.worker_roots().temporary_values.borrow()[root + 1];
                    let _guard = vm.stripe_write(list)?;
                    let layout = vm.list_layout_unlocked(list)?;
                    if index > layout.length {
                        return Err(VmError::InvalidIndex);
                    }
                    if layout.length == MAX_CAPACITY {
                        return Err(VmError::ObjectTooLarge);
                    }
                    if layout.length < layout.capacity {
                        for cursor in (index..layout.length).rev() {
                            let prev = read_u64_atomic(layout.buffer, cursor);
                            write_u64_atomic(layout.buffer, cursor + 1, prev);
                        }
                        write_u64_atomic(layout.buffer, index, value.raw());
                        write_u64_atomic(
                            layout.wrapper,
                            0,
                            TaggedValue::from_u64((layout.length + 1) as u64).raw(),
                        );
                        return Ok(());
                    }
                    layout
                        .capacity
                        .checked_mul(2)
                        .ok_or(VmError::ObjectTooLarge)?
                        .clamp(4, MAX_CAPACITY)
                };

                let buffer = vm.list_buffer(needed_cap)?;
                let buf_slot = vm.worker_roots().temporary_values.borrow().len();
                vm.worker_roots().temporary_values.borrow_mut().push(buffer);

                let list = vm.worker_roots().temporary_values.borrow()[root];
                let value = vm.worker_roots().temporary_values.borrow()[root + 1];
                let buffer = vm.worker_roots().temporary_values.borrow()[buf_slot];
                let _guard = vm.stripe_write(list)?;
                let old = vm.list_layout_unlocked(list)?;
                if index > old.length {
                    return Err(VmError::InvalidIndex);
                }
                if old.length == MAX_CAPACITY {
                    return Err(VmError::ObjectTooLarge);
                }
                if old.length < old.capacity {
                    for cursor in (index..old.length).rev() {
                        let prev = read_u64_atomic(old.buffer, cursor);
                        write_u64_atomic(old.buffer, cursor + 1, prev);
                    }
                    write_u64_atomic(old.buffer, index, value.raw());
                    write_u64_atomic(
                        old.wrapper,
                        0,
                        TaggedValue::from_u64((old.length + 1) as u64).raw(),
                    );
                    return Ok(());
                }
                if old.length >= needed_cap {
                    continue;
                }
                let pointer = buffer
                    .as_heap_ptr()
                    .ok_or(VmError::TypeError)?
                    .cast_mut()
                    .cast::<u64>();
                for i in 0..index {
                    write_u64_atomic(pointer, i, read_u64_atomic(old.buffer, i));
                }
                write_u64_atomic(pointer, index, value.raw());
                for i in index..old.length {
                    write_u64_atomic(pointer, i + 1, read_u64_atomic(old.buffer, i));
                }
                write_u64_atomic(
                    old.wrapper,
                    0,
                    TaggedValue::from_u64((old.length + 1) as u64).raw(),
                );
                write_u64_atomic(
                    old.wrapper,
                    1,
                    TaggedValue::from_u64(needed_cap as u64).raw(),
                );
                write_u64_atomic(old.wrapper, 2, buffer.raw());
                return Ok(());
            }
        })
    }

    pub(super) fn list_remove(
        &mut self,
        list: TaggedValue,
        index: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let index = Self::list_index(index)?;
        let _guard = self.stripe_write(list)?;
        let layout = self.list_layout_unlocked(list)?;
        if index >= layout.length {
            return Err(VmError::InvalidIndex);
        }
        let removed = TaggedValue::from_raw(read_u64_atomic(layout.buffer, index));
        for cursor in index + 1..layout.length {
            let next = read_u64_atomic(layout.buffer, cursor);
            write_u64_atomic(layout.buffer, cursor - 1, next);
        }
        let last_idx = layout.length - 1;
        write_u64_atomic(layout.buffer, last_idx, TaggedValue::UNIT.raw());
        write_u64_atomic(
            layout.wrapper,
            0,
            TaggedValue::from_u64(last_idx as u64).raw(),
        );
        Ok(removed)
    }

    pub(super) fn list_take(
        &mut self,
        list: TaggedValue,
        count: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let count = Self::list_index(count)?;
        let elems = self.snapshot_list_elements(list)?;
        let end = count.min(elems.len());
        let slice = &elems[..end];
        let base = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(slice, |vm| {
            vm.new_list_from_rooted_slice(base, 0, slice.len())
        })
    }

    pub(super) fn list_drop(
        &mut self,
        list: TaggedValue,
        count: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let count = Self::list_index(count)?;
        let elems = self.snapshot_list_elements(list)?;
        let start = count.min(elems.len());
        let slice = &elems[start..];
        let base = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(slice, |vm| {
            vm.new_list_from_rooted_slice(base, 0, slice.len())
        })
    }

    pub(super) fn list_first(&self, list: TaggedValue) -> Result<TaggedValue, VmError> {
        let _guard = self.stripe_read(list)?;
        let layout = self.list_layout_unlocked(list)?;
        if layout.length == 0 {
            Ok(TaggedValue::NULL)
        } else {
            Ok(TaggedValue::from_raw(read_u64_atomic(layout.buffer, 0)))
        }
    }

    pub(super) fn list_last(&self, list: TaggedValue) -> Result<TaggedValue, VmError> {
        let _guard = self.stripe_read(list)?;
        let layout = self.list_layout_unlocked(list)?;
        if layout.length == 0 {
            Ok(TaggedValue::NULL)
        } else {
            Ok(TaggedValue::from_raw(read_u64_atomic(
                layout.buffer,
                layout.length - 1,
            )))
        }
    }

    pub(super) fn list_append(
        &mut self,
        list: TaggedValue,
        other: TaggedValue,
    ) -> Result<(), VmError> {
        let root = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&[list, other], |vm| {
            let other = vm.worker_roots().temporary_values.borrow()[root + 1];
            let extra_elems = vm.snapshot_list_elements(other)?;
            let extra_base = vm.worker_roots().temporary_values.borrow().len();
            vm.with_temporary_roots(&extra_elems, |vm| {
                for offset in 0..extra_elems.len() {
                    let list = vm.worker_roots().temporary_values.borrow()[root];
                    let val = vm.worker_roots().temporary_values.borrow()[extra_base + offset];
                    vm.list_push(list, val)?;
                }
                Ok(())
            })
        })
    }

    pub(super) fn list_enumerate(&mut self, list: TaggedValue) -> Result<TaggedValue, VmError> {
        let elems = self.snapshot_list_elements(list)?;
        let len = elems.len();
        let i64_ty = type_pool::Intrinsic::I64.type_index();
        let any_ty = type_pool::Intrinsic::Any.type_index();
        let elem_base = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&elems, |vm| {
            let out = vm.new_list(len)?;
            let out_slot = vm.worker_roots().temporary_values.borrow().len();
            vm.worker_roots().temporary_values.borrow_mut().push(out);
            for index in 0..len {
                let value = vm.worker_roots().temporary_values.borrow()[elem_base + index];
                let pair = vm.alloc_tuple(
                    &[i64_ty, any_ty],
                    &[
                        TaggedValue::from_i64(
                            i64::try_from(index).map_err(|_| VmError::NumericOverflow)?,
                        ),
                        value,
                    ],
                )?;
                let out = vm.worker_roots().temporary_values.borrow()[out_slot];
                vm.list_set(out, TaggedValue::from_u64(index as u64), pair)?;
            }
            Ok(vm.worker_roots().temporary_values.borrow()[out_slot])
        })
    }

    pub(super) fn list_flatten(&mut self, list: TaggedValue) -> Result<TaggedValue, VmError> {
        let inners = self.snapshot_list_elements(list)?;
        let inner_base = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&inners, |vm| {
            let out = vm.new_list(0)?;
            let out_slot = vm.worker_roots().temporary_values.borrow().len();
            vm.worker_roots().temporary_values.borrow_mut().push(out);
            for index in 0..inners.len() {
                let inner = vm.worker_roots().temporary_values.borrow()[inner_base + index];
                let out = vm.worker_roots().temporary_values.borrow()[out_slot];
                vm.list_append(out, inner)?;
            }
            Ok(vm.worker_roots().temporary_values.borrow()[out_slot])
        })
    }

    pub(super) fn list_zip(
        &mut self,
        left: TaggedValue,
        right: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let left_elems = self.snapshot_list_elements(left)?;
        let right_elems = self.snapshot_list_elements(right)?;
        let end = left_elems.len().min(right_elems.len());
        let mut combined = Vec::with_capacity(end * 2);
        combined.extend_from_slice(&left_elems[..end]);
        combined.extend_from_slice(&right_elems[..end]);
        let base = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&combined, |vm| {
            let out = vm.new_list(end)?;
            let out_slot = vm.worker_roots().temporary_values.borrow().len();
            vm.worker_roots().temporary_values.borrow_mut().push(out);
            for index in 0..end {
                let (left_value, right_value) = {
                    let roots = vm.worker_roots().temporary_values.borrow();
                    (roots[base + index], roots[base + end + index])
                };
                let pair = vm.alloc_tuple(
                    &[
                        vm.reflected_type(left_value)?,
                        vm.reflected_type(right_value)?,
                    ],
                    &[left_value, right_value],
                )?;
                let out = vm.worker_roots().temporary_values.borrow()[out_slot];
                vm.list_set(out, TaggedValue::from_u64(index as u64), pair)?;
            }
            Ok(vm.worker_roots().temporary_values.borrow()[out_slot])
        })
    }

    pub(super) fn list_join(
        &self,
        list: TaggedValue,
        separator: TaggedValue,
    ) -> Result<String, VmError> {
        let separator =
            crate::builtin_ctx::heap_string_to_owned(separator).ok_or(VmError::TypeError)?;
        let _guard = self.stripe_read(list)?;
        let layout = self.list_layout_unlocked(list)?;
        let mut pieces = Vec::with_capacity(layout.length);
        for index in 0..layout.length {
            let value = TaggedValue::from_raw(read_u64_atomic(layout.buffer, index));
            let piece =
                crate::builtin_ctx::heap_string_to_owned(value).ok_or(VmError::TypeError)?;
            pieces.push(piece);
        }
        Ok(pieces.join(&separator))
    }

    /// Build a fresh List holding a new String for each piece, in order.
    pub(super) fn new_string_list(&mut self, pieces: &[&str]) -> Result<TaggedValue, VmError> {
        let list = self.new_list(pieces.len())?;
        let root = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&[list], |vm| {
            for (index, piece) in pieces.iter().enumerate() {
                let text = vm.alloc_string(piece)?;
                let list = vm.worker_roots().temporary_values.borrow()[root];
                vm.list_set(list, TaggedValue::from_u64(index as u64), text)?;
            }
            Ok(vm.worker_roots().temporary_values.borrow()[root])
        })
    }

    /// Build a fresh List of immediate char values; only the List allocates.
    pub(super) fn new_char_list(&mut self, text: &str) -> Result<TaggedValue, VmError> {
        let list = self.new_list(text.chars().count())?;
        let layout = self.list_layout_unlocked(list)?;
        for (index, ch) in text.chars().enumerate() {
            write_u64_atomic(layout.buffer, index, TaggedValue::from_char(ch).raw());
        }
        Ok(list)
    }

    /// Build a fresh List of immediate i64 values; only the List allocates.
    pub(super) fn new_i64_list(&mut self, values: &[i64]) -> Result<TaggedValue, VmError> {
        let list = self.new_list(values.len())?;
        let layout = self.list_layout_unlocked(list)?;
        for (index, value) in values.iter().enumerate() {
            write_u64_atomic(layout.buffer, index, TaggedValue::from_i64(*value).raw());
        }
        Ok(list)
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
        let root = vm.worker_roots().temporary_values.borrow().len();
        vm.with_temporary_roots(&[list], |vm| {
            let list = vm.worker_roots().temporary_values.borrow()[root];
            assert!(vm.list_get(list, TaggedValue::from_u64(0))?.is_unit());
            vm.list_set(list, TaggedValue::from_u64(1), TaggedValue::TRUE)?;
            for _ in 0..20 {
                let list = vm.worker_roots().temporary_values.borrow()[root];
                vm.list_push(list, TaggedValue::FALSE)?;
            }
            let list = vm.worker_roots().temporary_values.borrow()[root];
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
        assert!(vm.worker_roots().temporary_values.borrow().is_empty());
    }

    #[test]
    fn integer_indices_and_capacity_limits_are_checked_without_mutation() {
        let mut vm = make_vm();
        // Allocate beyond the legacy `u16::MAX` limit (`70_000 > 65_535`) to verify
        // `PayloadRole::LargeTagged` backing buffers alongside index & overflow guards.
        let large_len = (u16::MAX as usize) + 4_465;
        let list = vm.new_list(large_len).unwrap();
        vm.with_temporary_roots(&[list], |vm| {
            let layout = vm.list_layout(list)?;
            let buf_hdr = unsafe { ObjectHeader::from_payload_ptr(layout.buffer.cast::<u8>()) };
            assert_eq!(buf_hdr.payload_role(), Some(gc::PayloadRole::LargeTagged));
            assert_eq!(buf_hdr.payload_words(), large_len);
            assert_eq!(vm.list_len(list)?, large_len);
            assert!(matches!(
                vm.list_get(list, TaggedValue::from_i64(-1)),
                Err(VmError::InvalidIndex)
            ));
            assert!(matches!(
                vm.list_get(list, TaggedValue::from_u64(large_len as u64)),
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
            assert!(
                vm.list_get(list, TaggedValue::from_u64((large_len - 1) as u64))?
                    .is_unit()
            );
            assert!(matches!(
                vm.list_resize(
                    list,
                    TaggedValue::from_u64((MAX_CAPACITY + 1) as u64),
                    TaggedValue::TRUE
                ),
                Err(VmError::ObjectTooLarge)
            ));
            assert_eq!(vm.list_len(list)?, large_len);
            Ok(())
        })
        .unwrap();
        assert!(vm.worker_roots().temporary_values.borrow().is_empty());
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
            unsafe {
                pointer.write(TaggedValue::from_u64(1).raw());
            }
            assert!(matches!(vm.list_len(list), Err(VmError::TypeError)));
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
            let ty = vm.type_pool_read().list_type().unwrap();
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
                abi: nsbc::FunctionAbi::value_only(0),
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
            abi: nsbc::FunctionAbi::value_only(0),
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
                abi: nsbc::FunctionAbi::value_only(0),
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
            unsafe {
                pointer.write(TaggedValue::from_u64(65535).raw());
            }
            assert!(matches!(vm.format_aggregate(list), Err(VmError::TypeError)));
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
        vm.worker_roots()
            .temporary_values
            .borrow_mut()
            .push(TaggedValue::TRUE);
        let baseline = vm.worker_roots().temporary_values.borrow().clone();
        let result: Result<(), VmError> = vm.with_temporary_roots(&[TaggedValue::UNIT], |vm| {
            vm.worker_roots()
                .temporary_values
                .borrow_mut()
                .push(TaggedValue::FALSE);
            Err(VmError::TypeError)
        });
        assert!(matches!(result, Err(VmError::TypeError)));
        assert_eq!(*vm.worker_roots().temporary_values.borrow(), baseline);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            vm.with_temporary_roots(&[TaggedValue::UNIT], |vm| -> Result<(), VmError> {
                vm.worker_roots()
                    .temporary_values
                    .borrow_mut()
                    .push(TaggedValue::FALSE);
                panic!("intentional List action unwind");
            })
        }));
        assert!(result.is_err());
        assert_eq!(*vm.worker_roots().temporary_values.borrow(), baseline);
    }
}
