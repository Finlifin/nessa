//! Compact GC No-Scan `Bytes` object operations with O(1) zero-copy slicing and COW.
//!
//! Layout:
//! - Wrapper (`CollectionRole::Bytes`, `PayloadRole::Ordinary`, 4 words):
//!   `[len: u64, offset: u64, capacity: u64, buffer: Any]`
//! - Backing storage (`CollectionRole::BytesBuffer`, `PayloadRole::ByteBuffer`):
//!   `[share_count: u64][raw bytes: capacity]`
//!
//! Because backing storage uses `PayloadRole::ByteBuffer`, MMTk never scans its
//! bytes as `TaggedValue` pointers and seamlessly allocates buffers larger than
//! `LARGE_OBJECT_THRESHOLD` in Large Object Space (LOS) up to ~256 MiB.

use gc::{MAX_BYTE_BUFFER_BYTES, ObjectHeader, PayloadRole};
use runtime::{Number, TaggedValue};
use type_pool::{CollectionRole, Intrinsic};

use crate::{Vm, VmError};

/// Maximum data byte capacity of a single `Bytes` buffer (8 bytes reserved for `share_count`).
pub const MAX_BYTES_CAPACITY: usize = MAX_BYTE_BUFFER_BYTES - 8;

pub(crate) struct BytesLayout {
    pub(crate) wrapper: *mut u64,
    pub(crate) buffer_payload: *mut u8,
    pub(crate) data_ptr: *mut u8,
    pub(crate) length: usize,
    pub(crate) offset: usize,
    pub(crate) capacity: usize,
}

impl BytesLayout {
    #[inline]
    pub(crate) fn as_slice(&self) -> &[u8] {
        if self.length == 0 {
            &[]
        } else {
            // SAFETY: `bytes_layout` checked `offset + length <= capacity` on a live
            // `ByteBuffer` payload within the active non-moving mutator section.
            unsafe { std::slice::from_raw_parts(self.data_ptr.add(self.offset), self.length) }
        }
    }

    #[inline]
    pub(crate) fn share_count(&self) -> u64 {
        if self.buffer_payload.is_null() {
            0
        } else {
            // SAFETY: `bytes_layout` verified at least 8 bytes precede `data_ptr`.
            unsafe { self.buffer_payload.cast::<u64>().read() }
        }
    }
}

pub(crate) fn checked_usize_arg(value: TaggedValue) -> Result<usize, VmError> {
    // SAFETY: caller holds the active execution operation and roots `value`.
    let number = unsafe { Number::from_tagged(value) }.ok_or(VmError::TypeError)?;
    match number {
        Number::U64(n) => usize::try_from(n).map_err(|_| VmError::InvalidIndex),
        Number::I64(n) => usize::try_from(n).map_err(|_| VmError::InvalidIndex),
        Number::U128(n) => usize::try_from(n).map_err(|_| VmError::InvalidIndex),
        Number::I128(n) => usize::try_from(n).map_err(|_| VmError::InvalidIndex),
        Number::F64(_) => Err(VmError::TypeError),
    }
}

pub(crate) fn checked_byte_arg(value: TaggedValue) -> Result<u8, VmError> {
    // SAFETY: caller holds the active execution operation and roots `value`.
    let number = unsafe { Number::from_tagged(value) }.ok_or(VmError::TypeError)?;
    match number {
        Number::U64(n) => {
            u8::try_from(n).map_err(|_| VmError::InvalidArgument("byte out of 0..=255"))
        }
        Number::I64(n) => {
            u8::try_from(n).map_err(|_| VmError::InvalidArgument("byte out of 0..=255"))
        }
        Number::U128(n) => {
            u8::try_from(n).map_err(|_| VmError::InvalidArgument("byte out of 0..=255"))
        }
        Number::I128(n) => {
            u8::try_from(n).map_err(|_| VmError::InvalidArgument("byte out of 0..=255"))
        }
        Number::F64(_) => Err(VmError::TypeError),
    }
}

/// Borrow raw UTF-8 slice of a validated heap `String` without copying.
pub(crate) fn heap_str_slice<'a>(value: TaggedValue) -> Option<&'a [u8]> {
    let ptr = value.as_heap_ptr()?;
    // SAFETY: callers supply a live rooted `TaggedValue` inside a mutator operation
    // with no intervening allocation while the returned slice is used.
    let header = unsafe { ObjectHeader::from_payload_ptr(ptr) };
    if !header.is_ordinary()
        || header.type_index != Intrinsic::Str.type_index()
        || header.payload_words() == 0
    {
        return None;
    }
    // SAFETY: `payload_words() >= 1` guarantees the 8-byte length header is readable.
    let len = unsafe { *(ptr as *const u64) } as usize;
    if len > header.payload_words() * 8 - 8 {
        return None;
    }
    // SAFETY: `8 + len` fits within `payload_words() * 8`.
    let bytes = unsafe { std::slice::from_raw_parts(ptr.add(8), len) };
    std::str::from_utf8(bytes).ok()?;
    Some(bytes)
}

impl Vm {
    pub(crate) fn is_bytes_value(&self, value: TaggedValue) -> Result<bool, VmError> {
        let Some(pointer) = value.as_heap_ptr() else {
            return Ok(false);
        };
        // SAFETY: `value` is rooted by caller within an active mutator operation.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        if !header.is_ordinary() {
            return Ok(false);
        }
        match self
            .state
            .type_pool
            .checked_collection_role(header.type_index)
        {
            Ok(Some(CollectionRole::Bytes)) => {
                self.bytes_layout(value)?;
                Ok(true)
            }
            Ok(_) => Ok(false),
            Err(_) => Err(VmError::TypeError),
        }
    }

    pub(crate) fn bytes_layout(&self, value: TaggedValue) -> Result<BytesLayout, VmError> {
        let pointer = value.as_heap_ptr().ok_or(VmError::TypeError)?;
        // SAFETY: `value` is a live rooted VM object within an active operation.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        if !header.is_ordinary()
            || self
                .state
                .type_pool
                .checked_collection_role(header.type_index)
                .map_err(|_| VmError::TypeError)?
                != Some(CollectionRole::Bytes)
            || header.payload_words() != 4
        {
            return Err(VmError::TypeError);
        }
        let wrapper = pointer.cast_mut().cast::<u64>();
        // SAFETY: validated `Bytes` header guarantees four aligned `TaggedValue` words.
        let (length, offset, capacity, buffer) = unsafe {
            (
                TaggedValue::from_raw(wrapper.read()),
                TaggedValue::from_raw(wrapper.add(1).read()),
                TaggedValue::from_raw(wrapper.add(2).read()),
                TaggedValue::from_raw(wrapper.add(3).read()),
            )
        };
        let length = length
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(VmError::TypeError)?;
        let offset = offset
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(VmError::TypeError)?;
        let capacity = capacity
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .ok_or(VmError::TypeError)?;
        let end = offset.checked_add(length).ok_or(VmError::TypeError)?;
        if end > capacity || capacity > MAX_BYTES_CAPACITY {
            return Err(VmError::TypeError);
        }
        if capacity == 0 {
            if !buffer.is_null() {
                return Err(VmError::TypeError);
            }
            return Ok(BytesLayout {
                wrapper,
                buffer_payload: std::ptr::null_mut(),
                data_ptr: std::ptr::null_mut(),
                length,
                offset,
                capacity,
            });
        }
        let buf_ptr = buffer.as_heap_ptr().ok_or(VmError::TypeError)?;
        // SAFETY: `buf_ptr` is stored in the rooted wrapper's scanned slot.
        let buf_header = unsafe { ObjectHeader::from_payload_ptr(buf_ptr) };
        if buf_header.payload_role() != Some(PayloadRole::ByteBuffer)
            || self
                .state
                .type_pool
                .checked_collection_role(buf_header.type_index)
                .map_err(|_| VmError::TypeError)?
                != Some(CollectionRole::BytesBuffer)
        {
            return Err(VmError::TypeError);
        }
        let min_bytes = 8usize.checked_add(capacity).ok_or(VmError::TypeError)?;
        if buf_header.payload_words().saturating_mul(8) < min_bytes {
            return Err(VmError::TypeError);
        }
        let buffer_payload = buf_ptr.cast_mut();
        // SAFETY: `min_bytes >= 8 + capacity` was verified against `buf_header`.
        let data_ptr = unsafe { buffer_payload.add(8) };
        Ok(BytesLayout {
            wrapper,
            buffer_payload,
            data_ptr,
            length,
            offset,
            capacity,
        })
    }

    fn alloc_bytes_buffer(&mut self, capacity: usize) -> Result<TaggedValue, VmError> {
        if capacity == 0 {
            return Ok(TaggedValue::NULL);
        }
        if capacity > MAX_BYTES_CAPACITY {
            return Err(VmError::ObjectTooLarge);
        }
        let buf_ty = self
            .state
            .type_pool
            .bytes_buffer_type()
            .ok_or(VmError::TypeError)?;
        // SAFETY: active execution operation owns mutator; initialized before return.
        let ptr = unsafe { self.state.heap.alloc_byte_buffer(buf_ty, 8 + capacity) }
            .ok_or(VmError::OutOfMemory)?;
        // SAFETY: at least 8 bytes were allocated and 8-byte aligned; set `share_count = 1`.
        unsafe {
            ptr.as_ptr().cast::<u64>().write(1);
            Ok(TaggedValue::from_heap_ptr(ptr.as_ptr()))
        }
    }

    pub(crate) fn new_bytes_with_len(
        &mut self,
        capacity: usize,
        initial_len: usize,
    ) -> Result<TaggedValue, VmError> {
        if initial_len > capacity {
            return Err(VmError::InvalidArgument(
                "Bytes initial length exceeds capacity",
            ));
        }
        let bytes_ty = self
            .state
            .type_pool
            .bytes_type()
            .ok_or(VmError::TypeError)?;
        let buffer = self.alloc_bytes_buffer(capacity)?;
        let base = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&[buffer], |vm| {
            // SAFETY: `buffer` is rooted in `temporary_values` across `alloc_object`.
            let wrapper_ptr =
                unsafe { vm.state.heap.alloc_object(bytes_ty, 4) }.ok_or(VmError::OutOfMemory)?;
            let reloaded_buf = vm.roots.temporary_values.borrow()[base];
            let wrapper = wrapper_ptr.as_ptr().cast::<u64>();
            // SAFETY: 4-word wrapper was just allocated and is initialized before safe points.
            unsafe {
                wrapper.write(TaggedValue::from_u64(initial_len as u64).raw());
                wrapper.add(1).write(TaggedValue::from_u64(0).raw());
                wrapper
                    .add(2)
                    .write(TaggedValue::from_u64(capacity as u64).raw());
                wrapper.add(3).write(reloaded_buf.raw());
                Ok(TaggedValue::from_heap_ptr(wrapper_ptr.as_ptr()))
            }
        })
    }

    pub(crate) fn alloc_bytes_from_slice(&mut self, data: &[u8]) -> Result<TaggedValue, VmError> {
        let len = data.len();
        let bytes_ty = self
            .state
            .type_pool
            .bytes_type()
            .ok_or(VmError::TypeError)?;
        let buffer = self.alloc_bytes_buffer(len)?;
        if len > 0 {
            let buf_ptr = buffer.as_heap_ptr().expect("non-empty buffer is heap ptr");
            // SAFETY: no allocation has occurred since `alloc_bytes_buffer(len)`, and
            // `data` is a host slice disjoint from the newly allocated GC object.
            unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr(), buf_ptr.cast_mut().add(8), len);
            }
        }
        let base = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&[buffer], |vm| {
            // SAFETY: `buffer` is rooted in `temporary_values` across `alloc_object`.
            let wrapper_ptr =
                unsafe { vm.state.heap.alloc_object(bytes_ty, 4) }.ok_or(VmError::OutOfMemory)?;
            let reloaded_buf = vm.roots.temporary_values.borrow()[base];
            let wrapper = wrapper_ptr.as_ptr().cast::<u64>();
            // SAFETY: 4-word wrapper is initialized before publication.
            unsafe {
                wrapper.write(TaggedValue::from_u64(len as u64).raw());
                wrapper.add(1).write(TaggedValue::from_u64(0).raw());
                wrapper
                    .add(2)
                    .write(TaggedValue::from_u64(len as u64).raw());
                wrapper.add(3).write(reloaded_buf.raw());
                Ok(TaggedValue::from_heap_ptr(wrapper_ptr.as_ptr()))
            }
        })
    }

    /// Ensure `bytes_val` uniquely owns a buffer with `offset == 0` (or enough room)
    /// to accommodate `required_len` bytes, detaching via COW if shared.
    /// Returns the reloaded `Bytes` `TaggedValue`.
    pub(crate) fn ensure_bytes_unique(
        &mut self,
        bytes_val: TaggedValue,
        required_len: usize,
    ) -> Result<TaggedValue, VmError> {
        if required_len > MAX_BYTES_CAPACITY {
            return Err(VmError::ObjectTooLarge);
        }
        let layout = self.bytes_layout(bytes_val)?;
        if layout.capacity == 0 && required_len == 0 {
            return Ok(bytes_val);
        }
        if layout.capacity > 0 && layout.share_count() == 1 && required_len <= layout.capacity {
            if layout.offset + required_len > layout.capacity {
                // Compact uniquely owned slice to offset 0 in place without allocating.
                // SAFETY: `share_count == 1` guarantees unique ownership; `copy` handles overlap.
                unsafe {
                    if layout.length > 0 {
                        std::ptr::copy(
                            layout.data_ptr.add(layout.offset),
                            layout.data_ptr,
                            layout.length,
                        );
                    }
                    layout.wrapper.add(1).write(TaggedValue::from_u64(0).raw());
                }
            }
            return Ok(bytes_val);
        }

        let new_cap = if required_len <= layout.capacity {
            required_len
                .max(layout.length)
                .clamp(16, MAX_BYTES_CAPACITY)
        } else {
            layout
                .capacity
                .saturating_mul(2)
                .max(required_len)
                .clamp(16, MAX_BYTES_CAPACITY)
        };

        let base = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&[bytes_val], |vm| {
            let new_buf = vm.alloc_bytes_buffer(new_cap)?;
            let reloaded_bytes = vm.roots.temporary_values.borrow()[base];
            let old = vm.bytes_layout(reloaded_bytes)?;
            let new_buf_ptr = new_buf
                .as_heap_ptr()
                .expect("new_cap >= 16 yields heap buffer")
                .cast_mut();
            // SAFETY: no allocation occurs between `bytes_layout` and copying; `old` and
            // `new_buf_ptr` are distinct buffers.
            unsafe {
                if old.length > 0 {
                    std::ptr::copy_nonoverlapping(
                        old.data_ptr.add(old.offset),
                        new_buf_ptr.add(8),
                        old.length,
                    );
                }
                if !old.buffer_payload.is_null() {
                    let sc = old.buffer_payload.cast::<u64>().read();
                    if sc > 1 && sc < u64::MAX {
                        old.buffer_payload.cast::<u64>().write(sc - 1);
                    }
                }
                old.wrapper.add(1).write(TaggedValue::from_u64(0).raw());
                old.wrapper
                    .add(2)
                    .write(TaggedValue::from_u64(new_cap as u64).raw());
                old.wrapper.add(3).write(new_buf.raw());
            }
            Ok(reloaded_bytes)
        })
    }

    pub(crate) fn bytes_len(&self, bytes_val: TaggedValue) -> Result<usize, VmError> {
        Ok(self.bytes_layout(bytes_val)?.length)
    }

    pub(crate) fn bytes_cap(&self, bytes_val: TaggedValue) -> Result<usize, VmError> {
        let layout = self.bytes_layout(bytes_val)?;
        Ok(layout.capacity.saturating_sub(layout.offset))
    }

    pub(crate) fn bytes_get(&self, bytes_val: TaggedValue, index: usize) -> Result<u8, VmError> {
        let layout = self.bytes_layout(bytes_val)?;
        layout
            .as_slice()
            .get(index)
            .copied()
            .ok_or(VmError::InvalidIndex)
    }

    pub(crate) fn bytes_set(
        &mut self,
        bytes_val: TaggedValue,
        index: usize,
        byte: u8,
    ) -> Result<(), VmError> {
        let len = self.bytes_layout(bytes_val)?.length;
        if index >= len {
            return Err(VmError::InvalidIndex);
        }
        let reloaded = self.ensure_bytes_unique(bytes_val, len)?;
        let layout = self.bytes_layout(reloaded)?;
        // SAFETY: `ensure_bytes_unique` guaranteed `share_count == 1` and `index < layout.length`.
        unsafe {
            layout.data_ptr.add(layout.offset + index).write(byte);
        }
        Ok(())
    }

    pub(crate) fn bytes_push(&mut self, bytes_val: TaggedValue, byte: u8) -> Result<(), VmError> {
        let old_len = self.bytes_layout(bytes_val)?.length;
        let new_len = old_len.checked_add(1).ok_or(VmError::ObjectTooLarge)?;
        let reloaded = self.ensure_bytes_unique(bytes_val, new_len)?;
        let layout = self.bytes_layout(reloaded)?;
        // SAFETY: `ensure_bytes_unique` reserved `offset + new_len <= capacity` with unique owner.
        unsafe {
            layout.data_ptr.add(layout.offset + old_len).write(byte);
            layout
                .wrapper
                .write(TaggedValue::from_u64(new_len as u64).raw());
        }
        Ok(())
    }

    pub(crate) fn bytes_clear(&mut self, bytes_val: TaggedValue) -> Result<(), VmError> {
        let layout = self.bytes_layout(bytes_val)?;
        if !layout.buffer_payload.is_null() && layout.share_count() > 1 {
            // Detach shared buffer so later pushes on this cleared wrapper cannot clobber views.
            // SAFETY: `layout` validated both `wrapper` and `buffer_payload`.
            unsafe {
                let sc = layout.buffer_payload.cast::<u64>().read();
                if sc < u64::MAX {
                    layout.buffer_payload.cast::<u64>().write(sc - 1);
                }
                layout.wrapper.write(TaggedValue::from_u64(0).raw());
                layout.wrapper.add(1).write(TaggedValue::from_u64(0).raw());
                layout.wrapper.add(2).write(TaggedValue::from_u64(0).raw());
                layout.wrapper.add(3).write(TaggedValue::NULL.raw());
            }
        } else {
            // SAFETY: uniquely owned wrapper; reset `len = 0, offset = 0` retaining capacity.
            unsafe {
                layout.wrapper.write(TaggedValue::from_u64(0).raw());
                layout.wrapper.add(1).write(TaggedValue::from_u64(0).raw());
            }
        }
        Ok(())
    }

    /// O(1) zero-copy sub-slice sharing the backing `ByteBuffer` via COW reference counting.
    pub(crate) fn bytes_slice(
        &mut self,
        bytes_val: TaggedValue,
        start: usize,
        end: usize,
    ) -> Result<TaggedValue, VmError> {
        let layout = self.bytes_layout(bytes_val)?;
        if start > end || end > layout.length {
            return Err(VmError::InvalidIndex);
        }
        let slice_len = end - start;
        if slice_len == 0 {
            return self.new_bytes_with_len(0, 0);
        }
        let bytes_ty = self
            .state
            .type_pool
            .bytes_type()
            .ok_or(VmError::TypeError)?;
        // Increment share count before allocating wrapper; rollback if wrapper allocation fails.
        // SAFETY: `slice_len > 0` implies `layout.buffer_payload` is non-null and valid.
        unsafe {
            let sc = layout.buffer_payload.cast::<u64>().read();
            layout
                .buffer_payload
                .cast::<u64>()
                .write(sc.saturating_add(1));
        }
        let base = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&[bytes_val], |vm| {
            // SAFETY: `bytes_val` is rooted in `temporary_values` across `alloc_object`.
            let Some(wrapper_ptr) = (unsafe { vm.state.heap.alloc_object(bytes_ty, 4) }) else {
                let reloaded = vm.roots.temporary_values.borrow()[base];
                if let Ok(reloaded_layout) = vm.bytes_layout(reloaded)
                    && !reloaded_layout.buffer_payload.is_null()
                {
                    unsafe {
                        let sc = reloaded_layout.buffer_payload.cast::<u64>().read();
                        if sc > 1 && sc < u64::MAX {
                            reloaded_layout.buffer_payload.cast::<u64>().write(sc - 1);
                        }
                    }
                }
                return Err(VmError::OutOfMemory);
            };
            let reloaded = vm.roots.temporary_values.borrow()[base];
            let old = vm.bytes_layout(reloaded)?;
            let new_offset = old.offset + start;
            let wrapper = wrapper_ptr.as_ptr().cast::<u64>();
            // SAFETY: both `old` and `wrapper_ptr` are live with no intervening allocation.
            unsafe {
                let buf_val = old.wrapper.add(3).read();
                wrapper.write(TaggedValue::from_u64(slice_len as u64).raw());
                wrapper
                    .add(1)
                    .write(TaggedValue::from_u64(new_offset as u64).raw());
                wrapper
                    .add(2)
                    .write(TaggedValue::from_u64(old.capacity as u64).raw());
                wrapper.add(3).write(buf_val);
                Ok(TaggedValue::from_heap_ptr(wrapper_ptr.as_ptr()))
            }
        })
    }

    /// Deep copy into a fresh uniquely-owned `Bytes`.
    pub(crate) fn bytes_copy(&mut self, bytes_val: TaggedValue) -> Result<TaggedValue, VmError> {
        let len = self.bytes_layout(bytes_val)?.length;
        if len == 0 {
            return self.new_bytes_with_len(0, 0);
        }
        let bytes_ty = self
            .state
            .type_pool
            .bytes_type()
            .ok_or(VmError::TypeError)?;
        let base = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&[bytes_val], |vm| {
            let new_buf = vm.alloc_bytes_buffer(len)?;
            let src_val = vm.roots.temporary_values.borrow()[base];
            let src_layout = vm.bytes_layout(src_val)?;
            let new_buf_ptr = new_buf.as_heap_ptr().expect("len > 0").cast_mut();
            // SAFETY: no allocation between `bytes_layout` and copy; buffers are distinct.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    src_layout.data_ptr.add(src_layout.offset),
                    new_buf_ptr.add(8),
                    len,
                );
            }
            let inner_base = vm.roots.temporary_values.borrow().len();
            vm.with_temporary_roots(&[new_buf], |vm| {
                // SAFETY: `new_buf` is rooted across wrapper allocation.
                let wrapper_ptr = unsafe { vm.state.heap.alloc_object(bytes_ty, 4) }
                    .ok_or(VmError::OutOfMemory)?;
                let reloaded_buf = vm.roots.temporary_values.borrow()[inner_base];
                let wrapper = wrapper_ptr.as_ptr().cast::<u64>();
                unsafe {
                    wrapper.write(TaggedValue::from_u64(len as u64).raw());
                    wrapper.add(1).write(TaggedValue::from_u64(0).raw());
                    wrapper
                        .add(2)
                        .write(TaggedValue::from_u64(len as u64).raw());
                    wrapper.add(3).write(reloaded_buf.raw());
                    Ok(TaggedValue::from_heap_ptr(wrapper_ptr.as_ptr()))
                }
            })
        })
    }

    /// Append bytes from `src` (either `Bytes` or `String`) onto `dst`.
    pub(crate) fn bytes_extend(
        &mut self,
        dst: TaggedValue,
        src: TaggedValue,
    ) -> Result<(), VmError> {
        let dst_len = self.bytes_layout(dst)?.length;
        let src_is_str = heap_str_slice(src).is_some();
        let src_len = if let Some(s) = heap_str_slice(src) {
            s.len()
        } else {
            self.bytes_layout(src)?.length
        };
        if src_len == 0 {
            return Ok(());
        }
        let new_len = dst_len
            .checked_add(src_len)
            .ok_or(VmError::ObjectTooLarge)?;
        let base = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&[dst, src], |vm| {
            let dst_root = vm.roots.temporary_values.borrow()[base];
            let unique_dst = vm.ensure_bytes_unique(dst_root, new_len)?;
            let reloaded_src = vm.roots.temporary_values.borrow()[base + 1];
            let dst_layout = vm.bytes_layout(unique_dst)?;
            if src_is_str {
                let s = heap_str_slice(reloaded_src).ok_or(VmError::TypeError)?;
                // SAFETY: `s` is a String object distinct from `unique_dst`'s `ByteBuffer`.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        s.as_ptr(),
                        dst_layout.data_ptr.add(dst_layout.offset + dst_len),
                        src_len,
                    );
                }
            } else {
                let src_layout = vm.bytes_layout(reloaded_src)?;
                // SAFETY: `std::ptr::copy` handles self-append (`b.extend(b)`) safely even if
                // `src_layout` and `dst_layout` point into the same buffer.
                unsafe {
                    std::ptr::copy(
                        src_layout.data_ptr.add(src_layout.offset),
                        dst_layout.data_ptr.add(dst_layout.offset + dst_len),
                        src_len,
                    );
                }
            }
            // SAFETY: `dst_layout.wrapper` is live and `new_len <= dst_layout.capacity`.
            unsafe {
                dst_layout
                    .wrapper
                    .write(TaggedValue::from_u64(new_len as u64).raw());
            }
            Ok(())
        })
    }

    pub(crate) fn bytes_from_str(&mut self, str_val: TaggedValue) -> Result<TaggedValue, VmError> {
        let str_len = heap_str_slice(str_val).ok_or(VmError::TypeError)?.len();
        if str_len == 0 {
            return self.new_bytes_with_len(0, 0);
        }
        let bytes_ty = self
            .state
            .type_pool
            .bytes_type()
            .ok_or(VmError::TypeError)?;
        let base = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&[str_val], |vm| {
            let buf = vm.alloc_bytes_buffer(str_len)?;
            let reloaded_str = vm.roots.temporary_values.borrow()[base];
            let src_slice = heap_str_slice(reloaded_str).ok_or(VmError::TypeError)?;
            let buf_ptr = buf.as_heap_ptr().expect("str_len > 0").cast_mut();
            // SAFETY: no allocation between `heap_str_slice` and `copy_nonoverlapping`.
            unsafe {
                std::ptr::copy_nonoverlapping(src_slice.as_ptr(), buf_ptr.add(8), str_len);
            }
            let inner_base = vm.roots.temporary_values.borrow().len();
            vm.with_temporary_roots(&[buf], |vm| {
                // SAFETY: `buf` is rooted across wrapper allocation.
                let wrapper_ptr = unsafe { vm.state.heap.alloc_object(bytes_ty, 4) }
                    .ok_or(VmError::OutOfMemory)?;
                let reloaded_buf = vm.roots.temporary_values.borrow()[inner_base];
                let wrapper = wrapper_ptr.as_ptr().cast::<u64>();
                unsafe {
                    wrapper.write(TaggedValue::from_u64(str_len as u64).raw());
                    wrapper.add(1).write(TaggedValue::from_u64(0).raw());
                    wrapper
                        .add(2)
                        .write(TaggedValue::from_u64(str_len as u64).raw());
                    wrapper.add(3).write(reloaded_buf.raw());
                    Ok(TaggedValue::from_heap_ptr(wrapper_ptr.as_ptr()))
                }
            })
        })
    }

    /// Strict UTF-8 conversion: returns `TaggedValue::NULL` if invalid UTF-8 or larger than String max.
    pub(crate) fn bytes_to_str(&mut self, bytes_val: TaggedValue) -> Result<TaggedValue, VmError> {
        let owned = {
            let layout = self.bytes_layout(bytes_val)?;
            let Ok(text) = std::str::from_utf8(layout.as_slice()) else {
                return Ok(TaggedValue::NULL);
            };
            text.to_owned()
        };
        self.alloc_string(&owned)
    }

    pub(crate) fn bytes_from_list(
        &mut self,
        list_val: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let len = self.list_len(list_val)?;
        let mut raw = Vec::with_capacity(len);
        for i in 0..len {
            let elem = self.list_get(list_val, TaggedValue::from_u64(i as u64))?;
            raw.push(checked_byte_arg(elem)?);
        }
        self.alloc_bytes_from_slice(&raw)
    }

    pub(crate) fn bytes_to_list(&mut self, bytes_val: TaggedValue) -> Result<TaggedValue, VmError> {
        let raw = self.bytes_layout(bytes_val)?.as_slice().to_vec();
        if raw.len() > u16::MAX as usize {
            return Err(VmError::ObjectTooLarge);
        }
        let list = self.new_list(raw.len())?;
        let base = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&[list], |vm| {
            for (idx, b) in raw.into_iter().enumerate() {
                let reloaded_list = vm.roots.temporary_values.borrow()[base];
                vm.list_set(
                    reloaded_list,
                    TaggedValue::from_u64(idx as u64),
                    TaggedValue::from_i64(i64::from(b)),
                )?;
            }
            Ok(vm.roots.temporary_values.borrow()[base])
        })
    }

    pub(crate) fn bytes_find(
        &self,
        haystack: TaggedValue,
        needle: TaggedValue,
        start: usize,
    ) -> Result<Option<usize>, VmError> {
        let hay_layout = self.bytes_layout(haystack)?;
        let hay_slice = hay_layout.as_slice();
        let needle_slice: &[u8] = if let Some(s) = heap_str_slice(needle) {
            s
        } else {
            // Need `needle_layout` to live across `memmem::find`.
            let needle_layout = self.bytes_layout(needle)?;
            if start > hay_slice.len() {
                return Ok(None);
            }
            return Ok(
                memchr::memmem::find(&hay_slice[start..], needle_layout.as_slice())
                    .map(|pos| start + pos),
            );
        };
        if start > hay_slice.len() {
            return Ok(None);
        }
        Ok(memchr::memmem::find(&hay_slice[start..], needle_slice).map(|pos| start + pos))
    }

    pub(crate) fn bytes_eq(&self, left: TaggedValue, right: TaggedValue) -> Result<bool, VmError> {
        let l = self.bytes_layout(left)?;
        let r = self.bytes_layout(right)?;
        Ok(l.as_slice() == r.as_slice())
    }

    pub(crate) fn bytes_read_u64_le(
        &self,
        bytes_val: TaggedValue,
        offset: usize,
    ) -> Result<u64, VmError> {
        let layout = self.bytes_layout(bytes_val)?;
        let end = offset.checked_add(8).ok_or(VmError::InvalidIndex)?;
        let slice = layout.as_slice();
        if end > slice.len() {
            return Err(VmError::InvalidIndex);
        }
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&slice[offset..end]);
        Ok(u64::from_le_bytes(arr))
    }
}
