use gc::{
    ExternalRcHandle, GC_FLAGS, GcConfig, Heap, INLINE_BYTES_THRESHOLD, MAX_EXTERNAL_RC_BYTES,
    MAX_LARGE_TAGGED_WORDS, NessaSlot, ObjectHeader, external_rc_clone_view, heap_string_bytes,
};
use nsbc::{AddrMode, FuncId, Instruction, InstructionData, Opcode, ParameterAbi, Reg};
use parking_lot::{Condvar, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use runtime::{
    BuiltinFnId, BytecodeStore, CallFrame, ClosureEnv, ConstantPool, FunctionCode, GlobalTable,
    Number, TaggedValue, TaskId,
};
use scheduler::{Scheduler, WorkerNext, WorkerQueue};
use stack_pool::StackPool;
use std::cell::{RefCell, UnsafeCell};
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};
use str_interner::StrId;
use type_pool::{Intrinsic, TypeIndex, TypePool};

mod builtin_ctx;
mod bytes_obj;
#[cfg(test)]
mod callable_tests;
mod closures;
mod collections;
#[cfg(test)]
mod continuation_gc_tests;
mod continuation_roots;
mod display;
mod display_frames;
mod dynamic_equality;
#[cfg(test)]
mod effect_contract_tests;
mod effects;
mod enums;
#[cfg(test)]
mod error_value_tests;
mod error_values;
mod fields;
mod function_abi;
mod futures;
mod globals;
mod jit_bridge;
pub mod json_kernel;
mod maps;
mod method_access;
#[cfg(test)]
mod moving_allocation_tests;
mod numeric;
pub mod resources;
pub(crate) mod sync_primitives;
mod trait_proofs;
mod tuples;
mod types;
pub mod wasm_bridge;
pub use builtin_ctx::{
    BuiltinCtx, BuiltinFn, BuiltinFnTable, BuiltinOutcome, BuiltinRoot, HostCallOutcome,
    HostHandler, NativeValue, VmIoContext,
};
pub use jit::{JitConfig, JitMode, JitStats};
pub use resources::VmResourceTable;
pub use scheduler::{ProcessOutcome, ProcessSpec};

/// Number of cache-line-padded `RwLock` stripes protecting concurrent `List`/`Map` mutations.
pub const CONTAINER_STRIPE_COUNT: usize = 64;

/// Cache-line-aligned `RwLock` stripe avoiding false sharing across worker cores.
#[repr(align(64))]
pub(crate) struct CachePaddedRwLock(pub(crate) RwLock<()>);

impl Default for CachePaddedRwLock {
    fn default() -> Self {
        Self(RwLock::new(()))
    }
}

/// Cache-line-aligned `AtomicU64` counter avoiding false sharing across worker cores.
#[repr(align(64))]
pub(crate) struct CachePaddedAtomicU64(pub(crate) AtomicU64);

impl Default for CachePaddedAtomicU64 {
    fn default() -> Self {
        Self(AtomicU64::new(0))
    }
}

/// Read-mostly lock sharded by worker ID so concurrent worker reads hit strictly
/// core-local cache lines with zero cross-core atomic cache-line bouncing.
pub(crate) struct ShardedRwLock<T> {
    writer_gate: Mutex<()>,
    shards: Box<[CachePaddedRwLock]>,
    data: UnsafeCell<T>,
}

// SAFETY: Concurrent readers hold a shard read lock preventing any writer from entering;
// writers hold `writer_gate` and exclusive write locks on every shard `0..shards.len()`.
unsafe impl<T: Send> Send for ShardedRwLock<T> {}
unsafe impl<T: Send + Sync> Sync for ShardedRwLock<T> {}

pub(crate) struct ShardedReadGuard<'a, T> {
    data: &'a T,
    _shard_guard: RwLockReadGuard<'a, ()>,
}

impl<T> Deref for ShardedReadGuard<'_, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        self.data
    }
}

pub(crate) struct ShardedWriteGuard<'a, T> {
    data: *mut T,
    on_drop_epoch: Option<&'a AtomicU64>,
    _shard_guards: Vec<RwLockWriteGuard<'a, ()>>,
    _writer_gate: MutexGuard<'a, ()>,
}

impl<T> Drop for ShardedWriteGuard<'_, T> {
    fn drop(&mut self) {
        if let Some(epoch) = self.on_drop_epoch {
            epoch.fetch_add(1, Ordering::Release);
        }
    }
}

impl<T> Deref for ShardedWriteGuard<'_, T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: All shard write locks are held for the lifetime of `self`.
        unsafe { &*self.data }
    }
}

impl<T> DerefMut for ShardedWriteGuard<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: Exclusive `&mut self` + all shard write locks guarantee unique mutable access.
        unsafe { &mut *self.data }
    }
}

impl<T> ShardedRwLock<T> {
    pub(crate) fn new(value: T, shard_count: usize) -> Self {
        let count = shard_count.max(1);
        let shards: Vec<CachePaddedRwLock> =
            (0..count).map(|_| CachePaddedRwLock::default()).collect();
        Self {
            writer_gate: Mutex::new(()),
            shards: shards.into_boxed_slice(),
            data: UnsafeCell::new(value),
        }
    }

    #[inline]
    pub(crate) fn read(&self, worker_id: usize) -> ShardedReadGuard<'_, T> {
        let idx = worker_id % self.shards.len();
        let shard_guard = self.shards[idx].0.read();
        // SAFETY: Holding `shards[idx]` read lock blocks any writer from acquiring all shards.
        let data = unsafe { &*self.data.get() };
        ShardedReadGuard {
            data,
            _shard_guard: shard_guard,
        }
    }

    pub(crate) fn write(&self) -> ShardedWriteGuard<'_, T> {
        self.write_inner(None)
    }

    pub(crate) fn write_with_epoch<'a>(&'a self, epoch: &'a AtomicU64) -> ShardedWriteGuard<'a, T> {
        self.write_inner(Some(epoch))
    }

    fn write_inner<'a>(&'a self, on_drop_epoch: Option<&'a AtomicU64>) -> ShardedWriteGuard<'a, T> {
        let gate = self.writer_gate.lock();
        let mut guards = Vec::with_capacity(self.shards.len());
        for shard in self.shards.iter() {
            guards.push(shard.0.write());
        }
        if let Some(epoch) = on_drop_epoch {
            epoch.fetch_add(1, Ordering::Release);
        }
        ShardedWriteGuard {
            data: self.data.get(),
            on_drop_epoch,
            _shard_guards: guards,
            _writer_gate: gate,
        }
    }

    #[inline]
    pub(crate) fn data_ptr(&self) -> *mut T {
        self.data.get()
    }
}

/// Shared read guard over the VM's [`TypePool`].
pub struct VmTypePoolRef<'a>(ShardedReadGuard<'a, TypePool>);

impl Deref for VmTypePoolRef<'_> {
    type Target = TypePool;

    fn deref(&self) -> &TypePool {
        &self.0
    }
}

#[derive(Default)]
pub(crate) struct TraitProofRegistry {
    pub(crate) proofs: std::collections::HashMap<u64, Arc<trait_proofs::FrozenTraitProof>>,
    pub(crate) cache: std::collections::HashMap<(usize, TypeIndex), TaggedValue>,
}

pub(crate) struct WorkerRoots {
    pub(crate) temporary_values: RefCell<Vec<TaggedValue>>,
    pub(crate) native_pins: RefCell<Vec<TaggedValue>>,
}

impl WorkerRoots {
    fn new() -> Self {
        Self {
            temporary_values: RefCell::new(Vec::new()),
            native_pins: RefCell::new(Vec::new()),
        }
    }
}

pub(crate) struct WorkerRootSlot(UnsafeCell<WorkerRoots>);

impl WorkerRootSlot {
    fn new() -> Self {
        Self(UnsafeCell::new(WorkerRoots::new()))
    }
}

// SAFETY: During mutator execution, `worker_roots[worker_id]` is accessed exclusively by
// `worker_id`'s OS thread. During STW GC, all mutators are stopped before `scan_vm_roots`
// traverses all slots via `&mut VmRoots`.
unsafe impl Send for WorkerRootSlot {}
unsafe impl Sync for WorkerRootSlot {}

// ---------------------------------------------------------------------------
// Auxiliary Worker Epoch Synchronization
// ---------------------------------------------------------------------------

#[derive(Default)]
struct AuxEpochState {
    run_epoch: u64,
    epoch_running: bool,
    shutdown: bool,
    active_aux_workers: usize,
    first_error: Option<VmError>,
    saw_deadlock: bool,
}

pub(crate) struct AuxEpochSync {
    lock: Mutex<AuxEpochState>,
    start_cv: Condvar,
    done_cv: Condvar,
    all_aux_entered: AtomicBool,
}

impl AuxEpochSync {
    fn new() -> Self {
        Self {
            lock: Mutex::new(AuxEpochState::default()),
            start_cv: Condvar::new(),
            done_cv: Condvar::new(),
            all_aux_entered: AtomicBool::new(false),
        }
    }

    fn begin_run_epoch(&self, wake_aux_immediately: bool) {
        let mut guard = self.lock.lock();
        guard.run_epoch = guard.run_epoch.wrapping_add(1);
        guard.epoch_running = true;
        guard.first_error = None;
        guard.saw_deadlock = false;
        self.all_aux_entered.store(false, Ordering::Release);
        if wake_aux_immediately {
            self.start_cv.notify_all();
        }
    }

    fn record_error(&self, err: VmError) {
        let mut guard = self.lock.lock();
        if guard.first_error.is_none() {
            guard.first_error = Some(err);
        }
    }

    fn record_deadlock(&self) {
        let mut guard = self.lock.lock();
        guard.saw_deadlock = true;
    }

    fn end_run_epoch(&self) -> (Option<VmError>, bool) {
        let mut guard = self.lock.lock();
        guard.epoch_running = false;
        while guard.active_aux_workers > 0 {
            self.done_cv.wait(&mut guard);
        }
        (
            guard.first_error.take(),
            std::mem::replace(&mut guard.saw_deadlock, false),
        )
    }
}

/// Drop guard stored as the first field of the primary [`Vm`].
///
/// Signals shutdown and joins all auxiliary OS worker threads while `_roots` and `shared`
/// are still alive, guaranteeing every auxiliary MMTk `Mutator` unregisters cleanly before
/// `VmRoots` is unregistered and freed.
struct PrimaryAuxShutdown {
    shared: Option<Arc<SharedVm>>,
}

impl PrimaryAuxShutdown {
    fn new(shared: Arc<SharedVm>) -> Self {
        Self {
            shared: Some(shared),
        }
    }

    fn disarm(&mut self) {
        self.shared = None;
    }
}

impl Drop for PrimaryAuxShutdown {
    fn drop(&mut self) {
        let Some(shared) = self.shared.take() else {
            return;
        };
        {
            let mut guard = shared.epoch_sync.lock.lock();
            guard.shutdown = true;
            guard.epoch_running = false;
            shared.epoch_sync.start_cv.notify_all();
        }
        shared.roots.scheduler.request_abort();
        let handles = std::mem::take(&mut *shared.aux_handles.lock());
        for handle in handles {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// VM — the top-level virtual machine
// ---------------------------------------------------------------------------

/// The top-level Nessa virtual machine.
pub struct Vm {
    // 1st dropped: shuts down & joins aux OS threads while `_roots` and `shared` are still live.
    _aux_shutdown: Option<PrimaryAuxShutdown>,
    // 2nd dropped: unregisters `VmRoots` from MMTk after all aux mutators have exited.
    _roots: Option<gc::RootRegistration>,
    // 3rd dropped: drops this worker's MMTk `Heap` (`Mutator`).
    pub(crate) worker: Box<WorkerState>,
    // 4th dropped: frees `SharedVm` and `VmRoots` (`strong_count == 1`).
    pub(crate) shared: Arc<SharedVm>,
}

#[derive(Clone, Copy)]
pub(crate) enum CachedHeapTypeMeta {
    Collection(type_pool::CollectionRole),
    PlainStruct { expected_words: u16 },
    SingleWord,
    Slow,
}

#[derive(Clone, Copy)]
pub(crate) enum CachedTypeOpKind {
    Any,
    NoReturn,
    Intrinsic(Intrinsic),
    Optional(TypeIndex),
    EffectQualified(TypeIndex),
    ErrorQualified,
    PlainStruct { expected_words: u16 },
    Collection3Word,
    Slow,
}

#[derive(Default)]
pub(crate) struct WorkerMetaCache {
    pub(crate) epoch: u64,
    pub(crate) pool_len: usize,
    pub(crate) has_scopes: Option<bool>,
    pub(crate) validated_funcs: Vec<bool>,
    pub(crate) method_ic:
        runtime::FastHashMap<(FuncId, u32, TypeIndex, StrId), Option<type_pool::MethodSlot>>,
    pub(crate) list_roles: Option<Option<(TypeIndex, TypeIndex)>>,
    pub(crate) map_roles: Option<Option<(TypeIndex, TypeIndex)>>,
    pub(crate) future_by_inner: runtime::FastHashMap<TypeIndex, TypeIndex>,
    pub(crate) is_future_type: Vec<Option<bool>>,
    pub(crate) effect_meta: runtime::FastHashMap<TypeIndex, (TypeIndex, bool)>,
    pub(crate) indirect_contracts: runtime::FastHashMap<FuncId, Arc<[(usize, TypeIndex)]>>,
    pub(crate) canonical_types: Vec<Option<TypeIndex>>,
    pub(crate) heap_type_meta: Vec<Option<CachedHeapTypeMeta>>,
    pub(crate) type_op_kinds: Vec<Option<CachedTypeOpKind>>,
    pub(crate) has_error_shape: Vec<Option<Result<bool, TypeIndex>>>,
}

/// Per-worker execution state owned exclusively by one OS thread.
pub(crate) struct WorkerState {
    pub(crate) worker_id: usize,
    pub(crate) heap: Heap,
    pub(crate) type_query_scope: Option<u32>,
    pub(crate) slice_safepoints: u32,
    pub(crate) native_frame_depth: u32,
    pub(crate) local_queue_ptr: Option<NonNull<WorkerQueue>>,
    pub(crate) meta_cache: RefCell<WorkerMetaCache>,
    pub(crate) slot_vec_pool: Vec<Vec<TaggedValue>>,
    pub(crate) jit: jit::JitEngine,
    pub(crate) jit_synced_epoch: u64,
    pub(crate) jit_reported_fns: usize,
    pub(crate) jit_reported_bytes: usize,
    pub(crate) jit_reported_deopts: usize,
}

/// Shared runtime metadata and synchronized services across all workers of a [`Vm`].
pub(crate) struct SharedVm {
    pub(crate) gc_config: GcConfig,
    pub(crate) configured_workers: usize,
    pub(crate) metadata_epoch: AtomicU64,
    pub(crate) bytecode: BytecodeStore,
    pub(crate) builtins: BuiltinFnTable,
    pub(crate) method_call_scopes: runtime::FastHashMap<(FuncId, usize), u32>,
    pub(crate) script_args: Vec<String>,
    pub(crate) type_pool: ShardedRwLock<TypePool>,
    pub(crate) trait_registry: ShardedRwLock<TraitProofRegistry>,
    pub(crate) pending_root_entries: Mutex<std::collections::HashSet<TaskId>>,
    pub(crate) pending_root_count: AtomicUsize,
    pub(crate) rng: Mutex<resources::VmRng>,
    pub(crate) files: resources::VmFileTable,
    pub(crate) json: Mutex<json_kernel::VmJsonState>,
    pub(crate) resources: Mutex<resources::VmResourceTable>,
    pub(crate) embedded_wasm: ShardedRwLock<wasm_bridge::EmbeddedWasmStore>,
    pub(crate) ffi_allowlist: RwLock<Option<std::collections::HashSet<String>>>,
    pub(crate) host_handler: Mutex<Option<Box<dyn HostHandler>>>,
    pub(crate) jit_config: RwLock<jit::JitConfig>,
    pub(crate) jit_epoch: AtomicU64,
    pub(crate) jit_compiled_functions: AtomicUsize,
    pub(crate) jit_code_bytes: AtomicUsize,
    pub(crate) jit_deopt_count: AtomicUsize,
    pub(crate) next_future_id: AtomicU64,
    pub(crate) next_handler_scope_id: AtomicU64,
    pub(crate) await_all_ops: Arc<futures::AwaitAllTable>,
    pub(crate) container_stripes: Box<[CachePaddedRwLock; CONTAINER_STRIPE_COUNT]>,
    pub(crate) worker_slices: Box<[CachePaddedAtomicU64]>,
    pub(crate) epoch_sync: Arc<AuxEpochSync>,
    pub(crate) aux_started: AtomicBool,
    pub(crate) aux_handles: Mutex<Vec<std::thread::JoinHandle<()>>>,
    pub(crate) roots: Box<RootCell>,
}

/// The collector accesses only this independently stored root domain, never
/// the allocator or a WorkerState currently borrowed across allocation/collection.
pub(crate) struct VmRoots {
    pub(crate) globals: GlobalTable,
    pub(crate) constants: ShardedRwLock<ConstantPool>,
    pub(crate) scheduler: Scheduler,
    pub(crate) worker_roots: Box<[WorkerRootSlot]>,
    pub(crate) continuations: Mutex<
        std::collections::HashMap<runtime::ContinuationId, continuation_roots::ManagedContinuation>,
    >,
    pub(crate) weak_finalizers: Mutex<Vec<continuation_roots::WeakFinalizer>>,
    pub(crate) promises: futures::PromiseTable,
    pub(crate) handler_scopes: futures::HandlerScopeTable,
    pub(crate) json_writer_ancestors: Mutex<std::collections::HashMap<i64, Vec<TaggedValue>>>,
    pub(crate) wasm_host_imports: Mutex<std::collections::HashMap<i64, Vec<TaggedValue>>>,
    pub(crate) sync: Arc<sync_primitives::SyncRegistry>,
    pub(crate) uncollected_terminal_promises: AtomicUsize,
}

impl Drop for VmRoots {
    fn drop(&mut self) {
        for mut finalizer in self.weak_finalizers.get_mut().drain(..) {
            finalizer.run_cleanup();
        }
    }
}

pub(crate) struct RootCell(UnsafeCell<VmRoots>);

// SAFETY: All runtime accesses through `Deref` use interior atomic/lock synchronization or
// per-worker partitioning (`worker_roots[worker_id]`). Exclusive `&mut VmRoots` access in
// `scan_vm_roots` occurs strictly during MMTk stop-the-world when all mutators are parked.
unsafe impl Send for RootCell {}
unsafe impl Sync for RootCell {}

impl Deref for RootCell {
    type Target = VmRoots;
    fn deref(&self) -> &VmRoots {
        // SAFETY: Mutators access synchronized fields while holding a mutator session;
        // collector `&mut VmRoots` access happens only under STW.
        unsafe { &*self.0.get() }
    }
}

impl DerefMut for RootCell {
    fn deref_mut(&mut self) -> &mut VmRoots {
        self.0.get_mut()
    }
}

/// RAII guard binding a leased [`WorkerQueue`] pointer onto [`WorkerState`] for one worker loop.
struct ScopedWorkerQueueGuard<'a> {
    vm: &'a mut Vm,
}

impl Drop for ScopedWorkerQueueGuard<'_> {
    fn drop(&mut self) {
        self.vm.worker.local_queue_ptr = None;
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkerLoopExit {
    AllFinished,
    Deadlock,
    Aborted,
}

impl Vm {
    /// Resolve the default worker thread count (`NESSA_WORKER_THREADS` override in `1..=64`,
    /// otherwise `std::thread::available_parallelism()` clamped to `1..=8`).
    pub fn default_worker_threads() -> usize {
        if let Ok(raw) = std::env::var("NESSA_WORKER_THREADS")
            && let Ok(parsed) = raw.trim().parse::<usize>()
            && parsed >= 1
        {
            return parsed.clamp(1, 64);
        }
        std::thread::available_parallelism()
            .map(|n| n.get().clamp(1, 8))
            .unwrap_or(1)
    }

    pub fn new(type_pool: TypePool, gc_config: &GcConfig, stack_pool: Arc<StackPool>) -> Self {
        Self::new_with_workers(
            type_pool,
            gc_config,
            stack_pool,
            Self::default_worker_threads(),
        )
    }

    pub fn new_with_workers(
        type_pool: TypePool,
        gc_config: &GcConfig,
        stack_pool: Arc<StackPool>,
        worker_threads: usize,
    ) -> Self {
        let configured_workers = worker_threads.clamp(1, 64);
        let worker = Box::new(WorkerState {
            worker_id: 0,
            heap: gc::gc_init(gc_config),
            type_query_scope: None,
            slice_safepoints: 64,
            native_frame_depth: 0,
            local_queue_ptr: None,
            meta_cache: RefCell::new(WorkerMetaCache::default()),
            slot_vec_pool: Vec::with_capacity(64),
            jit: Self::new_default_jit(),
            jit_synced_epoch: 0,
            jit_reported_fns: 0,
            jit_reported_bytes: 0,
            jit_reported_deopts: 0,
        });
        let worker_roots: Vec<WorkerRootSlot> = (0..configured_workers)
            .map(|_| WorkerRootSlot::new())
            .collect();
        let worker_slices: Vec<CachePaddedAtomicU64> = (0..configured_workers)
            .map(|_| CachePaddedAtomicU64::default())
            .collect();
        let stripes: [CachePaddedRwLock; CONTAINER_STRIPE_COUNT] =
            std::array::from_fn(|_| CachePaddedRwLock::default());
        let scheduler = Scheduler::with_workers(stack_pool, configured_workers);
        let await_all_ops = Arc::new(futures::AwaitAllTable::new());
        let sync = Arc::new(sync_primitives::SyncRegistry::new());
        {
            let await_all_for_hook = Arc::clone(&await_all_ops);
            let sync_for_hook = Arc::clone(&sync);
            let weak_sched = scheduler.downgrade();
            scheduler.set_cancel_hook(Arc::new(move |tid| {
                if let Some(st) = await_all_for_hook.remove(tid) {
                    st.early_exit.store(true, Ordering::Release);
                }
                if let Some(sched) = weak_sched.upgrade() {
                    sync_for_hook.cancel_task(&sched, tid);
                }
            }));
        }
        let roots = Box::new(RootCell(UnsafeCell::new(VmRoots {
            globals: GlobalTable::new(),
            constants: ShardedRwLock::new(ConstantPool::new(), configured_workers),
            scheduler,
            worker_roots: worker_roots.into_boxed_slice(),
            continuations: Mutex::new(std::collections::HashMap::new()),
            weak_finalizers: Mutex::new(Vec::new()),
            promises: futures::PromiseTable::new(),
            handler_scopes: futures::HandlerScopeTable::new(),
            json_writer_ancestors: Mutex::new(std::collections::HashMap::new()),
            wasm_host_imports: Mutex::new(std::collections::HashMap::new()),
            sync,
            uncollected_terminal_promises: AtomicUsize::new(0),
        })));
        // SAFETY: `roots` is a stable heap allocation outliving `_roots`. `PrimaryAuxShutdown`
        // drops first (joining all aux threads), then `_roots` unregisters `VmRoots`, and only
        // after `worker` drops does `Arc<SharedVm>` drop `roots`.
        let registration = unsafe {
            gc::RootRegistration::new_with_conditional_roots(
                roots.0.get().cast(),
                scan_vm_roots,
                continuation_roots::CALLBACKS,
            )
        };
        let shared = Arc::new(SharedVm {
            gc_config: gc_config.clone(),
            configured_workers,
            metadata_epoch: AtomicU64::new(1),
            bytecode: BytecodeStore::new(),
            builtins: BuiltinFnTable::new(),
            method_call_scopes: runtime::FastHashMap::default(),
            script_args: Vec::new(),
            type_pool: ShardedRwLock::new(type_pool, configured_workers),
            trait_registry: ShardedRwLock::new(TraitProofRegistry::default(), configured_workers),
            pending_root_entries: Mutex::new(std::collections::HashSet::new()),
            pending_root_count: AtomicUsize::new(0),
            rng: Mutex::new(resources::VmRng::new()),
            files: resources::VmFileTable::new(),
            json: Mutex::new(json_kernel::VmJsonState::new()),
            resources: Mutex::new(resources::VmResourceTable::new()),
            embedded_wasm: ShardedRwLock::new(
                wasm_bridge::EmbeddedWasmStore::default(),
                configured_workers,
            ),
            ffi_allowlist: RwLock::new(None),
            host_handler: Mutex::new(None),
            jit_config: RwLock::new(jit::JitConfig::default()),
            jit_epoch: AtomicU64::new(0),
            jit_compiled_functions: AtomicUsize::new(0),
            jit_code_bytes: AtomicUsize::new(0),
            jit_deopt_count: AtomicUsize::new(0),
            next_future_id: AtomicU64::new(1),
            next_handler_scope_id: AtomicU64::new(1),
            await_all_ops,
            container_stripes: Box::new(stripes),
            worker_slices: worker_slices.into_boxed_slice(),
            epoch_sync: Arc::new(AuxEpochSync::new()),
            aux_started: AtomicBool::new(false),
            aux_handles: Mutex::new(Vec::new()),
            roots,
        });
        let aux_shutdown = PrimaryAuxShutdown::new(Arc::clone(&shared));
        Self {
            _aux_shutdown: Some(aux_shutdown),
            _roots: Some(registration),
            worker,
            shared,
        }
    }

    #[inline]
    pub fn worker_count(&self) -> usize {
        self.shared.configured_workers
    }

    #[inline]
    pub fn scheduler_snapshot_handle(&self) -> Scheduler {
        self.shared.roots.scheduler.clone()
    }

    /// Snapshot per-worker task execution slice counts (`0..worker_count()`).
    pub fn worker_slice_counts(&self) -> Vec<u64> {
        self.shared
            .worker_slices
            .iter()
            .map(|c| c.0.load(Ordering::Relaxed))
            .collect()
    }

    // -- Internal Helper Accessors -------------------------------------------

    #[inline]
    pub(crate) fn roots(&self) -> &VmRoots {
        &self.shared.roots
    }

    #[inline]
    pub(crate) fn scheduler(&self) -> &Scheduler {
        &self.shared.roots.scheduler
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn heap(&self) -> &Heap {
        &self.worker.heap
    }

    #[cfg(test)]
    #[inline]
    pub(crate) fn heap_mut(&mut self) -> &mut Heap {
        &mut self.worker.heap
    }

    #[inline]
    pub(crate) fn worker_roots(&self) -> &WorkerRoots {
        // SAFETY: `self.worker.worker_id` is uniquely owned by the current OS thread,
        // and `worker_roots` has length `configured_workers`.
        unsafe {
            &*self.shared.roots.worker_roots[self.worker.worker_id]
                .0
                .get()
        }
    }

    #[inline]
    pub(crate) fn local_queue(&self) -> Option<&WorkerQueue> {
        // SAFETY: `local_queue_ptr` is set exclusively for the stack lifetime of
        // `lease_worker_queue` inside `run_worker_loop` and cleared by `ScopedWorkerQueueGuard`.
        self.worker
            .local_queue_ptr
            .map(|ptr| unsafe { ptr.as_ref() })
    }

    #[inline]
    pub(crate) fn type_pool_read(&self) -> ShardedReadGuard<'_, TypePool> {
        self.shared.type_pool.read(self.worker.worker_id)
    }

    #[inline]
    pub(crate) fn type_pool_write(&self) -> ShardedWriteGuard<'_, TypePool> {
        self.shared
            .type_pool
            .write_with_epoch(&self.shared.metadata_epoch)
    }

    #[inline]
    pub(crate) fn bump_metadata_epoch(&self) {
        self.shared.metadata_epoch.fetch_add(1, Ordering::Release);
    }

    #[inline]
    pub(crate) fn sync_meta_cache(&self) -> std::cell::RefMut<'_, WorkerMetaCache> {
        let cur = self.shared.metadata_epoch.load(Ordering::Acquire);
        let mut cache = self.worker.meta_cache.borrow_mut();
        if cache.epoch != cur {
            cache.epoch = cur;
            cache.pool_len = 0;
            cache.has_scopes = None;
            cache.validated_funcs.clear();
            cache.method_ic.clear();
            cache.list_roles = None;
            cache.map_roles = None;
            cache.future_by_inner.clear();
            cache.is_future_type.clear();
            cache.effect_meta.clear();
            cache.indirect_contracts.clear();
            cache.canonical_types.clear();
            cache.heap_type_meta.clear();
            cache.type_op_kinds.clear();
            cache.has_error_shape.clear();
        }
        cache
    }

    #[inline]
    pub(crate) fn trait_registry_read(&self) -> ShardedReadGuard<'_, TraitProofRegistry> {
        self.shared.trait_registry.read(self.worker.worker_id)
    }

    #[inline]
    pub(crate) fn trait_registry_write(&self) -> ShardedWriteGuard<'_, TraitProofRegistry> {
        self.shared.trait_registry.write()
    }

    #[inline]
    pub(crate) fn constants_read(&self) -> ShardedReadGuard<'_, ConstantPool> {
        self.shared.roots.constants.read(self.worker.worker_id)
    }

    /// Exclusive mutable access to [`SharedVm`] outside `Vm::run()`.
    ///
    /// Note: `_aux_shutdown` holds a second `Arc<SharedVm>` on the primary `Vm`, so
    /// outside `Vm::run()` (when `active_aux_workers == 0`) we temporarily release and
    /// restore `_aux_shutdown` to obtain `&mut SharedVm` via `Arc::get_mut`.
    pub(crate) fn with_exclusive_shared_mut<R>(&mut self, f: impl FnOnce(&mut SharedVm) -> R) -> R {
        let had_shutdown = if let Some(mut shutdown) = self._aux_shutdown.take() {
            shutdown.disarm();
            true
        } else {
            false
        };
        let shared_mut = Arc::get_mut(&mut self.shared)
            .expect("exclusive SharedVm access requires calling outside Vm::run");
        let result = f(shared_mut);
        if had_shutdown {
            self._aux_shutdown = Some(PrimaryAuxShutdown::new(Arc::clone(&self.shared)));
        }
        result
    }

    #[inline]
    pub(crate) fn task_ref(&self, task_id: TaskId) -> Option<&runtime::TaskState> {
        // SAFETY: Called by the worker owning `Running` `task_id` or post-run host queries.
        unsafe { self.shared.roots.scheduler.owner_task(task_id) }
    }

    #[inline]
    pub(crate) fn task_mut(&mut self, task_id: TaskId) -> Option<&mut runtime::TaskState> {
        // SAFETY: `&mut Vm` is uniquely owned by this worker thread, which exclusively owns
        // `Running` `task_id` (or exclusive `&mut Vm` outside `Vm::run`).
        unsafe { self.shared.roots.scheduler.owner_task_mut(task_id) }
    }

    #[inline]
    pub(crate) fn stripe_index_of(value: TaggedValue) -> Result<usize, VmError> {
        let ptr = value.as_heap_ptr().ok_or(VmError::TypeError)?;
        // SAFETY: `value` is a live heap reference within an active mutator operation;
        // `gc_meta` stripe bits are immutable after allocation and preserved across GC copies.
        let header = unsafe { ObjectHeader::from_payload_ptr(ptr) };
        Ok(header.stripe_index())
    }

    #[inline]
    pub(crate) fn stripe_read(
        &self,
        value: TaggedValue,
    ) -> Result<Option<RwLockReadGuard<'_, ()>>, VmError> {
        let idx = Self::stripe_index_of(value)?;
        if !self.shared.aux_started.load(Ordering::Acquire) {
            return Ok(None);
        }
        Ok(Some(self.shared.container_stripes[idx].0.read()))
    }

    #[inline]
    pub(crate) fn stripe_write(
        &self,
        value: TaggedValue,
    ) -> Result<Option<RwLockWriteGuard<'_, ()>>, VmError> {
        let idx = Self::stripe_index_of(value)?;
        if !self.shared.aux_started.load(Ordering::Acquire) {
            return Ok(None);
        }
        Ok(Some(self.shared.container_stripes[idx].0.write()))
    }

    #[inline]
    pub(crate) fn recycle_frame_slots(&mut self, mut old_slots: Vec<TaggedValue>) {
        let cap = old_slots.capacity();
        if cap > 0 && cap <= 256 && self.worker.slot_vec_pool.len() < 128 {
            old_slots.clear();
            self.worker.slot_vec_pool.push(old_slots);
        }
    }

    #[inline]
    pub(crate) fn alloc_task_frame_slots(
        &mut self,
        task_id: TaskId,
        count: usize,
    ) -> Result<(), VmError> {
        if count == 0 {
            let task = self.task_mut(task_id).ok_or(VmError::InvalidTask)?;
            let old = std::mem::take(&mut task.local_slots);
            self.recycle_frame_slots(old);
            return Ok(());
        }
        let recycled = if count <= 256 {
            self.worker.slot_vec_pool.pop()
        } else {
            None
        };
        let mut slots = recycled.unwrap_or_default();
        if slots.capacity() < count {
            let target = if count <= 16 { 16 } else { count };
            if slots.try_reserve_exact(target - slots.len()).is_err() {
                return Err(VmError::OutOfMemory);
            }
        }
        slots.resize(count, TaggedValue::UNIT);
        let task = self.task_mut(task_id).ok_or(VmError::InvalidTask)?;
        let old = std::mem::replace(&mut task.local_slots, slots);
        self.recycle_frame_slots(old);
        Ok(())
    }

    /// Lazily start auxiliary OS worker threads (`1..configured_workers`) the first time
    /// concurrent tasks exist, waking them immediately if a `Vm::run()` epoch is active.
    pub(crate) fn ensure_aux_workers_started(&self) {
        if self.shared.configured_workers <= 1 {
            return;
        }
        if self.shared.aux_started.load(Ordering::Acquire) {
            if !self
                .shared
                .epoch_sync
                .all_aux_entered
                .load(Ordering::Acquire)
            {
                self.shared.epoch_sync.start_cv.notify_all();
            }
            return;
        }
        let mut handles = self.shared.aux_handles.lock();
        if self.shared.aux_started.load(Ordering::Acquire) {
            drop(handles);
            if !self
                .shared
                .epoch_sync
                .all_aux_entered
                .load(Ordering::Acquire)
            {
                self.shared.epoch_sync.start_cv.notify_all();
            }
            return;
        }
        // Publish `aux_started = true` BEFORE spawning auxiliary mutator threads so
        // Worker 0 transitions into striped container locking prior to any concurrent access.
        self.shared.aux_started.store(true, Ordering::Release);
        for worker_id in 1..self.shared.configured_workers {
            let weak_shared: Weak<SharedVm> = Arc::downgrade(&self.shared);
            let gc_config = self.shared.gc_config.clone();
            let sync = Arc::clone(&self.shared.epoch_sync);
            let total_aux = self.shared.configured_workers - 1;
            let handle = std::thread::Builder::new()
                .name(format!("nessa-worker-{worker_id}"))
                .spawn(move || {
                    aux_worker_thread_main(worker_id, total_aux, gc_config, weak_shared, sync)
                })
                .expect("failed to spawn Nessa worker thread");
            handles.push(handle);
        }
        drop(handles);
        self.shared.epoch_sync.start_cv.notify_all();
    }

    // -- Public & Runtime API ------------------------------------------------

    /// Request a synchronous, exhaustive collection. False means the configured
    /// plan (for example NoGC) did not perform a collection.
    pub fn collect_garbage(&mut self) -> Result<bool, VmError> {
        let _operation = self.execution_operation()?;
        // SAFETY: the operation keeps this mutator live, and public APIs retain
        // managed references only in registered roots or owned host snapshots.
        Ok(unsafe { self.worker.heap.collect_garbage() })
    }

    pub fn completed_collections(&self) -> u64 {
        self.worker.heap.completed_collections()
    }

    pub fn constant_string(&mut self, index: u32) -> Result<String, VmError> {
        let _operation = self.operation();
        let value = self.constant_value(index)?;
        builtin_ctx::heap_string_to_owned(value).ok_or(VmError::TypeError)
    }

    pub(crate) fn constant_value(&self, index: u32) -> Result<TaggedValue, VmError> {
        self.constants_read()
            .values()
            .get(index as usize)
            .copied()
            .ok_or(VmError::InvalidConstant(index))
    }

    pub(crate) fn constant_u32(&self, index: u32) -> Result<u32, VmError> {
        let value = self.constant_value(index)?;
        if let Some(raw) = value.as_u64() {
            u32::try_from(raw).map_err(|_| VmError::TypeError)
        } else if let Some(raw) = value.as_i64() {
            u32::try_from(raw).map_err(|_| VmError::TypeError)
        } else {
            Err(VmError::TypeError)
        }
    }

    pub(crate) fn operation(&self) -> gc::MutatorSession<'static> {
        // SAFETY: all callers keep Vm alive until the guard is dropped, and
        // `self.worker.heap` is owned exclusively by this thread.
        unsafe { self.worker.heap.begin_operation() }
    }

    pub(crate) fn execution_operation(&self) -> Result<gc::MutatorSession<'static>, VmError> {
        // SAFETY: same lifetime/publication guarantees as operation; rejects
        // cross-VM reentry that would leave the caller running while blocked.
        unsafe { self.worker.heap.begin_execution() }.map_err(|_| VmError::CrossVmOperation)
    }

    pub fn type_pool(&self) -> VmTypePoolRef<'_> {
        VmTypePoolRef(self.type_pool_read())
    }

    pub fn install_type_pool(&mut self, type_pool: TypePool) {
        let _operation = self.operation();
        self.worker.type_query_scope = None;
        self.with_exclusive_shared_mut(|shared| {
            let mut reg = shared.trait_registry.write();
            reg.proofs.clear();
            reg.cache.clear();
            drop(reg);
            *shared.type_pool.write() = type_pool;
            shared.method_call_scopes.clear();
            shared.metadata_epoch.fetch_add(1, Ordering::Release);
        });
        self.invalidate_jit();
    }

    /// Allocate, initialize and publish a constant in one operation.
    pub fn push_constant(&mut self, constant: &nsbc::Constant) -> Result<u32, VmError> {
        let _operation = self.execution_operation()?;
        let value = match constant {
            nsbc::Constant::Int(value) => self.number_value(Number::I64(*value))?,
            nsbc::Constant::UInt(value) => self.number_value(Number::U64(*value))?,
            nsbc::Constant::Int128(value) => self.number_value(Number::I128(*value))?,
            nsbc::Constant::UInt128(value) => self.number_value(Number::U128(*value))?,
            nsbc::Constant::Float(value) => self.number_value(Number::F64(*value))?,
            nsbc::Constant::Str(value) => self.alloc_string(value)?,
            nsbc::Constant::Char(value) => TaggedValue::from_char(*value),
            nsbc::Constant::Type(ty) => self.type_value(*ty)?,
            nsbc::Constant::Enum {
                type_index,
                variant,
            } => self.enum_constant(*type_index, *variant)?,
        };
        Ok(self.shared.roots.constants.write().push(value))
    }

    /// Return an owned scalar snapshot; no borrowed task or heap value escapes.
    pub fn task_result_i64(&mut self, task: TaskId) -> Result<i64, VmError> {
        self.task_result_number(task)?
            .to_i64_checked()
            .ok_or(VmError::TypeError)
    }

    /// Return an owned `Option<String>` (`Ok(None)` when result register is `null`).
    pub fn task_result_string(&mut self, task: TaskId) -> Result<Option<String>, VmError> {
        let _operation = self.operation();
        let value = self
            .scheduler()
            .get_task(task)
            .ok_or(VmError::InvalidTask)?
            .registers
            .get(Reg(0));
        if value.is_null() {
            return Ok(None);
        }
        builtin_ctx::heap_string_to_owned(value)
            .map(Some)
            .ok_or(VmError::TypeError)
    }

    /// Return an owned `Option<bool>` (`Ok(None)` when result register is `null`).
    pub fn task_result_bool(&mut self, task: TaskId) -> Result<Option<bool>, VmError> {
        let _operation = self.operation();
        let value = self
            .scheduler()
            .get_task(task)
            .ok_or(VmError::InvalidTask)?
            .registers
            .get(Reg(0));
        if value.is_null() {
            return Ok(None);
        }
        value.as_bool().map(Some).ok_or(VmError::TypeError)
    }

    /// Return whether the finished task's `r0` holds `null`.
    pub fn task_result_is_null(&self, task: TaskId) -> Result<bool, VmError> {
        let _operation = self.operation();
        Ok(self
            .scheduler()
            .get_task(task)
            .ok_or(VmError::InvalidTask)?
            .registers
            .get(Reg(0))
            .is_null())
    }

    /// Return whether the finished task's `r0` holds `Unit`.
    pub fn task_result_is_unit(&self, task: TaskId) -> Result<bool, VmError> {
        let _operation = self.operation();
        Ok(self
            .scheduler()
            .get_task(task)
            .ok_or(VmError::InvalidTask)?
            .registers
            .get(Reg(0))
            .is_unit())
    }

    pub fn active_stack_count(&self) -> usize {
        let _operation = self.operation();
        self.scheduler().stack_pool().active_count()
    }

    pub fn active_task_count(&self) -> usize {
        let _operation = self.operation();
        self.scheduler().active_task_count()
    }

    pub fn occupied_task_count(&self) -> usize {
        let _operation = self.operation();
        self.scheduler().task_count()
    }

    pub fn promise_count(&self) -> usize {
        let _operation = self.operation();
        self.shared.roots.promises.len()
    }

    /// Set command-line arguments visible to Nessa programs via `ENV_ARGS`.
    pub fn set_script_args(&mut self, args: Vec<String>) {
        self.with_exclusive_shared_mut(|shared| {
            shared.script_args = args;
        });
    }

    /// Access the host-owned monotonic resource registry.
    pub fn resources(&self) -> MutexGuard<'_, resources::VmResourceTable> {
        self.shared.resources.lock()
    }

    /// Mutably access the host-owned monotonic resource registry.
    pub fn resources_mut(&mut self) -> MutexGuard<'_, resources::VmResourceTable> {
        self.shared.resources.lock()
    }

    /// Current active native C/host frame nesting depth on this worker.
    pub fn native_frame_depth(&self) -> u32 {
        self.worker.native_frame_depth
    }

    /// Execute `f` under an RAII native-frame guard (`native_frame_depth > 0`),
    /// preventing effect handler / continuation jumps across the native stack frame.
    pub fn with_native_frame<R>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<R, VmError>,
    ) -> Result<R, VmError> {
        struct NativeFrameScope(*mut u32);
        impl Drop for NativeFrameScope {
            fn drop(&mut self) {
                // SAFETY: `WorkerState` is boxed at a stable address and outlives `f`.
                unsafe {
                    *self.0 -= 1;
                }
            }
        }
        self.worker.native_frame_depth = self
            .worker
            .native_frame_depth
            .checked_add(1)
            .ok_or(VmError::NumericOverflow)?;
        let _guard = NativeFrameScope(&raw mut self.worker.native_frame_depth);
        f(self)
    }

    /// Configure an optional runtime FFI library allowlist (`None` allows all manifest-authorized libraries).
    pub fn set_ffi_allowlist(&mut self, allowlist: Option<std::collections::HashSet<String>>) {
        *self.shared.ffi_allowlist.write() = allowlist;
    }

    pub fn is_ffi_library_allowed(&self, name: &str) -> bool {
        match &*self.shared.ffi_allowlist.read() {
            None => true,
            Some(allowed) => allowed.contains(name),
        }
    }

    /// Borrow a read-only `(ptr, len)` slice from a rooted `String` or `Bytes` value
    /// inside a non-allocating native FFI call section.
    pub fn raw_ro_slice(&self, val: TaggedValue) -> Result<(*const u8, usize), VmError> {
        if let Some(slice) = bytes_obj::heap_str_slice(val) {
            return Ok((slice.as_ptr(), slice.len()));
        }
        let layout = self.bytes_layout(val)?;
        let slice = layout.as_slice();
        Ok((slice.as_ptr(), slice.len()))
    }

    /// Borrow a writable `(ptr, logical_len, buffer_identity)` tuple from a uniquely-owned
    /// (`share_count == 1`) rooted `Bytes` value inside a non-allocating native FFI call section.
    pub fn raw_mut_bytes_buf(
        &self,
        val: TaggedValue,
    ) -> Result<(*mut u8, usize, *const u8), VmError> {
        let layout = self.bytes_layout(val)?;
        if layout.buffer_payload.is_null() {
            return Ok((
                std::ptr::NonNull::<u8>::dangling().as_ptr(),
                0,
                std::ptr::null(),
            ));
        }
        if layout.share_count() != 1 {
            return Err(VmError::InvalidArgument(
                "mutable FFI Bytes buffer must be uniquely owned before native call",
            ));
        }
        // SAFETY: `bytes_layout` checked `offset + length <= capacity` on a live uniquely-owned `ByteBuffer`.
        let ptr = unsafe { layout.data_ptr.add(layout.offset) };
        Ok((ptr, layout.length, layout.buffer_payload.cast_const()))
    }

    /// Return the configured script command-line arguments.
    pub fn script_args(&self) -> &[String] {
        &self.shared.script_args
    }

    /// Install an embedder [`HostHandler`] invoked by privileged `__host_call` invocations.
    pub fn set_host_handler(&mut self, handler: Box<dyn HostHandler>) {
        *self.shared.host_handler.lock() = Some(handler);
    }

    /// Remove and return the currently installed [`HostHandler`], if any.
    pub fn take_host_handler(&mut self) -> Option<Box<dyn HostHandler>> {
        self.shared.host_handler.lock().take()
    }

    /// Resume a suspended task (`TaskId`) onto the scheduler ready queue.
    pub fn wake_task(&mut self, task_id: TaskId) -> bool {
        let _operation = self.operation();
        self.scheduler().wake(task_id)
    }

    /// Return whether at least one task is currently in the ready queue.
    pub fn has_ready_tasks(&self) -> bool {
        let _operation = self.operation();
        self.scheduler().has_other_ready_tasks()
    }

    /// Return whether any task is currently suspended waiting for wakeup.
    pub fn has_suspended_tasks(&self) -> bool {
        let _operation = self.operation();
        self.scheduler().has_suspended_tasks()
    }

    pub(crate) fn format_backtrace(
        &self,
        task_id: TaskId,
        msg: Option<&str>,
        skip_frames: usize,
    ) -> Result<String, VmError> {
        let task = self.task_ref(task_id).ok_or(VmError::InvalidTask)?;

        let mut frames: Vec<(FuncId, u32)> = Vec::new();
        for segment in task.stacks.active_chain() {
            frames.push((segment.current_func, segment.pc));
            for caller in segment.call_stack.iter().rev() {
                frames.push((caller.func_id, caller.return_pc.saturating_sub(1)));
            }
        }

        let mut out = String::new();
        if let Some(prefix) = msg {
            out.push_str(prefix);
            out.push('\n');
        }
        out.push_str("stack traceback:");

        let total = frames.len();
        for (idx, (func_id, pc)) in frames.into_iter().enumerate().skip(skip_frames) {
            out.push_str("\n\t");
            if idx + 1 == total && func_id.0 == 0 {
                out.push_str(&format!("[fn#0]:pc {pc}: in main chunk"));
            } else if let Some(label) = self.describe_function_frame(func_id) {
                out.push_str(&format!(
                    "[fn#{}]:pc {pc}: in function '{label}'",
                    func_id.0
                ));
            } else {
                out.push_str(&format!(
                    "[fn#{}]:pc {pc}: in function <fn#{}>",
                    func_id.0, func_id.0
                ));
            }
        }
        Ok(out)
    }

    fn describe_function_frame(&self, func_id: FuncId) -> Option<String> {
        let tp = self.type_pool_read();
        for raw in 0..tp.len() {
            let ty = TypeIndex::from_raw(raw as u32);
            for slot in tp.methods_of(ty) {
                if slot.func_id == func_id.0
                    && let Some(method_name) = str_interner::try_get(slot.name)
                {
                    if let Some(owner_name) = tp.display_name(ty) {
                        return Some(format!("{owner_name}.{method_name}"));
                    }
                    return Some(method_name.to_string());
                }
            }
        }
        None
    }

    /// Register a native builtin implementation.
    pub fn register_builtin(&mut self, id: BuiltinFnId, f: BuiltinFn) {
        let _operation = self.operation();
        self.with_exclusive_shared_mut(|shared| {
            shared.builtins.register(id, f);
        });
    }

    /// Enable direct interpreter fast-paths for standard builtins registered during initialization.
    pub fn enable_standard_builtin_fast_paths(&mut self) {
        let _operation = self.operation();
        self.with_exclusive_shared_mut(|shared| {
            shared.builtins.enable_fast_paths();
        });
    }

    /// Add a compiled function to the VM.
    pub fn add_function(&mut self, code: FunctionCode) -> FuncId {
        let _operation = self.operation();
        self.with_exclusive_shared_mut(|shared| {
            let id = shared.bytecode.add_function(code);
            shared.metadata_epoch.fetch_add(1, Ordering::Release);
            id
        })
    }

    /// Maximum UTF-8 byte length of a managed `String` (`1 GiB`).
    pub const MAX_STRING_BYTES: usize = MAX_EXTERNAL_RC_BYTES;

    #[inline]
    pub(crate) fn check_string_bytes(byte_len: usize) -> Result<(), VmError> {
        if byte_len > Self::MAX_STRING_BYTES {
            Err(VmError::ObjectTooLarge)
        } else {
            Ok(())
        }
    }

    #[inline]
    fn inline_string_payload_words(byte_len: usize) -> u16 {
        debug_assert!(byte_len <= INLINE_BYTES_THRESHOLD);
        (8usize + byte_len).div_ceil(8) as u16
    }

    /// Allocate a heap string and return its `TaggedValue`.
    ///
    /// Strings up to [`INLINE_BYTES_THRESHOLD`] (`8,168 B`) stay inline in Immix
    /// (`[len: u64][UTF-8 bytes...]`, `PayloadRole::Ordinary`); larger strings up to
    /// [`Self::MAX_STRING_BYTES`] (`1 GiB`) use a 3-word Immix thin shell backed by an
    /// off-heap reference-counted [`ExternalRcHandle`] (`PayloadRole::ExternalRc`).
    pub(crate) fn alloc_string(&mut self, s: &str) -> Result<TaggedValue, VmError> {
        let bytes = s.as_bytes();
        Self::check_string_bytes(bytes.len())?;
        let str_type = Intrinsic::Str.type_index();
        if bytes.len() <= INLINE_BYTES_THRESHOLD {
            let payload_words = Self::inline_string_payload_words(bytes.len());
            // SAFETY: callers hold an operation, and publish the initialized value
            // before ending it. No lock is held across allocation.
            match unsafe { self.worker.heap.alloc_object(str_type, payload_words) } {
                Some(ptr) => unsafe {
                    *(ptr.as_ptr() as *mut u64) = bytes.len() as u64;
                    std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.as_ptr().add(8), bytes.len());
                    Ok(TaggedValue::from_heap_ptr(ptr.as_ptr()))
                },
                None => Err(VmError::OutOfMemory),
            }
        } else {
            let handle = ExternalRcHandle::from_slice(bytes).ok_or(VmError::OutOfMemory)?;
            // SAFETY: active mutator operation; `handle` is fully initialized.
            match unsafe {
                self.worker
                    .heap
                    .alloc_external_rc(str_type, handle, 0, bytes.len())
            } {
                Some(ptr) => Ok(unsafe { TaggedValue::from_heap_ptr(ptr.as_ptr()) }),
                None => Err(VmError::OutOfMemory),
            }
        }
    }

    pub(crate) fn alloc_string_concat(
        &mut self,
        task_id: TaskId,
        left: TaggedValue,
        right: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let len_a = builtin_ctx::with_heap_string_str(left, str::len).ok_or(VmError::TypeError)?;
        let len_b = builtin_ctx::with_heap_string_str(right, str::len).ok_or(VmError::TypeError)?;
        let total = len_a
            .checked_add(len_b)
            .ok_or(VmError::DisplaySizeExceeded)?;
        self.check_display_bytes(task_id, total)?;
        if len_a == 0 {
            return Ok(right);
        }
        if len_b == 0 {
            return Ok(left);
        }
        Self::check_string_bytes(total)?;
        let str_type = Intrinsic::Str.type_index();
        if total <= INLINE_BYTES_THRESHOLD {
            let payload_words = Self::inline_string_payload_words(total);
            let root = self.worker_roots().temporary_values.borrow().len();
            self.with_temporary_roots(&[left, right], |vm| {
                // SAFETY: active mutator operation; `left` and `right` are rooted in
                // `temporary_values[root..root+2]` across `alloc_object` and reloaded below.
                let ptr = unsafe { vm.worker.heap.alloc_object(str_type, payload_words) }
                    .ok_or(VmError::OutOfMemory)?;
                let (la, rb) = {
                    let roots = vm.worker_roots().temporary_values.borrow();
                    (roots[root], roots[root + 1])
                };
                let pa = la.as_heap_ptr().ok_or(VmError::TypeError)?;
                let pb = rb.as_heap_ptr().ok_or(VmError::TypeError)?;
                // SAFETY: `la` and `rb` are rooted live `Str` objects; `ptr` owns `8 + total` bytes.
                unsafe {
                    let bytes_a = heap_string_bytes(pa).ok_or(VmError::TypeError)?;
                    let bytes_b = heap_string_bytes(pb).ok_or(VmError::TypeError)?;
                    *(ptr.as_ptr() as *mut u64) = total as u64;
                    std::ptr::copy_nonoverlapping(bytes_a.as_ptr(), ptr.as_ptr().add(8), len_a);
                    std::ptr::copy_nonoverlapping(
                        bytes_b.as_ptr(),
                        ptr.as_ptr().add(8 + len_a),
                        len_b,
                    );
                    Ok(TaggedValue::from_heap_ptr(ptr.as_ptr()))
                }
            })
        } else {
            // Copy both slices into the off-heap RC buffer before allocating the Immix shell
            // so `handle` is completely initialized before any GC safepoint inside `alloc_external_rc`.
            // SAFETY: all `total` bytes are overwritten below before publication.
            let handle =
                unsafe { ExternalRcHandle::new_uninit(total) }.ok_or(VmError::OutOfMemory)?;
            let pa = left.as_heap_ptr().ok_or(VmError::TypeError)?;
            let pb = right.as_heap_ptr().ok_or(VmError::TypeError)?;
            // SAFETY: no managed allocation occurred since `left`/`right` validation; `handle` is disjoint.
            unsafe {
                let bytes_a = heap_string_bytes(pa).ok_or(VmError::TypeError)?;
                let bytes_b = heap_string_bytes(pb).ok_or(VmError::TypeError)?;
                let dst = handle.data_mut_ptr();
                std::ptr::copy_nonoverlapping(bytes_a.as_ptr(), dst, len_a);
                std::ptr::copy_nonoverlapping(bytes_b.as_ptr(), dst.add(len_a), len_b);
            }
            // SAFETY: active mutator operation; `handle` is fully initialized.
            let ptr = unsafe {
                self.worker
                    .heap
                    .alloc_external_rc(str_type, handle, 0, total)
            }
            .ok_or(VmError::OutOfMemory)?;
            Ok(unsafe { TaggedValue::from_heap_ptr(ptr.as_ptr()) })
        }
    }

    pub(crate) fn alloc_string_subrange(
        &mut self,
        task_id: TaskId,
        src: TaggedValue,
        start_byte: usize,
        end_byte: usize,
    ) -> Result<TaggedValue, VmError> {
        let src_len = builtin_ctx::with_heap_string_str(src, str::len).ok_or(VmError::TypeError)?;
        let slice_len = end_byte.saturating_sub(start_byte);
        self.check_display_bytes(task_id, slice_len)?;
        if start_byte == 0 && end_byte == src_len {
            return Ok(src);
        }
        Self::check_string_bytes(slice_len)?;
        let str_type = Intrinsic::Str.type_index();
        if slice_len <= INLINE_BYTES_THRESHOLD {
            let payload_words = Self::inline_string_payload_words(slice_len);
            let root = self.worker_roots().temporary_values.borrow().len();
            self.with_temporary_roots(&[src], |vm| {
                // SAFETY: active mutator operation; `src` is rooted across `alloc_object`.
                let ptr = unsafe { vm.worker.heap.alloc_object(str_type, payload_words) }
                    .ok_or(VmError::OutOfMemory)?;
                let reloaded = vm.worker_roots().temporary_values.borrow()[root];
                let ps = reloaded.as_heap_ptr().ok_or(VmError::TypeError)?;
                // SAFETY: `[start_byte..end_byte]` was verified on UTF-8 character boundaries of `src`.
                unsafe {
                    let src_bytes = heap_string_bytes(ps).ok_or(VmError::TypeError)?;
                    *(ptr.as_ptr() as *mut u64) = slice_len as u64;
                    if slice_len > 0 {
                        std::ptr::copy_nonoverlapping(
                            src_bytes.as_ptr().add(start_byte),
                            ptr.as_ptr().add(8),
                            slice_len,
                        );
                    }
                    Ok(TaggedValue::from_heap_ptr(ptr.as_ptr()))
                }
            })
        } else {
            let ps = src.as_heap_ptr().ok_or(VmError::TypeError)?;
            // If `src` is already an `ExternalRc` string and the slice retains at least 1/4 of
            // the backing capacity, share the `ExternalRcHandle` in O(1) with zero copying!
            // Otherwise allocate a right-sized `ExternalRcHandle` to avoid pinning huge buffers.
            // SAFETY: `ps` is a live `Str` payload within an active mutator operation.
            let handle_and_offset = unsafe {
                if let Some((handle, base_offset, _)) = external_rc_clone_view(ps)
                    && slice_len >= handle.capacity() / 4
                {
                    Some((handle, base_offset + start_byte))
                } else {
                    let src_bytes = heap_string_bytes(ps).ok_or(VmError::TypeError)?;
                    let copied = ExternalRcHandle::from_slice(&src_bytes[start_byte..end_byte])
                        .ok_or(VmError::OutOfMemory)?;
                    Some((copied, 0usize))
                }
            };
            let (handle, new_offset) = handle_and_offset.expect("checked Some");
            // SAFETY: active mutator operation; `handle` covers `new_offset..(new_offset + slice_len)`.
            let ptr = unsafe {
                self.worker
                    .heap
                    .alloc_external_rc(str_type, handle, new_offset, slice_len)
            }
            .ok_or(VmError::OutOfMemory)?;
            Ok(unsafe { TaggedValue::from_heap_ptr(ptr.as_ptr()) })
        }
    }

    /// Spawn a root task that starts executing the given function.
    pub fn spawn_root(&mut self, func_id: FuncId) -> TaskId {
        let _operation = self.operation();
        let task = self.scheduler().spawn(func_id, None);
        if self.shared.pending_root_entries.lock().insert(task) {
            self.shared
                .pending_root_count
                .fetch_add(1, Ordering::Release);
        }
        task
    }

    /// Run the VM until all runnable tasks finish, an uncaught root error occurs, or deadlock is detected.
    pub fn run(&mut self) -> VmResult {
        // Validate upfront that no foreign VM holds an active operation on this thread,
        // then drop the session so Worker #0 holds no active mutator session while parked.
        drop(match self.execution_operation() {
            Ok(operation) => operation,
            Err(error) => return VmResult::Error(error),
        });

        self.scheduler().clear_abort();
        let wake_immediately =
            self.shared.configured_workers > 1 && self.scheduler().ready_count() > 1;
        self.shared.epoch_sync.begin_run_epoch(wake_immediately);
        if wake_immediately {
            self.ensure_aux_workers_started();
        }

        let primary_exit = self.run_worker_loop();
        if primary_exit == WorkerLoopExit::Deadlock {
            self.shared.epoch_sync.record_deadlock();
        }

        let (first_error, saw_deadlock) = self.shared.epoch_sync.end_run_epoch();
        self.scheduler().clear_abort();

        if let Some(error) = first_error {
            if let Ok(_op) = self.execution_operation() {
                self.drain_cancelled_tasks_on_abort();
            }
            return VmResult::Error(error);
        }
        if saw_deadlock {
            return VmResult::Error(VmError::DeadlockDetected);
        }

        let had_uncollected = self
            .shared
            .roots
            .uncollected_terminal_promises
            .swap(0, Ordering::AcqRel)
            > 0;
        if (had_uncollected || self.shared.roots.promises.len() > 0)
            && let Ok(_op) = self.execution_operation()
        {
            // SAFETY: All workers have exited the run epoch and all live values reside in VM roots.
            unsafe {
                self.worker.heap.collect_garbage();
            }
        }
        VmResult::Finished
    }

    fn run_worker_loop(&mut self) -> WorkerLoopExit {
        let shared = Arc::clone(&self.shared);
        let lease = shared
            .roots
            .scheduler
            .lease_worker_queue(self.worker.worker_id);
        let queue_ptr = NonNull::from(lease.queue());
        self.worker.local_queue_ptr = Some(queue_ptr);
        let guard = ScopedWorkerQueueGuard { vm: self };
        let mut tick: u64 = guard.vm.worker.worker_id as u64;
        let mut active_op: Option<gc::MutatorSession<'static>> = None;

        loop {
            if active_op.is_some() {
                guard.vm.worker.heap.safe_point();
            }
            let task_id = match shared.roots.scheduler.try_claim_for_worker(
                guard.vm.worker.worker_id,
                lease.queue(),
                &mut tick,
            ) {
                Some(id) => id,
                None => {
                    // Release mutator session before spinning/parking in `next_for_worker`
                    // so STW GC never waits on a sleeping worker.
                    active_op = None;
                    let next = shared.roots.scheduler.next_for_worker(
                        guard.vm.worker.worker_id,
                        lease.queue(),
                        &mut tick,
                    );
                    match next {
                        WorkerNext::Task(id) => id,
                        WorkerNext::AllFinished => return WorkerLoopExit::AllFinished,
                        WorkerNext::Deadlock => return WorkerLoopExit::Deadlock,
                        WorkerNext::Aborted => return WorkerLoopExit::Aborted,
                    }
                }
            };

            if active_op.is_none() {
                match guard.vm.execution_operation() {
                    Ok(op) => active_op = Some(op),
                    Err(err) => {
                        shared.roots.scheduler.finish(task_id);
                        shared.epoch_sync.record_error(err);
                        shared.roots.scheduler.request_abort();
                        return WorkerLoopExit::Aborted;
                    }
                }
            }

            let prev_reclaimed = shared
                .roots
                .uncollected_terminal_promises
                .load(Ordering::Relaxed);
            if prev_reclaimed >= 32_768
                && shared
                    .roots
                    .uncollected_terminal_promises
                    .compare_exchange(prev_reclaimed, 0, Ordering::AcqRel, Ordering::Relaxed)
                    .is_ok()
            {
                // SAFETY: Between task slices, `task_id` and all other tasks hold all managed
                // references in registered `VmRoots` (`TaskArena`, globals, constants, worker_roots).
                unsafe {
                    guard.vm.worker.heap.collect_garbage();
                }
            }

            let is_pending_root = shared.pending_root_count.load(Ordering::Acquire) > 0 && {
                let removed = shared.pending_root_entries.lock().remove(&task_id);
                if removed {
                    shared.pending_root_count.fetch_sub(1, Ordering::AcqRel);
                }
                removed
            };
            if is_pending_root {
                let function = guard
                    .vm
                    .task_ref(task_id)
                    .expect("scheduled task exists")
                    .current_func;
                if let Err(error) = guard
                    .vm
                    .check_entry_environment(function, None)
                    .and_then(|()| guard.vm.check_user_argument_abi(function, 0))
                {
                    shared.roots.scheduler.finish(task_id);
                    shared.epoch_sync.record_error(error);
                    shared.roots.scheduler.request_abort();
                    return WorkerLoopExit::Aborted;
                }
            }

            let result = guard.vm.execute_task(task_id);
            match result {
                ExecOutcome::Finished => {
                    let (spawned_promise, val) = guard
                        .vm
                        .task_ref(task_id)
                        .map(|t| (t.spawned_promise, t.registers.get(Reg(0))))
                        .unwrap_or((None, TaggedValue::UNIT));
                    if let Some(fid) = spawned_promise {
                        guard.vm.complete_promise(fid, val);
                        shared.roots.scheduler.reap_task(task_id);
                    } else {
                        shared.roots.scheduler.finish(task_id);
                    }
                }
                ExecOutcome::Error(e) => {
                    let spawned_promise = shared.roots.scheduler.task_spawned_promise(task_id);
                    let children = shared.roots.scheduler.task_children(task_id);
                    for child in children {
                        guard.vm.request_cancel_task_subtree(child);
                    }
                    if let Some(fid) = spawned_promise {
                        guard.vm.fail_promise(fid, e);
                        shared.roots.scheduler.reap_task(task_id);
                    } else {
                        shared.roots.scheduler.finish(task_id);
                        shared.epoch_sync.record_error(e);
                        shared.roots.scheduler.request_abort();
                        return WorkerLoopExit::Aborted;
                    }
                }
                ExecOutcome::Yield => {
                    shared.roots.scheduler.yield_task(task_id);
                }
                ExecOutcome::Suspend => {
                    shared
                        .roots
                        .scheduler
                        .suspend_running(Some(lease.queue()), task_id);
                }
                ExecOutcome::Cancelled => {
                    let spawned_promise = shared.roots.scheduler.task_spawned_promise(task_id);
                    if let Some(fid) = spawned_promise {
                        guard.vm.finish_cancelled_promise(fid);
                        shared.roots.scheduler.reap_task(task_id);
                    } else {
                        shared.roots.scheduler.finish(task_id);
                    }
                }
            }
        }
    }

    fn drain_cancelled_tasks_on_abort(&mut self) {
        let remaining = self.scheduler().active_task_ids();
        for tid in remaining {
            self.request_cancel_task_subtree(tid);
        }
        while let Some(task_id) = self.scheduler().next_ready() {
            self.scheduler().set_running(task_id);
            match self.execute_task(task_id) {
                ExecOutcome::Finished => {
                    let (spawned_promise, val) = self
                        .task_ref(task_id)
                        .map(|t| (t.spawned_promise, t.registers.get(Reg(0))))
                        .unwrap_or((None, TaggedValue::UNIT));
                    if let Some(fid) = spawned_promise {
                        self.complete_promise(fid, val);
                        self.scheduler().reap_task(task_id);
                    } else {
                        self.scheduler().finish(task_id);
                    }
                }
                ExecOutcome::Cancelled => {
                    let spawned_promise = self.scheduler().task_spawned_promise(task_id);
                    if let Some(fid) = spawned_promise {
                        self.finish_cancelled_promise(fid);
                        self.scheduler().reap_task(task_id);
                    } else {
                        self.scheduler().finish(task_id);
                    }
                }
                ExecOutcome::Error(err) => {
                    let spawned_promise = self.scheduler().task_spawned_promise(task_id);
                    let children = self.scheduler().task_children(task_id);
                    for child in children {
                        self.request_cancel_task_subtree(child);
                    }
                    if let Some(fid) = spawned_promise {
                        self.fail_promise(fid, err);
                        self.scheduler().reap_task(task_id);
                    } else {
                        self.scheduler().finish(task_id);
                    }
                }
                ExecOutcome::Yield => {
                    self.scheduler().yield_task(task_id);
                }
                ExecOutcome::Suspend => {
                    self.scheduler().suspend(task_id);
                }
            }
        }
    }

    /// Execute one task until it finishes, suspends, errors, or yields.
    fn execute_task(&mut self, task_id: TaskId) -> ExecOutcome {
        const SLICE_SAFEPOINT_BUDGET: u32 = 32_768;
        self.worker.slice_safepoints = SLICE_SAFEPOINT_BUDGET;
        self.shared.worker_slices[self.worker.worker_id]
            .0
            .fetch_add(1, Ordering::Relaxed);
        if self.scheduler().is_cancelled(task_id)
            && let Some(task) = self.task_mut(task_id)
        {
            task.cancelled = true;
        }
        // Bytecode is immutable while a task runs, so a function's checked
        // entry contract only needs revalidation when control changes function.
        let mut validated_func = None;
        let mut last_entered_frame: Option<(FuncId, usize, usize)> = None;
        let jit_enabled = self.worker.jit.is_enabled();
        self.worker.heap.safe_point();
        loop {
            if GC_FLAGS.gc_requested.load(Ordering::Relaxed) {
                self.worker.heap.safe_point();
            }
            // Get current task's PC, function, active frame identity, and cooperative unwind flags.
            let (mut pc, func_id, is_cancelled, unwinding_cancel, paused_in_unwind_step, frame_sig) = {
                let task = match self.task_ref(task_id) {
                    Some(t) => t,
                    None => return ExecOutcome::Error(VmError::InvalidTask),
                };
                let ctx_addr = std::ptr::from_ref::<runtime::StackContext>(task) as usize;
                (
                    task.pc,
                    task.current_func,
                    task.cancelled,
                    task.unwinding_cancel,
                    task.paused_in_unwind_step,
                    (task.current_func, task.call_stack.len(), ctx_addr),
                )
            };
            if paused_in_unwind_step {
                if let Some(task) = self.task_mut(task_id) {
                    task.paused_in_unwind_step = false;
                }
                match self.step_cancel_unwind(task_id) {
                    futures::CancelUnwindAction::ContinueBytecode => {
                        validated_func = None;
                        continue;
                    }
                    futures::CancelUnwindAction::Suspend => return ExecOutcome::Suspend,
                    futures::CancelUnwindAction::Done => return ExecOutcome::Cancelled,
                }
            }
            if is_cancelled && !unwinding_cancel {
                self.shared.roots.scheduler.cancel_task_waiters(task_id);
                if let Some(task) = self.task_mut(task_id) {
                    task.unwinding_cancel = true;
                }
                match self.step_cancel_unwind(task_id) {
                    futures::CancelUnwindAction::ContinueBytecode => {
                        validated_func = None;
                        continue;
                    }
                    futures::CancelUnwindAction::Suspend => return ExecOutcome::Suspend,
                    futures::CancelUnwindAction::Done => return ExecOutcome::Cancelled,
                }
            }

            let checked = if validated_func == Some(func_id) {
                self.shared
                    .bytecode
                    .try_get_function(func_id)
                    .ok_or(VmError::InvalidFunction(func_id))
            } else {
                self.checked_function(func_id)
            };
            let func_len = match checked {
                Ok(function) => function.instructions.len(),
                Err(error) => return ExecOutcome::Error(error),
            };
            validated_func = Some(func_id);
            if pc as usize >= func_len {
                return ExecOutcome::Finished;
            }

            if jit_enabled && !unwinding_cancel {
                if last_entered_frame != Some(frame_sig) {
                    last_entered_frame = Some(frame_sig);
                    if pc == 0 {
                        self.maybe_compile_on_hotness(func_id, jit::ENTRY_HOTNESS_INCREMENT);
                    }
                }

                if let Some(jit_outcome) = self.try_run_jit(task_id, func_id, pc) {
                    match jit_outcome {
                        jit_bridge::JitStepOutcome::Finished => return ExecOutcome::Finished,
                        jit_bridge::JitStepOutcome::Return { ret_pc, val } => {
                            if let Some(outcome) =
                                self.complete_return(task_id, ret_pc, val, &mut validated_func)
                            {
                                return outcome;
                            }
                            continue;
                        }
                        jit_bridge::JitStepOutcome::TierUp(osr_pc) => {
                            self.compile_jit_function(func_id, true);
                            if let Some(task) = self.task_mut(task_id) {
                                task.pc = osr_pc;
                            }
                            continue;
                        }
                        jit_bridge::JitStepOutcome::Yield(resume_pc) => {
                            if let Some(task) = self.task_mut(task_id) {
                                task.pc = resume_pc;
                            }
                            return ExecOutcome::Yield;
                        }
                        jit_bridge::JitStepOutcome::SuspendRetry(retry_pc) => {
                            if let Some(task) = self.task_mut(task_id) {
                                task.pc = retry_pc;
                            }
                            return ExecOutcome::Suspend;
                        }
                        jit_bridge::JitStepOutcome::ContinueTaskLoop => {
                            validated_func = None;
                            continue;
                        }
                        jit_bridge::JitStepOutcome::YieldAtTaskPc => {
                            return ExecOutcome::Yield;
                        }
                        jit_bridge::JitStepOutcome::SuspendAtTaskPc => {
                            return ExecOutcome::Suspend;
                        }
                        jit_bridge::JitStepOutcome::Cancelled => {
                            if let Some(task) = self.task_mut(task_id) {
                                task.cancelled = true;
                            }
                            continue;
                        }
                        jit_bridge::JitStepOutcome::Error(err) => return ExecOutcome::Error(err),
                        jit_bridge::JitStepOutcome::InterpretOne(interp_pc) => {
                            pc = interp_pc;
                            if pc as usize >= func_len {
                                return ExecOutcome::Finished;
                            }
                        }
                    }
                }
            }

            let instr = match self.shared.bytecode.get_meta(func_id).decoded[pc as usize] {
                Some(i) => i,
                None => {
                    let instr_word =
                        self.shared.bytecode.get_function(func_id).instructions[pc as usize];
                    return ExecOutcome::Error(VmError::InvalidInstruction(instr_word));
                }
            };

            // Advance PC before executing (jumps will override).
            if let Some(task) = self.task_mut(task_id) {
                task.pc = pc + 1;
            }

            match self.dispatch(task_id, instr) {
                DispatchResult::Continue => {}
                DispatchResult::Return(val) => {
                    if let Some(outcome) =
                        self.complete_return(task_id, pc, val, &mut validated_func)
                    {
                        return outcome;
                    }
                }
                DispatchResult::Yield => return ExecOutcome::Yield,
                DispatchResult::SuspendRetry => {
                    if let Some(task) = self.task_mut(task_id) {
                        task.pc = pc;
                    }
                    return ExecOutcome::Suspend;
                }
                DispatchResult::StartCancelUnwind => {
                    self.shared.roots.scheduler.cancel_task_waiters(task_id);
                    if let Some(task) = self.task_mut(task_id) {
                        task.unwinding_cancel = true;
                    }
                    match self.step_cancel_unwind(task_id) {
                        futures::CancelUnwindAction::ContinueBytecode => {
                            validated_func = None;
                        }
                        futures::CancelUnwindAction::Suspend => return ExecOutcome::Suspend,
                        futures::CancelUnwindAction::Done => return ExecOutcome::Cancelled,
                    }
                }
                DispatchResult::ResumeCancelUnwind => {
                    if let Some(task) = self.task_mut(task_id) {
                        task.is_unwind_target = false;
                    }
                    match self.step_cancel_unwind(task_id) {
                        futures::CancelUnwindAction::ContinueBytecode => {
                            validated_func = None;
                        }
                        futures::CancelUnwindAction::Suspend => return ExecOutcome::Suspend,
                        futures::CancelUnwindAction::Done => return ExecOutcome::Cancelled,
                    }
                }
                DispatchResult::Error(e) => return ExecOutcome::Error(e),
            }
        }
    }

    pub(crate) fn complete_return(
        &mut self,
        task_id: TaskId,
        ret_pc: u32,
        val: TaggedValue,
        validated_func: &mut Option<FuncId>,
    ) -> Option<ExecOutcome> {
        let return_inside_unwind_defer = self
            .task_ref(task_id)
            .is_some_and(|t| t.unwinding_cancel && t.is_unwind_target);
        if return_inside_unwind_defer {
            if let Some(task) = self.task_mut(task_id) {
                task.is_unwind_target = false;
            }
            return match self.step_cancel_unwind(task_id) {
                futures::CancelUnwindAction::ContinueBytecode => {
                    *validated_func = None;
                    None
                }
                futures::CancelUnwindAction::Suspend => Some(ExecOutcome::Suspend),
                futures::CancelUnwindAction::Done => Some(ExecOutcome::Cancelled),
            };
        }
        if let Err(error) = self.check_display_return(task_id, val) {
            return Some(ExecOutcome::Error(error));
        }
        let at_root = self
            .task_ref(task_id)
            .is_some_and(|t| t.call_stack.is_empty() && t.stacks.is_at_task_root());
        if at_root {
            match self.check_task_root_return(task_id, val) {
                Ok(Some(_)) => {
                    if let Some(task) = self.task_mut(task_id) {
                        task.pc = ret_pc;
                    }
                    return Some(ExecOutcome::Suspend);
                }
                Ok(None) => {}
                Err(err) => return Some(ExecOutcome::Error(err)),
            }
        }
        // Pop call frame.
        let mut recycled_slots = None;
        let should_finish = {
            let task = self.task_mut(task_id).unwrap();
            if let Some(frame) = task.call_stack.pop() {
                task.pc = frame.return_pc;
                task.current_func = frame.func_id;
                recycled_slots = Some(std::mem::replace(&mut task.local_slots, frame.local_slots));
                task.display_state = frame.display_state;
                task.unwind_stack = frame.unwind_stack;
                task.is_unwind_target = frame.is_unwind_target;
                if !frame.saved_regs.is_empty() {
                    for (reg, saved_val) in &frame.saved_regs {
                        task.registers.set(*reg, *saved_val);
                    }
                }
                task.registers.set(Reg(0), val);
                false
            } else if task.stacks.return_from_delimiter(val) {
                false
            } else {
                task.registers.regs.fill(TaggedValue::UNIT);
                task.registers.set(Reg(0), val);
                recycled_slots = Some(std::mem::take(&mut task.local_slots));
                task.display_state = None;
                task.unwind_stack.clear();
                true
            }
        };
        if let Some(old) = recycled_slots {
            self.recycle_frame_slots(old);
        }
        if should_finish {
            Some(ExecOutcome::Finished)
        } else {
            None
        }
    }

    #[inline]
    fn poll_safepoint(&mut self, task_id: TaskId) -> DispatchResult {
        self.worker.heap.safe_point();
        let slot_cancelled = self.scheduler().is_cancelled(task_id);
        let should_unwind = if let Some(task) = self.task_mut(task_id) {
            if slot_cancelled {
                task.cancelled = true;
            }
            task.cancelled && !task.unwinding_cancel
        } else {
            false
        };
        if should_unwind {
            return DispatchResult::StartCancelUnwind;
        }
        if self.scheduler().is_aborted() || GC_FLAGS.preempt_requested.load(Ordering::Relaxed) {
            return DispatchResult::Yield;
        }
        self.worker.slice_safepoints = self.worker.slice_safepoints.saturating_sub(1);
        if self.worker.slice_safepoints == 0 {
            self.worker.slice_safepoints = 4096;
            if self.scheduler().has_other_ready_tasks() {
                return DispatchResult::Yield;
            }
        }
        DispatchResult::Continue
    }

    #[inline]
    fn cached_has_scopes(&self) -> bool {
        if let Some(v) = self.sync_meta_cache().has_scopes {
            return v;
        }
        let has = !self.type_pool_read().scopes().is_empty();
        self.sync_meta_cache().has_scopes = Some(has);
        has
    }

    /// Dispatch a single instruction.
    fn dispatch(&mut self, task_id: TaskId, instr: Instruction) -> DispatchResult {
        let scope = if nsbc::instruction_requires_scope(instr.opcode) {
            let found = self.task_ref(task_id).and_then(|task| {
                task.pc.checked_sub(1).and_then(|pc| {
                    self.shared
                        .method_call_scopes
                        .get(&(task.current_func, pc as usize))
                        .copied()
                })
            });
            if found.is_none() && self.cached_has_scopes() {
                return DispatchResult::Error(VmError::MissingMethodContext);
            }
            found
        } else {
            None
        };
        let previous = self.worker.type_query_scope;
        self.worker.type_query_scope = scope;
        let result = self.dispatch_in_scope(task_id, instr);
        self.worker.type_query_scope = previous;
        result
    }

    fn dispatch_in_scope(&mut self, task_id: TaskId, instr: Instruction) -> DispatchResult {
        macro_rules! task {
            () => {
                self.task_mut(task_id).unwrap()
            };
        }

        match instr.opcode {
            // ── Arithmetic (R-type) ────────────────────────────────
            Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::Div
            | Opcode::Mod
            | Opcode::Neg
            | Opcode::BitAnd
            | Opcode::BitOr
            | Opcode::BitXor
            | Opcode::BitNot
            | Opcode::Shl
            | Opcode::Shr
            | Opcode::UShr => self.dispatch_numeric(task_id, instr),
            Opcode::CmpEq
            | Opcode::CmpNe
            | Opcode::CmpLt
            | Opcode::CmpLe
            | Opcode::CmpGt
            | Opcode::CmpGe => self.dispatch_comparison(task_id, instr),

            // ── Register manipulation (R-type) ────────────────────
            Opcode::Mov => {
                let (dst, s1, _) = r_regs(&instr);
                let t = task!();
                let val = t.registers.get(s1);
                t.registers.set(dst, val);
                DispatchResult::Continue
            }
            Opcode::Swap => {
                let (dst, s1, _) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(dst);
                let b = t.registers.get(s1);
                t.registers.set(dst, b);
                t.registers.set(s1, a);
                DispatchResult::Continue
            }

            // ── Constants & types (A-type) ────────────────────────
            Opcode::LoadSlot | Opcode::StoreSlot => {
                let (register, high, low) = a_fields(&instr);
                let index = ((high.0 as usize) << 12) | low as usize;
                let task = task!();
                if index >= task.local_slots.len() {
                    return DispatchResult::Error(VmError::InvalidSlot(index as u32));
                }
                if instr.opcode == Opcode::LoadSlot {
                    let value = task.local_slots[index];
                    task.registers.set(register, value);
                } else {
                    task.local_slots[index] = task.registers.get(register);
                }
                DispatchResult::Continue
            }
            Opcode::AllocateSlots => {
                let count = e_payload(&instr);
                if count > (1 << 17) {
                    return DispatchResult::Error(VmError::InvalidInstruction(instr.encode()));
                }
                match self.alloc_task_frame_slots(task_id, count as usize) {
                    Ok(()) => DispatchResult::Continue,
                    Err(err) => DispatchResult::Error(err),
                }
            }
            Opcode::Load => {
                let (dst, _, imm12) = a_fields(&instr);
                let val = match instr.amode {
                    AddrMode::Imm => TaggedValue::from_i64(Instruction::sext_imm12(imm12) as i64),
                    AddrMode::Const => match self.constant_value(imm12 as u32) {
                        Ok(value) => value,
                        Err(error) => return DispatchResult::Error(error),
                    },
                };
                if val.as_enum().is_some()
                    && let Err(error) = self.enum_layout(val)
                {
                    return DispatchResult::Error(error);
                }
                task!().registers.set(dst, val);
                DispatchResult::Continue
            }
            Opcode::LoadConstWide => {
                let (dst, base, imm12) = a_fields(&instr);
                let idx = Instruction::wide_const_index(base, imm12);
                let val = match self.constant_value(idx) {
                    Ok(value) => value,
                    Err(error) => return DispatchResult::Error(error),
                };
                if val.as_enum().is_some()
                    && let Err(error) = self.enum_layout(val)
                {
                    return DispatchResult::Error(error);
                }
                task!().registers.set(dst, val);
                DispatchResult::Continue
            }
            Opcode::LoadUnit => {
                let (dst, _, _) = a_fields(&instr);
                task!().registers.set(dst, TaggedValue::UNIT);
                DispatchResult::Continue
            }
            Opcode::LoadTrue => {
                let (dst, _, _) = a_fields(&instr);
                task!().registers.set(dst, TaggedValue::TRUE);
                DispatchResult::Continue
            }
            Opcode::LoadFalse => {
                let (dst, _, _) = a_fields(&instr);
                task!().registers.set(dst, TaggedValue::FALSE);
                DispatchResult::Continue
            }
            Opcode::LoadNull => {
                let (dst, _, _) = a_fields(&instr);
                task!().registers.set(dst, TaggedValue::NULL);
                DispatchResult::Continue
            }
            Opcode::TypeCheck | Opcode::TypeCast | Opcode::TypeCastSafe | Opcode::TypeAssert => {
                self.dispatch_type_operation(task_id, instr)
            }

            // ── Memory access (A-type) ────────────────────────────
            Opcode::LoadField => {
                let (dst, source, index) = a_fields(&instr);
                let object = task!().registers.get(source);
                match self.load_field_value(object, index as u32) {
                    Ok(value) => task!().registers.set(dst, value),
                    Err(error) => return DispatchResult::Error(error),
                }
                DispatchResult::Continue
            }
            Opcode::StoreField => {
                let (source, destination, index) = a_fields(&instr);
                let object = task!().registers.get(destination);
                let value = task!().registers.get(source);
                match self.store_field_value(object, index as u32, value) {
                    Ok(()) => DispatchResult::Continue,
                    Err(error) => DispatchResult::Error(error),
                }
            }
            Opcode::LoadIndex => {
                let (dst, object, index) = a_fields(&instr);
                let list = task!().registers.get(object);
                if index >= runtime::GP_REGISTER_COUNT as u16 {
                    return DispatchResult::Error(VmError::InvalidIndex);
                }
                let Ok(index) = u8::try_from(index) else {
                    return DispatchResult::Error(VmError::InvalidIndex);
                };
                let index = task!().registers.get(Reg(index));
                match self.collection_get(list, index) {
                    Ok(value) => {
                        task!().registers.set(dst, value);
                        DispatchResult::Continue
                    }
                    Err(error) => DispatchResult::Error(error),
                }
            }
            Opcode::StoreIndex => {
                let (source, object, index) = a_fields(&instr);
                let list = task!().registers.get(object);
                if index >= runtime::GP_REGISTER_COUNT as u16 {
                    return DispatchResult::Error(VmError::InvalidIndex);
                }
                let Ok(index) = u8::try_from(index) else {
                    return DispatchResult::Error(VmError::InvalidIndex);
                };
                let index = task!().registers.get(Reg(index));
                let value = task!().registers.get(source);
                match self.collection_set(list, index, value) {
                    Ok(()) => DispatchResult::Continue,
                    Err(error) => DispatchResult::Error(error),
                }
            }
            Opcode::LoadGlobal | Opcode::LoadGlobalWide => {
                let (dst, base, imm12) = a_fields(&instr);
                if instr.opcode == Opcode::LoadGlobal && base.0 != 0 {
                    return DispatchResult::Error(VmError::InvalidInstruction(instr.encode()));
                }
                let gidx = if instr.opcode == Opcode::LoadGlobalWide {
                    Instruction::wide_const_index(base, imm12)
                } else {
                    imm12 as u32
                };
                let val = match self.shared.roots.globals.get(gidx) {
                    Ok(value) => value,
                    Err(error) => return DispatchResult::Error(error.into()),
                };
                task!().registers.set(dst, val);
                DispatchResult::Continue
            }
            Opcode::StoreGlobal | Opcode::StoreGlobalWide => {
                let (dst, base, imm12) = a_fields(&instr);
                if instr.opcode == Opcode::StoreGlobal && base.0 != 0 {
                    return DispatchResult::Error(VmError::InvalidInstruction(instr.encode()));
                }
                let gidx = if instr.opcode == Opcode::StoreGlobalWide {
                    Instruction::wide_const_index(base, imm12)
                } else {
                    imm12 as u32
                };
                let val = task!().registers.get(dst);
                if let Err(error) = self.store_global(gidx, val) {
                    return DispatchResult::Error(error);
                }
                DispatchResult::Continue
            }

            // ── Object creation (A-type) ──────────────────────────
            Opcode::NewObject => {
                let (dst, _, imm12) = a_fields(&instr);
                let (type_idx, words) = {
                    let tp = self.type_pool_read();
                    let Some(type_idx) = tp.canonical_type(TypeIndex::from_raw(imm12 as u32))
                    else {
                        return DispatchResult::Error(VmError::TypeError);
                    };
                    if tp.is_reserved_collection_role(type_idx) {
                        return DispatchResult::Error(VmError::TypeError);
                    }
                    let words = match &tp.get(type_idx).kind {
                        type_pool::TypeKind::Struct { fields, .. } => fields.len(),
                        type_pool::TypeKind::Tuple { elements } => elements.len(),
                        type_pool::TypeKind::Newtype { .. } => 1,
                        _ => return DispatchResult::Error(VmError::TypeError),
                    };
                    if words > MAX_LARGE_TAGGED_WORDS {
                        return DispatchResult::Error(VmError::ObjectTooLarge);
                    }
                    (type_idx, words)
                };
                // SAFETY: slice owns an execution operation, no lock is held across allocation,
                // and all payload slots are initialized before publishing to `dst`.
                match unsafe { self.worker.heap.alloc_tagged_words(type_idx, words) } {
                    Some(ptr) => {
                        for index in 0..words {
                            unsafe {
                                ptr.as_ptr()
                                    .cast::<u64>()
                                    .add(index)
                                    .write(TaggedValue::UNIT.raw());
                            }
                        }
                        let val = unsafe { TaggedValue::from_heap_ptr(ptr.as_ptr()) };
                        task!().registers.set(dst, val);
                    }
                    None => {
                        return DispatchResult::Error(VmError::OutOfMemory);
                    }
                }
                DispatchResult::Continue
            }
            Opcode::ErrorOk | Opcode::ErrorErr | Opcode::ErrorIsOk | Opcode::ErrorPayload => {
                let (destination, source, index) = a_fields(&instr);
                if instr.amode != AddrMode::Imm {
                    return DispatchResult::Error(VmError::TypeError);
                }
                let value = task!().registers.get(source);
                let result = match instr.opcode {
                    Opcode::ErrorOk | Opcode::ErrorErr => self
                        .constant_value(index as u32)
                        .and_then(|constant| match constant.as_type() {
                            Some(target) => {
                                self.construct_error(value, target, instr.opcode == Opcode::ErrorOk)
                            }
                            None => Err(VmError::TypeError),
                        }),
                    _ if index != 0 => Err(VmError::TypeError),
                    _ => self.error_layout(value, 0).and_then(|layout| {
                        let layout = layout.ok_or(VmError::TypeError)?;
                        Ok(if instr.opcode == Opcode::ErrorIsOk {
                            TaggedValue::from_bool(layout.tag == type_pool::TypeId::ZERO)
                        } else {
                            layout.payload
                        })
                    }),
                };
                match result {
                    Ok(value) => {
                        task!().registers.set(destination, value);
                        DispatchResult::Continue
                    }
                    Err(error) => DispatchResult::Error(error),
                }
            }
            Opcode::NewEnum | Opcode::EnumIs | Opcode::EnumField => {
                let (destination, source, index) = a_fields(&instr);
                let value = task!().registers.get(source);
                let result = match instr.opcode {
                    Opcode::NewEnum => self
                        .constant_value(index as u32)
                        .and_then(|constant| self.new_enum(constant, value)),
                    Opcode::EnumIs => self
                        .constant_value(index as u32)
                        .and_then(|constant| self.enum_is(value, constant))
                        .map(TaggedValue::from_bool),
                    _ => self.enum_field(value, index as u32),
                };
                match result {
                    Ok(value) => {
                        task!().registers.set(destination, value);
                        DispatchResult::Continue
                    }
                    Err(error) => DispatchResult::Error(error),
                }
            }
            Opcode::MatchFail => {
                if e_payload(&instr) == 0 {
                    DispatchResult::Error(VmError::NoMatchingCase)
                } else {
                    DispatchResult::Error(VmError::InvalidInstruction(instr.encode()))
                }
            }
            Opcode::NewList => {
                let (dst, _, length) = a_fields(&instr);
                match self.new_list(length as usize) {
                    Ok(value) => {
                        task!().registers.set(dst, value);
                        DispatchResult::Continue
                    }
                    Err(error) => DispatchResult::Error(error),
                }
            }
            Opcode::NewMap => {
                let (dst, base, capacity) = a_fields(&instr);
                if base.0 != 0 {
                    return DispatchResult::Error(VmError::InvalidInstruction(instr.encode()));
                }
                match self.new_map(capacity as usize) {
                    Ok(value) => {
                        task!().registers.set(dst, value);
                        DispatchResult::Continue
                    }
                    Err(error) => DispatchResult::Error(error),
                }
            }
            Opcode::NewClosure => {
                let (dst, base, imm12) = a_fields(&instr);
                let func_id = FuncId(imm12 as u32);
                let capture_count = base.0 as u16;
                self.exec_new_closure(task_id, dst, func_id, capture_count)
            }
            Opcode::NewClosureWide => {
                let (dst, base, imm12) = a_fields(&instr);
                let capture_count = base.0 as u16;
                let func_id = match self.constant_u32(imm12 as u32) {
                    Ok(id) => FuncId(id),
                    Err(error) => return DispatchResult::Error(error),
                };
                self.exec_new_closure(task_id, dst, func_id, capture_count)
            }

            // ── Control flow (J-type) ─────────────────────────────
            Opcode::Jmp | Opcode::JmpFar => {
                let (_, offset) = j_fields(&instr);
                let cur_fn = {
                    let t = task!();
                    t.pc = ((t.pc as i64) + (offset as i64) - 1) as u32;
                    t.current_func
                };
                if offset <= 0 {
                    self.maybe_compile_on_hotness(cur_fn, jit::BACKEDGE_HOTNESS_INCREMENT);
                    return self.poll_safepoint(task_id);
                }
                DispatchResult::Continue
            }
            Opcode::JmpIf => {
                let (cond, offset) = j_fields(&instr);
                let taken_fn = {
                    let t = task!();
                    let val = t.registers.get(cond);
                    if is_truthy(val) {
                        t.pc = ((t.pc as i64) + (offset as i64) - 1) as u32;
                        Some(t.current_func)
                    } else {
                        None
                    }
                };
                if let Some(cur_fn) = taken_fn
                    && offset <= 0
                {
                    self.maybe_compile_on_hotness(cur_fn, jit::BACKEDGE_HOTNESS_INCREMENT);
                    return self.poll_safepoint(task_id);
                }
                DispatchResult::Continue
            }
            Opcode::JmpIfNot => {
                let (cond, offset) = j_fields(&instr);
                let taken_fn = {
                    let t = task!();
                    let val = t.registers.get(cond);
                    if !is_truthy(val) {
                        t.pc = ((t.pc as i64) + (offset as i64) - 1) as u32;
                        Some(t.current_func)
                    } else {
                        None
                    }
                };
                if let Some(cur_fn) = taken_fn
                    && offset <= 0
                {
                    self.maybe_compile_on_hotness(cur_fn, jit::BACKEDGE_HOTNESS_INCREMENT);
                    return self.poll_safepoint(task_id);
                }
                DispatchResult::Continue
            }
            Opcode::JmpIfNull => {
                let (cond, offset) = j_fields(&instr);
                let taken_fn = {
                    let t = task!();
                    let val = t.registers.get(cond);
                    if val.is_null() {
                        t.pc = ((t.pc as i64) + (offset as i64) - 1) as u32;
                        Some(t.current_func)
                    } else {
                        None
                    }
                };
                if let Some(cur_fn) = taken_fn
                    && offset <= 0
                {
                    self.maybe_compile_on_hotness(cur_fn, jit::BACKEDGE_HOTNESS_INCREMENT);
                    return self.poll_safepoint(task_id);
                }
                DispatchResult::Continue
            }
            Opcode::JmpIfNotNull => {
                let (cond, offset) = j_fields(&instr);
                let taken_fn = {
                    let t = task!();
                    let val = t.registers.get(cond);
                    if !val.is_null() {
                        t.pc = ((t.pc as i64) + (offset as i64) - 1) as u32;
                        Some(t.current_func)
                    } else {
                        None
                    }
                };
                if let Some(cur_fn) = taken_fn
                    && offset <= 0
                {
                    self.maybe_compile_on_hotness(cur_fn, jit::BACKEDGE_HOTNESS_INCREMENT);
                    return self.poll_safepoint(task_id);
                }
                DispatchResult::Continue
            }

            // ── Calls (C-type) ────────────────────────────────────
            Opcode::Call => {
                let payload = c_payload(&instr);
                let (arg_count, func_id_raw) = Instruction::c_arg_count_func_id(payload);
                let arg_len = arg_count as usize;
                if arg_len > runtime::GP_REGISTER_COUNT {
                    return DispatchResult::Error(VmError::TypeError);
                }
                let mut arg_buf = [TaggedValue::UNIT; runtime::GP_REGISTER_COUNT];
                arg_buf[..arg_len].copy_from_slice(&task!().registers.regs[..arg_len]);
                self.invoke_closure_physical(
                    task_id,
                    FuncId(func_id_raw),
                    None,
                    &arg_buf[..arg_len],
                )
            }

            Opcode::CallFar => {
                let payload = c_payload(&instr);
                let (arg_count, const_idx) = Instruction::c_arg_count_func_id(payload);
                let func_id_raw = match self.constant_u32(const_idx) {
                    Ok(id) => id,
                    Err(error) => return DispatchResult::Error(error),
                };
                let arg_len = arg_count as usize;
                if arg_len > runtime::GP_REGISTER_COUNT {
                    return DispatchResult::Error(VmError::TypeError);
                }
                let mut arg_buf = [TaggedValue::UNIT; runtime::GP_REGISTER_COUNT];
                arg_buf[..arg_len].copy_from_slice(&task!().registers.regs[..arg_len]);
                self.invoke_closure_physical(
                    task_id,
                    FuncId(func_id_raw),
                    None,
                    &arg_buf[..arg_len],
                )
            }

            Opcode::CallWasm => {
                let payload = c_payload(&instr);
                let (arg_count, binding_id) = Instruction::c_arg_count_func_id(payload);
                self.exec_call_wasm(task_id, binding_id, arg_count)
            }

            Opcode::CallIndirect | Opcode::CallIndirectProof => {
                let payload = c_payload(&instr);
                let (arg_count, closure_reg) = Instruction::c_call_indirect(payload);
                let closure_val = task!().registers.get(closure_reg);

                let payload_ptr = match closure_val.as_heap_ptr() {
                    Some(p) => p,
                    None => {
                        return if instr.opcode == Opcode::CallIndirectProof {
                            self.exec_call_apply_proof(task_id, closure_reg, arg_count)
                        } else {
                            self.exec_call_method(
                                task_id,
                                closure_reg,
                                str_interner::intern("apply"),
                                arg_count,
                            )
                        };
                    }
                };

                // SAFETY: heap values reference live allocations; verify the
                // closure layout before reading function metadata or captures.
                let header = unsafe { ObjectHeader::from_payload_ptr(payload_ptr) };
                if !header.is_ordinary() {
                    return DispatchResult::Error(VmError::TypeError);
                }
                if header.type_index != Intrinsic::Closure.type_index() {
                    return if instr.opcode == Opcode::CallIndirectProof {
                        self.exec_call_apply_proof(task_id, closure_reg, arg_count)
                    } else {
                        self.exec_call_method(
                            task_id,
                            closure_reg,
                            str_interner::intern("apply"),
                            arg_count,
                        )
                    };
                }
                if header.payload_words() < 2 || arg_count as usize > runtime::GP_REGISTER_COUNT {
                    return DispatchResult::Error(VmError::TypeError);
                }
                // SAFETY: the initialized ClosureEnv layout was checked above.
                let func_id = unsafe { ClosureEnv::func_id(payload_ptr) };
                let arg_len = arg_count as usize;
                let mut arg_buf = [TaggedValue::UNIT; runtime::GP_REGISTER_COUNT];
                arg_buf[..arg_len].copy_from_slice(&task!().registers.regs[..arg_len]);
                let all_value_params = instr.opcode == Opcode::CallIndirectProof
                    || self.checked_function(func_id).is_ok_and(|code| {
                        code.abi.parameters.len() == arg_len
                            && code
                                .abi
                                .parameters
                                .iter()
                                .all(|p| *p == ParameterAbi::Value)
                    });
                if all_value_params {
                    if let Err(error) =
                        self.prepare_indirect_arguments(func_id, &mut arg_buf[..arg_len])
                    {
                        return DispatchResult::Error(error);
                    }
                    let closure_val = task!().registers.get(closure_reg);
                    self.invoke_closure_physical(
                        task_id,
                        func_id,
                        Some(closure_val),
                        &arg_buf[..arg_len],
                    )
                } else {
                    let mut physical =
                        match self.expand_logical_arguments(func_id, &arg_buf[..arg_len]) {
                            Ok(arguments) => arguments,
                            Err(error) => return DispatchResult::Error(error),
                        };
                    if let Err(error) = self.prepare_indirect_arguments(func_id, &mut physical) {
                        return DispatchResult::Error(error);
                    }
                    let closure_val = task!().registers.get(closure_reg);
                    self.invoke_closure_physical(task_id, func_id, Some(closure_val), &physical)
                }
            }

            Opcode::TraitProof | Opcode::TraitAssert | Opcode::TraitProject => {
                let (dst, base, view) = a_fields(&instr);
                let view = TypeIndex::from_raw(view as u32);
                let source = task!().registers.get(base);
                let result = match instr.opcode {
                    Opcode::TraitProof => self.acquire_trait_proof(source, view),
                    Opcode::TraitProject => self.project_trait_proof(source, view),
                    Opcode::TraitAssert => {
                        let data = task!().registers.get(dst);
                        self.assert_trait_pair(source, data, view).map(|()| data)
                    }
                    _ => unreachable!("matched trait proof opcode"),
                };
                match result {
                    Ok(value) => {
                        task!().registers.set(dst, value);
                        DispatchResult::Continue
                    }
                    Err(error) => DispatchResult::Error(error),
                }
            }
            Opcode::TraitCall => {
                let (count, proof_register, slot) = Instruction::c_call_method(c_payload(&instr));
                if count as usize > runtime::GP_REGISTER_COUNT {
                    return DispatchResult::Error(VmError::TypeError);
                }
                let proof = task!().registers.get(proof_register);
                let args = task!().registers.regs[..count as usize].to_vec();
                self.dispatch_trait_call(task_id, proof, slot as usize, &args)
            }
            Opcode::CallMethod => {
                let payload = c_payload(&instr);
                let (arg_count, recv_reg, method_id) = Instruction::c_call_method(payload);
                self.exec_call_method(task_id, recv_reg, StrId::from_raw(method_id), arg_count)
            }
            Opcode::CallMethodFar => {
                let payload = c_payload(&instr);
                let (arg_count, recv_reg, const_idx) = Instruction::c_call_method(payload);
                let method_id = match self.constant_u32(const_idx) {
                    Ok(id) => id,
                    Err(error) => return DispatchResult::Error(error),
                };
                self.exec_call_method(task_id, recv_reg, StrId::from_raw(method_id), arg_count)
            }
            Opcode::CallBuiltin => {
                let payload = c_payload(&instr);
                let (arg_count, builtin_id) = Instruction::c_arg_count_func_id(payload);
                self.dispatch_builtin(task_id, builtin_id, arg_count)
            }
            Opcode::Return => {
                let src_reg = Instruction::c_return_reg(c_payload(&instr));
                let val = task!().registers.get(src_reg);
                DispatchResult::Return(val)
            }
            Opcode::ReturnUnit => DispatchResult::Return(TaggedValue::UNIT),

            // ── Effects (E-type) ──────────────────────────────────
            Opcode::EffectCallDyn => self.dispatch_effect(task_id, &instr),
            Opcode::PushHandlerClosure | Opcode::PushCapturingHandler => {
                self.dispatch_push_handler_closure(task_id, &instr)
            }
            Opcode::ResetClosure => self.dispatch_reset_closure(task_id, &instr),
            Opcode::ResumeContinuation | Opcode::ResumeContinuationOnce => {
                self.dispatch_language_resume(task_id, &instr)
            }
            Opcode::PopHandler => self.dispatch_pop_handler(task_id),
            Opcode::AwaitPopHandler => self.dispatch_await_pop_handler(task_id),
            Opcode::AwaitFuture => self.dispatch_await_future(task_id, &instr),

            // ── System (E-type) ───────────────────────────────────
            Opcode::Safepoint => self.poll_safepoint(task_id),
            Opcode::Unreachable => DispatchResult::Error(VmError::UnreachableExecuted),
            Opcode::Nop => DispatchResult::Continue,
        }
    }

    /// Save callee-saved regs and transfer control to `target`.
    fn push_call_frame(
        &mut self,
        task_id: TaskId,
        target: FuncId,
        arguments: &[TaggedValue],
    ) -> DispatchResult {
        let (display_state, cycle) = match self.display_call_state(task_id, target, arguments) {
            Ok(state) => state,
            Err(error) => return DispatchResult::Error(error),
        };
        let clobbers_cs = self
            .shared
            .bytecode
            .try_get_function(target)
            .is_none_or(|_| self.shared.bytecode.get_meta(target).clobbers_callee_saved);
        let t = self.task_mut(task_id).unwrap();
        let mut saved = runtime::SavedRegs::new();
        if clobbers_cs {
            for &r in Reg::CALLEE_SAVED {
                saved.push(Reg(r), t.registers.get(Reg(r)));
            }
        }
        let frame = CallFrame {
            display_state: t.display_state.take(),
            return_pc: t.pc,
            func_id: t.current_func,
            saved_regs: saved,
            local_slots: std::mem::take(&mut t.local_slots),
            unwind_stack: std::mem::take(&mut t.unwind_stack),
            is_unwind_target: std::mem::replace(&mut t.is_unwind_target, false),
        };
        t.call_stack.push(frame);
        t.current_func = target;
        t.pc = 0;
        t.display_state = display_state;
        if cycle {
            t.registers.regs[..arguments.len()].copy_from_slice(arguments);
            return match self.alloc_string("<cycle>") {
                Ok(value) => DispatchResult::Return(value),
                Err(error) => DispatchResult::Error(error),
            };
        }
        DispatchResult::Continue
    }

    fn exec_call_method(
        &mut self,
        task_id: TaskId,
        recv_reg: Reg,
        method_str_id: StrId,
        arg_count: u8,
    ) -> DispatchResult {
        let receiver = self.task_mut(task_id).unwrap().registers.get(recv_reg);
        let recv_type = match self.reflected_type(receiver) {
            Ok(ty) => ty,
            Err(error) => return DispatchResult::Error(error),
        };

        let selected = match self.selected_method(task_id, recv_type, method_str_id) {
            Ok(selected) => selected,
            Err(error) => return DispatchResult::Error(error),
        };
        if let Some(method) = selected {
            let target_func_id = FuncId(method.func_id);
            let logical_count = match self.checked_function(target_func_id) {
                Ok(function) => function.abi.logical_parameter_count(),
                Err(error) => return DispatchResult::Error(error),
            };
            if let Err(error) = self.check_entry_environment(target_func_id, None) {
                return DispatchResult::Error(error);
            }
            if arg_count as usize >= runtime::GP_REGISTER_COUNT {
                return DispatchResult::Error(VmError::ArityError {
                    expected: (runtime::GP_REGISTER_COUNT - 1) as u8,
                    got: arg_count,
                });
            }
            let Some(expected) = logical_count.checked_sub(1) else {
                return DispatchResult::Error(VmError::TypeError);
            };
            if arg_count as usize + 1 != logical_count {
                return DispatchResult::Error(VmError::ArityError {
                    expected: expected as u8,
                    got: arg_count,
                });
            }
            if let Some(result) =
                self.exec_collection_call(task_id, receiver, recv_type, method_str_id, arg_count)
            {
                return result;
            }
            let arg_len = arg_count as usize;
            let mut arg_buf = [TaggedValue::UNIT; runtime::GP_REGISTER_COUNT];
            arg_buf[0] = receiver;
            {
                let t = self.task_ref(task_id).unwrap();
                arg_buf[1..1 + arg_len].copy_from_slice(&t.registers.regs[..arg_len]);
            }
            self.invoke_closure(task_id, target_func_id, None, &arg_buf[..1 + arg_len])
        } else {
            if let Some(result) =
                self.exec_collection_call(task_id, receiver, recv_type, method_str_id, arg_count)
            {
                return result;
            }

            if recv_type == Intrinsic::Continuation.type_index() {
                match str_interner::get(method_str_id).as_str() {
                    "clone" => {
                        if arg_count != 0 {
                            return DispatchResult::Error(VmError::ArityError {
                                expected: 0,
                                got: arg_count,
                            });
                        }
                        return self.clone_language_continuation(task_id, receiver);
                    }
                    "apply" => {
                        if arg_count != 1 {
                            return DispatchResult::Error(VmError::ArityError {
                                expected: 1,
                                got: arg_count,
                            });
                        }
                        let value = self
                            .task_ref(task_id)
                            .expect("dispatched task exists")
                            .registers
                            .get(Reg(0));
                        return self.resume_language_value(task_id, receiver, value);
                    }
                    _ => {}
                }
            }

            DispatchResult::Error(VmError::MethodNotFound)
        }
    }

    fn exec_call_apply_proof(
        &mut self,
        task_id: TaskId,
        recv_reg: Reg,
        arg_count: u8,
    ) -> DispatchResult {
        let receiver = self
            .task_ref(task_id)
            .expect("dispatched task exists")
            .registers
            .get(recv_reg);
        let recv_type = match self.reflected_type(receiver) {
            Ok(ty) => ty,
            Err(error) => return DispatchResult::Error(error),
        };
        let apply_name = str_interner::intern("apply");
        let selected = match self.selected_method(task_id, recv_type, apply_name) {
            Ok(selected) => selected,
            Err(error) => return DispatchResult::Error(error),
        };
        if let Some(method) = selected {
            let target_func_id = FuncId(method.func_id);
            let abi = match self.checked_function(target_func_id) {
                Ok(function) => function.abi.clone(),
                Err(error) => return DispatchResult::Error(error),
            };
            if let Err(error) = self.check_entry_environment(target_func_id, None) {
                return DispatchResult::Error(error);
            }
            let Some(self_param) = abi.parameters.first() else {
                return DispatchResult::Error(VmError::TypeError);
            };
            let expected_explicit: usize = abi.parameters[1..]
                .iter()
                .map(|parameter| match parameter {
                    ParameterAbi::Value => 1,
                    ParameterAbi::Trait { .. } | ParameterAbi::TraitSelf { .. } => 2,
                })
                .sum();
            if arg_count as usize != expected_explicit {
                return DispatchResult::Error(VmError::ArityError {
                    expected: expected_explicit as u8,
                    got: arg_count,
                });
            }
            let mut physical = match self_param {
                ParameterAbi::Value => vec![receiver],
                ParameterAbi::Trait { view } | ParameterAbi::TraitSelf { view } => {
                    let proof = match self.acquire_trait_proof(receiver, *view) {
                        Ok(proof) => proof,
                        Err(error) => return DispatchResult::Error(error),
                    };
                    vec![proof, receiver]
                }
            };
            let t = self.task_ref(task_id).unwrap();
            physical.extend_from_slice(&t.registers.regs[..arg_count as usize]);
            if let Err(error) = self.prepare_indirect_arguments(target_func_id, &mut physical) {
                return DispatchResult::Error(error);
            }
            self.invoke_closure_physical(task_id, target_func_id, None, &physical)
        } else if recv_type == Intrinsic::Continuation.type_index() {
            let handle = match Self::language_continuation_handle(receiver) {
                Ok(handle) => handle,
                Err(error) => return DispatchResult::Error(error),
            };
            let contract = match self.continuation_input_contract(task_id, handle) {
                Ok(contract) => contract,
                Err(error) => return DispatchResult::Error(error),
            };
            let input_ty = match self.canonical_type(contract.input) {
                Ok(ty) => ty,
                Err(error) => return DispatchResult::Error(error),
            };
            let is_trait_input = matches!(
                self.type_pool_read().get(input_ty).kind,
                type_pool::TypeKind::Trait { .. }
            );
            if is_trait_input {
                if arg_count != 2 {
                    return DispatchResult::Error(VmError::ArityError {
                        expected: 2,
                        got: arg_count,
                    });
                }
                let t = self.task_ref(task_id).unwrap();
                let proof = t.registers.get(Reg(0));
                let value = t.registers.get(Reg(1));
                if let Err(error) = self.assert_trait_pair(proof, value, input_ty) {
                    return DispatchResult::Error(error);
                }
                return self.resume_language_value(task_id, receiver, value);
            }
            if arg_count != 1 {
                return DispatchResult::Error(VmError::ArityError {
                    expected: 1,
                    got: arg_count,
                });
            }
            let value = self
                .task_ref(task_id)
                .expect("dispatched task exists")
                .registers
                .get(Reg(0));
            self.resume_language_value(task_id, receiver, value)
        } else {
            DispatchResult::Error(VmError::MethodNotFound)
        }
    }

    fn exec_new_closure(
        &mut self,
        task_id: TaskId,
        dst: Reg,
        func_id: FuncId,
        capture_count: u16,
    ) -> DispatchResult {
        let cap_len = capture_count as usize;
        if cap_len > runtime::GP_REGISTER_COUNT {
            return DispatchResult::Error(VmError::TypeError);
        }
        if let Err(error) = self.check_capture_abi(func_id, cap_len) {
            return DispatchResult::Error(error);
        }
        let mut captures = [TaggedValue::UNIT; runtime::GP_REGISTER_COUNT];
        {
            let t = self.task_ref(task_id).expect("dispatched task exists");
            captures[..cap_len].copy_from_slice(&t.registers.regs[..cap_len]);
        }
        if let Err(error) = self.check_capture_proofs(func_id, &captures[..cap_len]) {
            return DispatchResult::Error(error);
        }
        let payload_words = ClosureEnv::payload_words(capture_count);
        let closure_type = Intrinsic::Closure.type_index();

        // SAFETY: slice holds an operation. Captures are re-read from rooted registers
        // after allocation, and the completed closure is published to `dst`.
        match unsafe { self.worker.heap.alloc_object(closure_type, payload_words) } {
            Some(ptr) => {
                let t = self.task_mut(task_id).unwrap();
                captures[..cap_len].copy_from_slice(&t.registers.regs[..cap_len]);
                unsafe {
                    ClosureEnv::write(ptr.as_ptr(), func_id, &captures[..cap_len]);
                }
                let val = unsafe { TaggedValue::from_heap_ptr(ptr.as_ptr()) };
                t.registers.set(dst, val);
                DispatchResult::Continue
            }
            None => DispatchResult::Error(VmError::OutOfMemory),
        }
    }

    /// Fast inline execution for the hottest collection/string builtins without constructing `BuiltinCtx`.
    #[inline]
    fn try_dispatch_fast_builtin(
        &mut self,
        task_id: TaskId,
        builtin_id: u32,
        arg_count: u8,
    ) -> Option<DispatchResult> {
        use runtime::ids;
        macro_rules! check_arity {
            ($exp:expr) => {
                if arg_count != $exp {
                    return Some(DispatchResult::Error(VmError::ArityError {
                        expected: $exp,
                        got: arg_count,
                    }));
                }
            };
        }
        let res: Result<TaggedValue, VmError> = match builtin_id {
            ids::STR_LEN => {
                check_arity!(1);
                let s = self.task_ref(task_id)?.registers.get(Reg(0));
                builtin_ctx::with_heap_string_str(s, |text| text.len() as i64)
                    .ok_or(VmError::TypeError)
                    .and_then(|n| self.number_value(runtime::Number::I64(n)))
            }
            ids::STR_CONCAT => {
                check_arity!(2);
                let (a, b) = {
                    let r = &self.task_ref(task_id)?.registers;
                    (r.get(Reg(0)), r.get(Reg(1)))
                };
                self.alloc_string_concat(task_id, a, b)
            }
            ids::LIST_INIT => {
                check_arity!(0);
                self.new_list(0)
            }
            ids::LIST_LEN => {
                check_arity!(1);
                let list = self.task_ref(task_id)?.registers.get(Reg(0));
                self.list_len(list)
                    .and_then(|n| i64::try_from(n).map_err(|_| VmError::NumericOverflow))
                    .and_then(|n| self.number_value(runtime::Number::I64(n)))
            }
            ids::LIST_GET => {
                check_arity!(2);
                let (list, idx) = {
                    let r = &self.task_ref(task_id)?.registers;
                    (r.get(Reg(0)), r.get(Reg(1)))
                };
                self.list_get(list, idx)
            }
            ids::LIST_SET => {
                check_arity!(3);
                let (list, idx, val) = {
                    let r = &self.task_ref(task_id)?.registers;
                    (r.get(Reg(0)), r.get(Reg(1)), r.get(Reg(2)))
                };
                self.list_set(list, idx, val).map(|()| TaggedValue::UNIT)
            }
            ids::LIST_PUSH => {
                check_arity!(2);
                let (list, val) = {
                    let r = &self.task_ref(task_id)?.registers;
                    (r.get(Reg(0)), r.get(Reg(1)))
                };
                self.list_push(list, val).map(|()| TaggedValue::UNIT)
            }
            ids::LIST_POP => {
                check_arity!(1);
                let list = self.task_ref(task_id)?.registers.get(Reg(0));
                self.list_pop(list)
            }
            ids::MAP_INIT => {
                check_arity!(0);
                self.new_map(0)
            }
            ids::MAP_LEN => {
                check_arity!(1);
                let map = self.task_ref(task_id)?.registers.get(Reg(0));
                self.map_len(map)
                    .and_then(|n| i64::try_from(n).map_err(|_| VmError::NumericOverflow))
                    .and_then(|n| self.number_value(runtime::Number::I64(n)))
            }
            ids::MAP_GET => {
                check_arity!(2);
                let (map, key) = {
                    let r = &self.task_ref(task_id)?.registers;
                    (r.get(Reg(0)), r.get(Reg(1)))
                };
                self.map_get(map, key)
            }
            ids::MAP_SET => {
                check_arity!(3);
                let (map, key, val) = {
                    let r = &self.task_ref(task_id)?.registers;
                    (r.get(Reg(0)), r.get(Reg(1)), r.get(Reg(2)))
                };
                self.map_set(map, key, val).map(|()| TaggedValue::UNIT)
            }
            ids::MAP_CONTAINS => {
                check_arity!(2);
                let (map, key) = {
                    let r = &self.task_ref(task_id)?.registers;
                    (r.get(Reg(0)), r.get(Reg(1)))
                };
                self.map_contains(map, key).map(TaggedValue::from_bool)
            }
            ids::SCALAR_EQ => {
                check_arity!(2);
                let (a, b) = {
                    let r = &self.task_ref(task_id)?.registers;
                    (r.get(Reg(0)), r.get(Reg(1)))
                };
                self.scalar_equal(a, b).map(TaggedValue::from_bool)
            }
            _ => return None,
        };
        Some(match res {
            Ok(val) => {
                if let Some(t) = self.task_mut(task_id) {
                    t.registers.set(Reg(0), val);
                    DispatchResult::Continue
                } else {
                    DispatchResult::Error(VmError::InvalidTask)
                }
            }
            Err(err) => DispatchResult::Error(err),
        })
    }

    /// Dispatch a builtin function call.
    /// Arguments are in r0..rN by calling convention. Result goes into r0.
    fn dispatch_builtin(
        &mut self,
        task_id: TaskId,
        builtin_id: u32,
        arg_count: u8,
    ) -> DispatchResult {
        let Some(f) = self.shared.builtins.get(builtin_id) else {
            return DispatchResult::Error(VmError::UnknownBuiltin(builtin_id));
        };
        if self.shared.builtins.can_fast_path(builtin_id)
            && let Some(fast) = self.try_dispatch_fast_builtin(task_id, builtin_id, arg_count)
        {
            return fast;
        }
        let mut ctx = BuiltinCtx::new(self, task_id, arg_count);
        if let Err(e) = f(&mut ctx) {
            return DispatchResult::Error(e);
        }
        match ctx.take_outcome() {
            BuiltinOutcome::Continue => DispatchResult::Continue,
            BuiltinOutcome::Yield => DispatchResult::Yield,
            BuiltinOutcome::SuspendRetry => DispatchResult::SuspendRetry,
            BuiltinOutcome::StartCancelUnwind => DispatchResult::StartCancelUnwind,
            BuiltinOutcome::ResumeCancelUnwind => DispatchResult::ResumeCancelUnwind,
            BuiltinOutcome::Exit(code) => {
                std::process::exit(code);
            }
            BuiltinOutcome::Panic(msg) => DispatchResult::Error(VmError::Panic(msg)),
        }
    }
}

fn aux_worker_thread_main(
    worker_id: usize,
    total_aux: usize,
    gc_config: GcConfig,
    weak_shared: Weak<SharedVm>,
    sync: Arc<AuxEpochSync>,
) {
    let mut worker = Some(Box::new(WorkerState {
        worker_id,
        heap: gc::gc_init(&gc_config),
        type_query_scope: None,
        slice_safepoints: 64,
        native_frame_depth: 0,
        local_queue_ptr: None,
        meta_cache: RefCell::new(WorkerMetaCache::default()),
        slot_vec_pool: Vec::with_capacity(64),
        jit: Vm::new_default_jit(),
        jit_synced_epoch: 0,
        jit_reported_fns: 0,
        jit_reported_bytes: 0,
        jit_reported_deopts: 0,
    }));
    let mut last_epoch = 0u64;

    loop {
        {
            let mut guard = sync.lock.lock();
            while (!guard.epoch_running || guard.run_epoch == last_epoch) && !guard.shutdown {
                sync.start_cv.wait(&mut guard);
            }
            if guard.shutdown {
                break;
            }
            last_epoch = guard.run_epoch;
            guard.active_aux_workers += 1;
            if guard.active_aux_workers >= total_aux {
                sync.all_aux_entered.store(true, Ordering::Release);
            }
        }

        if let Some(shared) = weak_shared.upgrade() {
            let shared_for_panic = Arc::clone(&shared);
            let mut aux_vm = Vm {
                _aux_shutdown: None,
                _roots: None,
                worker: worker.take().expect("aux worker state present"),
                shared,
            };
            let run_res =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| aux_vm.run_worker_loop()));
            let Vm {
                worker: returned_worker,
                shared,
                ..
            } = aux_vm;
            worker = Some(returned_worker);
            drop(shared);
            match run_res {
                Ok(WorkerLoopExit::Deadlock) => {
                    sync.record_deadlock();
                }
                Ok(_) => {}
                Err(payload) => {
                    let msg = if let Some(s) = payload.downcast_ref::<&str>() {
                        (*s).to_string()
                    } else if let Some(s) = payload.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "auxiliary worker panicked".to_string()
                    };
                    eprintln!("NESSA AUX WORKER {worker_id} PANIC: {msg}");
                    sync.record_error(VmError::Panic(msg));
                    shared_for_panic.roots.scheduler.request_abort();
                }
            }
            drop(shared_for_panic);
        }

        let mut guard = sync.lock.lock();
        guard.active_aux_workers -= 1;
        if guard.active_aux_workers == 0 {
            sync.done_cv.notify_all();
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatch helpers
// ---------------------------------------------------------------------------

fn r_regs(instr: &Instruction) -> (Reg, Reg, Reg) {
    match instr.data {
        InstructionData::R {
            dst, src1, src2, ..
        } => (dst, src1, src2),
        _ => unreachable!("expected R-type"),
    }
}

fn a_fields(instr: &Instruction) -> (Reg, Reg, u16) {
    match instr.data {
        InstructionData::A { dst, base, imm12 } => (dst, base, imm12),
        _ => unreachable!("expected A-type"),
    }
}

fn j_fields(instr: &Instruction) -> (Reg, i32) {
    match instr.data {
        InstructionData::J { cond, offset } => (cond, offset),
        _ => unreachable!("expected J-type"),
    }
}

fn c_payload(instr: &Instruction) -> u32 {
    match instr.data {
        InstructionData::C { payload } => payload,
        _ => unreachable!("expected C-type"),
    }
}

fn e_payload(instr: &Instruction) -> u32 {
    match instr.data {
        InstructionData::E { payload } => payload,
        _ => unreachable!("expected E-type"),
    }
}

fn is_truthy(val: TaggedValue) -> bool {
    if val.is_null() || val.is_unit() {
        return false;
    }
    if let Some(b) = val.as_bool() {
        return b;
    }
    // SAFETY: callers are instruction dispatchers with rooted live values
    // and an active execution operation. Scalar decoding does not allocate.
    if let Some(number) = unsafe { Number::from_tagged(val) } {
        return number.partial_cmp_numeric(Number::I64(0)) != Some(std::cmp::Ordering::Equal);
    }
    true
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Outcome of a dispatch of a single instruction.
enum DispatchResult {
    Continue,
    Return(TaggedValue),
    Yield,
    SuspendRetry,
    StartCancelUnwind,
    ResumeCancelUnwind,
    Error(VmError),
}

/// Outcome of executing a task.
pub(crate) enum ExecOutcome {
    Finished,
    Yield,
    Suspend,
    Cancelled,
    Error(VmError),
}

/// Result of running the VM.
#[derive(Debug)]
pub enum VmResult {
    Finished,
    Error(VmError),
}

/// VM runtime errors.
#[derive(Debug, Clone)]
pub enum VmError {
    InvalidInstruction(u32),
    InvalidTask,
    TaskCancelled,
    CrossTaskContinuationCapture,
    DeadlockDetected,
    InvalidFunction(FuncId),
    InvalidSlot(u32),
    InvalidField(u32),
    InvalidEnumVariant(u32),
    UnsupportedEnumEquality,
    UnsupportedEquality,
    NoMatchingCase,
    UnreachableExecuted,
    InvalidIndex,
    /// A native argument has the right type but violates the operation's domain.
    InvalidArgument(&'static str),
    DisplayDepthExceeded,
    DisplaySizeExceeded,
    InvalidConstant(u32),
    InvalidGlobal(u32),
    UninitializedGlobal(u32),
    ImmutableGlobal(u32),
    GlobalsAlreadyInitialized,
    InvalidType(TypeIndex),
    Stack(runtime::StackError),
    UnhandledEffect(TypeIndex),
    OutOfMemory,
    ObjectTooLarge,
    UnsupportedFunctionAbi,
    InvalidTraitProof,
    TraitProofLimit,
    TraitProofMetadata(type_pool::TraitProofError),
    CrossVmOperation,
    TypeError,
    UnsupportedLegacyErrorLayout(TypeIndex),
    NumericOverflow,
    DivisionByZero,
    InvalidShift,
    ArityError {
        expected: u8,
        got: u8,
    },
    UnknownBuiltin(u32),
    MethodNotFound,
    MethodAccess(type_pool::MethodAccessError),
    MethodAccessDenied,
    AmbiguousMethod,
    MissingMethodContext,
    TraitLookup(type_pool::TraitLookupError),
    MissingTraitContext,
    EffectAcrossNativeFrame,
    WasmTrap(String),
    Panic(String),
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInstruction(word) => write!(f, "invalid instruction {word:#010x}"),
            Self::InvalidTask => f.write_str("invalid task"),
            Self::TaskCancelled => f.write_str("awaited task was cancelled"),
            Self::CrossTaskContinuationCapture => {
                f.write_str("cannot capture a continuation across an async task boundary")
            }
            Self::DeadlockDetected => f.write_str(
                "deadlock detected: all remaining tasks are suspended with no active I/O or runnable wakers",
            ),
            Self::InvalidFunction(id) => write!(f, "invalid function id {}", id.0),
            Self::InvalidSlot(index) => write!(f, "invalid local slot {index}"),
            Self::InvalidField(index) => write!(f, "invalid field index {index}"),
            Self::InvalidEnumVariant(tag) => write!(f, "invalid enum variant {tag}"),
            Self::UnsupportedEnumEquality => f.write_str("unsupported payload enum equality"),
            Self::UnsupportedEquality => f.write_str("unsupported scalar equality operand"),
            Self::NoMatchingCase => f.write_str("non-exhaustive match failed"),
            Self::UnreachableExecuted => f.write_str("entered unreachable code"),
            Self::InvalidIndex => f.write_str("index out of bounds"),
            Self::InvalidArgument(reason) => write!(f, "invalid argument: {reason}"),
            Self::DisplayDepthExceeded => f.write_str("display formatting depth exceeded"),
            Self::DisplaySizeExceeded => f.write_str("display formatting size exceeded"),
            Self::InvalidConstant(index) => write!(f, "invalid constant index {index}"),
            Self::InvalidGlobal(index) => write!(f, "invalid global index {index}"),
            Self::UninitializedGlobal(index) => write!(f, "uninitialized global {index}"),
            Self::ImmutableGlobal(index) => write!(f, "cannot mutate immutable global {index}"),
            Self::GlobalsAlreadyInitialized => f.write_str("globals already initialized"),
            Self::InvalidType(ty) => write!(f, "invalid type index {}", ty.as_u32()),
            Self::Stack(error) => write!(f, "stack error: {error:?}"),
            Self::UnhandledEffect(ty) => write!(f, "unhandled effect {}", ty.as_u32()),
            Self::OutOfMemory => f.write_str("out of memory"),
            Self::ObjectTooLarge => f.write_str("object exceeds maximum size"),
            Self::UnsupportedFunctionAbi => f.write_str("unsupported function ABI"),
            Self::InvalidTraitProof => f.write_str("invalid trait proof"),
            Self::TraitProofLimit => f.write_str("trait proof registry limit reached"),
            Self::TraitProofMetadata(error) => write!(f, "invalid trait proof metadata: {error:?}"),
            Self::CrossVmOperation => f.write_str("cross-VM reentry is not permitted"),
            Self::TypeError => f.write_str("type error"),
            Self::UnsupportedLegacyErrorLayout(ty) => {
                write!(f, "unsupported error layout for type {}", ty.as_u32())
            }
            Self::NumericOverflow => f.write_str("numeric overflow"),
            Self::DivisionByZero => f.write_str("division by zero"),
            Self::InvalidShift => f.write_str("invalid bit shift"),
            Self::ArityError { expected, got } => {
                write!(f, "arity mismatch: expected {expected}, got {got}")
            }
            Self::UnknownBuiltin(id) => write!(f, "unknown builtin {id}"),
            Self::MethodNotFound => f.write_str("method not found"),
            Self::MethodAccess(error) => write!(f, "invalid method access metadata: {error:?}"),
            Self::MethodAccessDenied => f.write_str("method access denied"),
            Self::AmbiguousMethod => f.write_str("ambiguous method call"),
            Self::MissingMethodContext => f.write_str("missing lexical method call context"),
            Self::TraitLookup(error) => write!(f, "trait lookup error: {error:?}"),
            Self::MissingTraitContext => f.write_str("missing lexical trait context"),
            Self::EffectAcrossNativeFrame => {
                f.write_str("cannot capture or suspend continuation across active native FFI frame")
            }
            Self::WasmTrap(message) => write!(f, "{message}"),
            Self::Panic(message) => write!(f, "panic: {message}"),
        }
    }
}

impl std::error::Error for VmError {}

// ---------------------------------------------------------------------------
// GC root scanning — enumerates all roots in the VM for MMTk
// ---------------------------------------------------------------------------

/// Root scan callback owned by Vm's lifetime-bound registration.
///
/// Walks all root sources in the Vm:
///   1. Global variable table, constant pool, and per-worker temporary value roots
///   2. Completed promises still holding unconsumed values for waiters
///   3. Active/pending-delimiter stack contexts and inherited `ancestor_handlers` across all tasks
///   4. Per-worker native pinning roots
unsafe fn scan_vm_roots(context: *const (), visitor: &mut dyn FnMut(NessaSlot)) {
    // SAFETY: `RootCell` is stable `UnsafeCell` storage. MMTk STW stops all mutators
    // (and no mutator ever holds `constants` or `promises` locks across allocation/safepoints),
    // giving exclusive access to `VmRoots` until collection finishes.
    let vm = unsafe { &mut *context.cast::<VmRoots>().cast_mut() };
    vm.globals.visit_raw_slots_mut(|slot_ptr: *mut u64| {
        // SAFETY: `slot_ptr` is an aligned `u64` slot inside `GlobalTable` stable across STW.
        visitor(unsafe { NessaSlot::from_raw_ptr(slot_ptr) });
    });

    fn visit_tagged_slot(visitor: &mut dyn FnMut(NessaSlot), value: &mut TaggedValue) {
        // SAFETY: this exclusive aligned value slot lives until tracing ends.
        visitor(unsafe { NessaSlot::from_raw_ptr((value as *mut TaggedValue).cast::<u64>()) });
    }

    // SAFETY: Under STW, no mutator is reading or writing `constants` or `promises`.
    let constants = unsafe { &mut *vm.constants.data_ptr() };
    for value in constants.values_mut() {
        visit_tagged_slot(visitor, value);
    }

    for slot in vm.worker_roots.iter_mut() {
        let worker_roots = slot.0.get_mut();
        for value in worker_roots.temporary_values.get_mut() {
            visit_tagged_slot(visitor, value);
        }
    }

    // SAFETY: Under STW, no mutator is reading or writing `json_writer_ancestors`.
    let json_ancestors = unsafe { &mut *vm.json_writer_ancestors.data_ptr() };
    for stack in json_ancestors.values_mut() {
        for value in stack.iter_mut() {
            visit_tagged_slot(visitor, value);
        }
    }

    // SAFETY: Under STW, no mutator is reading or writing `wasm_host_imports`.
    let wasm_imports = unsafe { &mut *vm.wasm_host_imports.data_ptr() };
    for closures in wasm_imports.values_mut() {
        for value in closures.iter_mut() {
            visit_tagged_slot(visitor, value);
        }
    }

    // SAFETY: Under STW, all mutator threads are parked and hold no promise shard locks.
    unsafe {
        vm.promises.for_each_shard_stopped_mut(|promises| {
            for entry in promises.values_mut() {
                if (!entry.waiters.is_empty() || !entry.all_waiters.is_empty())
                    && let futures::PromiseState::Completed(ref mut value) = entry.state
                {
                    visit_tagged_slot(visitor, value);
                }
            }
        });
    }

    vm.sync.visit_roots_stopped(|value| {
        visit_tagged_slot(visitor, value);
    });

    for task in vm.scheduler.iter_tasks_mut() {
        for handler in &mut task.ancestor_handlers {
            visit_tagged_slot(visitor, &mut handler.closure_env);
        }
        for stack_context in task.stacks.root_contexts_mut() {
            continuation_roots::visit_stack_slots(stack_context, visitor);
        }
    }

    for slot in vm.worker_roots.iter_mut() {
        let worker_roots = slot.0.get_mut();
        for value in worker_roots.native_pins.get_mut() {
            // SAFETY: only copied values handed to a live native call enter this
            // buffer. Their addresses and native lifetime remain fixed under STW.
            visitor(unsafe {
                NessaSlot::from_pinning_root((value as *mut TaggedValue).cast::<u64>())
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use nsbc::FunctionAbi;

    use super::*;

    pub(super) struct TestVm {
        // Keep fixture mutations under one operation; release before Vm drops.
        _operation: gc::MutatorSession<'static>,
        vm: Vm,
    }

    impl std::ops::Deref for TestVm {
        type Target = Vm;
        fn deref(&self) -> &Vm {
            &self.vm
        }
    }

    impl std::ops::DerefMut for TestVm {
        fn deref_mut(&mut self) -> &mut Vm {
            &mut self.vm
        }
    }

    pub(super) fn make_vm() -> TestVm {
        let vm = Vm::new_with_workers(
            TypePool::with_intrinsics(),
            &GcConfig::default(),
            Arc::new(StackPool::new()),
            1,
        );
        let operation = vm.operation();
        TestVm {
            _operation: operation,
            vm,
        }
    }

    fn encode_instrs(instrs: &[Instruction]) -> Vec<u32> {
        instrs.iter().map(|i| i.encode()).collect()
    }

    #[test]
    fn simple_add_and_return() {
        let mut vm = make_vm();
        let code = encode_instrs(&[
            Instruction::load_imm(Reg(0), 10),
            Instruction::load_imm(Reg(1), 32),
            Instruction::add(Reg(2), Reg(0), Reg(1)),
            Instruction::ret(Reg(2)),
        ]);
        let func = FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: code,
            register_count: 3,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        };
        vm.add_function(func);
        let task_id = vm.spawn_root(FuncId(0));
        let result = vm.run();
        assert!(matches!(result, VmResult::Finished));
        let val = vm
            .scheduler()
            .get_task(task_id)
            .unwrap()
            .registers
            .get(Reg(0));
        assert_eq!(val.as_i64(), Some(42));
    }

    #[test]
    fn wide_local_slot_load_and_store_return_the_saved_value() {
        let mut vm = make_vm();
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: encode_instrs(&[
                Instruction::allocate_slots(70001),
                Instruction::load_imm(Reg(0), 42),
                Instruction::store_slot(70000, Reg(0)),
                Instruction::load_imm(Reg(0), 1),
                Instruction::load_slot(Reg(0), 70000),
                Instruction::ret(Reg(0)),
            ]),
            register_count: 32,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        let task = vm.spawn_root(FuncId(0));
        assert!(matches!(vm.run(), VmResult::Finished));
        let task = vm.scheduler().get_task(task).unwrap();
        assert_eq!(task.registers.get(Reg(0)).as_i64(), Some(42));
        assert_eq!(task.local_slots.capacity(), 0);
    }

    #[test]
    fn invalid_local_slot_access_reports_an_error_and_releases_stacks() {
        for access in [
            Instruction::load_slot(Reg(0), 1),
            Instruction::store_slot(1, Reg(0)),
        ] {
            let mut vm = make_vm();
            vm.add_function(FunctionCode {
                display_owner: None,
                abi: FunctionAbi::value_only(0),
                func_id: FuncId(0),
                instructions: encode_instrs(&[Instruction::allocate_slots(1), access]),
                register_count: 32,
                param_count: 0,
                is_closure: false,
                function_type: type_pool::TypeIndex::INVALID,
            });
            vm.spawn_root(FuncId(0));
            assert!(matches!(vm.run(), VmResult::Error(VmError::InvalidSlot(1))));
            assert_eq!(vm.scheduler().stack_pool().active_count(), 0);
        }
    }

    #[test]
    fn invalid_field_access_cannot_read_or_write_past_the_object() {
        for store in [false, true] {
            let mut vm = make_vm();
            let ty = vm.type_pool_write().register(type_pool::TypeInfo {
                kind: type_pool::TypeKind::Tuple {
                    elements: vec![Intrinsic::I64.type_index()],
                },
                type_id: type_pool::TypeId(99, 99),
                size: 8,
                align: 8,
            });
            let access = if store {
                Instruction::store_field(Reg(1), 1, Reg(0))
            } else {
                Instruction::load_field(Reg(0), Reg(1), 1)
            };
            vm.add_function(FunctionCode {
                display_owner: None,
                abi: FunctionAbi::value_only(0),
                func_id: FuncId(0),
                instructions: encode_instrs(&[Instruction::new_object(Reg(1), ty), access]),
                register_count: 32,
                param_count: 0,
                is_closure: false,
                function_type: type_pool::TypeIndex::INVALID,
            });
            vm.spawn_root(FuncId(0));
            assert!(matches!(
                vm.run(),
                VmResult::Error(VmError::InvalidField(1))
            ));
        }
    }

    #[test]
    fn conditional_jump() {
        let mut vm = make_vm();
        let code = encode_instrs(&[
            Instruction::load_true(Reg(0)),
            Instruction::jmp_if(Reg(0), 2),
            Instruction::load_imm(Reg(1), 0),
            Instruction::load_imm(Reg(1), 42),
            Instruction::ret(Reg(1)),
        ]);
        let func = FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: code,
            register_count: 2,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        };
        vm.add_function(func);
        let task_id = vm.spawn_root(FuncId(0));
        vm.run();
        let val = vm
            .scheduler()
            .get_task(task_id)
            .unwrap()
            .registers
            .get(Reg(0));
        assert_eq!(val.as_i64(), Some(42));
    }

    #[test]
    fn function_call_and_return() {
        let mut vm = make_vm();
        let main_code = encode_instrs(&[
            Instruction::load_imm(Reg(0), 5),
            Instruction::call(1, 1),
            Instruction::ret(Reg(0)),
        ]);
        let func1_code =
            encode_instrs(&[Instruction::load_imm(Reg(1), 10), Instruction::ret(Reg(1))]);

        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: main_code,
            register_count: 2,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(1),
            func_id: FuncId(1),
            instructions: func1_code,
            register_count: 2,
            param_count: 1,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });

        let task_id = vm.spawn_root(FuncId(0));
        vm.run();
        let val = vm
            .scheduler()
            .get_task(task_id)
            .unwrap()
            .registers
            .get(Reg(0));
        assert_eq!(val.as_i64(), Some(10));
    }

    #[test]
    fn return_unit() {
        let mut vm = make_vm();
        let code = encode_instrs(&[Instruction::return_unit()]);
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: code,
            register_count: 0,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        let task_id = vm.spawn_root(FuncId(0));
        vm.run();
        let val = vm
            .scheduler()
            .get_task(task_id)
            .unwrap()
            .registers
            .get(Reg(0));
        assert!(val.is_unit());
    }

    #[test]
    fn globals_load_store() {
        let mut vm = make_vm();
        let gidx =
            vm.with_exclusive_shared_mut(|shared| shared.roots.globals.alloc(TaggedValue::UNIT));
        let code = encode_instrs(&[
            Instruction::load_imm(Reg(2), 99),
            Instruction::store_global(gidx as u16, Reg(2)),
            Instruction::load_global(Reg(1), gidx as u16),
            Instruction::ret(Reg(1)),
        ]);
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: code,
            register_count: 3,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        let task_id = vm.spawn_root(FuncId(0));
        assert!(matches!(vm.run(), VmResult::Finished));
        let val = vm
            .scheduler()
            .get_task(task_id)
            .unwrap()
            .registers
            .get(Reg(0));
        assert_eq!(val.as_i64(), Some(99));
    }

    #[test]
    fn comparison_ops() {
        let mut vm = make_vm();
        let code = encode_instrs(&[
            Instruction::load_imm(Reg(0), 5),
            Instruction::load_imm(Reg(1), 10),
            Instruction::r_type(Opcode::CmpLt, Reg(2), Reg(0), Reg(1)),
            Instruction::ret(Reg(2)),
        ]);
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: code,
            register_count: 3,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        let task_id = vm.spawn_root(FuncId(0));
        vm.run();
        let val = vm
            .scheduler()
            .get_task(task_id)
            .unwrap()
            .registers
            .get(Reg(0));
        assert_eq!(val.as_bool(), Some(true));
    }

    #[test]
    fn call_builtin_to_i64() {
        let mut vm = make_vm();
        vm.register_builtin(runtime::ids::TO_I64, |ctx| {
            let v = ctx.arg(0)?;
            let i = v.as_i64().ok_or(VmError::TypeError)?;
            ctx.return_i64(i)?;
            Ok(())
        });
        let code = encode_instrs(&[
            Instruction::load_imm(Reg(0), 7),
            Instruction::call_builtin(runtime::ids::TO_I64, 1),
            Instruction::ret(Reg(0)),
        ]);
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: code,
            register_count: 1,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        let task_id = vm.spawn_root(FuncId(0));
        let result = vm.run();
        assert!(matches!(result, VmResult::Finished));
        let val = vm
            .scheduler()
            .get_task(task_id)
            .unwrap()
            .registers
            .get(Reg(0));
        assert_eq!(val.as_i64(), Some(7));
    }

    #[test]
    fn unreachable_opcode_reports_clear_error() {
        let mut vm = make_vm();
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: encode_instrs(&[Instruction::unreachable()]),
            register_count: 1,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        vm.spawn_root(FuncId(0));
        let VmResult::Error(error) = vm.run() else {
            panic!("expected unreachable error");
        };
        assert!(matches!(error, VmError::UnreachableExecuted));
        assert_eq!(error.to_string(), "entered unreachable code");
    }

    #[test]
    fn far_calls_reject_invalid_and_non_integer_constants() {
        for call in [
            Instruction::call_far(0, 0),
            Instruction::call_method_far(Reg(0), 0, 0),
        ] {
            let mut missing = make_vm();
            missing.add_function(FunctionCode {
                display_owner: None,
                abi: FunctionAbi::value_only(0),
                func_id: FuncId(0),
                instructions: encode_instrs(&[Instruction::load_true(Reg(0)), call]),
                register_count: 2,
                param_count: 0,
                is_closure: false,
                function_type: type_pool::TypeIndex::INVALID,
            });
            missing.spawn_root(FuncId(0));
            assert!(matches!(
                missing.run(),
                VmResult::Error(VmError::InvalidConstant(0))
            ));
            drop(missing);

            let mut non_int = make_vm();
            non_int.roots().constants.write().push(TaggedValue::TRUE);
            non_int.add_function(FunctionCode {
                display_owner: None,
                abi: FunctionAbi::value_only(0),
                func_id: FuncId(0),
                instructions: encode_instrs(&[Instruction::load_true(Reg(0)), call]),
                register_count: 2,
                param_count: 0,
                is_closure: false,
                function_type: type_pool::TypeIndex::INVALID,
            });
            non_int.spawn_root(FuncId(0));
            assert!(matches!(non_int.run(), VmResult::Error(VmError::TypeError)));
        }
    }

    #[test]
    fn scalar_cmp_eq_rejects_non_scalar_heap_tuples() {
        let mut vm = make_vm();
        let tuple_ty = vm
            .type_pool_write()
            .intern_structural(type_pool::TypeKind::Tuple {
                elements: vec![Intrinsic::I64.type_index()],
            });
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: encode_instrs(&[
                Instruction::new_object(Reg(0), tuple_ty),
                Instruction::new_object(Reg(1), tuple_ty),
                Instruction::r_type(Opcode::CmpEq, Reg(2), Reg(0), Reg(1)),
                Instruction::ret(Reg(2)),
            ]),
            register_count: 3,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        vm.spawn_root(FuncId(0));
        assert!(matches!(
            vm.run(),
            VmResult::Error(VmError::UnsupportedEquality)
        ));
    }

    #[test]
    fn root_entry_rejects_capturing_closure_function() {
        let mut vm = make_vm();
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::closure_values(1, 0),
            func_id: FuncId(0),
            instructions: encode_instrs(&[Instruction::ret(Reg(0))]),
            register_count: 1,
            param_count: 1,
            is_closure: true,
            function_type: type_pool::TypeIndex::INVALID,
        });
        vm.spawn_root(FuncId(0));
        assert!(matches!(vm.run(), VmResult::Error(VmError::TypeError)));
    }
}
