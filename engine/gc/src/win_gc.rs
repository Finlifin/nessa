//! Native Windows Stop-The-World Precise Mark-Sweep + Block/LOS Garbage Collector.
//!
//! Provides the exact same [`Heap`], [`NessaSlot`], [`WeakObject`], [`NessaActivePlan`], and
//! STW [`mutators`] / [`roots`] invariants as the Unix MMTk backend without requiring POSIX
//! `mmap` overcommit or Linux/macOS virtual-address reservations, supporting Windows 10 1709+
//! and Wine cleanly.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::ptr::NonNull;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

use type_pool::TypeIndex;

use crate::{
    EXTERNAL_RC_SHELL_WORDS, ExternalRcHandle, GC_FLAGS, GcConfig, LARGE_TAGGED_PREFIX_BYTES,
    MAX_LARGE_TAGGED_WORDS, NessaSlot, ObjectHeader, PayloadRole, external_rc, mutators, roots,
    visit_payload_reference_slots,
};

/// Bit 0 of `ObjectHeader::gc_meta` (reserved bits `0..=2` for collector mark/forwarding state).
pub(crate) const MARK_BIT: u32 = 0x1;

/// Per-mutator allocation flush quantum (256 KiB) before updating global heap pressure.
const LOCAL_FLUSH_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Address(usize);

impl Address {
    #[inline]
    pub const fn as_usize(self) -> usize {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OpaquePointer(usize);

impl OpaquePointer {
    pub const UNINITIALIZED: Self = Self(0);

    #[inline]
    pub const fn from_address(addr: usize) -> Self {
        Self(addr)
    }

    #[inline]
    pub const fn to_address(self) -> Address {
        Address(self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VMThread(pub OpaquePointer);

impl VMThread {
    pub const UNINITIALIZED: Self = Self(OpaquePointer::UNINITIALIZED);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VMMutatorThread(pub VMThread);

/// Per-worker mutator state owned by [`Heap`].
///
/// Allocations append to `local_objects` without locking on the fast path; the collector
/// drains `local_objects` exclusively while mutators are stopped in `mutators::stop_all()`.
pub struct WinMutator {
    local_objects: Vec<usize>,
    local_bytes: usize,
}

impl WinMutator {
    fn new() -> Self {
        Self {
            local_objects: Vec::with_capacity(256),
            local_bytes: 0,
        }
    }
}

struct CollectorSharedState {
    /// Objects flushed from destroyed mutators or overflowed local buffers between collections.
    flushed_objects: Vec<usize>,
    /// Approximate bytes allocated since last collection across flushed mutator batches.
    flushed_bytes: usize,
    /// Dynamic byte threshold triggering background STW collection.
    next_gc_threshold: usize,
    /// Base configured heap size floor.
    min_threshold: usize,
    /// Monotonic requested GC ticket counter.
    requested_ticket: u64,
    /// Monotonic completed GC ticket counter.
    completed_ticket: u64,
}

struct WinCollector {
    state: Mutex<CollectorSharedState>,
    worker_cv: Condvar,
    done_cv: Condvar,
    live_bytes_approx: AtomicUsize,
}

static WIN_COLLECTOR: OnceLock<&'static WinCollector> = OnceLock::new();

fn collector(config_heap_size: usize) -> &'static WinCollector {
    WIN_COLLECTOR.get_or_init(|| {
        let min_threshold = config_heap_size.max(4 * 1024 * 1024);
        let boxed = Box::new(WinCollector {
            state: Mutex::new(CollectorSharedState {
                flushed_objects: Vec::with_capacity(4096),
                flushed_bytes: 0,
                next_gc_threshold: min_threshold,
                min_threshold,
                requested_ticket: 0,
                completed_ticket: 0,
            }),
            worker_cv: Condvar::new(),
            done_cv: Condvar::new(),
            live_bytes_approx: AtomicUsize::new(0),
        });
        let static_ref: &'static WinCollector = Box::leak(boxed);
        std::thread::Builder::new()
            .name("nessa-gc-worker".to_string())
            .spawn(move || gc_worker_loop(static_ref))
            .expect("failed to spawn Windows GC worker thread");
        static_ref
    })
}

fn gc_worker_loop(col: &'static WinCollector) {
    let mut tracked_objects: Vec<usize> = Vec::with_capacity(8192);
    let mut mark_stack: Vec<usize> = Vec::with_capacity(2048);

    loop {
        let target_ticket = {
            let mut guard = col.state.lock().unwrap();
            while guard.completed_ticket >= guard.requested_ticket {
                guard = col.worker_cv.wait(guard).unwrap();
            }
            guard.requested_ticket
        };

        // 1. Stop all active mutator operations across all VM worker threads.
        let stopped_mutators = mutators::stop_all();

        // 2. Drain flushed allocations and every stopped mutator's local allocation buffer.
        {
            let mut guard = col.state.lock().unwrap();
            tracked_objects.append(&mut guard.flushed_objects);
            guard.flushed_bytes = 0;
        }
        for mutator_ptr in stopped_mutators {
            // SAFETY: `mutators::stop_all()` guarantees exclusive access to all registered
            // `WinMutator` instances until `mutators::resume()`.
            let m = unsafe { &mut *mutator_ptr };
            tracked_objects.append(&mut m.local_objects);
            m.local_bytes = 0;
        }

        // 3. Prepare conditional / weak roots under STW.
        // SAFETY: All mutators are stopped for the entire collection epoch.
        unsafe {
            roots::prepare_conditional_roots();
            external_rc::prepare_stw();
        };

        // 4. Mark from VM root slots.
        mark_stack.clear();
        // SAFETY: All mutators are stopped; root slots hold aligned `TaggedValue` words.
        unsafe {
            roots::scan_roots(&mut |slot| {
                if let Some(payload_addr) = slot.load_payload_addr() {
                    mark_object(payload_addr, &mut mark_stack);
                }
            });
        }
        drain_mark_stack(&mut mark_stack);

        // 5. Fixed-point conditional root tracing, followed by weak sweep and forward.
        loop {
            // SAFETY: Called under STW after transitive mark closure drained.
            let exposed_new = unsafe {
                external_rc::trace_stw();
                roots::trace_conditional_roots(&mut |slot| {
                    if let Some(payload_addr) = slot.load_payload_addr() {
                        mark_object(payload_addr, &mut mark_stack);
                    }
                })
            };
            drain_mark_stack(&mut mark_stack);
            if !exposed_new {
                break;
            }
        }
        // SAFETY: Fixed point reached with no newly exposed conditional edges; `WeakObject::is_reachable`
        // inspects `MARK_BIT` before unmarked allocations are freed in step 6.
        unsafe {
            roots::sweep_conditional_roots();
            external_rc::sweep_stw();
            external_rc::forward_stw();
            roots::forward_conditional_roots(&mut |_slot| {});
        }

        // 6. Sweep unmarked objects and clear `MARK_BIT` on survivors.
        let mut surviving_bytes = 0usize;
        let mut write_idx = 0usize;
        for read_idx in 0..tracked_objects.len() {
            let payload_addr = tracked_objects[read_idx];
            let payload_ptr = payload_addr as *mut u8;
            // SAFETY: Every entry in `tracked_objects` was allocated by `Heap` with an initialized
            // `ObjectHeader` at `payload_ptr.sub(ObjectHeader::SIZE)` and allocation start at
            // `payload_ptr.sub(header.header_offset())`.
            unsafe {
                let header = ObjectHeader::from_payload_ptr_mut(payload_ptr);
                let obj_size = header.object_size();
                if (header.gc_meta & MARK_BIT) != 0 {
                    header.gc_meta &= !MARK_BIT;
                    tracked_objects[write_idx] = payload_addr;
                    write_idx += 1;
                    surviving_bytes = surviving_bytes.saturating_add(obj_size);
                } else {
                    let block_ptr = payload_ptr.sub(header.header_offset());
                    let layout = Layout::from_size_align_unchecked(obj_size, 8);
                    dealloc(block_ptr, layout);
                }
            }
        }
        tracked_objects.truncate(write_idx);

        // 7. Update collection thresholds, resume mutators, and notify any explicit waiters.
        col.live_bytes_approx
            .store(surviving_bytes, Ordering::Release);
        {
            let mut guard = col.state.lock().unwrap();
            guard.next_gc_threshold = surviving_bytes.saturating_mul(2).max(guard.min_threshold);
            guard.completed_ticket = target_ticket;
        }
        mutators::resume();
        col.done_cv.notify_all();
    }
}

#[inline]
fn mark_object(payload_addr: usize, mark_stack: &mut Vec<usize>) {
    // SAFETY: Root/field slots with `(raw & 0b111) == 0 && raw != 0` point to live managed object payloads.
    let header = unsafe { ObjectHeader::from_payload_ptr_mut(payload_addr as *mut u8) };
    if (header.gc_meta & MARK_BIT) == 0 {
        header.gc_meta |= MARK_BIT;
        mark_stack.push(payload_addr);
    }
}

fn drain_mark_stack(mark_stack: &mut Vec<usize>) {
    while let Some(payload_addr) = mark_stack.pop() {
        // SAFETY: `payload_addr` was marked from a live managed allocation with valid `ObjectHeader`.
        let header = unsafe { ObjectHeader::from_payload_ptr(payload_addr as *const u8) };
        visit_payload_reference_slots(payload_addr, header, |slot_addr| {
            // SAFETY: `slot_addr` is an aligned 8-byte `TaggedValue` field inside the live payload.
            let raw = unsafe { *(slot_addr as *const usize) };
            if (raw & 0b111) == 0 && raw != 0 {
                mark_object(raw, mark_stack);
            }
        });
    }
}

impl NessaSlot {
    #[inline]
    pub(crate) fn load_payload_addr(&self) -> Option<usize> {
        // SAFETY: `self.addr` addresses an aligned, initialized `u64`/`usize` slot under STW.
        let raw = unsafe { *(self.addr as *const usize) };
        if (raw & 0b111) == 0 && raw != 0 {
            Some(raw)
        } else {
            None
        }
    }
}

pub struct NessaActivePlan;

impl NessaActivePlan {
    pub fn is_mutator(tls: VMThread) -> bool {
        mutators::contains(tls)
    }
}

pub struct Heap {
    pub(crate) mutator: Box<WinMutator>,
    pub(crate) tls: VMMutatorThread,
    alloc_count: AtomicUsize,
    _tls_identity: Box<u8>,
}

// SAFETY: Moving `Heap` transfers exclusive ownership of its `Box<WinMutator>` and TLS token
// without changing their heap addresses; collector access is gated by the STW barrier.
unsafe impl Send for Heap {}

impl Heap {
    pub fn completed_collections(&self) -> u64 {
        mutators::completed_collections()
    }

    /// # Safety
    /// Hold a mutator operation with all live references published to roots.
    pub unsafe fn collect_garbage(&self) -> bool {
        let col = collector(64 * 1024 * 1024);
        {
            let mut guard = col.state.lock().unwrap();
            guard.requested_ticket += 1;
            GC_FLAGS.gc_requested.store(true, Ordering::Release);
            col.worker_cv.notify_one();
        }
        mutators::block(self.tls);
        true
    }

    /// # Safety
    /// Heap must outlive the returned guard and calls for one Heap must be exclusive to the thread.
    pub unsafe fn begin_operation(&self) -> mutators::MutatorSession<'static> {
        mutators::begin(self.tls)
    }

    /// # Safety
    /// Same requirements as `begin_operation`.
    pub unsafe fn begin_execution(
        &self,
    ) -> Result<mutators::MutatorSession<'static>, mutators::MutatorAccessError> {
        mutators::begin_execution(self.tls)
    }

    /// # Safety
    /// Hold a mutator operation and publish/root the returned pointer before the next safepoint.
    pub unsafe fn alloc_object(
        &mut self,
        type_index: TypeIndex,
        payload_words: u16,
    ) -> Option<NonNull<u8>> {
        // SAFETY: Caller upholds mutator session and root publication invariants.
        unsafe { self.alloc_with_role(type_index, payload_words, PayloadRole::Ordinary) }
    }

    unsafe fn alloc_with_role(
        &mut self,
        type_index: TypeIndex,
        payload_words: u16,
        role: PayloadRole,
    ) -> Option<NonNull<u8>> {
        let payload_bytes = (payload_words as usize).checked_mul(8)?;
        let mut header = ObjectHeader::new(type_index, payload_words);
        header.gc_meta |= (role as u32) << 19;
        // SAFETY: `header` matches `payload_bytes` exact size.
        unsafe { self.alloc_raw_object(header, payload_bytes) }
    }

    /// # Safety
    /// Caller holds a mutator operation and initializes the 3-word Error envelope before safepoint.
    pub unsafe fn alloc_error_envelope(&mut self, type_index: TypeIndex) -> Option<NonNull<u8>> {
        // SAFETY: Role is stamped in header prior to publication.
        unsafe { self.alloc_with_role(type_index, 3, PayloadRole::ErrorEnvelope) }
    }

    /// # Safety
    /// Caller holds a mutator operation and publishes the byte buffer before safepoint.
    pub unsafe fn alloc_byte_buffer(
        &mut self,
        type_index: TypeIndex,
        byte_len: usize,
    ) -> Option<NonNull<u8>> {
        let payload_words = byte_len.checked_add(7)? / 8;
        let header = ObjectHeader::new_byte_buffer(type_index, payload_words)?;
        let payload_bytes = payload_words.checked_mul(8)?;
        // SAFETY: `header` encodes `ByteBuffer` role and exact 25-bit `payload_words`.
        unsafe { self.alloc_raw_object(header, payload_bytes) }
    }

    /// Allocate a scanned `TaggedValue`-slot object of `payload_words` words,
    /// using [`PayloadRole::Ordinary`] up to `u16::MAX` words and [`PayloadRole::LargeTagged`]
    /// (8-byte `u64` prefix + 16-byte `ObjectHeader`) up to [`MAX_LARGE_TAGGED_WORDS`].
    ///
    /// # Safety
    /// Same mutator-operation, initialization, and root-publication obligations as
    /// [`Heap::alloc_object`].
    pub unsafe fn alloc_tagged_words(
        &mut self,
        type_index: TypeIndex,
        payload_words: usize,
    ) -> Option<NonNull<u8>> {
        if let Ok(small_words) = u16::try_from(payload_words) {
            // SAFETY: Caller supplies mutator operation and root obligations.
            return unsafe { self.alloc_object(type_index, small_words) };
        }
        if payload_words > MAX_LARGE_TAGGED_WORDS {
            return None;
        }
        let payload_bytes = payload_words.checked_mul(8)?;
        let header_offset = LARGE_TAGGED_PREFIX_BYTES + ObjectHeader::SIZE;
        let total_size = header_offset.checked_add(payload_bytes)?;
        let layout = Layout::from_size_align(total_size, 8).ok()?;
        // SAFETY: `total_size >= 24 > 0` and alignment is 8.
        let block_ptr = unsafe { alloc_zeroed(layout) };
        if block_ptr.is_null() {
            return None;
        }
        let header = ObjectHeader::new_large_tagged(type_index);
        // SAFETY: `block_ptr` points to `total_size` zeroed, 8-byte aligned bytes:
        // `[0..8]` = `u64` word count, `[8..24]` = `ObjectHeader`, `[24..]` = zeroed slots.
        let payload_ptr = unsafe {
            block_ptr.cast::<u64>().write(payload_words as u64);
            block_ptr
                .add(LARGE_TAGGED_PREFIX_BYTES)
                .cast::<ObjectHeader>()
                .write(header);
            block_ptr.add(header_offset)
        };
        let m = &mut *self.mutator;
        m.local_objects.push(payload_ptr as usize);
        m.local_bytes = m.local_bytes.saturating_add(total_size);
        if m.local_bytes >= LOCAL_FLUSH_BYTES {
            flush_mutator_batch(m);
        }

        let prev = *self.alloc_count.get_mut();
        *self.alloc_count.get_mut() = prev.wrapping_add(1);
        NonNull::new(payload_ptr)
    }

    /// Allocate a 3-word [`PayloadRole::ExternalRc`] thin shell bound to `handle`
    /// over `offset..(offset + len)`.
    ///
    /// # Safety
    /// Caller must hold an active mutator operation and publish the returned pointer
    /// into a registered root before another allocation or safepoint.
    pub unsafe fn alloc_external_rc(
        &mut self,
        type_index: TypeIndex,
        handle: ExternalRcHandle,
        offset: usize,
        len: usize,
    ) -> Option<NonNull<u8>> {
        if offset.checked_add(len)? > handle.capacity() {
            return None;
        }
        // SAFETY: Caller holds an active mutator operation; `handle` is not yet registered,
        // so any collection scheduled during allocation cannot sweep it prematurely.
        let ptr = unsafe {
            self.alloc_with_role(type_index, EXTERNAL_RC_SHELL_WORDS, PayloadRole::ExternalRc)?
        };
        let words = ptr.as_ptr().cast::<u64>();
        // SAFETY: `ptr` owns 3 aligned `u64` words under exclusive mutator ownership;
        // `register_shell` writes word 0 (`raw_handle`) and registers the `WeakObject`.
        unsafe {
            words.add(1).write(offset as u64);
            words.add(2).write(len as u64);
            external_rc::register_shell(ptr.as_ptr(), handle);
        }
        Some(ptr)
    }

    unsafe fn alloc_raw_object(
        &mut self,
        header: ObjectHeader,
        payload_bytes: usize,
    ) -> Option<NonNull<u8>> {
        let total_size = ObjectHeader::SIZE.checked_add(payload_bytes)?;
        let layout = Layout::from_size_align(total_size, 8).ok()?;
        // SAFETY: `total_size >= ObjectHeader::SIZE == 16 > 0` and alignment is 8.
        let block_ptr = unsafe { alloc_zeroed(layout) };
        if block_ptr.is_null() {
            return None;
        }
        // SAFETY: `block_ptr` points to `total_size` zeroed, 8-byte aligned bytes.
        let payload_ptr = unsafe {
            block_ptr.cast::<ObjectHeader>().write(header);
            block_ptr.add(ObjectHeader::SIZE)
        };
        let m = &mut *self.mutator;
        m.local_objects.push(payload_ptr as usize);
        m.local_bytes = m.local_bytes.saturating_add(total_size);
        if m.local_bytes >= LOCAL_FLUSH_BYTES {
            flush_mutator_batch(m);
        }

        let prev = *self.alloc_count.get_mut();
        *self.alloc_count.get_mut() = prev.wrapping_add(1);
        NonNull::new(payload_ptr)
    }

    pub fn should_yield(&self) -> bool {
        GC_FLAGS.gc_requested.load(Ordering::Relaxed)
    }

    pub fn safe_point(&self) {
        if self.should_yield() {
            mutators::poll(self.tls);
        }
        if mutators::can_initiate_gc(self.tls) && external_rc::take_gc_needed() {
            // SAFETY: `can_initiate_gc` verifies a top-level active `begin_execution` session
            // with all live references published to roots at this safepoint.
            unsafe {
                let _ = self.collect_garbage();
            }
        }
    }

    pub fn alloc_count(&self) -> usize {
        self.alloc_count.load(Ordering::Relaxed)
    }

    /// # Safety
    /// `payload_ptr` must originate from `Heap::alloc_object` / `alloc_byte_buffer`.
    pub unsafe fn header_of(payload_ptr: NonNull<u8>) -> &'static mut ObjectHeader {
        // SAFETY: Caller guarantees valid managed payload pointer.
        unsafe { ObjectHeader::from_payload_ptr_mut(payload_ptr.as_ptr()) }
    }
}

fn flush_mutator_batch(m: &mut WinMutator) {
    if m.local_objects.is_empty() {
        m.local_bytes = 0;
        return;
    }
    let col = collector(64 * 1024 * 1024);
    let live_approx = col.live_bytes_approx.load(Ordering::Relaxed);
    let mut guard = col.state.lock().unwrap();
    guard.flushed_objects.append(&mut m.local_objects);
    guard.flushed_bytes = guard.flushed_bytes.saturating_add(m.local_bytes);
    m.local_bytes = 0;
    if live_approx.saturating_add(guard.flushed_bytes) >= guard.next_gc_threshold
        && guard.requested_ticket == guard.completed_ticket
    {
        guard.requested_ticket += 1;
        GC_FLAGS.gc_requested.store(true, Ordering::Release);
        col.worker_cv.notify_one();
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        let lifecycle = mutators::lifecycle();
        flush_mutator_batch(&mut self.mutator);
        lifecycle.unregister(self.tls);
    }
}

pub fn gc_init(config: &GcConfig) -> Heap {
    let col = collector(config.heap_size);
    if col.live_bytes_approx.load(Ordering::Relaxed) == 0 {
        external_rc::init_pressure_threshold(config.heap_size);
    }
    let tls_identity = Box::new(0u8);
    let addr = (&*tls_identity as *const u8) as usize;
    let tls = VMMutatorThread(VMThread(OpaquePointer::from_address(addr)));
    let lifecycle = mutators::lifecycle();
    let mut mutator = Box::new(WinMutator::new());
    lifecycle.register(tls, &mut *mutator);
    Heap {
        mutator,
        tls,
        alloc_count: AtomicUsize::new(0),
        _tls_identity: tls_identity,
    }
}
