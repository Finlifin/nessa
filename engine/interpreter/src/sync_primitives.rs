//! Task-aware synchronization primitives and lock-free atomics backing `std.sync`.
//!
//! Blocking operations (`Channel`, `Mutex`, `RwLock`, `Semaphore`, `Once`, `Barrier`) never
//! park OS worker threads: contended tasks register in FIFO wait queues and return
//! [`BuiltinOutcome::SuspendRetry`], allowing the `Scheduler` worker thread to steal and run
//! other ready tasks immediately. All `TaggedValue` payloads stored inside `Channel`, `Mutex`,
//! `RwLock`, and `Once` are traced and updated in place during MMTk stop-the-world GC via
//! [`SyncRegistry::visit_roots_stopped_mut`].

use std::collections::VecDeque;
use std::ptr;
use std::sync::atomic::{AtomicI64, AtomicPtr, AtomicUsize, Ordering};

use parking_lot::Mutex;
use runtime::{FastHashMap, FastHashSet, TaggedValue, TaskId};
use scheduler::Scheduler;

use crate::VmError;

const SYNC_SHARDS: usize = 64;
const ATOMIC_CHUNK_BITS: usize = 10;
const ATOMIC_CHUNK_SIZE: usize = 1 << ATOMIC_CHUNK_BITS;
const ATOMIC_CHUNK_MASK: usize = ATOMIC_CHUNK_SIZE - 1;
const ATOMIC_MAX_CHUNKS: usize = 1024;

struct AtomicChunk {
    slots: [AtomicI64; ATOMIC_CHUNK_SIZE],
}

impl AtomicChunk {
    fn new_boxed() -> Box<Self> {
        let mut vec = Vec::with_capacity(ATOMIC_CHUNK_SIZE);
        for _ in 0..ATOMIC_CHUNK_SIZE {
            vec.push(AtomicI64::new(0));
        }
        let boxed = vec.into_boxed_slice();
        // SAFETY: `AtomicChunk` is a single-field wrapper around `[AtomicI64; ATOMIC_CHUNK_SIZE]`
        // and `boxed` has exact length `ATOMIC_CHUNK_SIZE`.
        unsafe { Box::from_raw(Box::into_raw(boxed) as *mut AtomicChunk) }
    }
}

/// Append-only chunked arena of `AtomicI64` cells for `AtomicInt` and `AtomicBool`.
///
/// Cell loads, stores, swaps, fetch-adds, and CAS operations are lock-free `SeqCst` atomics
/// over published chunk pointers; allocating a new 1024-cell chunk serializes under `alloc_lock`.
struct AtomicArena {
    chunks: Box<[AtomicPtr<AtomicChunk>; ATOMIC_MAX_CHUNKS]>,
    alloc_lock: Mutex<()>,
    next_idx: AtomicUsize,
}

// SAFETY: Chunks are append-only until `Drop`, and individual cells are `AtomicI64`.
unsafe impl Send for AtomicArena {}
// SAFETY: Concurrent access uses `Acquire`/`Release` chunk publication and `SeqCst` cell ops.
unsafe impl Sync for AtomicArena {}

impl Drop for AtomicArena {
    fn drop(&mut self) {
        for ptr_slot in self.chunks.iter_mut() {
            let p = *ptr_slot.get_mut();
            if !p.is_null() {
                // SAFETY: Exclusive `&mut self` during `Drop` owns every allocated `AtomicChunk`.
                unsafe { drop(Box::from_raw(p)) };
            }
        }
    }
}

impl AtomicArena {
    fn new() -> Self {
        let chunks: Vec<AtomicPtr<AtomicChunk>> = (0..ATOMIC_MAX_CHUNKS)
            .map(|_| AtomicPtr::new(ptr::null_mut()))
            .collect();
        let chunks: Box<[AtomicPtr<AtomicChunk>; ATOMIC_MAX_CHUNKS]> = chunks
            .into_boxed_slice()
            .try_into()
            .expect("exact ATOMIC_MAX_CHUNKS length");
        Self {
            chunks,
            alloc_lock: Mutex::new(()),
            // Start at 1 so `0` is never a valid handle ID.
            next_idx: AtomicUsize::new(1),
        }
    }

    fn alloc(&self, initial: i64) -> Result<i64, VmError> {
        let idx = self.next_idx.fetch_add(1, Ordering::AcqRel);
        let chunk_idx = idx >> ATOMIC_CHUNK_BITS;
        if chunk_idx >= ATOMIC_MAX_CHUNKS {
            return Err(VmError::OutOfMemory);
        }
        let mut chunk_ptr = self.chunks[chunk_idx].load(Ordering::Acquire);
        if chunk_ptr.is_null() {
            let _guard = self.alloc_lock.lock();
            chunk_ptr = self.chunks[chunk_idx].load(Ordering::Acquire);
            if chunk_ptr.is_null() {
                let fresh = Box::into_raw(AtomicChunk::new_boxed());
                self.chunks[chunk_idx].store(fresh, Ordering::Release);
                chunk_ptr = fresh;
            }
        }
        // SAFETY: Published `AtomicChunk` pointers stay valid for the lifetime of `AtomicArena`.
        let chunk = unsafe { &*chunk_ptr };
        chunk.slots[idx & ATOMIC_CHUNK_MASK].store(initial, Ordering::SeqCst);
        Ok(idx as i64)
    }

    #[inline]
    fn get(&self, id: i64) -> Result<&AtomicI64, VmError> {
        if id <= 0 {
            return Err(VmError::InvalidArgument("invalid atomic handle"));
        }
        let idx = id as usize;
        if idx >= self.next_idx.load(Ordering::Acquire) {
            return Err(VmError::InvalidArgument("invalid atomic handle"));
        }
        let chunk_idx = idx >> ATOMIC_CHUNK_BITS;
        if chunk_idx >= ATOMIC_MAX_CHUNKS {
            return Err(VmError::InvalidArgument("invalid atomic handle"));
        }
        let chunk_ptr = self.chunks[chunk_idx].load(Ordering::Acquire);
        if chunk_ptr.is_null() {
            return Err(VmError::InvalidArgument("invalid atomic handle"));
        }
        // SAFETY: Published `AtomicChunk` pointers stay valid for the lifetime of `AtomicArena`.
        let chunk = unsafe { &*chunk_ptr };
        Ok(&chunk.slots[idx & ATOMIC_CHUNK_MASK])
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WaitTarget {
    Channel(i64),
    Mutex(i64),
    RwLock(i64),
    Semaphore(i64),
    Once(i64),
    Barrier(i64),
}

struct ChannelEntry {
    capacity: usize,
    closed: bool,
    buf: VecDeque<TaggedValue>,
    /// Unbuffered (`capacity == 0`) sender offers waiting for a receiver.
    offers: VecDeque<(TaskId, TaggedValue)>,
    /// Unbuffered senders whose offer was accepted by a receiver.
    accepted_senders: FastHashSet<TaskId>,
    /// Direct values handed to a waiting receiver by `try_send` on an unbuffered channel.
    accepted_receivers: FastHashMap<TaskId, TaggedValue>,
    /// Buffered (`capacity > 0`) senders waiting for buffer space.
    send_waiters: VecDeque<TaskId>,
    recv_waiters: VecDeque<TaskId>,
    woken_sender: Option<TaskId>,
    woken_receiver: Option<TaskId>,
}

struct MutexEntry {
    locked_by: Option<TaskId>,
    value: TaggedValue,
    waiters: VecDeque<TaskId>,
    woken: Option<TaskId>,
}

struct RwLockEntry {
    readers: FastHashMap<TaskId, usize>,
    writer: Option<TaskId>,
    value: TaggedValue,
    read_waiters: VecDeque<TaskId>,
    write_waiters: VecDeque<TaskId>,
    woken_writer: Option<TaskId>,
    woken_readers: FastHashSet<TaskId>,
}

struct SemaphoreEntry {
    permits: usize,
    waiters: VecDeque<TaskId>,
    woken: FastHashSet<TaskId>,
}

enum OnceState {
    Uninit,
    Running(TaskId),
    Complete(TaggedValue),
}

struct OnceEntry {
    state: OnceState,
    waiters: VecDeque<TaskId>,
    woken_runner: Option<TaskId>,
}

struct BarrierEntry {
    parties: usize,
    generation: u64,
    broken: bool,
    arrived: Vec<TaskId>,
    completed_waiters: FastHashMap<TaskId, (u64, bool)>,
}

#[derive(Default)]
struct SyncShard {
    channels: FastHashMap<i64, ChannelEntry>,
    mutexes: FastHashMap<i64, MutexEntry>,
    rwlocks: FastHashMap<i64, RwLockEntry>,
    semaphores: FastHashMap<i64, SemaphoreEntry>,
    onces: FastHashMap<i64, OnceEntry>,
    barriers: FastHashMap<i64, BarrierEntry>,
}

/// Action returned by task-aware sync operations to `BuiltinCtx`.
pub enum SyncPoll<T> {
    Ready(T),
    SuspendRetry,
    Cancelled,
}

type WaitIndexMap = FastHashMap<TaskId, Vec<WaitTarget>>;

/// Sharded registry of all live `std.sync` primitives in a VM instance.
pub(crate) struct SyncRegistry {
    next_id: AtomicI64,
    shards: Box<[Mutex<SyncShard>; SYNC_SHARDS]>,
    wait_index: Box<[Mutex<WaitIndexMap>; SYNC_SHARDS]>,
    active_waiters: AtomicUsize,
    owned_index: Box<[Mutex<WaitIndexMap>; SYNC_SHARDS]>,
    active_owners: AtomicUsize,
    committed_index: Box<[Mutex<WaitIndexMap>; SYNC_SHARDS]>,
    active_committed: AtomicUsize,
    atomics: AtomicArena,
}

impl SyncRegistry {
    pub(crate) fn new() -> Self {
        let shards: [Mutex<SyncShard>; SYNC_SHARDS] =
            std::array::from_fn(|_| Mutex::new(SyncShard::default()));
        let wait_index: [Mutex<WaitIndexMap>; SYNC_SHARDS] =
            std::array::from_fn(|_| Mutex::new(FastHashMap::default()));
        let owned_index: [Mutex<WaitIndexMap>; SYNC_SHARDS] =
            std::array::from_fn(|_| Mutex::new(FastHashMap::default()));
        let committed_index: [Mutex<WaitIndexMap>; SYNC_SHARDS] =
            std::array::from_fn(|_| Mutex::new(FastHashMap::default()));
        Self {
            next_id: AtomicI64::new(1),
            shards: Box::new(shards),
            wait_index: Box::new(wait_index),
            active_waiters: AtomicUsize::new(0),
            owned_index: Box::new(owned_index),
            active_owners: AtomicUsize::new(0),
            committed_index: Box::new(committed_index),
            active_committed: AtomicUsize::new(0),
            atomics: AtomicArena::new(),
        }
    }

    #[inline]
    fn alloc_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    #[inline]
    fn shard(&self, id: i64) -> &Mutex<SyncShard> {
        &self.shards[(id as usize) & (SYNC_SHARDS - 1)]
    }

    #[inline]
    fn wait_shard(&self, tid: TaskId) -> &Mutex<FastHashMap<TaskId, Vec<WaitTarget>>> {
        &self.wait_index[(tid.slot() as usize) & (SYNC_SHARDS - 1)]
    }

    #[inline]
    fn owned_shard(&self, tid: TaskId) -> &Mutex<FastHashMap<TaskId, Vec<WaitTarget>>> {
        &self.owned_index[(tid.slot() as usize) & (SYNC_SHARDS - 1)]
    }

    #[inline]
    fn committed_shard(&self, tid: TaskId) -> &Mutex<FastHashMap<TaskId, Vec<WaitTarget>>> {
        &self.committed_index[(tid.slot() as usize) & (SYNC_SHARDS - 1)]
    }

    fn record_wait(&self, tid: TaskId, target: WaitTarget) {
        let mut map = self.wait_shard(tid).lock();
        let list = map.entry(tid).or_default();
        if !list.contains(&target) {
            list.push(target);
            self.active_waiters.fetch_add(1, Ordering::Release);
        }
    }

    fn clear_wait(&self, tid: TaskId, target: WaitTarget) {
        if self.active_waiters.load(Ordering::Relaxed) == 0 {
            return;
        }
        let mut map = self.wait_shard(tid).lock();
        if let Some(list) = map.get_mut(&tid)
            && let Some(pos) = list.iter().position(|&x| x == target)
        {
            list.swap_remove(pos);
            self.active_waiters.fetch_sub(1, Ordering::Release);
            if list.is_empty() {
                map.remove(&tid);
            }
        }
    }

    fn record_owned(&self, tid: TaskId, target: WaitTarget) {
        let mut map = self.owned_shard(tid).lock();
        let list = map.entry(tid).or_default();
        if !list.contains(&target) {
            list.push(target);
            self.active_owners.fetch_add(1, Ordering::Release);
        }
    }

    fn clear_owned(&self, tid: TaskId, target: WaitTarget) {
        if self.active_owners.load(Ordering::Relaxed) == 0 {
            return;
        }
        let mut map = self.owned_shard(tid).lock();
        if let Some(list) = map.get_mut(&tid)
            && let Some(pos) = list.iter().position(|&x| x == target)
        {
            list.swap_remove(pos);
            self.active_owners.fetch_sub(1, Ordering::Release);
            if list.is_empty() {
                map.remove(&tid);
            }
        }
    }

    fn record_committed(&self, tid: TaskId, target: WaitTarget) {
        let mut map = self.committed_shard(tid).lock();
        let list = map.entry(tid).or_default();
        if !list.contains(&target) {
            list.push(target);
            self.active_committed.fetch_add(1, Ordering::Release);
        }
    }

    fn clear_committed(&self, tid: TaskId, target: WaitTarget) {
        if self.active_committed.load(Ordering::Relaxed) == 0 {
            return;
        }
        let mut map = self.committed_shard(tid).lock();
        if let Some(list) = map.get_mut(&tid)
            && let Some(pos) = list.iter().position(|&x| x == target)
        {
            list.swap_remove(pos);
            self.active_committed.fetch_sub(1, Ordering::Release);
            if list.is_empty() {
                map.remove(&tid);
            }
        }
    }

    /// Trace all GC-managed `TaggedValue`s stored inside `Channel`, `Mutex`, `RwLock`, and `Once`
    /// under MMTk stop-the-world when all mutator threads are parked.
    pub(crate) fn visit_roots_stopped(&self, mut visit: impl FnMut(&mut TaggedValue)) {
        for shard_mutex in self.shards.iter() {
            let mut shard = shard_mutex.lock();
            for ch in shard.channels.values_mut() {
                for slot in &mut ch.buf {
                    visit(slot);
                }
                for (_, slot) in &mut ch.offers {
                    visit(slot);
                }
                for slot in ch.accepted_receivers.values_mut() {
                    visit(slot);
                }
            }
            for mu in shard.mutexes.values_mut() {
                visit(&mut mu.value);
            }
            for rw in shard.rwlocks.values_mut() {
                visit(&mut rw.value);
            }
            for once in shard.onces.values_mut() {
                if let OnceState::Complete(ref mut val) = once.state {
                    visit(val);
                }
            }
        }
    }

    /// Remove `tid` from any wait queues on cancellation and cascade any unconsumed wakeups
    /// to the next live waiter so messages, permits, and lock handoffs are never lost.
    pub(crate) fn cancel_task(&self, sched: &Scheduler, tid: TaskId) {
        if self.active_waiters.load(Ordering::Acquire) == 0 {
            return;
        }
        let targets = {
            let mut map = self.wait_shard(tid).lock();
            map.remove(&tid).unwrap_or_default()
        };
        if targets.is_empty() {
            return;
        }
        self.active_waiters
            .fetch_sub(targets.len(), Ordering::Release);

        let mut to_wake: Vec<(TaskId, Option<WaitTarget>)> = Vec::new();
        for target in targets {
            match target {
                WaitTarget::Channel(id) => {
                    let mut shard = self.shard(id).lock();
                    if let Some(ch) = shard.channels.get_mut(&id) {
                        ch.send_waiters.retain(|&w| w != tid);
                        ch.recv_waiters.retain(|&w| w != tid);
                        ch.offers.retain(|&(s, _)| s != tid);
                        if ch.woken_sender == Some(tid) {
                            ch.woken_sender = None;
                            if !ch.closed
                                && ch.buf.len() < ch.capacity
                                && let Some(next_s) = pop_live_waiter(sched, &mut ch.send_waiters)
                            {
                                ch.woken_sender = Some(next_s);
                                to_wake.push((next_s, Some(target)));
                            }
                        }
                        if ch.woken_receiver == Some(tid) {
                            ch.woken_receiver = None;
                            if (!ch.buf.is_empty() || !ch.offers.is_empty() || ch.closed)
                                && let Some(next_r) = pop_live_waiter(sched, &mut ch.recv_waiters)
                            {
                                ch.woken_receiver = Some(next_r);
                                to_wake.push((next_r, Some(target)));
                            }
                        }
                    }
                }
                WaitTarget::Mutex(id) => {
                    let mut shard = self.shard(id).lock();
                    if let Some(mu) = shard.mutexes.get_mut(&id) {
                        mu.waiters.retain(|&w| w != tid);
                        if mu.woken == Some(tid) {
                            mu.woken = None;
                            if mu.locked_by.is_none()
                                && let Some(next_w) = pop_live_waiter(sched, &mut mu.waiters)
                            {
                                mu.woken = Some(next_w);
                                to_wake.push((next_w, Some(target)));
                            }
                        }
                    }
                }
                WaitTarget::RwLock(id) => {
                    let mut shard = self.shard(id).lock();
                    if let Some(rw) = shard.rwlocks.get_mut(&id) {
                        rw.read_waiters.retain(|&w| w != tid);
                        rw.write_waiters.retain(|&w| w != tid);
                        let removed_reader = rw.woken_readers.remove(&tid);
                        let removed_writer = if rw.woken_writer == Some(tid) {
                            rw.woken_writer = None;
                            true
                        } else {
                            false
                        };
                        if (removed_reader || removed_writer)
                            && rw.writer.is_none()
                            && rw.readers.is_empty()
                            && rw.woken_readers.is_empty()
                            && rw.woken_writer.is_none()
                        {
                            let mut rw_wakes = Vec::new();
                            schedule_next_rwlock_turn(sched, rw, &mut rw_wakes);
                            for w in rw_wakes {
                                to_wake.push((w, Some(target)));
                            }
                        }
                    }
                }
                WaitTarget::Semaphore(id) => {
                    let mut shard = self.shard(id).lock();
                    if let Some(sem) = shard.semaphores.get_mut(&id) {
                        sem.waiters.retain(|&w| w != tid);
                        if sem.woken.remove(&tid)
                            && sem.permits > 0
                            && let Some(next_w) = pop_live_waiter(sched, &mut sem.waiters)
                        {
                            sem.woken.insert(next_w);
                            to_wake.push((next_w, Some(target)));
                        }
                    }
                }
                WaitTarget::Once(id) => {
                    let mut shard = self.shard(id).lock();
                    if let Some(once) = shard.onces.get_mut(&id) {
                        once.waiters.retain(|&w| w != tid);
                        if once.woken_runner == Some(tid) {
                            once.woken_runner = None;
                            if matches!(once.state, OnceState::Uninit)
                                && let Some(next_w) = pop_live_waiter(sched, &mut once.waiters)
                            {
                                once.woken_runner = Some(next_w);
                                to_wake.push((next_w, Some(target)));
                            }
                        }
                    }
                }
                WaitTarget::Barrier(id) => {
                    let mut shard = self.shard(id).lock();
                    if let Some(bar) = shard.barriers.get_mut(&id) {
                        let prev_len = bar.arrived.len();
                        bar.arrived.retain(|&w| w != tid);
                        if bar.arrived.len() < prev_len {
                            bar.broken = true;
                            for w in bar.arrived.drain(..) {
                                to_wake.push((w, None));
                            }
                        }
                    }
                }
            }
        }

        for (w, maybe_target) in to_wake {
            if let Some(t) = maybe_target {
                self.record_wait(w, t);
            }
            let _ = sched.wake(w);
        }
    }

    /// Release any locks / active `Once` initialization still held by `tid` when `tid`
    /// terminates abnormally without unlocking, and purge any unread committed wakeups.
    pub(crate) fn on_task_terminated(&self, sched: &Scheduler, tid: TaskId) {
        if self.active_committed.load(Ordering::Acquire) > 0 {
            let committed = self
                .committed_shard(tid)
                .lock()
                .remove(&tid)
                .unwrap_or_default();
            if !committed.is_empty() {
                self.active_committed
                    .fetch_sub(committed.len(), Ordering::Release);
                for target in committed {
                    let mut shard = match target {
                        WaitTarget::Channel(id) | WaitTarget::Barrier(id) => self.shard(id).lock(),
                        _ => continue,
                    };
                    match target {
                        WaitTarget::Channel(id) => {
                            if let Some(ch) = shard.channels.get_mut(&id) {
                                ch.accepted_senders.remove(&tid);
                                ch.accepted_receivers.remove(&tid);
                            }
                        }
                        WaitTarget::Barrier(id) => {
                            if let Some(bar) = shard.barriers.get_mut(&id) {
                                bar.completed_waiters.remove(&tid);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }

        if self.active_owners.load(Ordering::Acquire) == 0 {
            return;
        }
        let owned = self
            .owned_shard(tid)
            .lock()
            .remove(&tid)
            .unwrap_or_default();
        if owned.is_empty() {
            return;
        }
        self.active_owners.fetch_sub(owned.len(), Ordering::Release);

        let mut to_wake: Vec<(TaskId, WaitTarget)> = Vec::new();
        for target in owned {
            match target {
                WaitTarget::Mutex(id) => {
                    let mut shard = self.shard(id).lock();
                    if let Some(mu) = shard.mutexes.get_mut(&id)
                        && mu.locked_by == Some(tid)
                    {
                        mu.locked_by = None;
                        if let Some(nw) = pop_live_waiter(sched, &mut mu.waiters) {
                            mu.woken = Some(nw);
                            to_wake.push((nw, target));
                        }
                    }
                }
                WaitTarget::RwLock(id) => {
                    let mut shard = self.shard(id).lock();
                    if let Some(rw) = shard.rwlocks.get_mut(&id) {
                        let mut released = false;
                        if rw.writer == Some(tid) {
                            rw.writer = None;
                            released = true;
                        }
                        if rw.readers.remove(&tid).is_some() {
                            released = true;
                        }
                        if released
                            && rw.writer.is_none()
                            && rw.readers.is_empty()
                            && rw.woken_readers.is_empty()
                            && rw.woken_writer.is_none()
                        {
                            let mut rw_wakes = Vec::new();
                            schedule_next_rwlock_turn(sched, rw, &mut rw_wakes);
                            for w in rw_wakes {
                                to_wake.push((w, target));
                            }
                        }
                    }
                }
                WaitTarget::Once(id) => {
                    let mut shard = self.shard(id).lock();
                    if let Some(once) = shard.onces.get_mut(&id)
                        && matches!(once.state, OnceState::Running(owner) if owner == tid)
                    {
                        once.state = OnceState::Uninit;
                        if let Some(nw) = pop_live_waiter(sched, &mut once.waiters) {
                            once.woken_runner = Some(nw);
                            to_wake.push((nw, target));
                        }
                    }
                }
                _ => {}
            }
        }

        for (w, target) in to_wake {
            self.record_wait(w, target);
            let _ = sched.wake(w);
        }
    }

    // =========================================================================
    // Channel
    // =========================================================================

    pub(crate) fn chan_new(&self, capacity: i64) -> Result<i64, VmError> {
        if capacity < 0 {
            return Err(VmError::InvalidArgument("Channel capacity must be >= 0"));
        }
        let cap = usize::try_from(capacity).map_err(|_| VmError::NumericOverflow)?;
        let id = self.alloc_id();
        let entry = ChannelEntry {
            capacity: cap,
            closed: false,
            buf: VecDeque::with_capacity(cap.min(64)),
            offers: VecDeque::new(),
            accepted_senders: FastHashSet::default(),
            accepted_receivers: FastHashMap::default(),
            send_waiters: VecDeque::new(),
            recv_waiters: VecDeque::new(),
            woken_sender: None,
            woken_receiver: None,
        };
        self.shard(id).lock().channels.insert(id, entry);
        Ok(id)
    }

    /// `0` = sent, `1` = closed.
    pub(crate) fn chan_send(
        &self,
        sched: &Scheduler,
        id: i64,
        task_id: TaskId,
        val: TaggedValue,
        can_abort_on_cancel: bool,
    ) -> Result<SyncPoll<i64>, VmError> {
        let target = WaitTarget::Channel(id);
        let mut to_wake: Vec<TaskId> = Vec::new();
        let mut clear_committed_self = false;
        let outcome = {
            let mut shard = self.shard(id).lock();
            let ch = shard
                .channels
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid Channel handle"))?;

            if ch.accepted_senders.remove(&task_id) {
                clear_committed_self = true;
                SyncPoll::Ready(0)
            } else if can_abort_on_cancel && sched.is_cancelled_not_unwinding(task_id) {
                ch.offers.retain(|&(s, _)| s != task_id);
                ch.send_waiters.retain(|&w| w != task_id);
                if ch.woken_sender == Some(task_id) {
                    ch.woken_sender = None;
                    if !ch.closed
                        && ch.buf.len() < ch.capacity
                        && let Some(next_s) = pop_live_waiter(sched, &mut ch.send_waiters)
                    {
                        ch.woken_sender = Some(next_s);
                        to_wake.push(next_s);
                    }
                }
                SyncPoll::Cancelled
            } else if ch.closed {
                ch.offers.retain(|&(s, _)| s != task_id);
                ch.send_waiters.retain(|&w| w != task_id);
                if ch.woken_sender == Some(task_id) {
                    ch.woken_sender = None;
                }
                SyncPoll::Ready(1)
            } else if ch.capacity > 0 {
                if ch.woken_sender == Some(task_id) {
                    ch.woken_sender = None;
                }
                if ch.buf.len() < ch.capacity {
                    ch.buf.push_back(val);
                    if ch.woken_receiver.is_none()
                        && let Some(rx) = pop_live_waiter(sched, &mut ch.recv_waiters)
                    {
                        ch.woken_receiver = Some(rx);
                        to_wake.push(rx);
                    }
                    SyncPoll::Ready(0)
                } else {
                    push_unique(&mut ch.send_waiters, task_id);
                    SyncPoll::SuspendRetry
                }
            } else {
                // Unbuffered rendezvous channel (`capacity == 0`): never buffer in `ch.buf`
                // so a cancelled receiver cannot strand or lose an unreceived message.
                if let Some(slot) = ch.offers.iter_mut().find(|(s, _)| *s == task_id) {
                    slot.1 = val;
                } else {
                    ch.offers.push_back((task_id, val));
                }
                if ch.woken_receiver.is_none()
                    && let Some(rx) = pop_live_waiter(sched, &mut ch.recv_waiters)
                {
                    ch.woken_receiver = Some(rx);
                    to_wake.push(rx);
                }
                SyncPoll::SuspendRetry
            }
        };

        if clear_committed_self {
            self.clear_committed(task_id, target);
        }
        match outcome {
            SyncPoll::Ready(_) | SyncPoll::Cancelled => self.clear_wait(task_id, target),
            SyncPoll::SuspendRetry => self.record_wait(task_id, target),
        }
        for w in to_wake {
            self.record_wait(w, target);
            let _ = sched.wake(w);
        }
        Ok(outcome)
    }

    /// Check if an unbuffered `send` offer was already accepted before aborting on cancel.
    pub(crate) fn chan_take_accepted_on_cancel(&self, id: i64, task_id: TaskId) -> bool {
        let target = WaitTarget::Channel(id);
        let accepted = {
            let mut shard = self.shard(id).lock();
            let Some(ch) = shard.channels.get_mut(&id) else {
                return false;
            };
            ch.accepted_senders.remove(&task_id)
        };
        if accepted {
            self.clear_committed(task_id, target);
            self.clear_wait(task_id, target);
        }
        accepted
    }

    /// `0` = sent, `1` = closed, `2` = full / would block.
    pub(crate) fn chan_try_send(
        &self,
        sched: &Scheduler,
        id: i64,
        val: TaggedValue,
    ) -> Result<i64, VmError> {
        let target = WaitTarget::Channel(id);
        let mut wake_rx = None;
        let mut direct_rx = false;
        let code = {
            let mut shard = self.shard(id).lock();
            let ch = shard
                .channels
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid Channel handle"))?;
            if ch.closed {
                1
            } else if ch.capacity > 0 {
                if ch.buf.len() < ch.capacity {
                    ch.buf.push_back(val);
                    if ch.woken_receiver.is_none()
                        && let Some(rx) = pop_live_waiter(sched, &mut ch.recv_waiters)
                    {
                        ch.woken_receiver = Some(rx);
                        wake_rx = Some(rx);
                    }
                    0
                } else {
                    2
                }
            } else if let Some(rx) = pop_live_waiter(sched, &mut ch.recv_waiters) {
                ch.accepted_receivers.insert(rx, val);
                ch.woken_receiver = Some(rx);
                wake_rx = Some(rx);
                direct_rx = true;
                0
            } else {
                2
            }
        };
        if let Some(rx) = wake_rx {
            if direct_rx {
                self.record_committed(rx, target);
            }
            self.record_wait(rx, target);
            let _ = sched.wake(rx);
        }
        Ok(code)
    }

    /// `(0, val)` = received, `(1, UNIT)` = closed & drained.
    pub(crate) fn chan_recv(
        &self,
        sched: &Scheduler,
        id: i64,
        task_id: TaskId,
        can_abort_on_cancel: bool,
    ) -> Result<SyncPoll<(i64, TaggedValue)>, VmError> {
        let target = WaitTarget::Channel(id);
        let mut to_wake: Vec<TaskId> = Vec::new();
        let mut accepted_tx = None;
        let mut clear_committed_self = false;
        let outcome = {
            let mut shard = self.shard(id).lock();
            let ch = shard
                .channels
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid Channel handle"))?;
            if ch.woken_receiver == Some(task_id) {
                ch.woken_receiver = None;
            }
            if let Some(val) = ch.accepted_receivers.remove(&task_id) {
                clear_committed_self = true;
                SyncPoll::Ready((0, val))
            } else if can_abort_on_cancel && sched.is_cancelled_not_unwinding(task_id) {
                ch.recv_waiters.retain(|&w| w != task_id);
                if (!ch.buf.is_empty() || !ch.offers.is_empty() || ch.closed)
                    && ch.woken_receiver.is_none()
                    && let Some(next_r) = pop_live_waiter(sched, &mut ch.recv_waiters)
                {
                    ch.woken_receiver = Some(next_r);
                    to_wake.push(next_r);
                }
                SyncPoll::Cancelled
            } else if let Some(val) = ch.buf.pop_front() {
                if ch.capacity > 0
                    && !ch.closed
                    && ch.woken_sender.is_none()
                    && let Some(tx) = pop_live_waiter(sched, &mut ch.send_waiters)
                {
                    ch.woken_sender = Some(tx);
                    to_wake.push(tx);
                }
                if (!ch.buf.is_empty() || !ch.offers.is_empty())
                    && ch.woken_receiver.is_none()
                    && let Some(rx) = pop_live_waiter(sched, &mut ch.recv_waiters)
                {
                    ch.woken_receiver = Some(rx);
                    to_wake.push(rx);
                }
                SyncPoll::Ready((0, val))
            } else if let Some((sender_tid, val)) = pop_live_offer(sched, &mut ch.offers) {
                ch.accepted_senders.insert(sender_tid);
                accepted_tx = Some(sender_tid);
                to_wake.push(sender_tid);
                if !ch.offers.is_empty()
                    && ch.woken_receiver.is_none()
                    && let Some(rx) = pop_live_waiter(sched, &mut ch.recv_waiters)
                {
                    ch.woken_receiver = Some(rx);
                    to_wake.push(rx);
                }
                SyncPoll::Ready((0, val))
            } else if ch.closed {
                SyncPoll::Ready((1, TaggedValue::UNIT))
            } else {
                push_unique(&mut ch.recv_waiters, task_id);
                SyncPoll::SuspendRetry
            }
        };

        if clear_committed_self {
            self.clear_committed(task_id, target);
        }
        if let Some(tx) = accepted_tx {
            self.record_committed(tx, target);
        }
        match outcome {
            SyncPoll::Ready(_) | SyncPoll::Cancelled => self.clear_wait(task_id, target),
            SyncPoll::SuspendRetry => self.record_wait(task_id, target),
        }
        for w in to_wake {
            self.record_wait(w, target);
            let _ = sched.wake(w);
        }
        Ok(outcome)
    }

    /// `(0, val)` = received, `(1, UNIT)` = closed & empty, `(2, UNIT)` = empty & open.
    pub(crate) fn chan_try_recv(
        &self,
        sched: &Scheduler,
        id: i64,
    ) -> Result<(i64, TaggedValue), VmError> {
        let target = WaitTarget::Channel(id);
        let mut to_wake: Vec<TaskId> = Vec::new();
        let mut accepted_tx = None;
        let res = {
            let mut shard = self.shard(id).lock();
            let ch = shard
                .channels
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid Channel handle"))?;
            if let Some(val) = ch.buf.pop_front() {
                if ch.capacity > 0
                    && !ch.closed
                    && ch.woken_sender.is_none()
                    && let Some(tx) = pop_live_waiter(sched, &mut ch.send_waiters)
                {
                    ch.woken_sender = Some(tx);
                    to_wake.push(tx);
                }
                (0, val)
            } else if let Some((sender_tid, val)) = pop_live_offer(sched, &mut ch.offers) {
                ch.accepted_senders.insert(sender_tid);
                accepted_tx = Some(sender_tid);
                to_wake.push(sender_tid);
                (0, val)
            } else if ch.closed {
                (1, TaggedValue::UNIT)
            } else {
                (2, TaggedValue::UNIT)
            }
        };
        if let Some(tx) = accepted_tx {
            self.record_committed(tx, target);
        }
        for w in to_wake {
            self.record_wait(w, target);
            let _ = sched.wake(w);
        }
        Ok(res)
    }

    pub(crate) fn chan_close(&self, sched: &Scheduler, id: i64) -> Result<(), VmError> {
        let mut to_wake: Vec<TaskId> = Vec::new();
        {
            let mut shard = self.shard(id).lock();
            let ch = shard
                .channels
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid Channel handle"))?;
            if !ch.closed {
                ch.closed = true;
                ch.woken_sender = None;
                ch.woken_receiver = None;
                to_wake.extend(ch.send_waiters.drain(..));
                to_wake.extend(ch.recv_waiters.drain(..));
                for (sender_tid, _) in ch.offers.drain(..) {
                    to_wake.push(sender_tid);
                }
            }
        }
        for w in to_wake {
            let _ = sched.wake(w);
        }
        Ok(())
    }

    pub(crate) fn chan_is_closed(&self, id: i64) -> Result<bool, VmError> {
        let shard = self.shard(id).lock();
        let ch = shard
            .channels
            .get(&id)
            .ok_or(VmError::InvalidArgument("invalid Channel handle"))?;
        Ok(ch.closed)
    }

    pub(crate) fn chan_len(&self, id: i64) -> Result<i64, VmError> {
        let shard = self.shard(id).lock();
        let ch = shard
            .channels
            .get(&id)
            .ok_or(VmError::InvalidArgument("invalid Channel handle"))?;
        Ok(ch.buf.len() as i64)
    }

    // =========================================================================
    // Mutex
    // =========================================================================

    pub(crate) fn mutex_new(&self, initial: TaggedValue) -> i64 {
        let id = self.alloc_id();
        let entry = MutexEntry {
            locked_by: None,
            value: initial,
            waiters: VecDeque::new(),
            woken: None,
        };
        self.shard(id).lock().mutexes.insert(id, entry);
        id
    }

    pub(crate) fn mutex_lock(
        &self,
        sched: &Scheduler,
        id: i64,
        task_id: TaskId,
        can_abort_on_cancel: bool,
    ) -> Result<SyncPoll<TaggedValue>, VmError> {
        let target = WaitTarget::Mutex(id);
        let mut next_w = None;
        let outcome = {
            let mut shard = self.shard(id).lock();
            let mu = shard
                .mutexes
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid Mutex handle"))?;
            if mu.locked_by == Some(task_id) {
                return Err(VmError::InvalidArgument(
                    "re-entrant Mutex lock on the same task would deadlock",
                ));
            }
            let was_woken = mu.woken == Some(task_id);
            if was_woken {
                mu.woken = None;
            }
            if can_abort_on_cancel && sched.is_cancelled_not_unwinding(task_id) {
                mu.waiters.retain(|&w| w != task_id);
                if was_woken && mu.locked_by.is_none() {
                    next_w = pop_live_waiter(sched, &mut mu.waiters);
                    mu.woken = next_w;
                }
                SyncPoll::Cancelled
            } else if mu.locked_by.is_none() {
                mu.locked_by = Some(task_id);
                SyncPoll::Ready(mu.value)
            } else {
                push_unique(&mut mu.waiters, task_id);
                SyncPoll::SuspendRetry
            }
        };
        match outcome {
            SyncPoll::Ready(_) => {
                self.clear_wait(task_id, target);
                self.record_owned(task_id, target);
            }
            SyncPoll::Cancelled => self.clear_wait(task_id, target),
            SyncPoll::SuspendRetry => self.record_wait(task_id, target),
        }
        if let Some(w) = next_w {
            self.record_wait(w, target);
            let _ = sched.wake(w);
        }
        Ok(outcome)
    }

    pub(crate) fn mutex_try_lock(&self, id: i64, task_id: TaskId) -> Result<bool, VmError> {
        let target = WaitTarget::Mutex(id);
        let acquired = {
            let mut shard = self.shard(id).lock();
            let mu = shard
                .mutexes
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid Mutex handle"))?;
            if mu.locked_by.is_none() {
                mu.locked_by = Some(task_id);
                if mu.woken == Some(task_id) {
                    mu.woken = None;
                }
                true
            } else {
                false
            }
        };
        if acquired {
            self.record_owned(task_id, target);
        }
        Ok(acquired)
    }

    pub(crate) fn mutex_unlock(
        &self,
        sched: &Scheduler,
        id: i64,
        task_id: TaskId,
    ) -> Result<(), VmError> {
        let target = WaitTarget::Mutex(id);
        let next_w = {
            let mut shard = self.shard(id).lock();
            let mu = shard
                .mutexes
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid Mutex handle"))?;
            if mu.locked_by != Some(task_id) {
                return Err(VmError::InvalidArgument(
                    "Mutex unlock called by non-owner task",
                ));
            }
            mu.locked_by = None;
            let nw = pop_live_waiter(sched, &mut mu.waiters);
            mu.woken = nw;
            nw
        };
        self.clear_owned(task_id, target);
        if let Some(w) = next_w {
            self.record_wait(w, target);
            let _ = sched.wake(w);
        }
        Ok(())
    }

    pub(crate) fn mutex_get(&self, id: i64, task_id: TaskId) -> Result<TaggedValue, VmError> {
        let shard = self.shard(id).lock();
        let mu = shard
            .mutexes
            .get(&id)
            .ok_or(VmError::InvalidArgument("invalid Mutex handle"))?;
        if mu.locked_by != Some(task_id) {
            return Err(VmError::InvalidArgument(
                "Mutex accessed without holding lock",
            ));
        }
        Ok(mu.value)
    }

    pub(crate) fn mutex_set(
        &self,
        id: i64,
        task_id: TaskId,
        val: TaggedValue,
    ) -> Result<(), VmError> {
        let mut shard = self.shard(id).lock();
        let mu = shard
            .mutexes
            .get_mut(&id)
            .ok_or(VmError::InvalidArgument("invalid Mutex handle"))?;
        if mu.locked_by != Some(task_id) {
            return Err(VmError::InvalidArgument(
                "Mutex mutated without holding lock",
            ));
        }
        mu.value = val;
        Ok(())
    }

    pub(crate) fn mutex_is_locked(&self, id: i64) -> Result<bool, VmError> {
        let shard = self.shard(id).lock();
        let mu = shard
            .mutexes
            .get(&id)
            .ok_or(VmError::InvalidArgument("invalid Mutex handle"))?;
        Ok(mu.locked_by.is_some())
    }

    // =========================================================================
    // RwLock
    // =========================================================================

    pub(crate) fn rwlock_new(&self, initial: TaggedValue) -> i64 {
        let id = self.alloc_id();
        let entry = RwLockEntry {
            readers: FastHashMap::default(),
            writer: None,
            value: initial,
            read_waiters: VecDeque::new(),
            write_waiters: VecDeque::new(),
            woken_writer: None,
            woken_readers: FastHashSet::default(),
        };
        self.shard(id).lock().rwlocks.insert(id, entry);
        id
    }

    pub(crate) fn rwlock_read_lock(
        &self,
        sched: &Scheduler,
        id: i64,
        task_id: TaskId,
        can_abort_on_cancel: bool,
    ) -> Result<SyncPoll<TaggedValue>, VmError> {
        let target = WaitTarget::RwLock(id);
        let mut to_wake = Vec::new();
        let outcome = {
            let mut shard = self.shard(id).lock();
            let rw = shard
                .rwlocks
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid RwLock handle"))?;
            if rw.writer == Some(task_id) {
                return Err(VmError::InvalidArgument(
                    "RwLock read_lock while holding write_lock on same task would deadlock",
                ));
            }
            let woken_for_batch = rw.woken_readers.remove(&task_id);
            if can_abort_on_cancel && sched.is_cancelled_not_unwinding(task_id) {
                rw.read_waiters.retain(|&w| w != task_id);
                if woken_for_batch
                    && rw.writer.is_none()
                    && rw.readers.is_empty()
                    && rw.woken_readers.is_empty()
                    && rw.woken_writer.is_none()
                {
                    schedule_next_rwlock_turn(sched, rw, &mut to_wake);
                }
                SyncPoll::Cancelled
            } else {
                let can_read = rw.writer.is_none()
                    && (woken_for_batch
                        || rw.readers.contains_key(&task_id)
                        || (rw.write_waiters.is_empty() && rw.woken_writer.is_none()));
                if can_read {
                    *rw.readers.entry(task_id).or_insert(0) += 1;
                    SyncPoll::Ready(rw.value)
                } else {
                    push_unique(&mut rw.read_waiters, task_id);
                    SyncPoll::SuspendRetry
                }
            }
        };
        match outcome {
            SyncPoll::Ready(_) => {
                self.clear_wait(task_id, target);
                self.record_owned(task_id, target);
            }
            SyncPoll::Cancelled => self.clear_wait(task_id, target),
            SyncPoll::SuspendRetry => self.record_wait(task_id, target),
        }
        for w in to_wake {
            self.record_wait(w, target);
            let _ = sched.wake(w);
        }
        Ok(outcome)
    }

    pub(crate) fn rwlock_try_read_lock(&self, id: i64, task_id: TaskId) -> Result<bool, VmError> {
        let target = WaitTarget::RwLock(id);
        let acquired = {
            let mut shard = self.shard(id).lock();
            let rw = shard
                .rwlocks
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid RwLock handle"))?;
            if rw.writer.is_none()
                && (rw.readers.contains_key(&task_id)
                    || (rw.write_waiters.is_empty() && rw.woken_writer.is_none()))
            {
                rw.woken_readers.remove(&task_id);
                *rw.readers.entry(task_id).or_insert(0) += 1;
                true
            } else {
                false
            }
        };
        if acquired {
            self.record_owned(task_id, target);
        }
        Ok(acquired)
    }

    pub(crate) fn rwlock_read_unlock(
        &self,
        sched: &Scheduler,
        id: i64,
        task_id: TaskId,
    ) -> Result<(), VmError> {
        let target = WaitTarget::RwLock(id);
        let mut to_wake = Vec::new();
        let mut fully_unlocked = false;
        {
            let mut shard = self.shard(id).lock();
            let rw = shard
                .rwlocks
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid RwLock handle"))?;
            let Some(cnt) = rw.readers.get_mut(&task_id) else {
                return Err(VmError::InvalidArgument(
                    "RwLock read_unlock called without read lock",
                ));
            };
            if *cnt > 1 {
                *cnt -= 1;
            } else {
                rw.readers.remove(&task_id);
                fully_unlocked = rw.writer != Some(task_id);
                rw.woken_readers.retain(|&r| is_live_un_cancelled(sched, r));
                if let Some(w) = rw.woken_writer
                    && !is_live_un_cancelled(sched, w)
                {
                    rw.woken_writer = None;
                }
                if rw.readers.is_empty()
                    && rw.woken_readers.is_empty()
                    && rw.writer.is_none()
                    && rw.woken_writer.is_none()
                {
                    if let Some(w) = pop_live_waiter(sched, &mut rw.write_waiters) {
                        rw.woken_writer = Some(w);
                        to_wake.push(w);
                    } else {
                        while let Some(r) = pop_live_waiter(sched, &mut rw.read_waiters) {
                            rw.woken_readers.insert(r);
                            to_wake.push(r);
                        }
                    }
                }
            }
        }
        if fully_unlocked {
            self.clear_owned(task_id, target);
        }
        for w in to_wake {
            self.record_wait(w, target);
            let _ = sched.wake(w);
        }
        Ok(())
    }

    pub(crate) fn rwlock_write_lock(
        &self,
        sched: &Scheduler,
        id: i64,
        task_id: TaskId,
        can_abort_on_cancel: bool,
    ) -> Result<SyncPoll<TaggedValue>, VmError> {
        let target = WaitTarget::RwLock(id);
        let mut to_wake = Vec::new();
        let outcome = {
            let mut shard = self.shard(id).lock();
            let rw = shard
                .rwlocks
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid RwLock handle"))?;
            if rw.writer == Some(task_id) || rw.readers.contains_key(&task_id) {
                return Err(VmError::InvalidArgument(
                    "re-entrant or upgraded RwLock write_lock on same task would deadlock",
                ));
            }
            let was_woken = rw.woken_writer == Some(task_id);
            if was_woken {
                rw.woken_writer = None;
            }
            if can_abort_on_cancel && sched.is_cancelled_not_unwinding(task_id) {
                rw.write_waiters.retain(|&w| w != task_id);
                if was_woken
                    && rw.writer.is_none()
                    && rw.readers.is_empty()
                    && rw.woken_readers.is_empty()
                {
                    schedule_next_rwlock_turn(sched, rw, &mut to_wake);
                }
                SyncPoll::Cancelled
            } else if rw.writer.is_none() && rw.readers.is_empty() && rw.woken_readers.is_empty() {
                rw.writer = Some(task_id);
                SyncPoll::Ready(rw.value)
            } else {
                push_unique(&mut rw.write_waiters, task_id);
                SyncPoll::SuspendRetry
            }
        };
        match outcome {
            SyncPoll::Ready(_) => {
                self.clear_wait(task_id, target);
                self.record_owned(task_id, target);
            }
            SyncPoll::Cancelled => self.clear_wait(task_id, target),
            SyncPoll::SuspendRetry => self.record_wait(task_id, target),
        }
        for w in to_wake {
            self.record_wait(w, target);
            let _ = sched.wake(w);
        }
        Ok(outcome)
    }

    pub(crate) fn rwlock_try_write_lock(&self, id: i64, task_id: TaskId) -> Result<bool, VmError> {
        let target = WaitTarget::RwLock(id);
        let acquired = {
            let mut shard = self.shard(id).lock();
            let rw = shard
                .rwlocks
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid RwLock handle"))?;
            if rw.writer.is_none() && rw.readers.is_empty() && rw.woken_readers.is_empty() {
                rw.writer = Some(task_id);
                if rw.woken_writer == Some(task_id) {
                    rw.woken_writer = None;
                }
                true
            } else {
                false
            }
        };
        if acquired {
            self.record_owned(task_id, target);
        }
        Ok(acquired)
    }

    pub(crate) fn rwlock_write_unlock(
        &self,
        sched: &Scheduler,
        id: i64,
        task_id: TaskId,
    ) -> Result<(), VmError> {
        let target = WaitTarget::RwLock(id);
        let mut to_wake = Vec::new();
        {
            let mut shard = self.shard(id).lock();
            let rw = shard
                .rwlocks
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid RwLock handle"))?;
            if rw.writer != Some(task_id) {
                return Err(VmError::InvalidArgument(
                    "RwLock write_unlock called by non-owner task",
                ));
            }
            rw.writer = None;
            schedule_next_rwlock_turn(sched, rw, &mut to_wake);
        }
        self.clear_owned(task_id, target);
        for w in to_wake {
            self.record_wait(w, target);
            let _ = sched.wake(w);
        }
        Ok(())
    }

    pub(crate) fn rwlock_get(&self, id: i64, task_id: TaskId) -> Result<TaggedValue, VmError> {
        let shard = self.shard(id).lock();
        let rw = shard
            .rwlocks
            .get(&id)
            .ok_or(VmError::InvalidArgument("invalid RwLock handle"))?;
        if rw.writer != Some(task_id) && !rw.readers.contains_key(&task_id) {
            return Err(VmError::InvalidArgument(
                "RwLock read without holding read or write lock",
            ));
        }
        Ok(rw.value)
    }

    pub(crate) fn rwlock_set(
        &self,
        id: i64,
        task_id: TaskId,
        val: TaggedValue,
    ) -> Result<(), VmError> {
        let mut shard = self.shard(id).lock();
        let rw = shard
            .rwlocks
            .get_mut(&id)
            .ok_or(VmError::InvalidArgument("invalid RwLock handle"))?;
        if rw.writer != Some(task_id) {
            return Err(VmError::InvalidArgument(
                "RwLock write without holding write lock",
            ));
        }
        rw.value = val;
        Ok(())
    }

    // =========================================================================
    // Semaphore
    // =========================================================================

    pub(crate) fn sem_new(&self, permits: i64) -> Result<i64, VmError> {
        if permits < 0 {
            return Err(VmError::InvalidArgument("Semaphore permits must be >= 0"));
        }
        let p = usize::try_from(permits).map_err(|_| VmError::NumericOverflow)?;
        let id = self.alloc_id();
        let entry = SemaphoreEntry {
            permits: p,
            waiters: VecDeque::new(),
            woken: FastHashSet::default(),
        };
        self.shard(id).lock().semaphores.insert(id, entry);
        Ok(id)
    }

    pub(crate) fn sem_acquire(
        &self,
        sched: &Scheduler,
        id: i64,
        task_id: TaskId,
        can_abort_on_cancel: bool,
    ) -> Result<SyncPoll<()>, VmError> {
        let target = WaitTarget::Semaphore(id);
        let mut next_w = None;
        let outcome = {
            let mut shard = self.shard(id).lock();
            let sem = shard
                .semaphores
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid Semaphore handle"))?;
            let was_woken = sem.woken.remove(&task_id);
            if can_abort_on_cancel && sched.is_cancelled_not_unwinding(task_id) {
                sem.waiters.retain(|&w| w != task_id);
                if was_woken
                    && sem.permits > 0
                    && let Some(nw) = pop_live_waiter(sched, &mut sem.waiters)
                {
                    sem.woken.insert(nw);
                    next_w = Some(nw);
                }
                SyncPoll::Cancelled
            } else if sem.permits > 0 {
                sem.permits -= 1;
                SyncPoll::Ready(())
            } else {
                push_unique(&mut sem.waiters, task_id);
                SyncPoll::SuspendRetry
            }
        };
        match outcome {
            SyncPoll::Ready(()) | SyncPoll::Cancelled => self.clear_wait(task_id, target),
            SyncPoll::SuspendRetry => self.record_wait(task_id, target),
        }
        if let Some(w) = next_w {
            self.record_wait(w, target);
            let _ = sched.wake(w);
        }
        Ok(outcome)
    }

    pub(crate) fn sem_try_acquire(&self, id: i64) -> Result<bool, VmError> {
        let mut shard = self.shard(id).lock();
        let sem = shard
            .semaphores
            .get_mut(&id)
            .ok_or(VmError::InvalidArgument("invalid Semaphore handle"))?;
        if sem.permits > 0 {
            sem.permits -= 1;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(crate) fn sem_release(
        &self,
        sched: &Scheduler,
        id: i64,
        count: i64,
    ) -> Result<(), VmError> {
        if count < 0 {
            return Err(VmError::InvalidArgument(
                "Semaphore release count must be >= 0",
            ));
        }
        if count == 0 {
            return Ok(());
        }
        let add = usize::try_from(count).map_err(|_| VmError::NumericOverflow)?;
        let target = WaitTarget::Semaphore(id);
        let mut to_wake = Vec::new();
        {
            let mut shard = self.shard(id).lock();
            let sem = shard
                .semaphores
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid Semaphore handle"))?;
            sem.permits = sem
                .permits
                .checked_add(add)
                .ok_or(VmError::NumericOverflow)?;
            for _ in 0..add {
                if let Some(w) = pop_live_waiter(sched, &mut sem.waiters) {
                    sem.woken.insert(w);
                    to_wake.push(w);
                } else {
                    break;
                }
            }
        }
        for w in to_wake {
            self.record_wait(w, target);
            let _ = sched.wake(w);
        }
        Ok(())
    }

    pub(crate) fn sem_available(&self, id: i64) -> Result<i64, VmError> {
        let shard = self.shard(id).lock();
        let sem = shard
            .semaphores
            .get(&id)
            .ok_or(VmError::InvalidArgument("invalid Semaphore handle"))?;
        Ok(sem.permits as i64)
    }

    // =========================================================================
    // Once / Lazy
    // =========================================================================

    pub(crate) fn once_new(&self) -> i64 {
        let id = self.alloc_id();
        let entry = OnceEntry {
            state: OnceState::Uninit,
            waiters: VecDeque::new(),
            woken_runner: None,
        };
        self.shard(id).lock().onces.insert(id, entry);
        id
    }

    /// `(0, val)` = already complete; `(1, UNIT)` = caller claimed initialization.
    pub(crate) fn once_begin(
        &self,
        sched: &Scheduler,
        id: i64,
        task_id: TaskId,
        can_abort_on_cancel: bool,
    ) -> Result<SyncPoll<(i64, TaggedValue)>, VmError> {
        let target = WaitTarget::Once(id);
        let mut next_w = None;
        let mut claimed_running = false;
        let outcome = {
            let mut shard = self.shard(id).lock();
            let once = shard
                .onces
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid Once handle"))?;
            let was_woken = once.woken_runner == Some(task_id);
            if was_woken {
                once.woken_runner = None;
            }
            if can_abort_on_cancel && sched.is_cancelled_not_unwinding(task_id) {
                once.waiters.retain(|&w| w != task_id);
                if was_woken && matches!(once.state, OnceState::Uninit) {
                    next_w = pop_live_waiter(sched, &mut once.waiters);
                    once.woken_runner = next_w;
                }
                SyncPoll::Cancelled
            } else {
                match once.state {
                    OnceState::Complete(val) => SyncPoll::Ready((0, val)),
                    OnceState::Uninit => {
                        once.state = OnceState::Running(task_id);
                        claimed_running = true;
                        SyncPoll::Ready((1, TaggedValue::UNIT))
                    }
                    OnceState::Running(owner) => {
                        if owner == task_id {
                            return Err(VmError::InvalidArgument(
                                "recursive Once/Lazy initialization on the same task would deadlock",
                            ));
                        }
                        push_unique(&mut once.waiters, task_id);
                        SyncPoll::SuspendRetry
                    }
                }
            }
        };
        if claimed_running {
            self.record_owned(task_id, target);
        }
        match outcome {
            SyncPoll::Ready(_) | SyncPoll::Cancelled => self.clear_wait(task_id, target),
            SyncPoll::SuspendRetry => self.record_wait(task_id, target),
        }
        if let Some(w) = next_w {
            self.record_wait(w, target);
            let _ = sched.wake(w);
        }
        Ok(outcome)
    }

    pub(crate) fn once_complete(
        &self,
        sched: &Scheduler,
        id: i64,
        task_id: TaskId,
        val: TaggedValue,
    ) -> Result<(), VmError> {
        let target = WaitTarget::Once(id);
        let mut to_wake = Vec::new();
        {
            let mut shard = self.shard(id).lock();
            let once = shard
                .onces
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid Once handle"))?;
            match once.state {
                OnceState::Running(owner) if owner == task_id => {
                    once.state = OnceState::Complete(val);
                    once.woken_runner = None;
                    to_wake.extend(once.waiters.drain(..));
                }
                _ => {
                    return Err(VmError::InvalidArgument(
                        "Once complete called without active initialization ownership",
                    ));
                }
            }
        }
        self.clear_owned(task_id, target);
        for w in to_wake {
            let _ = sched.wake(w);
        }
        Ok(())
    }

    /// If `task_id` failed or was cancelled during initialization before `once_complete`,
    /// reset `OnceState` back to `Uninit` and wake the next waiter. No-op if already completed.
    pub(crate) fn once_abort(
        &self,
        sched: &Scheduler,
        id: i64,
        task_id: TaskId,
    ) -> Result<(), VmError> {
        let target = WaitTarget::Once(id);
        let mut next_w = None;
        let mut was_owner = false;
        {
            let mut shard = self.shard(id).lock();
            let once = shard
                .onces
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid Once handle"))?;
            if matches!(once.state, OnceState::Running(owner) if owner == task_id) {
                was_owner = true;
                once.state = OnceState::Uninit;
                next_w = pop_live_waiter(sched, &mut once.waiters);
                once.woken_runner = next_w;
            }
        }
        if was_owner {
            self.clear_owned(task_id, target);
        }
        if let Some(w) = next_w {
            self.record_wait(w, target);
            let _ = sched.wake(w);
        }
        Ok(())
    }

    pub(crate) fn once_get(&self, id: i64) -> Result<Option<TaggedValue>, VmError> {
        let shard = self.shard(id).lock();
        let once = shard
            .onces
            .get(&id)
            .ok_or(VmError::InvalidArgument("invalid Once handle"))?;
        match once.state {
            OnceState::Complete(val) => Ok(Some(val)),
            OnceState::Uninit | OnceState::Running(_) => Ok(None),
        }
    }

    // =========================================================================
    // Barrier
    // =========================================================================

    pub(crate) fn barrier_new(&self, parties: i64) -> Result<i64, VmError> {
        if parties <= 0 {
            return Err(VmError::InvalidArgument("Barrier parties must be >= 1"));
        }
        let p = usize::try_from(parties).map_err(|_| VmError::NumericOverflow)?;
        let id = self.alloc_id();
        let entry = BarrierEntry {
            parties: p,
            generation: 0,
            broken: false,
            arrived: Vec::with_capacity(p.min(64)),
            completed_waiters: FastHashMap::default(),
        };
        self.shard(id).lock().barriers.insert(id, entry);
        Ok(id)
    }

    /// Returns `true` for the single leader task of each barrier cycle, `false` for others.
    pub(crate) fn barrier_wait(
        &self,
        sched: &Scheduler,
        id: i64,
        task_id: TaskId,
        can_abort_on_cancel: bool,
    ) -> Result<SyncPoll<bool>, VmError> {
        let target = WaitTarget::Barrier(id);
        let mut to_wake = Vec::new();
        let mut clear_committed_self = false;
        let mut mark_committed = Vec::new();
        let outcome = {
            let mut shard = self.shard(id).lock();
            let bar = shard
                .barriers
                .get_mut(&id)
                .ok_or(VmError::InvalidArgument("invalid Barrier handle"))?;
            if let Some((_, is_leader)) = bar.completed_waiters.remove(&task_id) {
                clear_committed_self = true;
                Ok(SyncPoll::Ready(is_leader))
            } else if can_abort_on_cancel && sched.is_cancelled_not_unwinding(task_id) {
                let prev_len = bar.arrived.len();
                bar.arrived.retain(|&w| w != task_id);
                if bar.arrived.len() < prev_len {
                    bar.broken = true;
                    to_wake.append(&mut bar.arrived);
                }
                Ok(SyncPoll::Cancelled)
            } else if bar.broken {
                Err(VmError::InvalidArgument(
                    "Barrier broken by cancelled participant",
                ))
            } else if bar.arrived.contains(&task_id) {
                Ok(SyncPoll::SuspendRetry)
            } else if bar.parties == 1 {
                bar.generation = bar.generation.wrapping_add(1);
                Ok(SyncPoll::Ready(true))
            } else if bar.arrived.len() + 1 == bar.parties {
                let cur_gen = bar.generation;
                bar.generation = bar.generation.wrapping_add(1);
                for w in bar.arrived.drain(..) {
                    bar.completed_waiters.insert(w, (cur_gen, false));
                    mark_committed.push(w);
                    to_wake.push(w);
                }
                Ok(SyncPoll::Ready(true))
            } else {
                bar.arrived.push(task_id);
                Ok(SyncPoll::SuspendRetry)
            }
        };
        if clear_committed_self {
            self.clear_committed(task_id, target);
        }
        for w in mark_committed {
            self.record_committed(w, target);
        }
        match &outcome {
            Ok(SyncPoll::Ready(_)) | Ok(SyncPoll::Cancelled) | Err(_) => {
                self.clear_wait(task_id, target);
            }
            Ok(SyncPoll::SuspendRetry) => self.record_wait(task_id, target),
        }
        for w in to_wake {
            let _ = sched.wake(w);
        }
        outcome
    }

    /// Check if `task_id` already completed its barrier cycle before aborting on cancellation.
    pub(crate) fn barrier_take_completed_on_cancel(
        &self,
        id: i64,
        task_id: TaskId,
    ) -> Option<bool> {
        let target = WaitTarget::Barrier(id);
        let is_leader = {
            let mut shard = self.shard(id).lock();
            let bar = shard.barriers.get_mut(&id)?;
            let (_, is_leader) = bar.completed_waiters.remove(&task_id)?;
            is_leader
        };
        self.clear_committed(task_id, target);
        self.clear_wait(task_id, target);
        Some(is_leader)
    }

    // =========================================================================
    // Lock-free Atomics (`AtomicInt` / `AtomicBool`)
    // =========================================================================

    pub(crate) fn atomic_new(&self, initial: i64) -> Result<i64, VmError> {
        self.atomics.alloc(initial)
    }

    #[inline]
    pub(crate) fn atomic_load(&self, id: i64) -> Result<i64, VmError> {
        Ok(self.atomics.get(id)?.load(Ordering::SeqCst))
    }

    #[inline]
    pub(crate) fn atomic_store(&self, id: i64, val: i64) -> Result<(), VmError> {
        self.atomics.get(id)?.store(val, Ordering::SeqCst);
        Ok(())
    }

    #[inline]
    pub(crate) fn atomic_swap(&self, id: i64, val: i64) -> Result<i64, VmError> {
        Ok(self.atomics.get(id)?.swap(val, Ordering::SeqCst))
    }

    #[inline]
    pub(crate) fn atomic_fetch_add(&self, id: i64, delta: i64) -> Result<i64, VmError> {
        Ok(self.atomics.get(id)?.fetch_add(delta, Ordering::SeqCst))
    }

    #[inline]
    pub(crate) fn atomic_cas(
        &self,
        id: i64,
        expected: i64,
        desired: i64,
    ) -> Result<(bool, i64), VmError> {
        match self.atomics.get(id)?.compare_exchange(
            expected,
            desired,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(prev) => Ok((true, prev)),
            Err(actual) => Ok((false, actual)),
        }
    }
}

#[inline]
fn push_unique(queue: &mut VecDeque<TaskId>, tid: TaskId) {
    if !queue.contains(&tid) {
        queue.push_back(tid);
    }
}

fn is_live_un_cancelled(sched: &Scheduler, tid: TaskId) -> bool {
    !sched.is_cancelled_not_unwinding(tid)
        && matches!(
            sched.task_status(tid),
            Some(
                runtime::TaskStatus::Ready
                    | runtime::TaskStatus::Running
                    | runtime::TaskStatus::Suspended
                    | runtime::TaskStatus::Waiting
            )
        )
}

fn pop_live_waiter(sched: &Scheduler, queue: &mut VecDeque<TaskId>) -> Option<TaskId> {
    while let Some(tid) = queue.pop_front() {
        if is_live_un_cancelled(sched, tid) {
            return Some(tid);
        }
    }
    None
}

fn pop_live_offer(
    sched: &Scheduler,
    offers: &mut VecDeque<(TaskId, TaggedValue)>,
) -> Option<(TaskId, TaggedValue)> {
    while let Some((tid, val)) = offers.pop_front() {
        if is_live_un_cancelled(sched, tid) {
            return Some((tid, val));
        }
    }
    None
}

fn schedule_next_rwlock_turn(sched: &Scheduler, rw: &mut RwLockEntry, to_wake: &mut Vec<TaskId>) {
    // Phase-fair handoff: after a write unlock (or cancelled woken writer), release any
    // accumulated batch of waiting readers first so readers cannot starve; otherwise wake
    // the next waiting writer.
    let mut woke_any_reader = false;
    while let Some(r) = pop_live_waiter(sched, &mut rw.read_waiters) {
        rw.woken_readers.insert(r);
        to_wake.push(r);
        woke_any_reader = true;
    }
    if !woke_any_reader && let Some(w) = pop_live_waiter(sched, &mut rw.write_waiters) {
        rw.woken_writer = Some(w);
        to_wake.push(w);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use nsbc::FuncId;
    use runtime::{TaggedValue, TaskStatus};
    use scheduler::Scheduler;

    use super::{SyncPoll, SyncRegistry};

    #[test]
    fn rwlock_reader_batch_early_unlock_preserves_writer_handoff() {
        let sched = Scheduler::with_workers(Arc::new(stack_pool::StackPool::new()), 2);
        let sync = SyncRegistry::new();
        let rw = sync.rwlock_new(TaggedValue::try_from_i64(10).unwrap());

        let w1 = sched.spawn(FuncId(0), None);
        let r0 = sched.spawn(FuncId(0), None);
        let r1 = sched.spawn(FuncId(0), None);
        let w0 = sched.spawn(FuncId(0), None);

        // `w1` acquires write lock; `r0`, `r1`, and `w0` suspend behind it.
        assert!(matches!(
            sync.rwlock_write_lock(&sched, rw, w1, true).unwrap(),
            SyncPoll::Ready(_)
        ));
        assert!(matches!(
            sync.rwlock_read_lock(&sched, rw, r0, true).unwrap(),
            SyncPoll::SuspendRetry
        ));
        sched.set_running(r0);
        sched.suspend(r0);
        assert!(matches!(
            sync.rwlock_read_lock(&sched, rw, r1, true).unwrap(),
            SyncPoll::SuspendRetry
        ));
        sched.set_running(r1);
        sched.suspend(r1);
        assert!(matches!(
            sync.rwlock_write_lock(&sched, rw, w0, true).unwrap(),
            SyncPoll::SuspendRetry
        ));
        sched.set_running(w0);
        sched.suspend(w0);

        // `w1` unlocks -> wakes reader batch `{r0, r1}`; `w0` must remain suspended.
        sync.rwlock_write_unlock(&sched, rw, w1).unwrap();
        assert_eq!(sched.task_status(r0), Some(TaskStatus::Ready));
        assert_eq!(sched.task_status(r1), Some(TaskStatus::Ready));
        assert_eq!(sched.task_status(w0), Some(TaskStatus::Suspended));

        // `r0` runs and unlocks BEFORE `r1` has even polled `rwlock_read_lock`!
        assert!(matches!(
            sync.rwlock_read_lock(&sched, rw, r0, true).unwrap(),
            SyncPoll::Ready(_)
        ));
        sync.rwlock_read_unlock(&sched, rw, r0).unwrap();
        // `r1` is still in `woken_readers`, so `w0` must NOT be woken prematurely.
        assert_eq!(sched.task_status(w0), Some(TaskStatus::Suspended));

        // Now `r1` acquires and unlocks -> must hand off to `w0` cleanly!
        assert!(matches!(
            sync.rwlock_read_lock(&sched, rw, r1, true).unwrap(),
            SyncPoll::Ready(_)
        ));
        sync.rwlock_read_unlock(&sched, rw, r1).unwrap();
        assert_eq!(sched.task_status(w0), Some(TaskStatus::Ready));

        assert!(matches!(
            sync.rwlock_write_lock(&sched, rw, w0, true).unwrap(),
            SyncPoll::Ready(_)
        ));
        sync.rwlock_write_unlock(&sched, rw, w0).unwrap();
    }
}
