//! Nessa GC — MMTk-based garbage collector.
//!
//! Integrates the MMTk framework (Memory Management Toolkit) and exposes a
//! `Heap` API for the interpreter and runtime.
//!
//! ## Object layout
//!
//! Every managed object has a 16-byte `ObjectHeader` followed by N×8-byte
//! `TaggedValue` payload words.  The `ObjectReference` used by MMTk points
//! to the *payload* start (after the header), matching Nessa's heap pointer
//! convention.

use type_pool::{Intrinsic, TypeIndex};

use mmtk::util::alloc::AllocationError;
use mmtk::util::copy::*;
use mmtk::util::opaque_pointer::*;
use mmtk::util::{Address, ObjectReference};
use mmtk::vm::slot::Slot;
use mmtk::vm::*;
use mmtk::{AllocationSemantics, MMTK, MMTKBuilder, Mutator};

use std::ptr::NonNull;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

mod mutators;
pub use mutators::{MutatorAccessError, MutatorSession};
mod roots;
pub use roots::{ConditionalRootCallbacks, RootRegistration, RootScanCallback};
mod weak;
pub use weak::WeakObject;

// ---------------------------------------------------------------------------
// ObjectHeader — 16 bytes before every heap object
// ---------------------------------------------------------------------------

/// VM-selected payload interpretation. Only checked allocators may publish
/// non-default roles; a role is never supplied by source or archive data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum PayloadRole {
    Ordinary = 0,
    ErrorEnvelope = 1,
}

/// Header stored at the beginning of every GC-managed heap object (16 bytes).
///
/// ```text
///   [TypeIndex: u32][gc_meta: u32][hash_or_fwd: u64]
/// ```
///
/// `gc_meta` bit layout:
///   - bits 0-2:   reserved for MMTk (mark bit, forwarding bits)
///   - bits 3-18:  payload word count (up to 65535 words)
///   - bits 19-21: VM payload role (0 ordinary, 1 Error envelope)
///   - bits 22-30: reserved
///   - bit  31:    pinned flag
///
/// `hash_or_fwd`:  identity hash (normal) / forwarding pointer (during GC).
#[repr(C)]
pub struct ObjectHeader {
    pub type_index: TypeIndex,
    pub gc_meta: u32,
    pub hash_or_fwd: u64,
}

impl ObjectHeader {
    pub const SIZE: usize = std::mem::size_of::<Self>();

    pub fn new(type_index: TypeIndex, payload_words: u16) -> Self {
        static HASH_COUNTER: AtomicU64 = AtomicU64::new(1);
        Self {
            type_index,
            gc_meta: (payload_words as u32) << 3,
            hash_or_fwd: HASH_COUNTER.fetch_add(1, Ordering::Relaxed),
        }
    }

    pub fn payload_role(&self) -> Option<PayloadRole> {
        match (self.gc_meta >> 19) & 7 {
            0 => Some(PayloadRole::Ordinary),
            1 => Some(PayloadRole::ErrorEnvelope),
            _ => None,
        }
    }

    pub fn is_ordinary(&self) -> bool {
        self.payload_role() == Some(PayloadRole::Ordinary)
    }

    pub fn payload_words(&self) -> usize {
        ((self.gc_meta >> 3) & 0xFFFF) as usize
    }

    pub fn object_size(&self) -> usize {
        Self::SIZE + self.payload_words() * 8
    }

    pub fn is_pinned(&self) -> bool {
        self.gc_meta & (1 << 31) != 0
    }

    pub fn set_pinned(&mut self, pinned: bool) {
        if pinned {
            self.gc_meta |= 1 << 31;
        } else {
            self.gc_meta &= !(1 << 31);
        }
    }

    /// # Safety
    /// `payload_ptr` must point to a valid payload from `Heap::alloc_object`.
    pub unsafe fn from_payload_ptr(payload_ptr: *const u8) -> &'static ObjectHeader {
        unsafe { &*(payload_ptr.sub(Self::SIZE) as *const ObjectHeader) }
    }

    /// # Safety
    /// Same as `from_payload_ptr`.
    pub unsafe fn from_payload_ptr_mut(payload_ptr: *mut u8) -> &'static mut ObjectHeader {
        unsafe { &mut *(payload_ptr.sub(Self::SIZE) as *mut ObjectHeader) }
    }
}

// ---------------------------------------------------------------------------
// NessaVM — VMBinding marker
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct NessaVM;

impl mmtk::vm::VMBinding for NessaVM {
    type VMObjectModel = NessaObjectModel;
    type VMScanning = NessaScanning;
    type VMCollection = NessaCollection;
    type VMActivePlan = NessaActivePlan;
    type VMReferenceGlue = NessaReferenceGlue;
    type VMSlot = NessaSlot;
    type VMMemorySlice = NessaMemorySlice;

    const MIN_ALIGNMENT: usize = 8;
    const MAX_ALIGNMENT: usize = 8;
    const USE_ALLOCATION_OFFSET: bool = false;
    const ALLOC_END_ALIGNMENT: usize = 8;
}

// ---------------------------------------------------------------------------
// NessaSlot — pointer to a TaggedValue that may hold a heap reference
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NessaSlot {
    addr: Address,
    pinning_root: bool,
}

impl NessaSlot {
    pub fn from_address(addr: Address) -> Self {
        Self {
            addr,
            pinning_root: false,
        }
    }

    /// Create a slot from a raw pointer (convenience for root scanning).
    ///
    /// # Safety
    /// The pointer must address a writable, initialized, aligned u64 slot whose
    /// address remains stable until collection completes. Collection must have
    /// exclusive access under the stop-the-world barrier, with no conflicting borrows.
    pub unsafe fn from_raw_ptr(ptr: *const u64) -> Self {
        Self {
            addr: unsafe { Address::from_usize(ptr as usize) },
            pinning_root: false,
        }
    }

    /// Mark a native copied-value root whose object address cannot be updated.
    /// Managed heap edges and ordinary VM roots must remain movable slots.
    ///
    /// # Safety
    /// The pointer is an aligned, initialized slot retained until collection ends.
    /// Its owner keeps every native copy alive only while this pinning root exists.
    pub unsafe fn from_pinning_root(ptr: *const u64) -> Self {
        Self {
            addr: unsafe { Address::from_usize(ptr as usize) },
            pinning_root: true,
        }
    }
}

// SAFETY: collector work owns access to these stable slots while all mutators
// are stopped; sending the slot does not permit unsynchronized mutator access.
unsafe impl Send for NessaSlot {}

impl Slot for NessaSlot {
    fn load(&self) -> Option<ObjectReference> {
        let raw: usize = unsafe { self.addr.load::<usize>() };
        // tag == 000 && non-null → heap pointer
        if (raw & 0b111) == 0 && raw != 0 {
            ObjectReference::from_raw_address(unsafe { Address::from_usize(raw) })
        } else {
            None
        }
    }

    fn store(&self, object: ObjectReference) {
        unsafe { self.addr.store::<usize>(object.to_raw_address().as_usize()) };
    }
}

// ---------------------------------------------------------------------------
// NessaMemorySlice
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct NessaMemorySlice {
    start: Address,
    len_bytes: usize,
}

pub struct NessaSlotIterator {
    cursor: Address,
    limit: Address,
}

impl Iterator for NessaSlotIterator {
    type Item = NessaSlot;
    fn next(&mut self) -> Option<Self::Item> {
        if self.cursor >= self.limit {
            return None;
        }
        let slot = NessaSlot::from_address(self.cursor);
        self.cursor += 8usize;
        Some(slot)
    }
}

impl mmtk::vm::slot::MemorySlice for NessaMemorySlice {
    type SlotType = NessaSlot;
    type SlotIterator = NessaSlotIterator;

    fn iter_slots(&self) -> Self::SlotIterator {
        NessaSlotIterator {
            cursor: self.start,
            limit: self.start + self.len_bytes,
        }
    }
    fn object(&self) -> Option<ObjectReference> {
        None
    }
    fn start(&self) -> Address {
        self.start
    }
    fn bytes(&self) -> usize {
        self.len_bytes
    }
    fn copy(src: &Self, tgt: &Self) {
        debug_assert_eq!(src.len_bytes, tgt.len_bytes);
        unsafe {
            std::ptr::copy_nonoverlapping(
                src.start.to_ptr::<u8>(),
                tgt.start.to_mut_ptr::<u8>(),
                src.len_bytes,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// NessaObjectModel
// ---------------------------------------------------------------------------

pub struct NessaObjectModel;

impl ObjectModel<NessaVM> for NessaObjectModel {
    const GLOBAL_LOG_BIT_SPEC: VMGlobalLogBitSpec = VMGlobalLogBitSpec::side_first();
    const LOCAL_FORWARDING_POINTER_SPEC: VMLocalForwardingPointerSpec =
        VMLocalForwardingPointerSpec::in_header(64); // hash_or_fwd at byte 8
    const LOCAL_FORWARDING_BITS_SPEC: VMLocalForwardingBitsSpec =
        VMLocalForwardingBitsSpec::in_header(33); // gc_meta bits 1-2
    // MMTk 0.32 Immix does not implement cyclic in-header mark bits. Keep mark
    // metadata on the side and lay out LOS metadata after it without overlap.
    const LOCAL_MARK_BIT_SPEC: VMLocalMarkBitSpec = VMLocalMarkBitSpec::side_first();
    const LOCAL_LOS_MARK_NURSERY_SPEC: VMLocalLOSMarkNurserySpec =
        VMLocalLOSMarkNurserySpec::side_after(Self::LOCAL_MARK_BIT_SPEC.as_spec());

    const OBJECT_REF_OFFSET_LOWER_BOUND: isize = ObjectHeader::SIZE as isize;
    const UNIFIED_OBJECT_REFERENCE_ADDRESS: bool = false;

    fn copy(
        from: ObjectReference,
        semantics: CopySemantics,
        copy_context: &mut GCWorkerCopyContext<NessaVM>,
    ) -> ObjectReference {
        let obj_start = Self::ref_to_object_start(from);
        let header: &ObjectHeader = unsafe { &*(obj_start.to_ptr::<ObjectHeader>()) };
        let total_size = header.object_size();

        let dst = copy_context.alloc_copy(from, total_size, 8, 0, semantics);
        unsafe {
            std::ptr::copy_nonoverlapping(
                obj_start.to_ptr::<u8>(),
                dst.to_mut_ptr::<u8>(),
                total_size,
            );
        }
        let new_objref =
            unsafe { ObjectReference::from_raw_address_unchecked(dst + ObjectHeader::SIZE) };
        copy_context.post_copy(new_objref, total_size, semantics);
        new_objref
    }

    fn copy_to(from: ObjectReference, to: ObjectReference, _region: Address) -> Address {
        let size = Self::get_current_size(from);
        let src = Self::ref_to_object_start(from);
        let dst = Self::ref_to_object_start(to);
        unsafe {
            std::ptr::copy_nonoverlapping(src.to_ptr::<u8>(), dst.to_mut_ptr::<u8>(), size);
        }
        dst + size
    }

    fn get_reference_when_copied_to(_from: ObjectReference, to: Address) -> ObjectReference {
        unsafe { ObjectReference::from_raw_address_unchecked(to + ObjectHeader::SIZE) }
    }

    fn get_current_size(object: ObjectReference) -> usize {
        let obj_start = Self::ref_to_object_start(object);
        let header: &ObjectHeader = unsafe { &*(obj_start.to_ptr::<ObjectHeader>()) };
        header.object_size()
    }

    fn get_size_when_copied(object: ObjectReference) -> usize {
        Self::get_current_size(object)
    }

    fn get_align_when_copied(_object: ObjectReference) -> usize {
        8
    }

    fn get_align_offset_when_copied(_object: ObjectReference) -> usize {
        0
    }

    fn get_type_descriptor(_reference: ObjectReference) -> &'static [i8] {
        &[]
    }

    fn ref_to_object_start(object: ObjectReference) -> Address {
        object.to_raw_address() - ObjectHeader::SIZE
    }

    fn ref_to_header(object: ObjectReference) -> Address {
        object.to_raw_address() - ObjectHeader::SIZE
    }

    fn dump_object(object: ObjectReference) {
        let hdr: &ObjectHeader =
            unsafe { &*(Self::ref_to_object_start(object).to_ptr::<ObjectHeader>()) };
        eprintln!(
            "NessaObj@{}: type={} words={} hash={}",
            object.to_raw_address(),
            hdr.type_index.as_u32(),
            hdr.payload_words(),
            hdr.hash_or_fwd,
        );
    }
}

// ---------------------------------------------------------------------------
// NessaScanning — root / object scanning
// ---------------------------------------------------------------------------

pub struct NessaScanning;

impl mmtk::vm::Scanning<NessaVM> for NessaScanning {
    fn scan_object<SV: SlotVisitor<NessaSlot>>(
        _tls: VMWorkerThread,
        object: ObjectReference,
        slot_visitor: &mut SV,
    ) {
        let obj_start = NessaObjectModel::ref_to_object_start(object);
        // SAFETY: the collector supplies a live object with its initialized
        // aligned ObjectHeader immediately before the payload.
        let header: &ObjectHeader = unsafe { &*(obj_start.to_ptr::<ObjectHeader>()) };
        let payload_start = object.to_raw_address();
        if header.payload_role() == Some(PayloadRole::ErrorEnvelope) {
            // SAFETY: only the checked role allocator publishes this exact
            // layout. The two full-ID words are scalars, never reference slots.
            assert_eq!(
                header.payload_words(),
                3,
                "invalid Error envelope allocation"
            );
            slot_visitor.visit_slot(NessaSlot::from_address(payload_start + 16usize));
            return;
        }
        assert!(header.is_ordinary(), "invalid VM payload role");
        let first_reference_word = if header.type_index == Intrinsic::Str.type_index()
            || header.type_index == Intrinsic::Continuation.type_index()
            || Intrinsic::ALL
                .iter()
                .any(|kind| kind.is_numeric() && header.type_index == kind.type_index())
        {
            // Numeric words, string bytes and continuation handles are scalar
            // payloads, even when their low bits resemble aligned pointers.
            return;
        } else if header.type_index == Intrinsic::Closure.type_index() {
            // ClosureEnv's raw function ID and capture count precede its values.
            2
        } else {
            0
        };
        for i in first_reference_word..header.payload_words() {
            slot_visitor.visit_slot(NessaSlot::from_address(payload_start + i * 8));
        }
    }

    fn notify_initial_thread_scan_complete(_partial_scan: bool, _tls: VMWorkerThread) {}

    fn scan_roots_in_mutator_thread(
        _tls: VMWorkerThread,
        _mutator: &'static mut Mutator<NessaVM>,
        _factory: impl RootsWorkFactory<NessaSlot>,
    ) {
        // Handled in scan_vm_specific_roots.
    }

    fn scan_vm_specific_roots(_tls: VMWorkerThread, mut factory: impl RootsWorkFactory<NessaSlot>) {
        let mut slots = Vec::new();
        let mut pinned = Vec::new();
        // SAFETY: root owners expose stable writable slots exclusively while
        // mutators are stopped. Native copied references alone require pinning.
        unsafe {
            roots::scan_roots(&mut |slot| {
                if let Some(object) = slot.load() {
                    if slot.pinning_root {
                        pinned.push(object);
                    } else {
                        slots.push(slot);
                    }
                }
            });
        }
        // MMTk processes pinning roots before ordinary closure tracing.
        if !pinned.is_empty() {
            factory.create_process_pinning_roots_work(pinned);
        }
        if !slots.is_empty() {
            factory.create_process_roots_work(slots);
        }
    }

    fn supports_return_barrier() -> bool {
        false
    }

    fn prepare_for_roots_re_scanning() {}

    fn process_weak_refs(
        worker: &mut mmtk::scheduler::GCWorker<NessaVM>,
        tracer_context: impl ObjectTracerContext<NessaVM>,
    ) -> bool {
        let changed = tracer_context.with_tracer(worker, |tracer| {
            // SAFETY: weak processing follows the completed closure under STW.
            // trace_object updates slots synchronously and queues heap scanning.
            unsafe {
                roots::trace_conditional_roots(&mut |slot| {
                    if let Some(object) = slot.load() {
                        slot.store(tracer.trace_object(object));
                    }
                })
            }
        });
        if !changed {
            // SAFETY: a pass exposed no new edges; all previously queued work
            // completed before this pass, so unreachable owners cannot revive.
            unsafe { roots::sweep_conditional_roots() };
        }
        changed
    }

    fn forward_weak_refs(
        worker: &mut mmtk::scheduler::GCWorker<NessaVM>,
        tracer_context: impl ObjectTracerContext<NessaVM>,
    ) {
        tracer_context.with_tracer(worker, |tracer| {
            // SAFETY: two-phase collectors forward only retained roots under STW.
            unsafe {
                roots::forward_conditional_roots(&mut |slot| {
                    if let Some(object) = slot.load() {
                        slot.store(tracer.trace_object(object));
                    }
                });
            }
        });
    }
}

// ---------------------------------------------------------------------------
// NessaCollection — stop-the-world coordination
// ---------------------------------------------------------------------------

pub struct NessaCollection;

impl mmtk::vm::Collection<NessaVM> for NessaCollection {
    fn stop_all_mutators<F>(_tls: VMWorkerThread, mut mutator_visitor: F)
    where
        F: FnMut(&'static mut Mutator<NessaVM>),
    {
        let stopped = mutators::stop_all();
        // SAFETY: the barrier stopped operations before any root work starts.
        unsafe { roots::prepare_conditional_roots() };
        for pointer in stopped {
            // SAFETY: collection owns mutator access during the STW barrier.
            // Registry entries remain owned by their heaps until unregister.
            mutator_visitor(unsafe { &mut *pointer });
        }
    }

    fn resume_mutators(_tls: VMWorkerThread) {
        mutators::resume();
    }

    fn block_for_gc(tls: VMMutatorThread) {
        mutators::block(tls);
    }

    fn spawn_gc_thread(_tls: VMThread, ctx: GCThreadContext<NessaVM>) {
        match ctx {
            GCThreadContext::Worker(worker) => {
                let mmtk = mmtk_ref();
                std::thread::Builder::new()
                    .name("nessa-gc-worker".into())
                    .spawn(move || {
                        let identity = Box::new(0u8);
                        // SAFETY: this nonzero-size token remains at its stable
                        // address until worker.run returns. MMTk never dereferences
                        // the opaque identity, but requires an initialized TLS.
                        let address =
                            unsafe { Address::from_usize((&*identity as *const u8) as usize) };
                        let tls = VMWorkerThread(VMThread(OpaquePointer::from_address(address)));
                        worker.run(tls, mmtk);
                        drop(identity);
                    })
                    .expect("failed to spawn GC worker thread");
            }
        }
    }

    fn out_of_memory(_tls: VMThread, err_kind: AllocationError) {
        panic!("Nessa: out of memory ({err_kind:?})");
    }

    fn is_collection_enabled() -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// NessaActivePlan — live mutator management
// ---------------------------------------------------------------------------

pub struct NessaActivePlan;

impl ActivePlan<NessaVM> for NessaActivePlan {
    fn is_mutator(tls: VMThread) -> bool {
        mutators::contains(tls)
    }

    fn mutator(tls: VMMutatorThread) -> &'static mut Mutator<NessaVM> {
        // SAFETY: MMTk calls this for a registered live mutator, with access
        // coordinated by its mutator thread or the collection barrier.
        unsafe { &mut *mutators::lookup(tls) }
    }

    fn mutators<'a>() -> Box<dyn Iterator<Item = &'a mut Mutator<NessaVM>> + 'a> {
        // SAFETY: MMTk enumerates mutators under the collection barrier. Each
        // registry identity owns a distinct Box, so references do not alias.
        Box::new(
            mutators::pointers()
                .into_iter()
                .map(|ptr| unsafe { &mut *ptr }),
        )
    }

    fn number_of_mutators() -> usize {
        mutators::count()
    }
}

// ---------------------------------------------------------------------------
// NessaReferenceGlue — weak reference handling (stub)
// ---------------------------------------------------------------------------

pub struct NessaReferenceGlue;

impl ReferenceGlue<NessaVM> for NessaReferenceGlue {
    type FinalizableType = ObjectReference;
    fn clear_referent(_new_reference: ObjectReference) {}
    fn get_referent(_object: ObjectReference) -> Option<ObjectReference> {
        None
    }
    fn set_referent(_reff: ObjectReference, _referent: ObjectReference) {}
    fn enqueue_references(_references: &[ObjectReference], _tls: VMWorkerThread) {}
}

// ---------------------------------------------------------------------------
// GcConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct GcConfig {
    pub heap_size: usize,
    pub plan: String,
    pub gc_threads: usize,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            heap_size: 64 * 1024 * 1024,
            plan: "Immix".into(),
            gc_threads: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// GcFlags — safe-point coordination flags
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct GcFlags {
    pub gc_requested: AtomicBool,
    pub preempt_requested: AtomicBool,
}

impl GcFlags {
    pub const fn new() -> Self {
        Self {
            gc_requested: AtomicBool::new(false),
            preempt_requested: AtomicBool::new(false),
        }
    }

    pub fn should_yield(&self) -> bool {
        self.gc_requested.load(Ordering::Relaxed) || self.preempt_requested.load(Ordering::Relaxed)
    }
}

pub static GC_FLAGS: GcFlags = GcFlags::new();

// ---------------------------------------------------------------------------
// Global MMTk instance
// ---------------------------------------------------------------------------

static MMTK_STATIC: OnceLock<&'static MMTK<NessaVM>> = OnceLock::new();
static COLLECTION_INITIALIZED: std::sync::Once = std::sync::Once::new();

fn mmtk_ref() -> &'static MMTK<NessaVM> {
    MMTK_STATIC.get().copied().expect("MMTk not initialized")
}

const LARGE_OBJECT_THRESHOLD: usize = 8192;

// ---------------------------------------------------------------------------
// Heap — MMTk-backed allocator (replaces the old bump-pointer Heap)
// ---------------------------------------------------------------------------

/// The managed heap backed by MMTk.
pub struct Heap {
    mutator: Box<Mutator<NessaVM>>,
    tls: VMMutatorThread,
    alloc_count: AtomicUsize,
    // A stable, distinct TLS identity even when Heap moves between threads.
    _tls_identity: Box<u8>,
}

// SAFETY: moving Heap transfers exclusive ownership of its mutator and TLS
// allocation. Moving does not change their addresses; allocation requires &mut
// Heap. Collector access still requires the stop-the-world protocol.
unsafe impl Send for Heap {}

impl Heap {
    /// Number of completed stop-the-world epochs in the process-wide collector.
    pub fn completed_collections(&self) -> u64 {
        mutators::completed_collections()
    }

    /// # Safety
    /// Hold a mutator operation with all live references published to roots.
    pub unsafe fn collect_garbage(&self) -> bool {
        mmtk_ref().handle_user_collection_request(self.tls, true, true)
    }

    /// Enter an operation that can mutate or read managed roots.
    ///
    /// # Safety
    /// Heap must outlive the returned guard. Calls for one Heap must be
    /// exclusive to the entering thread. Publish all newly allocated objects
    /// into registered roots before the outermost guard is dropped.
    /// Use begin_execution for operations that can allocate, poll or collect;
    /// a nested operation on another Heap must remain short and non-collecting.
    pub unsafe fn begin_operation(&self) -> MutatorSession<'static> {
        mutators::begin(self.tls)
    }

    /// Enter an operation that can allocate, poll or collect.
    ///
    /// # Safety
    /// Same lifetime, ownership and publication requirements as begin_operation.
    pub unsafe fn begin_execution(&self) -> Result<MutatorSession<'static>, MutatorAccessError> {
        mutators::begin_execution(self.tls)
    }

    /// Allocate a Nessa object with the given type and payload word count.
    ///
    /// Returns a pointer to the *payload* (after the 16-byte header).
    /// # Safety
    /// Hold a mutator operation through initialization and publication. Any
    /// live references that cross allocation must already be registered roots.
    /// Before another allocation or operation exit, root the returned object or
    /// discard it. Heap and mutator must be exclusively owned by this thread.
    /// No other Heap may have a running operation on the same thread.
    pub unsafe fn alloc_object(
        &mut self,
        type_index: TypeIndex,
        payload_words: u16,
    ) -> Option<NonNull<u8>> {
        // SAFETY: the public caller supplies the initialization/root obligations.
        unsafe { self.alloc_with_role(type_index, payload_words, PayloadRole::Ordinary) }
    }

    unsafe fn alloc_with_role(
        &mut self,
        type_index: TypeIndex,
        payload_words: u16,
        role: PayloadRole,
    ) -> Option<NonNull<u8>> {
        let payload_bytes = payload_words as usize * 8;
        let total_size = ObjectHeader::SIZE + payload_bytes;
        let semantics = if total_size > LARGE_OBJECT_THRESHOLD {
            AllocationSemantics::Los
        } else {
            AllocationSemantics::Default
        };

        let mutator = &mut *self.mutator;
        let addr = mmtk::memory_manager::alloc(mutator, total_size, 8, 0, semantics);
        if addr.is_zero() {
            return None;
        }

        let mut header = ObjectHeader::new(type_index, payload_words);
        header.gc_meta |= (role as u32) << 19;
        unsafe { std::ptr::write(addr.to_mut_ptr::<ObjectHeader>(), header) };

        let payload_addr = addr + ObjectHeader::SIZE;
        // Zero-initialize payload.
        unsafe {
            std::ptr::write_bytes(payload_addr.to_mut_ptr::<u8>(), 0, payload_bytes);
        }
        let objref = unsafe { ObjectReference::from_raw_address_unchecked(payload_addr) };
        mmtk::memory_manager::post_alloc(mutator, objref, total_size, semantics);
        self.alloc_count.fetch_add(1, Ordering::Relaxed);

        NonNull::new(payload_addr.to_mut_ptr::<u8>())
    }

    /// Allocate the VM's three-word Error envelope, assigning its scan role
    /// before returning for initialization and publication.
    ///
    /// # Safety
    /// The caller holds a mutator operation and validates a concrete qualified
    /// descriptor. Initialize both scalar ID words and the payload value, then
    /// root/publish before another allocation or operation exit. All inputs
    /// crossing this allocation must already be registered updateable roots.
    pub unsafe fn alloc_error_envelope(&mut self, type_index: TypeIndex) -> Option<NonNull<u8>> {
        // SAFETY: the caller supplies initialization/publication obligations;
        // the role is assigned in the header before MMTk post_alloc.
        unsafe { self.alloc_with_role(type_index, 3, PayloadRole::ErrorEnvelope) }
    }

    /// Check if a GC is requested.
    pub fn should_yield(&self) -> bool {
        GC_FLAGS.gc_requested.load(Ordering::Relaxed)
    }

    /// Called at safe-points.  Blocks if a GC is pending.
    pub fn safe_point(&self) {
        if self.should_yield() {
            mutators::poll(self.tls);
        }
    }

    pub fn alloc_count(&self) -> usize {
        self.alloc_count.load(Ordering::Relaxed)
    }

    /// Read the `ObjectHeader` for a given payload pointer.
    ///
    /// # Safety
    /// The pointer must originate from `alloc_object`.
    pub unsafe fn header_of(payload_ptr: NonNull<u8>) -> &'static mut ObjectHeader {
        unsafe { ObjectHeader::from_payload_ptr_mut(payload_ptr.as_ptr()) }
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        let lifecycle = mutators::lifecycle();
        lifecycle.unregister(self.tls);
        // MMTk flushes allocator state but leaves allocation ownership to the
        // binding. Heap's Box then reclaims it before the TLS identity is freed.
        mmtk::memory_manager::destroy_mutator(&mut self.mutator);
    }
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

/// Initialize the MMTk GC and return a ready-to-use `Heap`.
///
/// The MMTk runtime is initialized exactly once (on first call).
/// Subsequent calls reuse the existing runtime and bind a fresh mutator.
pub fn gc_init(config: &GcConfig) -> Heap {
    // Phase 1: create and store the MMTK instance (once).
    let mmtk_static = *MMTK_STATIC.get_or_init(|| {
        let mut builder = MMTKBuilder::new();
        let _ = builder.set_option("plan", &config.plan);

        let gc_threads = if config.gc_threads == 0 {
            std::thread::available_parallelism()
                .map(|n| n.get().max(1))
                .unwrap_or(1)
        } else {
            config.gc_threads
        };
        let _ = builder.set_option("threads", &gc_threads.to_string());
        let _ = builder.set_option("gc_trigger", &format!("FixedHeapSize:{}", config.heap_size));

        let mmtk_box = mmtk::memory_manager::mmtk_init::<NessaVM>(&builder);
        Box::leak(mmtk_box)
    });

    // Phase 2: spawn GC worker threads (once). This must happen after
    // MMTK_STATIC is set, because spawn_gc_thread calls mmtk_ref().
    COLLECTION_INITIALIZED.call_once(|| {
        mmtk::memory_manager::initialize_collection(
            mmtk_static,
            VMThread(OpaquePointer::UNINITIALIZED),
        );
    });

    let tls_identity = Box::new(0u8);
    // SAFETY: the identity allocation remains alive in Heap until after its
    // mutator is unregistered and destroyed. MMTk treats TLS as opaque.
    let address = unsafe { Address::from_usize((&*tls_identity as *const u8) as usize) };
    let tls = VMMutatorThread(VMThread(OpaquePointer::from_address(address)));
    let lifecycle = mutators::lifecycle();
    let mut mutator = mmtk::memory_manager::bind_mutator(mmtk_static, tls);
    lifecycle.register(tls, &mut *mutator);

    Heap {
        mutator,
        tls,
        alloc_count: AtomicUsize::new(0),
        _tls_identity: tls_identity,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_heaps_have_distinct_mutators_and_drop_independently() {
        let first = gc_init(&GcConfig::default());
        let mut second = gc_init(&GcConfig::default());
        let first_tls = first.tls;
        let second_tls = second.tls;
        assert_ne!(first_tls, second_tls);
        assert!(NessaActivePlan::is_mutator(first_tls.0));
        assert!(NessaActivePlan::is_mutator(second_tls.0));
        let first_pointer = &*first.mutator as *const Mutator<NessaVM>;
        let second_pointer = &*second.mutator as *const Mutator<NessaVM>;
        assert_eq!(mutators::lookup(first_tls).cast_const(), first_pointer);
        assert_eq!(mutators::lookup(second_tls).cast_const(), second_pointer);
        let pointers = mutators::pointers();
        assert!(pointers.contains(&(first_pointer as *mut _)));
        assert!(pointers.contains(&(second_pointer as *mut _)));

        drop(first);
        assert!(!NessaActivePlan::is_mutator(first_tls.0));
        assert!(NessaActivePlan::is_mutator(second_tls.0));
        assert_eq!(mutators::lookup(second_tls).cast_const(), second_pointer);
        // SAFETY: second remains alive and is exclusively used on this thread.
        let operation = unsafe { second.begin_operation() };
        // SAFETY: this operation reads then discards the object without any
        // subsequent allocation or publication of its unrooted pointer.
        let payload = unsafe { second.alloc_object(Intrinsic::Str.type_index(), 2) }.unwrap();
        // SAFETY: allocation is live and no collection is requested in this test.
        let header = unsafe { Heap::header_of(payload) };
        assert_eq!(header.type_index, Intrinsic::Str.type_index());
        assert_eq!(header.payload_words(), 2);
        drop(operation);

        // A collection snapshot must retain its mutators even if a moved Heap
        // is concurrently destroyed. This tests lifetime freezing, not STW.
        struct FrozenRegistry;
        impl Drop for FrozenRegistry {
            fn drop(&mut self) {
                mutators::resume();
            }
        }
        let _snapshot = mutators::stop_all();
        let frozen = FrozenRegistry;
        let (started_sender, started) = std::sync::mpsc::channel();
        let (finished_sender, finished) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            started_sender.send(()).unwrap();
            drop(second);
            finished_sender.send(()).unwrap();
        });
        started.recv().unwrap();
        assert!(NessaActivePlan::is_mutator(second_tls.0));
        assert!(finished.try_recv().is_err());
        drop(frozen);
        finished
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        thread.join().unwrap();
        assert!(!NessaActivePlan::is_mutator(second_tls.0));
    }

    #[test]
    fn object_header_layout() {
        assert_eq!(ObjectHeader::SIZE, 16);
        let hdr = ObjectHeader::new(TypeIndex::from_raw(42), 3);
        assert_eq!(hdr.payload_words(), 3);
        assert_eq!(hdr.object_size(), 16 + 24);
        assert!(!hdr.is_pinned());
    }

    #[test]
    fn object_header_pinned() {
        let mut hdr = ObjectHeader::new(TypeIndex::from_raw(1), 0);
        assert!(!hdr.is_pinned());
        hdr.set_pinned(true);
        assert!(hdr.is_pinned());
        assert_eq!(hdr.payload_words(), 0);
    }

    #[test]
    fn payload_size_survives_gc_metadata_changes_at_large_object_boundaries() {
        for words in [8191, 8192, u16::MAX] {
            let mut header = ObjectHeader::new(Intrinsic::Str.type_index(), words);
            header.set_pinned(true);
            header.gc_meta |= 0b111; // forwarding and reserved mark bits
            assert_eq!(header.payload_words(), words as usize);
            assert_eq!(
                header.object_size(),
                ObjectHeader::SIZE + words as usize * 8
            );
            header.set_pinned(false);
            assert_eq!(header.payload_words(), words as usize);
        }
    }

    #[test]
    fn gc_flags_default() {
        let flags = GcFlags::new();
        assert!(!flags.should_yield());
    }

    #[test]
    fn scanning_skips_scalar_layout_words_and_visits_closure_captures() {
        #[repr(C)]
        struct Fixture {
            header: ObjectHeader,
            payload: [u64; 4],
        }
        struct Visitor(Vec<NessaSlot>);
        impl SlotVisitor<NessaSlot> for Visitor {
            fn visit_slot(&mut self, slot: NessaSlot) {
                self.0.push(slot);
            }
        }

        for (kind, expected_words) in [
            (Intrinsic::Str.type_index(), Vec::new()),
            (Intrinsic::Continuation.type_index(), Vec::new()),
            (Intrinsic::I64.type_index(), Vec::new()),
            (Intrinsic::U128.type_index(), Vec::new()),
            (Intrinsic::I128.type_index(), Vec::new()),
            (Intrinsic::F64.type_index(), Vec::new()),
            (Intrinsic::Closure.type_index(), vec![2usize, 3]),
            (TypeIndex::from_raw(1000), vec![0usize, 1, 2, 3]),
        ] {
            let fixture = Fixture {
                header: ObjectHeader::new(kind, 4),
                payload: [8, 2, 16, 24],
            };
            // SAFETY: repr(C) places the four aligned words immediately after
            // the header. The stack allocation remains live during scanning.
            let address = unsafe { Address::from_usize(fixture.payload.as_ptr() as usize) };
            let object = ObjectReference::from_raw_address(address).unwrap();
            let mut visitor = Visitor(Vec::new());
            NessaScanning::scan_object(
                VMWorkerThread(VMThread::UNINITIALIZED),
                object,
                &mut visitor,
            );
            let expected: Vec<_> = expected_words
                .into_iter()
                .map(|index| NessaSlot::from_address(address + index * 8))
                .collect();
            assert_eq!(visitor.0, expected);
        }
    }
    #[test]
    fn error_role_scans_only_payload_and_survives_metadata_bits() {
        #[repr(C)]
        struct Fixture {
            header: ObjectHeader,
            payload: [u64; 3],
        }
        struct Visitor(Vec<NessaSlot>);
        impl SlotVisitor<NessaSlot> for Visitor {
            fn visit_slot(&mut self, slot: NessaSlot) {
                self.0.push(slot);
            }
        }
        let mut header = ObjectHeader::new(TypeIndex::from_raw(1000), 3);
        header.gc_meta |= (PayloadRole::ErrorEnvelope as u32) << 19;
        header.gc_meta |= 7;
        header.set_pinned(true);
        assert_eq!(header.payload_role(), Some(PayloadRole::ErrorEnvelope));
        assert_eq!(header.payload_words(), 3);
        header.set_pinned(false);
        let fixture = Fixture {
            header,
            payload: [0x1000, 0x2000, 0x3000],
        };
        // SAFETY: repr(C) fixture is live, aligned, initialized with a valid role.
        let address = unsafe { Address::from_usize(fixture.payload.as_ptr() as usize) };
        let object = ObjectReference::from_raw_address(address).unwrap();
        let mut visitor = Visitor(Vec::new());
        NessaScanning::scan_object(
            VMWorkerThread(VMThread::UNINITIALIZED),
            object,
            &mut visitor,
        );
        assert_eq!(visitor.0, vec![NessaSlot::from_address(address + 16usize)]);
        assert_eq!(fixture.payload, [0x1000, 0x2000, 0x3000]);
    }
}
