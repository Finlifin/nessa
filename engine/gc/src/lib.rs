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

use type_pool::TypeIndex;

use mmtk::util::alloc::AllocationError;
use mmtk::util::copy::*;
use mmtk::util::opaque_pointer::*;
use mmtk::util::{Address, ObjectReference};
use mmtk::vm::slot::Slot;
use mmtk::vm::*;
use mmtk::{AllocationSemantics, MMTK, MMTKBuilder, Mutator};

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};

// ---------------------------------------------------------------------------
// ObjectHeader — 16 bytes before every heap object
// ---------------------------------------------------------------------------

/// Header stored at the beginning of every GC-managed heap object (16 bytes).
///
/// ```text
///   [TypeIndex: u32][gc_meta: u32][hash_or_fwd: u64]
/// ```
///
/// `gc_meta` bit layout:
///   - bits 0-2:   reserved for MMTk (mark bit, forwarding bits)
///   - bits 3-15:  payload word count (up to 8191 words = 64 KB)
///   - bits 16-30: reserved
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
            gc_meta: ((payload_words as u32) & 0x1FFF) << 3,
            hash_or_fwd: HASH_COUNTER.fetch_add(1, Ordering::Relaxed),
        }
    }

    pub fn payload_words(&self) -> usize {
        ((self.gc_meta >> 3) & 0x1FFF) as usize
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
}

impl NessaSlot {
    pub fn from_address(addr: Address) -> Self {
        Self { addr }
    }

    /// Create a slot from a raw pointer (convenience for root scanning).
    ///
    /// # Safety
    /// The pointer must be valid and properly aligned.
    pub unsafe fn from_raw_ptr(ptr: *const u64) -> Self {
        Self {
            addr: unsafe { Address::from_usize(ptr as usize) },
        }
    }
}

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
    const LOCAL_MARK_BIT_SPEC: VMLocalMarkBitSpec = VMLocalMarkBitSpec::in_header(32); // gc_meta bit 0
    const LOCAL_LOS_MARK_NURSERY_SPEC: VMLocalLOSMarkNurserySpec =
        VMLocalLOSMarkNurserySpec::side_first();

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

/// Callback type: the runtime registers a function that enumerates all root
/// slots by calling a visitor for each.
pub type RootScanCallback = fn(&mut dyn FnMut(NessaSlot));

static ROOT_SCAN_CB: Mutex<Option<RootScanCallback>> = Mutex::new(None);

/// Register the root scanning callback (must be set before first GC).
pub fn register_root_scanner(cb: RootScanCallback) {
    *ROOT_SCAN_CB.lock().unwrap() = Some(cb);
}

/// Global pointer to the VM, set during engine initialization so the root
/// scanner can access all root sources (registers, globals, constants, stacks).
///
/// The VM pointer is stored as `*const ()` to avoid a circular dependency
/// (`gc` cannot depend on `interpreter`); the root scan callback (registered
/// from `interpreter`) casts it back to the concrete `Vm` type.
static VM_PTR: std::sync::atomic::AtomicPtr<()> =
    std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

/// Store the VM pointer for root scanning.
///
/// # Safety
/// The pointer must point to a valid `Vm` that outlives the GC.
pub unsafe fn set_vm_ptr(ptr: *const ()) {
    VM_PTR.store(ptr as *mut (), Ordering::Release);
}

/// Retrieve the stored VM pointer (returns null if not set).
pub fn get_vm_ptr() -> *const () {
    VM_PTR.load(Ordering::Acquire)
}

pub struct NessaScanning;

impl mmtk::vm::Scanning<NessaVM> for NessaScanning {
    fn scan_object<SV: SlotVisitor<NessaSlot>>(
        _tls: VMWorkerThread,
        object: ObjectReference,
        slot_visitor: &mut SV,
    ) {
        let obj_start = NessaObjectModel::ref_to_object_start(object);
        let header: &ObjectHeader = unsafe { &*(obj_start.to_ptr::<ObjectHeader>()) };
        let payload_start = object.to_raw_address();
        for i in 0..header.payload_words() {
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
        let cb = ROOT_SCAN_CB.lock().unwrap();
        if let Some(scan_fn) = *cb {
            let mut nodes = Vec::new();
            scan_fn(&mut |slot| {
                if let Some(objref) = slot.load() {
                    nodes.push(objref);
                }
            });
            if !nodes.is_empty() {
                factory.create_process_pinning_roots_work(nodes);
            }
        }
    }

    fn supports_return_barrier() -> bool {
        false
    }

    fn prepare_for_roots_re_scanning() {}

    fn process_weak_refs(
        _worker: &mut mmtk::scheduler::GCWorker<NessaVM>,
        _tracer_context: impl ObjectTracerContext<NessaVM>,
    ) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// NessaCollection — stop-the-world coordination
// ---------------------------------------------------------------------------

struct StwState {
    all_stopped: bool,
    should_resume: bool,
}

static STW: Mutex<StwState> = Mutex::new(StwState {
    all_stopped: false,
    should_resume: false,
});
static STW_CONDVAR: Condvar = Condvar::new();

pub struct NessaCollection;

impl mmtk::vm::Collection<NessaVM> for NessaCollection {
    fn stop_all_mutators<F>(_tls: VMWorkerThread, mut mutator_visitor: F)
    where
        F: FnMut(&'static mut Mutator<NessaVM>),
    {
        GC_FLAGS.gc_requested.store(true, Ordering::Release);

        let mut stw = STW.lock().unwrap();
        while !stw.all_stopped {
            stw = STW_CONDVAR.wait(stw).unwrap();
        }

        if let Some(m) = unsafe { current_mutator() } {
            mutator_visitor(m);
        }
    }

    fn resume_mutators(_tls: VMWorkerThread) {
        let mut stw = STW.lock().unwrap();
        stw.all_stopped = false;
        stw.should_resume = true;
        GC_FLAGS.gc_requested.store(false, Ordering::Release);
        STW_CONDVAR.notify_all();
    }

    fn block_for_gc(_tls: VMMutatorThread) {
        {
            let mut stw = STW.lock().unwrap();
            stw.all_stopped = true;
            stw.should_resume = false;
            STW_CONDVAR.notify_all();
        }
        let mut stw = STW.lock().unwrap();
        while !stw.should_resume {
            stw = STW_CONDVAR.wait(stw).unwrap();
        }
    }

    fn spawn_gc_thread(_tls: VMThread, ctx: GCThreadContext<NessaVM>) {
        match ctx {
            GCThreadContext::Worker(worker) => {
                let mmtk = mmtk_ref();
                std::thread::Builder::new()
                    .name("nessa-gc-worker".into())
                    .spawn(move || {
                        worker.run(VMWorkerThread(VMThread(OpaquePointer::UNINITIALIZED)), mmtk);
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
// NessaActivePlan — single-mutator management
// ---------------------------------------------------------------------------

static MUTATOR_PTR: AtomicPtr<Mutator<NessaVM>> = AtomicPtr::new(std::ptr::null_mut());

unsafe fn current_mutator() -> Option<&'static mut Mutator<NessaVM>> {
    let ptr = MUTATOR_PTR.load(Ordering::Acquire);
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &mut *ptr })
    }
}

pub struct NessaActivePlan;

impl ActivePlan<NessaVM> for NessaActivePlan {
    fn is_mutator(_tls: VMThread) -> bool {
        true
    }

    fn mutator(_tls: VMMutatorThread) -> &'static mut Mutator<NessaVM> {
        unsafe { current_mutator().expect("no mutator bound") }
    }

    fn mutators<'a>() -> Box<dyn Iterator<Item = &'a mut Mutator<NessaVM>> + 'a> {
        let ptr = MUTATOR_PTR.load(Ordering::Acquire);
        if ptr.is_null() {
            Box::new(std::iter::empty())
        } else {
            Box::new(std::iter::once(unsafe { &mut *ptr }))
        }
    }

    fn number_of_mutators() -> usize {
        if MUTATOR_PTR.load(Ordering::Relaxed).is_null() {
            0
        } else {
            1
        }
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
    mutator: *mut Mutator<NessaVM>,
    tls: VMMutatorThread,
    alloc_count: AtomicUsize,
}

unsafe impl Send for Heap {}

impl Heap {
    /// Allocate a Nessa object with the given type and payload word count.
    ///
    /// Returns a pointer to the *payload* (after the 16-byte header).
    pub fn alloc_object(
        &mut self,
        type_index: TypeIndex,
        payload_words: u16,
    ) -> Option<NonNull<u8>> {
        let payload_bytes = payload_words as usize * 8;
        let total_size = ObjectHeader::SIZE + payload_bytes;
        let semantics = if total_size > LARGE_OBJECT_THRESHOLD {
            AllocationSemantics::Los
        } else {
            AllocationSemantics::Default
        };

        let mutator = unsafe { &mut *self.mutator };
        let addr = mmtk::memory_manager::alloc(mutator, total_size, 8, 0, semantics);
        if addr.is_zero() {
            return None;
        }

        let header = ObjectHeader::new(type_index, payload_words);
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

    /// Check if a GC is requested.
    pub fn should_yield(&self) -> bool {
        GC_FLAGS.gc_requested.load(Ordering::Relaxed)
    }

    /// Called at safe-points.  Blocks if a GC is pending.
    pub fn safe_point(&self) {
        if self.should_yield() {
            NessaCollection::block_for_gc(self.tls);
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
        if !self.mutator.is_null() {
            let mutator = unsafe { &mut *self.mutator };
            mmtk::memory_manager::destroy_mutator(mutator);
            MUTATOR_PTR.store(std::ptr::null_mut(), Ordering::Release);
        }
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
        let _ = builder.set_option("heap_size", &config.heap_size.to_string());

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

    let tls = VMMutatorThread(VMThread(OpaquePointer::UNINITIALIZED));
    let mutator_box = mmtk::memory_manager::bind_mutator(mmtk_static, tls);
    let mutator_ptr = Box::into_raw(mutator_box);
    MUTATOR_PTR.store(mutator_ptr, Ordering::Release);

    Heap {
        mutator: mutator_ptr,
        tls,
        alloc_count: AtomicUsize::new(0),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
    fn gc_flags_default() {
        let flags = GcFlags::new();
        assert!(!flags.should_yield());
    }
}
