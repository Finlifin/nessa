pub mod reactor;
pub mod reactor_net;
#[cfg(windows)]
mod win_reactor;

use std::cell::UnsafeCell;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicUsize, Ordering};

use crossbeam_deque::{Injector, Steal, Stealer, Worker};
use nsbc::FuncId;
use parking_lot::{Condvar, Mutex};
use runtime::{EffectHandler, FastHashSet, TaskId, TaskState, TaskStatus};
use stack_pool::StackPool;

pub use reactor::{ProcessOutcome, ProcessSpec, TaskOpPoll, TaskOpResult};
pub use reactor_net::NetOutcome;

// ---------------------------------------------------------------------------
// EventLoop — safe wrapper around libuv's uv_loop_t (Unix) / monotonic loop (Windows)
// ---------------------------------------------------------------------------

/// Run mode for the host event loop.
pub enum RunMode {
    /// Run until no active handles/requests.
    Default,
    /// Poll once, may block.
    Once,
    /// Poll once, never block.
    NoWait,
}

#[cfg(unix)]
impl RunMode {
    fn into_uv(self) -> libuv_sys2::uv_run_mode {
        match self {
            RunMode::Default => libuv_sys2::uv_run_mode_UV_RUN_DEFAULT,
            RunMode::Once => libuv_sys2::uv_run_mode_UV_RUN_ONCE,
            RunMode::NoWait => libuv_sys2::uv_run_mode_UV_RUN_NOWAIT,
        }
    }
}

/// Thin safe wrapper around the platform event loop.
#[cfg(unix)]
pub struct EventLoop {
    handle: *mut libuv_sys2::uv_loop_t,
}

#[cfg(unix)]
impl EventLoop {
    /// Create a new event loop.
    pub fn new() -> Self {
        // SAFETY: `uv_loop_new` allocates and initializes a fresh `uv_loop_t`.
        unsafe {
            let handle = libuv_sys2::uv_loop_new();
            assert!(!handle.is_null(), "failed to allocate uv_loop_t");
            Self { handle }
        }
    }

    /// Run the event loop with the given mode. Returns the uv_run status.
    pub fn run(&mut self, mode: RunMode) -> i32 {
        // SAFETY: `self.handle` is initialized and exclusively owned via `&mut self`.
        unsafe { libuv_sys2::uv_run(self.handle, mode.into_uv()) }
    }

    /// Check if the loop has active handles or requests.
    pub fn is_alive(&self) -> bool {
        // SAFETY: `uv_loop_alive` inspects loop counters on a valid `uv_loop_t`.
        unsafe { libuv_sys2::uv_loop_alive(self.handle) != 0 }
    }

    /// Stop the event loop.
    pub fn stop(&mut self) {
        // SAFETY: `self.handle` is valid and owned via `&mut self`.
        unsafe { libuv_sys2::uv_stop(self.handle) }
    }

    /// Return the current timestamp in milliseconds.
    pub fn now(&self) -> u64 {
        // SAFETY: `self.handle` is valid.
        unsafe { libuv_sys2::uv_now(self.handle) }
    }

    /// Get the raw loop pointer (for registering handles).
    pub fn raw(&mut self) -> *mut libuv_sys2::uv_loop_t {
        self.handle
    }
}

#[cfg(unix)]
impl Drop for EventLoop {
    fn drop(&mut self) {
        // SAFETY: Drain pending close callbacks before deleting the owned loop.
        unsafe {
            libuv_sys2::uv_run(self.handle, libuv_sys2::uv_run_mode_UV_RUN_NOWAIT);
            libuv_sys2::uv_loop_delete(self.handle);
        }
    }
}

// SAFETY: `EventLoop` owns an isolated `uv_loop_t` heap allocation and has no thread-local ties
// when moved between threads under synchronization.
#[cfg(unix)]
unsafe impl Send for EventLoop {}

/// Lightweight Windows event-loop handle (all async I/O runs on `nessa-io-reactor`).
#[cfg(windows)]
pub struct EventLoop {
    origin: std::time::Instant,
}

#[cfg(windows)]
impl EventLoop {
    /// Create a new event loop.
    pub fn new() -> Self {
        Self {
            origin: std::time::Instant::now(),
        }
    }

    /// Poll once; async handles are driven on `nessa-io-reactor`.
    pub fn run(&mut self, _mode: RunMode) -> i32 {
        0
    }

    /// Check if the inline loop has active handles.
    pub fn is_alive(&self) -> bool {
        false
    }

    /// Stop the inline loop.
    pub fn stop(&mut self) {}

    /// Return monotonic elapsed milliseconds.
    pub fn now(&self) -> u64 {
        self.origin.elapsed().as_millis() as u64
    }
}

impl Default for EventLoop {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// TaskArena — Lock-free chunked O(1) generational slot table
// ---------------------------------------------------------------------------

const CHUNK_BITS: u32 = 10;
const CHUNK_SIZE: usize = 1 << CHUNK_BITS; // 1024 slots per chunk
const CHUNK_MASK: usize = CHUNK_SIZE - 1;
const MAX_CHUNKS: usize = 4096; // Up to 4,194,304 simultaneous live tasks

#[derive(Default)]
struct SlotMeta {
    occupied: bool,
    status: Option<TaskStatus>,
    wake_token: bool,
    external_waiter: bool,
    parent: Option<TaskId>,
    children: FastHashSet<TaskId>,
    promise_set: FastHashSet<u64>,
    spawned_promise: Option<u64>,
}

const FREE_SLOT_SHARDS: usize = 16;

struct ArenaSlot {
    generation: AtomicU32,
    cancelled: AtomicBool,
    meta: Mutex<SlotMeta>,
    /// Owned exclusively by the executing worker while `status == Running`, or
    /// visited exclusively under stopped-world GC / single-threaded inspection.
    task: UnsafeCell<Option<Box<TaskState>>>,
    /// Cached `TaskState` allocation retained across reap/spawn cycles under `meta` lock.
    recycled_task: UnsafeCell<Option<Box<TaskState>>>,
}

impl ArenaSlot {
    fn new() -> Self {
        Self {
            generation: AtomicU32::new(0),
            cancelled: AtomicBool::new(false),
            meta: Mutex::new(SlotMeta::default()),
            task: UnsafeCell::new(None),
            recycled_task: UnsafeCell::new(None),
        }
    }
}

struct ArenaChunk {
    slots: [ArenaSlot; CHUNK_SIZE],
}

impl ArenaChunk {
    fn new_boxed() -> Box<Self> {
        let mut vec = Vec::with_capacity(CHUNK_SIZE);
        for _ in 0..CHUNK_SIZE {
            vec.push(ArenaSlot::new());
        }
        let boxed_slice = vec.into_boxed_slice();
        // SAFETY: `ArenaChunk` is a single-field wrapper around `[ArenaSlot; CHUNK_SIZE]`
        // and `boxed_slice` has exact length `CHUNK_SIZE`.
        unsafe {
            let raw = Box::into_raw(boxed_slice) as *mut ArenaChunk;
            Box::from_raw(raw)
        }
    }
}

/// Concurrent generational `O(1)` task table with append-only fixed chunks.
///
/// Growing the arena allocates new chunks without relocating any existing `ArenaSlot`
/// or `TaskState`, keeping both fast worker access and STW GC root slots stable.
pub struct TaskArena {
    chunks: Box<[AtomicPtr<ArenaChunk>; MAX_CHUNKS]>,
    chunk_alloc_lock: Mutex<()>,
    high_water_slots: AtomicU32,
    free_slots: [Mutex<Vec<u32>>; FREE_SLOT_SHARDS],
    free_cursor: AtomicUsize,
    occupied: AtomicUsize,
}

// SAFETY: Cross-thread operations synchronize slot metadata through `ArenaSlot::meta` and
// `generation`/`cancelled` atomics; `ArenaSlot::task` (`UnsafeCell`) is touched only by the
// unique worker holding `TaskStatus::Running` (or under exclusive `&mut` / STW stop-the-world).
unsafe impl Send for TaskArena {}
// SAFETY: Same ownership partition as `Send` above.
unsafe impl Sync for TaskArena {}

impl TaskArena {
    pub fn new() -> Self {
        let chunks_vec: Vec<AtomicPtr<ArenaChunk>> = (0..MAX_CHUNKS)
            .map(|_| AtomicPtr::new(ptr::null_mut()))
            .collect();
        let chunks: Box<[AtomicPtr<ArenaChunk>; MAX_CHUNKS]> = chunks_vec
            .into_boxed_slice()
            .try_into()
            .expect("exact MAX_CHUNKS length");
        Self {
            chunks,
            chunk_alloc_lock: Mutex::new(()),
            high_water_slots: AtomicU32::new(0),
            free_slots: std::array::from_fn(|_| Mutex::new(Vec::new())),
            free_cursor: AtomicUsize::new(0),
            occupied: AtomicUsize::new(0),
        }
    }

    fn pop_free_slot(&self) -> Option<u32> {
        let start = self.free_cursor.fetch_add(1, Ordering::Relaxed) & (FREE_SLOT_SHARDS - 1);
        for off in 0..FREE_SLOT_SHARDS {
            let idx = (start + off) & (FREE_SLOT_SHARDS - 1);
            if let Some(slot_idx) = self.free_slots[idx].lock().pop() {
                return Some(slot_idx);
            }
        }
        None
    }

    #[inline]
    fn push_free_slot(&self, slot_idx: u32) {
        self.free_slots[(slot_idx as usize) & (FREE_SLOT_SHARDS - 1)]
            .lock()
            .push(slot_idx);
    }

    #[inline]
    fn slot(&self, slot_idx: u32) -> Option<&ArenaSlot> {
        let idx = slot_idx as usize;
        let chunk_idx = idx >> CHUNK_BITS;
        if chunk_idx >= MAX_CHUNKS {
            return None;
        }
        let chunk_ptr = self.chunks[chunk_idx].load(Ordering::Acquire);
        if chunk_ptr.is_null() {
            return None;
        }
        // SAFETY: Once published into `self.chunks[chunk_idx]`, the `ArenaChunk` lives
        // until `TaskArena::drop`.
        let chunk = unsafe { &*chunk_ptr };
        Some(&chunk.slots[idx & CHUNK_MASK])
    }

    fn ensure_slot(&self, slot_idx: u32) -> &ArenaSlot {
        let idx = slot_idx as usize;
        let chunk_idx = idx >> CHUNK_BITS;
        assert!(chunk_idx < MAX_CHUNKS, "task arena capacity exceeded");
        let mut chunk_ptr = self.chunks[chunk_idx].load(Ordering::Acquire);
        if chunk_ptr.is_null() {
            let _guard = self.chunk_alloc_lock.lock();
            chunk_ptr = self.chunks[chunk_idx].load(Ordering::Acquire);
            if chunk_ptr.is_null() {
                let fresh = Box::into_raw(ArenaChunk::new_boxed());
                self.chunks[chunk_idx].store(fresh, Ordering::Release);
                chunk_ptr = fresh;
            }
        }
        // SAFETY: `chunk_ptr` is non-null and stays valid for the lifetime of `TaskArena`.
        let chunk = unsafe { &*chunk_ptr };
        &chunk.slots[idx & CHUNK_MASK]
    }

    fn alloc_with<F>(&self, func_id: FuncId, pool: Option<Arc<StackPool>>, init: F) -> TaskId
    where
        F: FnOnce(TaskId, &mut TaskState, &mut SlotMeta),
    {
        let slot_idx = if let Some(reused) = self.pop_free_slot() {
            reused
        } else {
            let idx = self
                .high_water_slots
                .try_update(Ordering::AcqRel, Ordering::Acquire, |cur| {
                    (cur < (MAX_CHUNKS * CHUNK_SIZE) as u32).then_some(cur + 1)
                })
                .expect("task arena slot index overflow");
            let _ = self.ensure_slot(idx);
            idx
        };
        let slot = self.ensure_slot(slot_idx);
        let mut meta = slot.meta.lock();
        let generation = slot.generation.load(Ordering::Acquire);
        let id = TaskId::from_parts(slot_idx, generation);
        slot.cancelled.store(false, Ordering::Release);
        // SAFETY: `meta` is locked while transitioning this vacant slot to occupied,
        // so `recycled_task` has exclusive access.
        let recycled = unsafe { (*slot.recycled_task.get()).take() };
        let mut boxed_task = if let Some(mut task) = recycled {
            task.reinit_for_spawn(id, func_id, pool);
            task
        } else {
            Box::new(match pool {
                Some(pool) => TaskState::new_with_pool(id, func_id, pool),
                None => TaskState::new(id, func_id),
            })
        };
        meta.occupied = true;
        meta.status = Some(TaskStatus::Ready);
        meta.wake_token = false;
        meta.external_waiter = false;
        meta.parent = None;
        meta.children.clear();
        meta.promise_set.clear();
        meta.spawned_promise = None;
        init(id, &mut boxed_task, &mut meta);
        // SAFETY: `meta` is locked while transitioning this vacant slot to occupied,
        // and no reader can observe `id` before `alloc_with` returns `id`.
        unsafe {
            *slot.task.get() = Some(boxed_task);
        }
        self.occupied.fetch_add(1, Ordering::AcqRel);
        id
    }

    #[inline]
    pub fn occupied_count(&self) -> usize {
        self.occupied.load(Ordering::Acquire)
    }

    /// Borrow `id`'s `TaskState` on its owning worker thread (or post-run / STW).
    ///
    /// # Safety
    /// The caller must guarantee no concurrent mutable access to `id`'s `TaskState`
    /// is occurring on another thread (e.g., the caller is the worker executing `id`,
    /// or all worker threads are stopped).
    #[inline]
    pub unsafe fn task_unchecked(&self, id: TaskId) -> Option<&TaskState> {
        let slot = self.slot(id.slot())?;
        if slot.generation.load(Ordering::Acquire) != id.generation() {
            return None;
        }
        // SAFETY: Caller upholds exclusive-owner / stopped-world invariant.
        unsafe { (*slot.task.get()).as_deref() }
    }

    /// Mutably borrow `id`'s `TaskState` on its owning worker thread (or post-run / STW).
    ///
    /// # Safety
    /// The caller must guarantee exclusive access to `id`'s `TaskState` (e.g., the caller
    /// is the unique worker executing `id` with no aliased borrow, or holds `&mut TaskArena` / STW).
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn task_unchecked_mut(&self, id: TaskId) -> Option<&mut TaskState> {
        let slot = self.slot(id.slot())?;
        if slot.generation.load(Ordering::Acquire) != id.generation() {
            return None;
        }
        // SAFETY: Caller upholds exclusive access to this slot's `TaskState`.
        unsafe { (*slot.task.get()).as_deref_mut() }
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut TaskState> {
        let bound = self.high_water_slots.load(Ordering::Acquire);
        (0..bound).filter_map(|idx| {
            let slot = self.slot(idx)?;
            // SAFETY: `&mut self` guarantees exclusive access to all slots.
            unsafe { (*slot.task.get()).as_deref_mut() }
        })
    }

    /// Mutably iterate all occupied `TaskState`s when all mutator workers are stopped.
    ///
    /// # Safety
    /// Must only be called when all mutator workers are stopped (e.g. STW GC or `&mut Scheduler`)
    /// and non-mutator threads (`nessa-io-reactor`) never access `ArenaSlot::task`.
    pub unsafe fn iter_stopped_mut(&self) -> impl Iterator<Item = &mut TaskState> {
        let bound = self.high_water_slots.load(Ordering::Acquire);
        (0..bound).filter_map(|idx| {
            let slot = self.slot(idx)?;
            // SAFETY: Upheld by caller (stopped mutator world).
            unsafe { (*slot.task.get()).as_deref_mut() }
        })
    }

    /// Iterate all occupied `TaskState`s when no worker threads are concurrently mutating tasks.
    ///
    /// # Safety
    /// Must only be called when all workers are stopped (e.g. STW GC or outside `Vm::run`).
    pub unsafe fn iter_stopped(&self) -> impl Iterator<Item = &TaskState> {
        let bound = self.high_water_slots.load(Ordering::Acquire);
        (0..bound).filter_map(|idx| {
            let slot = self.slot(idx)?;
            // SAFETY: Caller guarantees all workers are stopped.
            unsafe { (*slot.task.get()).as_deref() }
        })
    }
}

impl Default for TaskArena {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for TaskArena {
    fn drop(&mut self) {
        for chunk_atom in self.chunks.iter() {
            let ptr = chunk_atom.swap(ptr::null_mut(), Ordering::AcqRel);
            if !ptr.is_null() {
                // SAFETY: Every non-null chunk pointer was allocated via `Box::into_raw`.
                unsafe {
                    drop(Box::from_raw(ptr));
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Work-Stealing Scheduler — M:N multi-worker task scheduler
// ---------------------------------------------------------------------------

/// Outcome of polling the scheduler from an OS worker thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerNext {
    /// Execute `TaskId` (already transitioned to `TaskStatus::Running`).
    Task(TaskId),
    /// All tasks have finished (or only external host-suspended tasks remain).
    AllFinished,
    /// Zero tasks are running or ready, zero I/O handles are active, and internal tasks remain suspended.
    Deadlock,
    /// Another worker hit an uncaught root error and requested global abort.
    Aborted,
}

#[derive(Default)]
struct ParkState {
    epoch: u64,
    sleeping_workers: usize,
    pending_notifies: usize,
}

struct Parker {
    lock: Mutex<ParkState>,
    cv: Condvar,
    sleeping_count: AtomicUsize,
    unnotified_sleepers: AtomicUsize,
}

impl Parker {
    fn new() -> Self {
        Self {
            lock: Mutex::new(ParkState::default()),
            cv: Condvar::new(),
            sleeping_count: AtomicUsize::new(0),
            unnotified_sleepers: AtomicUsize::new(0),
        }
    }

    #[inline]
    fn sync_counts(&self, state: &ParkState, order: Ordering) {
        self.sleeping_count.store(state.sleeping_workers, order);
        self.unnotified_sleepers.store(
            state
                .sleeping_workers
                .saturating_sub(state.pending_notifies),
            order,
        );
    }

    #[inline]
    fn notify_one_work(&self) {
        std::sync::atomic::fence(Ordering::SeqCst);
        if self.unnotified_sleepers.load(Ordering::Relaxed) == 0 {
            return;
        }
        let mut state = self.lock.lock();
        state.epoch = state.epoch.wrapping_add(1);
        if state.sleeping_workers > state.pending_notifies {
            state.pending_notifies += 1;
            self.unnotified_sleepers.store(
                state.sleeping_workers - state.pending_notifies,
                Ordering::Release,
            );
            self.cv.notify_one();
        }
    }

    #[inline]
    fn notify_all_state_change(&self) {
        std::sync::atomic::fence(Ordering::SeqCst);
        if self.sleeping_count.load(Ordering::Relaxed) == 0 {
            return;
        }
        let mut state = self.lock.lock();
        state.epoch = state.epoch.wrapping_add(1);
        if state.sleeping_workers > 0 {
            state.pending_notifies = state.sleeping_workers;
            self.unnotified_sleepers.store(0, Ordering::Release);
            self.cv.notify_all();
        }
    }
}

/// Worker-local FIFO deque of scheduled [`TaskId`]s.
pub type WorkerQueue = Worker<TaskId>;

/// Exclusive lease of one worker thread's local FIFO work-stealing deque.
pub struct WorkerQueueLease<'a> {
    scheduler: &'a Scheduler,
    worker_id: usize,
    queue: Option<Worker<TaskId>>,
}

impl WorkerQueueLease<'_> {
    #[inline]
    pub fn worker_id(&self) -> usize {
        self.worker_id
    }

    #[inline]
    pub fn queue(&self) -> &Worker<TaskId> {
        self.queue.as_ref().expect("active worker queue lease")
    }
}

impl Drop for WorkerQueueLease<'_> {
    fn drop(&mut self) {
        if let Some(q) = self.queue.take() {
            *self.scheduler.inner.local_queues[self.worker_id].lock() = Some(q);
        }
    }
}

pub(crate) struct SchedulerInner {
    worker_count: usize,
    injector: Injector<TaskId>,
    local_queues: Box<[Mutex<Option<Worker<TaskId>>>]>,
    stealers: Box<[Stealer<TaskId>]>,
    parker: Parker,
    arena: TaskArena,
    active_tasks: AtomicUsize,
    in_progress_tasks: AtomicUsize,
    running_tasks: AtomicUsize,
    ready_tasks: AtomicUsize,
    suspended_tasks: AtomicUsize,
    external_waiters: AtomicUsize,
    active_io_handles: AtomicUsize,
    aborted: AtomicBool,
    event_loop: Mutex<EventLoop>,
    reactor: reactor::ReactorController,
    cancel_hook: std::sync::OnceLock<Arc<dyn Fn(TaskId) + Send + Sync>>,
    stack_pool: Arc<StackPool>,
}

/// Multi-worker work-stealing task scheduler backed by `TaskArena` and `libuv` I/O reactor.
#[derive(Clone)]
pub struct Scheduler {
    inner: Arc<SchedulerInner>,
}

/// Non-owning handle to [`Scheduler`] avoiding reference cycles inside `cancel_hook`.
#[derive(Clone)]
pub struct WeakScheduler {
    inner: std::sync::Weak<SchedulerInner>,
}

impl WeakScheduler {
    #[inline]
    pub fn upgrade(&self) -> Option<Scheduler> {
        self.inner.upgrade().map(|inner| Scheduler { inner })
    }
}

impl Drop for SchedulerInner {
    fn drop(&mut self) {
        self.reactor.shutdown();
    }
}

impl SchedulerInner {
    #[inline]
    pub(crate) fn inc_active_io(&self) {
        self.active_io_handles.fetch_add(1, Ordering::AcqRel);
    }

    #[inline]
    pub(crate) fn dec_active_io(&self) {
        let prev = self.active_io_handles.fetch_sub(1, Ordering::AcqRel);
        if prev == 1 {
            self.parker.notify_all_state_change();
        }
    }

    fn apply_status_delta(
        &self,
        meta: &mut SlotMeta,
        new_status: TaskStatus,
    ) -> Option<TaskStatus> {
        let old_status = meta.status?;
        if old_status == new_status {
            return Some(old_status);
        }
        let was_in_progress = matches!(old_status, TaskStatus::Ready | TaskStatus::Running);
        let now_in_progress = matches!(new_status, TaskStatus::Ready | TaskStatus::Running);
        if !was_in_progress && now_in_progress {
            self.in_progress_tasks.fetch_add(1, Ordering::AcqRel);
        }
        match new_status {
            TaskStatus::Ready => {
                self.ready_tasks.fetch_add(1, Ordering::AcqRel);
            }
            TaskStatus::Running => {
                self.running_tasks.fetch_add(1, Ordering::AcqRel);
            }
            TaskStatus::Suspended | TaskStatus::Waiting => {
                self.suspended_tasks.fetch_add(1, Ordering::AcqRel);
            }
            TaskStatus::Finished => {
                self.active_tasks.fetch_sub(1, Ordering::AcqRel);
            }
        }
        meta.status = Some(new_status);
        match old_status {
            TaskStatus::Ready => {
                self.ready_tasks.fetch_sub(1, Ordering::AcqRel);
            }
            TaskStatus::Running => {
                self.running_tasks.fetch_sub(1, Ordering::AcqRel);
            }
            TaskStatus::Suspended | TaskStatus::Waiting => {
                self.suspended_tasks.fetch_sub(1, Ordering::AcqRel);
                if meta.external_waiter {
                    meta.external_waiter = false;
                    self.external_waiters.fetch_sub(1, Ordering::AcqRel);
                }
            }
            TaskStatus::Finished => {
                self.active_tasks.fetch_add(1, Ordering::AcqRel);
            }
        }
        if was_in_progress && !now_in_progress {
            self.in_progress_tasks.fetch_sub(1, Ordering::AcqRel);
        }
        Some(old_status)
    }

    /// Wake a task from a non-mutator thread (`nessa-io-reactor` or host callback) without
    /// touching `ArenaSlot::task`, keeping STW GC root scanning completely disjoint.
    pub(crate) fn wake_from_reactor(&self, id: TaskId) -> bool {
        self.wake_with_queue(None, id)
    }

    pub(crate) fn wake_with_queue(&self, local_queue: Option<&Worker<TaskId>>, id: TaskId) -> bool {
        let Some(slot) = self.arena.slot(id.slot()) else {
            return false;
        };
        let mut meta = slot.meta.lock();
        if slot.generation.load(Ordering::Acquire) != id.generation() || !meta.occupied {
            return false;
        }
        match meta.status {
            Some(TaskStatus::Suspended | TaskStatus::Waiting) => {
                meta.wake_token = false;
                let _ = self.apply_status_delta(&mut meta, TaskStatus::Ready);
                drop(meta);
                if let Some(local) = local_queue {
                    local.push(id);
                } else {
                    self.injector.push(id);
                }
                self.parker.notify_one_work();
                true
            }
            Some(TaskStatus::Running) => {
                meta.wake_token = true;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn cancel_waiters(&self, id: TaskId) {
        self.reactor.cancel_task(self, id);
        if let Some(cb) = self.cancel_hook.get() {
            cb(id);
        }
    }
}

impl Scheduler {
    pub fn new() -> Self {
        Self::with_workers(Arc::new(StackPool::new()), 1)
    }

    pub fn with_stack_pool(stack_pool: Arc<StackPool>) -> Self {
        Self::with_workers(stack_pool, 1)
    }

    /// Create a work-stealing scheduler configured for `worker_count` OS worker threads.
    pub fn with_workers(stack_pool: Arc<StackPool>, worker_count: usize) -> Self {
        let n = worker_count.max(1);
        let mut locals = Vec::with_capacity(n);
        let mut stealers = Vec::with_capacity(n);
        for _ in 0..n {
            let w = Worker::new_fifo();
            stealers.push(w.stealer());
            locals.push(Mutex::new(Some(w)));
        }
        Self {
            inner: Arc::new(SchedulerInner {
                worker_count: n,
                injector: Injector::new(),
                local_queues: locals.into_boxed_slice(),
                stealers: stealers.into_boxed_slice(),
                parker: Parker::new(),
                arena: TaskArena::new(),
                active_tasks: AtomicUsize::new(0),
                in_progress_tasks: AtomicUsize::new(0),
                running_tasks: AtomicUsize::new(0),
                ready_tasks: AtomicUsize::new(0),
                suspended_tasks: AtomicUsize::new(0),
                external_waiters: AtomicUsize::new(0),
                active_io_handles: AtomicUsize::new(0),
                aborted: AtomicBool::new(false),
                event_loop: Mutex::new(EventLoop::new()),
                reactor: reactor::ReactorController::new(),
                cancel_hook: std::sync::OnceLock::new(),
                stack_pool,
            }),
        }
    }

    #[inline]
    pub fn worker_count(&self) -> usize {
        self.inner.worker_count
    }

    #[inline]
    pub fn stack_pool(&self) -> &Arc<StackPool> {
        &self.inner.stack_pool
    }

    #[inline]
    pub fn downgrade(&self) -> WeakScheduler {
        WeakScheduler {
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// Register a callback invoked whenever a task is cancelled, finished, or reaped
    /// so external wait queues (e.g. `std.sync` primitives) immediately purge the task.
    pub fn set_cancel_hook(&self, hook: Arc<dyn Fn(TaskId) + Send + Sync>) {
        let _ = self.inner.cancel_hook.set(hook);
    }

    /// Cancel any pending `IoReactor` operation or registered sync wait-queue entry for `id`.
    pub fn cancel_task_waiters(&self, id: TaskId) {
        self.inner.cancel_waiters(id);
    }

    /// Poll a task-scoped operation on the asynchronous `libuv` reactor.
    pub fn poll_task_op(&self, task_id: TaskId) -> TaskOpPoll {
        self.inner.reactor.poll_task_op(task_id)
    }

    /// Submit a non-blocking `uv_timer_t` sleep operation for `task_id`.
    pub fn submit_sleep(&self, task_id: TaskId, duration_ms: u64) {
        self.inner
            .reactor
            .submit_sleep(&self.inner, task_id, duration_ms);
    }

    /// Submit a non-blocking `uv_spawn` child process operation for `task_id`.
    pub fn submit_process(&self, task_id: TaskId, spec: ProcessSpec) {
        self.inner
            .reactor
            .submit_process(&self.inner, task_id, spec);
    }

    /// Offload a blocking closure onto `libuv`'s `uv_queue_work` threadpool for `task_id`.
    pub fn submit_work<F, T>(&self, task_id: TaskId, work: F)
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        self.inner.reactor.submit_work(&self.inner, task_id, work);
    }

    pub fn submit_tcp_listen(&self, task_id: TaskId, host: String, port: i64, backlog: i64) {
        self.inner
            .reactor
            .submit_tcp_listen(&self.inner, task_id, host, port, backlog);
    }

    pub fn submit_tcp_accept(&self, task_id: TaskId, listener_id: i64) {
        self.inner
            .reactor
            .submit_tcp_accept(&self.inner, task_id, listener_id);
    }

    pub fn submit_tcp_connect(&self, task_id: TaskId, host: String, port: i64) {
        self.inner
            .reactor
            .submit_tcp_connect(&self.inner, task_id, host, port);
    }

    pub fn submit_tcp_read_bytes(&self, task_id: TaskId, stream_id: i64, max_bytes: usize) {
        self.inner
            .reactor
            .submit_tcp_read_bytes(&self.inner, task_id, stream_id, max_bytes);
    }

    pub fn submit_tcp_read_line(&self, task_id: TaskId, stream_id: i64) {
        self.inner
            .reactor
            .submit_tcp_read_line(&self.inner, task_id, stream_id);
    }

    pub fn submit_tcp_write(&self, task_id: TaskId, stream_id: i64, data: Vec<u8>) {
        self.inner
            .reactor
            .submit_tcp_write(&self.inner, task_id, stream_id, data);
    }

    pub fn submit_tcp_set_nodelay(&self, task_id: TaskId, stream_id: i64, enable: bool) {
        self.inner
            .reactor
            .submit_tcp_set_nodelay(&self.inner, task_id, stream_id, enable);
    }

    pub fn tcp_local_addr(&self, id: i64) -> Option<(String, i64)> {
        self.inner.reactor.tcp_local_addr(id)
    }

    pub fn tcp_peer_addr(&self, id: i64) -> Option<(String, i64)> {
        self.inner.reactor.tcp_peer_addr(id)
    }

    pub fn tcp_close(&self, id: i64) -> bool {
        self.inner.reactor.tcp_close(id)
    }

    pub fn submit_udp_bind(&self, task_id: TaskId, host: String, port: i64) {
        self.inner
            .reactor
            .submit_udp_bind(&self.inner, task_id, host, port);
    }

    pub fn submit_udp_send(
        &self,
        task_id: TaskId,
        sock_id: i64,
        host: String,
        port: i64,
        data: Vec<u8>,
    ) {
        self.inner
            .reactor
            .submit_udp_send(&self.inner, task_id, sock_id, host, port, data);
    }

    pub fn submit_udp_recv(&self, task_id: TaskId, sock_id: i64, max_bytes: usize) {
        self.inner
            .reactor
            .submit_udp_recv(&self.inner, task_id, sock_id, max_bytes);
    }

    pub fn udp_local_addr(&self, id: i64) -> Option<(String, i64)> {
        self.inner.reactor.udp_local_addr(id)
    }

    pub fn udp_close(&self, id: i64) -> bool {
        self.inner.reactor.udp_close(id)
    }

    pub fn submit_resolve_host(&self, task_id: TaskId, host: String) {
        self.inner
            .reactor
            .submit_resolve_host(&self.inner, task_id, host);
    }

    /// Lease worker `worker_id`'s local FIFO deque for the duration of a worker loop.
    pub fn lease_worker_queue(&self, worker_id: usize) -> WorkerQueueLease<'_> {
        let queue = self.inner.local_queues[worker_id]
            .lock()
            .take()
            .expect("worker queue leased only once per worker");
        WorkerQueueLease {
            scheduler: self,
            worker_id,
            queue: Some(queue),
        }
    }

    /// Poll the non-blocking libuv loop once.
    pub fn poll_io(&self) -> bool {
        self.inner.event_loop.lock().run(RunMode::NoWait) != 0
    }

    #[inline]
    pub fn inc_active_io(&self) {
        self.inner.inc_active_io();
    }

    #[inline]
    pub fn dec_active_io(&self) {
        self.inner.dec_active_io();
    }

    #[inline]
    pub fn active_io_count(&self) -> usize {
        self.inner.active_io_handles.load(Ordering::Acquire)
    }

    #[inline]
    pub fn request_abort(&self) {
        self.inner.aborted.store(true, Ordering::Release);
        self.inner.parker.notify_all_state_change();
    }

    #[inline]
    pub fn clear_abort(&self) {
        self.inner.aborted.store(false, Ordering::Release);
    }

    #[inline]
    pub fn is_aborted(&self) -> bool {
        self.inner.aborted.load(Ordering::Acquire)
    }

    fn apply_status_delta(
        &self,
        meta: &mut SlotMeta,
        new_status: TaskStatus,
    ) -> Option<TaskStatus> {
        self.inner.apply_status_delta(meta, new_status)
    }

    /// Spawn a new task into the global injector queue.
    pub fn spawn(&self, func_id: FuncId, parent: Option<TaskId>) -> TaskId {
        self.spawn_with_context(None, func_id, parent, None, Vec::new(), |_| {})
    }

    /// Allocate and initialize a `Ready` task slot and link it to `parent`, reusing
    /// recycled `ancestor_handlers` buffer capacity in place without allocating a `Vec`.
    pub fn alloc_unscheduled_task_from_slice<F>(
        &self,
        func_id: FuncId,
        parent: Option<TaskId>,
        spawned_promise: Option<u64>,
        ancestor_handlers: &[EffectHandler],
        setup: F,
    ) -> TaskId
    where
        F: FnOnce(&mut TaskState),
    {
        let inner = &self.inner;
        let pool = Arc::clone(&inner.stack_pool);
        let id = inner
            .arena
            .alloc_with(func_id, Some(pool), |_id, task, meta| {
                task.parent = parent;
                task.status = TaskStatus::Ready;
                task.spawned_promise = spawned_promise;
                task.ancestor_handlers.clear();
                task.ancestor_handlers.extend_from_slice(ancestor_handlers);
                setup(task);
                meta.parent = parent;
                meta.spawned_promise = spawned_promise;
                meta.status = Some(TaskStatus::Ready);
            });

        inner.active_tasks.fetch_add(1, Ordering::AcqRel);
        inner.in_progress_tasks.fetch_add(1, Ordering::AcqRel);
        inner.ready_tasks.fetch_add(1, Ordering::AcqRel);

        if let Some(pid) = parent
            && let Some(pslot) = inner.arena.slot(pid.slot())
        {
            let mut pmeta = pslot.meta.lock();
            if pslot.generation.load(Ordering::Acquire) == pid.generation() && pmeta.occupied {
                pmeta.children.insert(id);
                if let Some(fid) = spawned_promise {
                    pmeta.promise_set.insert(fid);
                }
                if pslot.cancelled.load(Ordering::Acquire)
                    && let Some(cslot) = inner.arena.slot(id.slot())
                {
                    cslot.cancelled.store(true, Ordering::Release);
                }
            }
        }
        id
    }

    /// Allocate and initialize a `Ready` task slot and link it to `parent`, without yet
    /// pushing its `TaskId` into any work queue so callers can register promise metadata
    /// before any thief worker can claim it.
    pub fn alloc_unscheduled_task<F>(
        &self,
        func_id: FuncId,
        parent: Option<TaskId>,
        spawned_promise: Option<u64>,
        ancestor_handlers: Vec<EffectHandler>,
        setup: F,
    ) -> TaskId
    where
        F: FnOnce(&mut TaskState),
    {
        self.alloc_unscheduled_task_from_slice(
            func_id,
            parent,
            spawned_promise,
            &ancestor_handlers,
            setup,
        )
    }

    /// Publish a task previously created by [`Self::alloc_unscheduled_task`] into a work queue.
    pub fn publish_spawned_task(&self, local_queue: Option<&Worker<TaskId>>, id: TaskId) {
        if let Some(local) = local_queue {
            local.push(id);
        } else {
            self.inner.injector.push(id);
        }
        self.inner.parker.notify_one_work();
    }

    /// Spawn a new task with optional worker-local queue placement, promise binding,
    /// ancestor handler snapshot, and initial `TaskState` setup before publication.
    pub fn spawn_with_context<F>(
        &self,
        local_queue: Option<&Worker<TaskId>>,
        func_id: FuncId,
        parent: Option<TaskId>,
        spawned_promise: Option<u64>,
        ancestor_handlers: Vec<EffectHandler>,
        setup: F,
    ) -> TaskId
    where
        F: FnOnce(&mut TaskState),
    {
        let id =
            self.alloc_unscheduled_task(func_id, parent, spawned_promise, ancestor_handlers, setup);
        self.publish_spawned_task(local_queue, id);
        id
    }

    fn steal_from_injector_only(&self) -> Option<TaskId> {
        loop {
            match self.inner.injector.steal() {
                Steal::Success(id) => return Some(id),
                Steal::Empty => return None,
                Steal::Retry => std::hint::spin_loop(),
            }
        }
    }

    /// Single-threaded helper: pop the next `Ready` task without transitioning it to `Running`
    /// (preserves compatibility with existing scheduler unit tests that call `set_running` next).
    pub fn next_ready(&self) -> Option<TaskId> {
        let inner = &self.inner;
        loop {
            let candidate = self.steal_from_injector_only().or_else(|| {
                for stealer in inner.stealers.iter() {
                    loop {
                        match stealer.steal() {
                            Steal::Success(id) => return Some(id),
                            Steal::Empty => break,
                            Steal::Retry => std::hint::spin_loop(),
                        }
                    }
                }
                None
            })?;
            if let Some(slot) = inner.arena.slot(candidate.slot()) {
                let meta = slot.meta.lock();
                if slot.generation.load(Ordering::Acquire) == candidate.generation()
                    && meta.occupied
                    && meta.status == Some(TaskStatus::Ready)
                {
                    return Some(candidate);
                }
            }
        }
    }

    fn pop_raw_candidate(
        &self,
        worker_id: usize,
        local: &Worker<TaskId>,
        tick: &mut u64,
    ) -> Option<TaskId> {
        *tick = tick.wrapping_add(1);
        if (*tick).is_multiple_of(61)
            && let Some(id) = self.steal_batch_from_injector(local)
        {
            return Some(id);
        }
        if let Some(id) = local.pop() {
            return Some(id);
        }
        if let Some(id) = self.steal_batch_from_injector(local) {
            return Some(id);
        }
        let n = self.inner.stealers.len();
        if n > 1 {
            let start = (*tick as usize) % n;
            for offset in 0..n {
                let target = (start + offset) % n;
                if target == worker_id {
                    continue;
                }
                loop {
                    match self.inner.stealers[target].steal_batch_and_pop(local) {
                        Steal::Success(id) => return Some(id),
                        Steal::Empty => break,
                        Steal::Retry => std::hint::spin_loop(),
                    }
                }
            }
        }
        None
    }

    fn steal_batch_from_injector(&self, local: &Worker<TaskId>) -> Option<TaskId> {
        loop {
            match self.inner.injector.steal_batch_and_pop(local) {
                Steal::Success(id) => return Some(id),
                Steal::Empty => return None,
                Steal::Retry => std::hint::spin_loop(),
            }
        }
    }

    /// Non-blocking attempt to claim the next runnable task for `worker_id` without parking.
    pub fn try_claim_for_worker(
        &self,
        worker_id: usize,
        local: &Worker<TaskId>,
        tick: &mut u64,
    ) -> Option<TaskId> {
        let inner = &self.inner;
        while let Some(id) = self.pop_raw_candidate(worker_id, local, tick) {
            let Some(slot) = inner.arena.slot(id.slot()) else {
                continue;
            };
            let mut meta = slot.meta.lock();
            if slot.generation.load(Ordering::Acquire) == id.generation()
                && meta.occupied
                && meta.status == Some(TaskStatus::Ready)
            {
                meta.wake_token = false;
                let _ = self.apply_status_delta(&mut meta, TaskStatus::Running);
                drop(meta);
                // SAFETY: This worker just atomically claimed `TaskStatus::Running` on `id`.
                if let Some(task) = unsafe { inner.arena.task_unchecked_mut(id) } {
                    task.status = TaskStatus::Running;
                    task.cancelled = slot.cancelled.load(Ordering::Acquire);
                }
                return Some(id);
            }
        }
        None
    }

    /// Claim the next runnable task for `worker_id`, parking on `Parker` when idle and
    /// returning `AllFinished`, `Deadlock`, or `Aborted` on global termination.
    ///
    /// Callers MUST NOT hold an active GC mutator session while calling this method.
    pub fn next_for_worker(
        &self,
        worker_id: usize,
        local: &Worker<TaskId>,
        tick: &mut u64,
    ) -> WorkerNext {
        let inner = &self.inner;
        loop {
            if inner.aborted.load(Ordering::Acquire) {
                return WorkerNext::Aborted;
            }
            if let Some(id) = self.try_claim_for_worker(worker_id, local, tick) {
                if inner.ready_tasks.load(Ordering::Acquire) > 0 {
                    inner.parker.notify_one_work();
                }
                return WorkerNext::Task(id);
            }

            // Brief spin before taking the parker lock to absorb sub-microsecond handoffs.
            let mut spun_task = None;
            if inner.worker_count > 1 {
                for _ in 0..32 {
                    std::hint::spin_loop();
                    if inner.aborted.load(Ordering::Acquire) {
                        return WorkerNext::Aborted;
                    }
                    if inner.ready_tasks.load(Ordering::Acquire) > 0
                        && let Some(id) = self.try_claim_for_worker(worker_id, local, tick)
                    {
                        spun_task = Some(id);
                        break;
                    }
                }
            }
            if let Some(id) = spun_task {
                if inner.ready_tasks.load(Ordering::Acquire) > 0 {
                    inner.parker.notify_one_work();
                }
                return WorkerNext::Task(id);
            }

            let mut guard = inner.parker.lock.lock();
            guard.sleeping_workers += 1;
            inner.parker.sync_counts(&guard, Ordering::SeqCst);

            if inner.aborted.load(Ordering::Acquire) {
                guard.sleeping_workers -= 1;
                guard.pending_notifies = guard.pending_notifies.min(guard.sleeping_workers);
                inner.parker.sync_counts(&guard, Ordering::Release);
                return WorkerNext::Aborted;
            }
            if let Some(id) = self.try_claim_for_worker(worker_id, local, tick) {
                guard.sleeping_workers -= 1;
                guard.pending_notifies = guard.pending_notifies.min(guard.sleeping_workers);
                let more_ready = inner.ready_tasks.load(Ordering::Acquire) > 0;
                if more_ready && guard.sleeping_workers > guard.pending_notifies {
                    guard.pending_notifies += 1;
                    inner.parker.sync_counts(&guard, Ordering::Release);
                    inner.parker.cv.notify_one();
                } else {
                    inner.parker.sync_counts(&guard, Ordering::Release);
                }
                return WorkerNext::Task(id);
            }

            let in_progress = inner.in_progress_tasks.load(Ordering::Acquire);
            let io = inner.active_io_handles.load(Ordering::Acquire);

            if in_progress == 0 && io == 0 {
                let active = inner.active_tasks.load(Ordering::Acquire);
                let suspended = inner.suspended_tasks.load(Ordering::Acquire);
                let external = inner.external_waiters.load(Ordering::Acquire);
                guard.sleeping_workers -= 1;
                guard.pending_notifies = guard.sleeping_workers;
                inner.parker.sync_counts(&guard, Ordering::Release);
                if guard.sleeping_workers > 0 {
                    inner.parker.cv.notify_all();
                }
                if active == 0 || (suspended > 0 && suspended == external) {
                    return WorkerNext::AllFinished;
                }
                return WorkerNext::Deadlock;
            }

            if inner.ready_tasks.load(Ordering::Acquire) > 0 {
                guard.sleeping_workers -= 1;
                guard.pending_notifies = guard.pending_notifies.min(guard.sleeping_workers);
                inner.parker.sync_counts(&guard, Ordering::Release);
                drop(guard);
                std::thread::yield_now();
                continue;
            }

            inner.parker.cv.wait(&mut guard);
            guard.sleeping_workers -= 1;
            guard.pending_notifies = guard
                .pending_notifies
                .saturating_sub(1)
                .min(guard.sleeping_workers);
            inner.parker.sync_counts(&guard, Ordering::Release);
        }
    }

    /// Mark a task as running (used by single-threaded callers after `next_ready`).
    pub fn set_running(&self, id: TaskId) {
        let Some(slot) = self.inner.arena.slot(id.slot()) else {
            return;
        };
        let mut meta = slot.meta.lock();
        if slot.generation.load(Ordering::Acquire) != id.generation() || !meta.occupied {
            return;
        }
        meta.wake_token = false;
        let _ = self.apply_status_delta(&mut meta, TaskStatus::Running);
        drop(meta);
        // SAFETY: Caller transitioned `id` to `Running` and owns it exclusively.
        if let Some(task) = unsafe { self.inner.arena.task_unchecked_mut(id) } {
            task.status = TaskStatus::Running;
            task.cancelled = slot.cancelled.load(Ordering::Acquire);
        }
    }

    /// Suspend a currently running task, immediately re-queuing it as `Ready` if a
    /// concurrent `wake(id)` or fresh cancellation arrived (`wake_token == true`).
    pub fn suspend_running(&self, local_queue: Option<&Worker<TaskId>>, id: TaskId) {
        let inner = &self.inner;
        let Some(slot) = inner.arena.slot(id.slot()) else {
            return;
        };
        // Update `TaskState::status` BEFORE releasing `Running` ownership so no waker
        // can steal and run `id` concurrently with this write.
        // SAFETY: Caller is the worker that currently owns `id` in `Running` state.
        if let Some(task) = unsafe { inner.arena.task_unchecked_mut(id) } {
            task.status = TaskStatus::Suspended;
        }
        let mut meta = slot.meta.lock();
        if slot.generation.load(Ordering::Acquire) != id.generation()
            || !meta.occupied
            || meta.status == Some(TaskStatus::Finished)
        {
            return;
        }
        if meta.wake_token {
            meta.wake_token = false;
            let _ = self.apply_status_delta(&mut meta, TaskStatus::Ready);
            drop(meta);
            if let Some(local) = local_queue {
                local.push(id);
            } else {
                inner.injector.push(id);
            }
            inner.parker.notify_one_work();
        } else {
            let _ = self.apply_status_delta(&mut meta, TaskStatus::Suspended);
            let remaining_in_progress = inner.in_progress_tasks.load(Ordering::Acquire);
            drop(meta);
            if remaining_in_progress == 0 {
                inner.parker.notify_all_state_change();
            }
        }
    }

    /// Suspend a task (single-threaded compatibility wrapper).
    pub fn suspend(&self, id: TaskId) {
        self.suspend_running(None, id);
    }

    /// Mark `id` as waiting on an external host wakeup (`HostCallOutcome::SuspendRetry`)
    /// so global deadlock detection treats its suspension as intentional host pause.
    pub fn mark_external_waiter(&self, id: TaskId) {
        let Some(slot) = self.inner.arena.slot(id.slot()) else {
            return;
        };
        let mut meta = slot.meta.lock();
        if slot.generation.load(Ordering::Acquire) == id.generation()
            && meta.occupied
            && !meta.external_waiter
        {
            meta.external_waiter = true;
            self.inner.external_waiters.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Return a running task to the global FIFO injector after cooperative yielding.
    pub fn yield_task(&self, id: TaskId) {
        let inner = &self.inner;
        let Some(slot) = inner.arena.slot(id.slot()) else {
            return;
        };
        let mut meta = slot.meta.lock();
        if slot.generation.load(Ordering::Acquire) != id.generation()
            || !meta.occupied
            || meta.status != Some(TaskStatus::Running)
        {
            return;
        }
        // Update `task.status` while still holding `meta` BEFORE pushing to `injector`,
        // preventing any thief from observing `id` before this write completes.
        // SAFETY: Caller is the running owner of `id` and `meta` is still locked.
        if let Some(task) = unsafe { inner.arena.task_unchecked_mut(id) } {
            task.status = TaskStatus::Ready;
        }
        meta.wake_token = false;
        let _ = self.apply_status_delta(&mut meta, TaskStatus::Ready);
        drop(meta);
        inner.injector.push(id);
        inner.parker.notify_one_work();
    }

    /// Wake a task (`Suspended`/`Waiting` -> `Ready`, or set `wake_token = true` if still `Running`).
    pub fn wake_with_queue(&self, local_queue: Option<&Worker<TaskId>>, id: TaskId) -> bool {
        self.inner.wake_with_queue(local_queue, id)
    }

    /// Wake a task into the global injector queue.
    pub fn wake(&self, id: TaskId) -> bool {
        self.inner.wake_with_queue(None, id)
    }

    /// Resume a suspended/waiting task (compatibility alias for `wake`).
    pub fn resume(&self, id: TaskId) {
        let _ = self.wake(id);
    }

    /// Mark a single task as finished and release its segmented stacks without reaping its slot.
    ///
    /// Must only be called by the worker owning `id` (or when `id` is not running on another worker).
    pub fn finish_only(&self, id: TaskId) {
        self.inner.cancel_waiters(id);
        let inner = &self.inner;
        let Some(slot) = inner.arena.slot(id.slot()) else {
            return;
        };
        let mut meta = slot.meta.lock();
        if slot.generation.load(Ordering::Acquire) != id.generation() || !meta.occupied {
            return;
        }
        let was_active = meta.status != Some(TaskStatus::Finished);
        // SAFETY: Caller is either the owning worker finishing `id` or tearing down a
        // non-running task while holding `meta`.
        if let Some(task) = unsafe { inner.arena.task_unchecked_mut(id) } {
            task.status = TaskStatus::Finished;
            task.stacks.finish();
        }
        meta.wake_token = false;
        let _ = self.apply_status_delta(&mut meta, TaskStatus::Finished);
        let remaining_active = inner.active_tasks.load(Ordering::Acquire);
        let remaining_in_progress = inner.in_progress_tasks.load(Ordering::Acquire);
        drop(meta);
        if was_active && (remaining_active == 0 || remaining_in_progress == 0) {
            inner.parker.notify_all_state_change();
        }
    }

    /// Mark a task as finished and force-cancel any remaining children.
    pub fn finish(&self, id: TaskId) {
        let children = self.task_children(id);
        for child_id in children {
            self.cancel(child_id);
        }
        self.finish_only(id);
    }

    /// Reap a finished task from `TaskArena`, detaching it from its parent's `SlotMeta.children`
    /// without touching the parent's `TaskState`, and recycling its slot with `generation + 1`.
    pub fn reap_task(&self, id: TaskId) -> bool {
        self.inner.cancel_waiters(id);
        let inner = &self.inner;
        let Some(slot) = inner.arena.slot(id.slot()) else {
            return false;
        };
        let parent = {
            let mut meta = slot.meta.lock();
            if slot.generation.load(Ordering::Acquire) != id.generation() || !meta.occupied {
                return false;
            }
            let parent = meta.parent;
            let _ = self.apply_status_delta(&mut meta, TaskStatus::Finished);
            meta.occupied = false;
            meta.status = None;
            meta.wake_token = false;
            meta.children.clear();
            meta.promise_set.clear();
            // SAFETY: `id` is being reaped by its owning worker; `meta` is locked and
            // `generation` is incremented before releasing `meta`.
            let taken = unsafe { (*slot.task.get()).take() };
            if let Some(mut boxed_task) = taken {
                boxed_task.stacks.finish();
                boxed_task.children.clear();
                boxed_task.promise_set.clear();
                boxed_task.ancestor_handlers.clear();
                // SAFETY: `meta` is locked; store cleaned `TaskState` for next slot reuse
                // (all GC-managed fields were cleared above or in `execute_task`, and
                // `scan_vm_roots` only visits `slot.task`, never `slot.recycled_task`).
                unsafe {
                    *slot.recycled_task.get() = Some(boxed_task);
                }
            }
            let next_gen = id.generation().wrapping_add(1);
            slot.generation.store(next_gen, Ordering::Release);
            slot.cancelled.store(false, Ordering::Release);
            inner.arena.occupied.fetch_sub(1, Ordering::AcqRel);
            parent
        };

        if let Some(pid) = parent
            && let Some(pslot) = inner.arena.slot(pid.slot())
        {
            let mut pmeta = pslot.meta.lock();
            if pslot.generation.load(Ordering::Acquire) == pid.generation() && pmeta.occupied {
                pmeta.children.remove(&id);
            }
        }

        inner.arena.push_free_slot(id.slot());
        if inner.active_tasks.load(Ordering::Acquire) == 0
            || inner.in_progress_tasks.load(Ordering::Acquire) == 0
        {
            inner.parker.notify_all_state_change();
        }
        true
    }

    /// Request cooperative cancellation across `root_id` and all its descendants.
    ///
    /// Operates strictly on `SlotMeta` + `slot.cancelled` without touching foreign `TaskState`s,
    /// and immediately cancels any pending reactor or sync wait-queue registrations.
    pub fn request_cancel_subtree(&self, root_id: TaskId) -> Vec<TaskId> {
        let inner = &self.inner;
        let mut visited = Vec::new();
        let mut stack = vec![root_id];
        while let Some(id) = stack.pop() {
            let Some(slot) = inner.arena.slot(id.slot()) else {
                continue;
            };
            let (children, should_enqueue, cancel_waiters) = {
                let mut meta = slot.meta.lock();
                if slot.generation.load(Ordering::Acquire) != id.generation() || !meta.occupied {
                    continue;
                }
                visited.push(id);
                let children: Vec<TaskId> = meta.children.iter().copied().collect();
                let mut enqueue = false;
                let not_finished = meta.status != Some(TaskStatus::Finished);
                if not_finished {
                    let was_cancelled = slot.cancelled.swap(true, Ordering::AcqRel);
                    match meta.status {
                        Some(TaskStatus::Suspended | TaskStatus::Waiting) => {
                            meta.wake_token = false;
                            let _ = self.apply_status_delta(&mut meta, TaskStatus::Ready);
                            enqueue = true;
                        }
                        Some(TaskStatus::Running) if !was_cancelled => {
                            meta.wake_token = true;
                        }
                        _ => {}
                    }
                }
                (children, enqueue, not_finished)
            };
            for child_id in children.into_iter().rev() {
                stack.push(child_id);
            }
            if cancel_waiters {
                inner.cancel_waiters(id);
            }
            if should_enqueue {
                inner.injector.push(id);
                inner.parker.notify_one_work();
            }
        }
        visited
    }

    /// Immediately cancel a task and all its children (used for final teardown / unit tests).
    pub fn cancel(&self, id: TaskId) {
        let _ = self.cancel_subtree(id);
    }

    /// Immediately cascade-cancel `root_id` and all its descendants, releasing stacks
    /// for non-running tasks.
    pub fn cancel_subtree(&self, root_id: TaskId) -> Vec<TaskId> {
        let inner = &self.inner;
        let mut visited = Vec::new();
        let mut stack = vec![root_id];
        while let Some(id) = stack.pop() {
            let Some(slot) = inner.arena.slot(id.slot()) else {
                continue;
            };
            let (children, finish_now) = {
                let mut meta = slot.meta.lock();
                if slot.generation.load(Ordering::Acquire) != id.generation() || !meta.occupied {
                    continue;
                }
                visited.push(id);
                let children: Vec<TaskId> = meta.children.iter().copied().collect();
                slot.cancelled.store(true, Ordering::Release);
                let is_running = meta.status == Some(TaskStatus::Running);
                if is_running {
                    meta.wake_token = true;
                }
                (children, !is_running)
            };
            for child_id in children.into_iter().rev() {
                stack.push(child_id);
            }
            inner.cancel_waiters(id);
            if finish_now {
                self.finish_only(id);
            }
        }
        visited
    }

    // -- Fast Lock-Free & SlotMeta Queries -----------------------------------

    /// Single-instruction atomic cancellation check used at `Safepoint`s.
    #[inline]
    pub fn is_cancelled(&self, id: TaskId) -> bool {
        let Some(slot) = self.inner.arena.slot(id.slot()) else {
            return false;
        };
        slot.generation.load(Ordering::Acquire) == id.generation()
            && slot.cancelled.load(Ordering::Acquire)
    }

    pub fn task_status(&self, id: TaskId) -> Option<TaskStatus> {
        let slot = self.inner.arena.slot(id.slot())?;
        let meta = slot.meta.lock();
        if slot.generation.load(Ordering::Acquire) != id.generation() || !meta.occupied {
            return None;
        }
        meta.status
    }

    pub fn task_children(&self, id: TaskId) -> Vec<TaskId> {
        let Some(slot) = self.inner.arena.slot(id.slot()) else {
            return Vec::new();
        };
        let meta = slot.meta.lock();
        if slot.generation.load(Ordering::Acquire) != id.generation() || !meta.occupied {
            return Vec::new();
        }
        meta.children.iter().copied().collect()
    }

    pub fn task_spawned_promise(&self, id: TaskId) -> Option<u64> {
        let slot = self.inner.arena.slot(id.slot())?;
        let meta = slot.meta.lock();
        if slot.generation.load(Ordering::Acquire) != id.generation() || !meta.occupied {
            return None;
        }
        meta.spawned_promise
    }

    pub fn insert_task_promise(&self, id: TaskId, future_id: u64) {
        if let Some(slot) = self.inner.arena.slot(id.slot()) {
            let mut meta = slot.meta.lock();
            if slot.generation.load(Ordering::Acquire) == id.generation() && meta.occupied {
                meta.promise_set.insert(future_id);
            }
        }
    }

    pub fn remove_task_promise(&self, id: TaskId, future_id: u64) {
        if let Some(slot) = self.inner.arena.slot(id.slot()) {
            let mut meta = slot.meta.lock();
            if slot.generation.load(Ordering::Acquire) == id.generation() && meta.occupied {
                meta.promise_set.remove(&future_id);
            }
        }
    }

    pub fn first_task_promise(&self, id: TaskId) -> Option<u64> {
        let slot = self.inner.arena.slot(id.slot())?;
        let meta = slot.meta.lock();
        if slot.generation.load(Ordering::Acquire) != id.generation() || !meta.occupied {
            return None;
        }
        meta.promise_set.iter().next().copied()
    }

    pub fn task_promises_snapshot(&self, id: TaskId) -> Vec<u64> {
        let Some(slot) = self.inner.arena.slot(id.slot()) else {
            return Vec::new();
        };
        let meta = slot.meta.lock();
        if slot.generation.load(Ordering::Acquire) != id.generation() || !meta.occupied {
            return Vec::new();
        }
        meta.promise_set.iter().copied().collect()
    }

    /// Snapshot all non-finished `TaskId`s using `SlotMeta` locks without touching `TaskState`.
    pub fn active_task_ids(&self) -> Vec<TaskId> {
        let bound = self.inner.arena.high_water_slots.load(Ordering::Acquire);
        let mut out = Vec::new();
        for idx in 0..bound {
            if let Some(slot) = self.inner.arena.slot(idx) {
                let meta = slot.meta.lock();
                let generation = slot.generation.load(Ordering::Acquire);
                if meta.occupied && meta.status != Some(TaskStatus::Finished) {
                    out.push(TaskId::from_parts(idx, generation));
                }
            }
        }
        out
    }

    /// Structured snapshot `(id, parent, status, cancelled)` of all occupied tasks.
    pub fn tree_snapshot(&self) -> Vec<(TaskId, Option<TaskId>, TaskStatus, bool)> {
        let bound = self.inner.arena.high_water_slots.load(Ordering::Acquire);
        let mut out = Vec::new();
        for idx in 0..bound {
            if let Some(slot) = self.inner.arena.slot(idx) {
                let meta = slot.meta.lock();
                let generation = slot.generation.load(Ordering::Acquire);
                if meta.occupied
                    && let Some(status) = meta.status
                {
                    let cancelled = slot.cancelled.load(Ordering::Acquire);
                    out.push((
                        TaskId::from_parts(idx, generation),
                        meta.parent,
                        status,
                        cancelled,
                    ));
                }
            }
        }
        out
    }

    /// Render a deterministic human-readable tree snapshot of all active tasks
    /// (matching `docs/concurrency-and-async-algebraic-effect/task-tree.md`).
    pub fn format_tree_snapshot(&self) -> String {
        let mut nodes = self.tree_snapshot();
        nodes.sort_by_key(|(tid, _, _, _)| (tid.slot(), tid.generation()));

        let mut info = runtime::FastHashMap::default();
        for &(tid, parent, status, cancelled) in &nodes {
            info.insert(tid, (parent, status, cancelled));
        }
        let mut children_map: runtime::FastHashMap<TaskId, Vec<TaskId>> =
            runtime::FastHashMap::default();
        let mut roots = Vec::new();
        for &(tid, parent, _, _) in &nodes {
            if let Some(p) = parent
                && info.contains_key(&p)
            {
                children_map.entry(p).or_default().push(tid);
            } else {
                roots.push(tid);
            }
        }

        fn status_label(status: TaskStatus, cancelled: bool) -> &'static str {
            match (status, cancelled) {
                (TaskStatus::Ready, false) => "Ready",
                (TaskStatus::Ready, true) => "Ready (Cancelled)",
                (TaskStatus::Running, false) => "Running",
                (TaskStatus::Running, true) => "Running (Cancelled)",
                (TaskStatus::Suspended, false) => "Suspended",
                (TaskStatus::Suspended, true) => "Suspended (Cancelled)",
                (TaskStatus::Waiting, false) => "Waiting",
                (TaskStatus::Waiting, true) => "Waiting (Cancelled)",
                (TaskStatus::Finished, false) => "Finished",
                (TaskStatus::Finished, true) => "Finished (Cancelled)",
            }
        }

        fn render_subtree(
            tid: TaskId,
            prefix: &str,
            is_root: bool,
            is_last: bool,
            info: &runtime::FastHashMap<TaskId, (Option<TaskId>, TaskStatus, bool)>,
            children_map: &runtime::FastHashMap<TaskId, Vec<TaskId>>,
            lines: &mut Vec<String>,
        ) {
            let &(_, status, cancelled) = info.get(&tid).expect("snapshot node present");
            let label = status_label(status, cancelled);
            if is_root {
                lines.push(format!("[Root Task #{}] {}", tid.0, label));
            } else {
                let branch = if is_last { "└── " } else { "├── " };
                lines.push(format!("{prefix}{branch}[Task #{}] {label}", tid.0));
            }
            if let Some(kids) = children_map.get(&tid) {
                let child_prefix = if is_root {
                    String::new()
                } else if is_last {
                    format!("{prefix}    ")
                } else {
                    format!("{prefix}│   ")
                };
                let last_idx = kids.len().saturating_sub(1);
                for (i, &kid) in kids.iter().enumerate() {
                    render_subtree(
                        kid,
                        &child_prefix,
                        false,
                        i == last_idx,
                        info,
                        children_map,
                        lines,
                    );
                }
            }
        }

        let mut lines = Vec::with_capacity(nodes.len());
        let last_root = roots.len().saturating_sub(1);
        for (i, &root_tid) in roots.iter().enumerate() {
            render_subtree(
                root_tid,
                "",
                true,
                i == last_root,
                &info,
                &children_map,
                &mut lines,
            );
        }
        lines.join("\n")
    }

    /// Whether another ready task is waiting in any ready queue (`O(1)` atomic load).
    #[inline]
    pub fn has_other_ready_tasks(&self) -> bool {
        self.inner.ready_tasks.load(Ordering::Acquire) > 0
    }

    /// Whether any task is currently in `Suspended` or `Waiting` state (`O(1)` atomic load).
    #[inline]
    pub fn has_suspended_tasks(&self) -> bool {
        self.inner.suspended_tasks.load(Ordering::Acquire) > 0
    }

    #[inline]
    pub fn task_count(&self) -> usize {
        self.inner.arena.occupied_count()
    }

    #[inline]
    pub fn active_task_count(&self) -> usize {
        self.inner.active_tasks.load(Ordering::Acquire)
    }

    #[inline]
    pub fn ready_count(&self) -> usize {
        self.inner.ready_tasks.load(Ordering::Acquire)
    }

    // -- Exclusive Owner / Stopped-World Task Access -------------------------

    /// Borrow `id`'s `TaskState` on its owning worker thread (or outside multi-worker execution).
    ///
    /// # Safety
    /// See [`TaskArena::task_unchecked`].
    #[inline]
    pub unsafe fn owner_task(&self, id: TaskId) -> Option<&TaskState> {
        // SAFETY: Upheld by caller.
        unsafe { self.inner.arena.task_unchecked(id) }
    }

    /// Mutably borrow `id`'s `TaskState` on its owning worker thread (or under STW).
    ///
    /// # Safety
    /// See [`TaskArena::task_unchecked_mut`].
    #[inline]
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn owner_task_mut(&self, id: TaskId) -> Option<&mut TaskState> {
        // SAFETY: Upheld by caller.
        unsafe { self.inner.arena.task_unchecked_mut(id) }
    }

    /// Single-threaded / host-inspection immutable task accessor (valid when no other thread
    /// is mutating `id`).
    #[inline]
    pub fn get_task(&self, id: TaskId) -> Option<&TaskState> {
        // SAFETY: Used in single-threaded scheduler tests and post-run host queries.
        unsafe { self.inner.arena.task_unchecked(id) }
    }

    /// Exclusive `&mut Scheduler` mutable task accessor (used by STW GC and unit tests).
    #[inline]
    pub fn get_task_mut(&mut self, id: TaskId) -> Option<&mut TaskState> {
        // SAFETY: `&mut self` guarantees all mutator workers are stopped and `nessa-io-reactor`
        // never accesses `ArenaSlot::task`.
        unsafe { self.inner.arena.task_unchecked_mut(id) }
    }

    /// Iterate over all occupied tasks under stopped-world / single-threaded access.
    pub fn iter_tasks(&self) -> impl Iterator<Item = &TaskState> {
        // SAFETY: Called under STW root scanning or post-run inspection.
        unsafe { self.inner.arena.iter_stopped() }
    }

    /// Mutably iterate over all occupied tasks under exclusive `&mut Scheduler` access.
    pub fn iter_tasks_mut(&mut self) -> impl Iterator<Item = &mut TaskState> {
        // SAFETY: `&mut self` guarantees all mutator workers are stopped and `nessa-io-reactor`
        // never touches `ArenaSlot::task`.
        unsafe { self.inner.arena.iter_stopped_mut() }
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_and_schedule() {
        let sched = Scheduler::new();
        let t1 = sched.spawn(FuncId(0), None);
        let t2 = sched.spawn(FuncId(1), None);
        assert_eq!(sched.task_count(), 2);

        let next = sched.next_ready().unwrap();
        assert_eq!(next, t1);
        sched.set_running(next);

        let next2 = sched.next_ready().unwrap();
        assert_eq!(next2, t2);
    }

    #[test]
    fn structured_concurrency() {
        let sched = Scheduler::new();
        let parent = sched.spawn(FuncId(0), None);
        let child1 = sched.spawn(FuncId(1), Some(parent));
        let child2 = sched.spawn(FuncId(2), Some(parent));

        sched.finish(parent);
        assert_eq!(sched.task_status(child1), Some(TaskStatus::Finished));
        assert_eq!(sched.task_status(child2), Some(TaskStatus::Finished));
    }

    #[test]
    fn suspend_resume_and_wake_token_prevent_lost_wakeup() {
        let sched = Scheduler::new();
        let t = sched.spawn(FuncId(0), None);
        let _ = sched.next_ready();
        sched.set_running(t);

        // Simulate wakeup arriving BEFORE `suspend_running` executes!
        assert!(sched.wake(t));
        sched.suspend_running(None, t);
        assert_eq!(sched.task_status(t), Some(TaskStatus::Ready));
        assert_eq!(sched.next_ready(), Some(t));

        // Normal suspend -> wake cycle.
        sched.set_running(t);
        sched.suspend_running(None, t);
        assert_eq!(sched.task_status(t), Some(TaskStatus::Suspended));
        assert!(sched.wake(t));
        assert_eq!(sched.task_status(t), Some(TaskStatus::Ready));
        assert!(sched.next_ready().is_some());
    }

    #[test]
    fn yielding_requeues_running_tasks_once() {
        let scheduler = Scheduler::new();
        let task = scheduler.spawn(FuncId(0), None);
        assert_eq!(scheduler.next_ready(), Some(task));
        scheduler.set_running(task);
        scheduler.yield_task(task);
        scheduler.yield_task(task);
        assert_eq!(scheduler.ready_count(), 1);
        assert_eq!(scheduler.next_ready(), Some(task));
        scheduler.set_running(task);
        assert_eq!(scheduler.task_status(task), Some(TaskStatus::Running));
        scheduler.finish(task);
        scheduler.yield_task(task);
        assert_eq!(scheduler.next_ready(), None);
    }

    #[test]
    fn cancel_releases_all_delimited_and_captured_child_stacks() {
        let pool = Arc::new(StackPool::new());
        let mut sched = Scheduler::with_stack_pool(Arc::clone(&pool));
        let parent = sched.spawn(FuncId(0), None);
        let child = sched.spawn(FuncId(1), Some(parent));
        let task = sched.get_task_mut(child).unwrap();
        task.stacks
            .enter_delimiter(runtime::PromptId(1), FuncId(2), &[])
            .unwrap();
        task.stacks
            .capture(runtime::PromptId(1), nsbc::Reg(0))
            .unwrap();
        task.stacks
            .enter_delimiter(runtime::PromptId(2), FuncId(3), &[])
            .unwrap();
        assert_eq!(pool.active_count(), 4);
        sched.cancel(parent);
        assert_eq!(pool.active_count(), 0);
        assert_eq!(sched.task_status(child), Some(TaskStatus::Finished));
        sched.cancel(parent);
        assert_eq!(pool.active_count(), 0);
    }

    #[test]
    fn multi_worker_stealing_executes_and_reaps_ten_thousand_tasks_without_races() {
        let sched = Arc::new(Scheduler::with_workers(Arc::new(StackPool::new()), 4));
        let executed = Arc::new(AtomicUsize::new(0));
        // Seed 100 root tasks; each spawns 99 children when executed = 10,000 tasks total.
        for _ in 0..100 {
            sched.spawn(FuncId(1), None);
        }
        std::thread::scope(|scope| {
            for worker_id in 0..4 {
                let sched = Arc::clone(&sched);
                let executed = Arc::clone(&executed);
                scope.spawn(move || {
                    let lease = sched.lease_worker_queue(worker_id);
                    let mut tick = 0u64;
                    loop {
                        match sched.next_for_worker(worker_id, lease.queue(), &mut tick) {
                            WorkerNext::Task(tid) => {
                                let func = unsafe { sched.owner_task(tid).unwrap().current_func.0 };
                                executed.fetch_add(1, Ordering::Relaxed);
                                if func == 1 {
                                    for _ in 0..99 {
                                        sched.spawn_with_context(
                                            Some(lease.queue()),
                                            FuncId(2),
                                            None,
                                            None,
                                            Vec::new(),
                                            |_| {},
                                        );
                                    }
                                }
                                assert!(sched.reap_task(tid));
                            }
                            WorkerNext::AllFinished => break,
                            other => panic!("unexpected worker outcome: {other:?}"),
                        }
                    }
                });
            }
        });
        assert_eq!(executed.load(Ordering::Acquire), 10_000);
        assert_eq!(sched.task_count(), 0);
        assert_eq!(sched.active_task_count(), 0);
    }

    #[test]
    fn deadlock_detection_reports_deadlock_when_only_suspended_tasks_remain() {
        let sched = Arc::new(Scheduler::with_workers(Arc::new(StackPool::new()), 2));
        let t = sched.spawn(FuncId(0), None);
        std::thread::scope(|scope| {
            let mut outcomes = Vec::new();
            for worker_id in 0..2 {
                let sched = Arc::clone(&sched);
                outcomes.push(scope.spawn(move || {
                    let lease = sched.lease_worker_queue(worker_id);
                    let mut tick = 0u64;
                    loop {
                        match sched.next_for_worker(worker_id, lease.queue(), &mut tick) {
                            WorkerNext::Task(tid) => {
                                assert_eq!(tid, t);
                                sched.suspend_running(Some(lease.queue()), tid);
                            }
                            other => return other,
                        }
                    }
                }));
            }
            for h in outcomes {
                assert_eq!(h.join().unwrap(), WorkerNext::Deadlock);
            }
        });
    }
}
