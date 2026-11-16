//! Async effect promises, `Future` heap handles, task-tree cancellation, and `await` dispatch.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gc::{ObjectHeader, WeakObject};
use nsbc::{Instruction, Reg};
use parking_lot::{Mutex, MutexGuard};
use runtime::{
    ClosureEnv, EffectHandler, FastHashMap, FastHashSet, TaggedValue, TaskId, UnwindEntry,
};
use type_pool::{TypeId, TypeIndex, TypeKind};

use crate::{DispatchResult, Vm, VmError, e_payload};

pub(crate) const PROMISE_SHARD_COUNT: usize = 64;

#[repr(align(64))]
struct PromiseShard(Mutex<FastHashMap<u64, PromiseEntry>>);

impl Default for PromiseShard {
    fn default() -> Self {
        Self(Mutex::new(FastHashMap::default()))
    }
}

pub(crate) struct PromiseTable {
    shards: Box<[PromiseShard; PROMISE_SHARD_COUNT]>,
}

pub(crate) struct LockedPromiseShards<'a> {
    guards: [Option<MutexGuard<'a, FastHashMap<u64, PromiseEntry>>>; PROMISE_SHARD_COUNT],
}

impl LockedPromiseShards<'_> {
    #[inline]
    pub(crate) fn get(&self, fid: u64) -> Option<&PromiseEntry> {
        let idx = (fid as usize) & (PROMISE_SHARD_COUNT - 1);
        self.guards[idx].as_ref()?.get(&fid)
    }

    #[inline]
    pub(crate) fn get_mut(&mut self, fid: u64) -> Option<&mut PromiseEntry> {
        let idx = (fid as usize) & (PROMISE_SHARD_COUNT - 1);
        self.guards[idx].as_mut()?.get_mut(&fid)
    }
}

impl PromiseTable {
    pub(crate) fn new() -> Self {
        Self {
            shards: Box::new(std::array::from_fn(|_| PromiseShard::default())),
        }
    }

    #[inline]
    pub(crate) fn shard(&self, fid: u64) -> &Mutex<FastHashMap<u64, PromiseEntry>> {
        &self.shards[(fid as usize) & (PROMISE_SHARD_COUNT - 1)].0
    }

    /// Lock all shards touched by `fids` in strict ascending shard order (`0..64`),
    /// providing deadlock-free multi-promise atomic inspection and waiter registration.
    pub(crate) fn lock_fids(&self, fids: &[u64]) -> LockedPromiseShards<'_> {
        let mut mask = 0u64;
        for &fid in fids {
            mask |= 1u64 << ((fid as usize) & (PROMISE_SHARD_COUNT - 1));
        }
        let mut guards: [Option<MutexGuard<'_, FastHashMap<u64, PromiseEntry>>>;
            PROMISE_SHARD_COUNT] = std::array::from_fn(|_| None);
        let mut rem = mask;
        while rem != 0 {
            let idx = rem.trailing_zeros() as usize;
            guards[idx] = Some(self.shards[idx].0.lock());
            rem &= rem - 1;
        }
        LockedPromiseShards { guards }
    }

    pub(crate) fn len(&self) -> usize {
        self.shards.iter().map(|s| s.0.lock().len()).sum()
    }

    /// Visit every promise shard mutably during MMTk stop-the-world collection.
    ///
    /// # Safety
    /// Must only be called during STW GC when all mutator threads are parked and no
    /// mutator holds any shard lock.
    pub(crate) unsafe fn for_each_shard_stopped_mut(
        &self,
        mut f: impl FnMut(&mut FastHashMap<u64, PromiseEntry>),
    ) {
        for shard in self.shards.iter() {
            // SAFETY: Caller guarantees exclusive STW collector access with all mutators stopped.
            let map = unsafe { &mut *shard.0.data_ptr() };
            f(map);
        }
    }
}

#[repr(align(64))]
struct HandlerScopeShard(Mutex<FastHashMap<u64, FastHashSet<u64>>>);

impl Default for HandlerScopeShard {
    fn default() -> Self {
        Self(Mutex::new(FastHashMap::default()))
    }
}

pub(crate) struct HandlerScopeTable {
    shards: Box<[HandlerScopeShard; PROMISE_SHARD_COUNT]>,
}

impl HandlerScopeTable {
    pub(crate) fn new() -> Self {
        Self {
            shards: Box::new(std::array::from_fn(|_| HandlerScopeShard::default())),
        }
    }

    #[inline]
    pub(crate) fn shard(&self, scope_id: u64) -> &Mutex<FastHashMap<u64, FastHashSet<u64>>> {
        &self.shards[(scope_id as usize) & (PROMISE_SHARD_COUNT - 1)].0
    }
}

#[derive(Debug, Clone)]
pub(crate) enum PromiseState {
    Pending,
    Completed(TaggedValue),
    Failed(VmError),
    Cancelled,
}

pub(crate) struct AwaitAllState {
    pub task_id: TaskId,
    pub remaining: AtomicUsize,
    pub early_exit: AtomicBool,
}

#[repr(align(64))]
struct AwaitAllShard(Mutex<FastHashMap<TaskId, Arc<AwaitAllState>>>);

impl Default for AwaitAllShard {
    fn default() -> Self {
        Self(Mutex::new(FastHashMap::default()))
    }
}

pub(crate) struct AwaitAllTable {
    shards: Box<[AwaitAllShard; PROMISE_SHARD_COUNT]>,
    active_count: AtomicUsize,
}

impl AwaitAllTable {
    pub(crate) fn new() -> Self {
        Self {
            shards: Box::new(std::array::from_fn(|_| AwaitAllShard::default())),
            active_count: AtomicUsize::new(0),
        }
    }

    #[inline]
    fn shard(&self, tid: TaskId) -> &Mutex<FastHashMap<TaskId, Arc<AwaitAllState>>> {
        &self.shards[(tid.slot() as usize) & (PROMISE_SHARD_COUNT - 1)].0
    }

    #[inline]
    pub(crate) fn get(&self, tid: TaskId) -> Option<Arc<AwaitAllState>> {
        if self.active_count.load(Ordering::Acquire) == 0 {
            return None;
        }
        self.shard(tid).lock().get(&tid).cloned()
    }

    #[inline]
    pub(crate) fn insert(&self, tid: TaskId, state: Arc<AwaitAllState>) {
        self.active_count.fetch_add(1, Ordering::AcqRel);
        if self.shard(tid).lock().insert(tid, state).is_some() {
            self.active_count.fetch_sub(1, Ordering::AcqRel);
        }
    }

    #[inline]
    pub(crate) fn remove(&self, tid: TaskId) -> Option<Arc<AwaitAllState>> {
        if self.active_count.load(Ordering::Acquire) == 0 {
            return None;
        }
        let removed = self.shard(tid).lock().remove(&tid);
        if removed.is_some() {
            self.active_count.fetch_sub(1, Ordering::AcqRel);
        }
        removed
    }
}

pub(crate) struct PromiseEntry {
    pub parent_task: TaskId,
    pub handler_scope_id: u64,
    pub child_task: TaskId,
    pub state: PromiseState,
    pub waiters: Vec<TaskId>,
    pub all_waiters: Vec<Arc<AwaitAllState>>,
    pub observed_failure: bool,
    pub in_handler_scope: bool,
    pub owner: Option<WeakObject>,
    pub traced: bool,
}

pub(super) enum CancelUnwindAction {
    ContinueBytecode,
    Suspend,
    Done,
}

impl Vm {
    pub(super) fn alloc_future_wrapper(
        &mut self,
        inner_ty: TypeIndex,
        future_id: u64,
    ) -> Option<TaggedValue> {
        let cached = self
            .sync_meta_cache()
            .future_by_inner
            .get(&inner_ty)
            .copied();
        let future_ty = match cached {
            Some(ft) => ft,
            None => {
                let existing = self.type_pool_read().find_future(inner_ty);
                let ft = existing.unwrap_or_else(|| self.type_pool_write().intern_future(inner_ty));
                let mut cache = self.sync_meta_cache();
                cache.future_by_inner.insert(inner_ty, ft);
                let raw = ft.as_u32() as usize;
                if cache.is_future_type.len() <= raw {
                    cache.is_future_type.resize(raw + 1, None);
                }
                cache.is_future_type[raw] = Some(true);
                ft
            }
        };
        // SAFETY: the caller holds a mutator operation, no lock is held across allocation,
        // and the wrapper is rooted/published before the next safepoint.
        let pointer = unsafe { self.worker.heap.alloc_object(future_ty, 1)? };
        let payload = TaggedValue::from_u64(future_id).raw();
        // SAFETY: the freshly allocated payload has one 8-byte aligned u64 word.
        unsafe {
            pointer.as_ptr().cast::<u64>().write(payload);
            Some(TaggedValue::from_heap_ptr(pointer.as_ptr()))
        }
    }

    pub(crate) fn decode_future_id(&self, value: TaggedValue) -> Result<u64, VmError> {
        let pointer = value.as_heap_ptr().ok_or(VmError::TypeError)?;
        // SAFETY: callers hold a mutator operation and supply live rooted VM values.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        if !header.is_ordinary() || header.payload_words() != 1 {
            return Err(VmError::TypeError);
        }
        let raw_ty = header.type_index.as_u32() as usize;
        let cached = self
            .sync_meta_cache()
            .is_future_type
            .get(raw_ty)
            .copied()
            .flatten();
        let is_future = match cached {
            Some(flag) => flag,
            None => {
                let canon = self.canonical_type(header.type_index)?;
                let flag = matches!(
                    self.type_pool_read().get(canon).kind,
                    TypeKind::Future { .. }
                );
                let mut cache = self.sync_meta_cache();
                if cache.is_future_type.len() <= raw_ty {
                    cache.is_future_type.resize(raw_ty + 1, None);
                }
                cache.is_future_type[raw_ty] = Some(flag);
                flag
            }
        };
        if !is_future {
            return Err(VmError::TypeError);
        }
        // SAFETY: payload_words() == 1 proves the single u64 word is initialized and immutable.
        let raw = unsafe { pointer.cast::<u64>().read() };
        TaggedValue::from_raw(raw)
            .as_u64()
            .ok_or(VmError::TypeError)
    }

    pub(super) fn spawn_async_effect(
        &mut self,
        task_id: TaskId,
        handler: &EffectHandler,
        canon_ret_type: TypeIndex,
        arg_count: u8,
    ) -> DispatchResult {
        let handler_func = handler.handler_func;
        let handler_scope_id = handler.scope_id;
        let arg_len = arg_count as usize;
        if let Err(err) = self
            .check_user_argument_abi(handler_func, arg_len)
            .and_then(|()| self.check_entry_environment(handler_func, Some(handler.closure_env)))
        {
            return DispatchResult::Error(err);
        }

        // `task_id`'s registers `r0..r{arg_len}` are already unconditional GC roots in `TaskArena`;
        // root `handler.closure_env` in `temporary_values` across `alloc_future_wrapper`.
        let env_slot = self.worker_roots().temporary_values.borrow().len();
        self.with_temporary_roots(&[handler.closure_env], |vm| {
            let future_id = vm
                .shared
                .next_future_id
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                    cur.checked_add(1)
                })
                .map_err(|_| VmError::OutOfMemory)?;
            let future_val = vm
                .alloc_future_wrapper(canon_ret_type, future_id)
                .ok_or(VmError::OutOfMemory)?;

            let environment = vm.worker_roots().temporary_values.borrow()[env_slot];
            let layout = Self::closure_layout(environment)?;
            let capture_count = layout.capture_count as usize;
            let total_params = capture_count + arg_len;
            if total_params > runtime::GP_REGISTER_COUNT {
                return Err(VmError::TypeError);
            }

            let Some(code) = vm.shared.bytecode.try_get_function(handler_func) else {
                return Err(VmError::InvalidFunction(handler_func));
            };
            if code.param_count as usize != total_params {
                return Err(VmError::ArityError {
                    expected: code.param_count,
                    got: total_params as u8,
                });
            }

            let mut param_buf = [TaggedValue::UNIT; runtime::GP_REGISTER_COUNT];
            for (index, slot) in param_buf.iter_mut().enumerate().take(capture_count) {
                // SAFETY: `index` is bounded by validated `layout.capture_count`.
                *slot = unsafe { ClosureEnv::get_capture(layout.pointer, index as u32) };
            }
            let mut inline_handlers = [EffectHandler::PLACEHOLDER; 8];
            let mut spill_handlers: Option<Vec<EffectHandler>> = None;
            let inline_len = {
                let parent_task = vm.task_ref(task_id).expect("dispatched parent task exists");
                param_buf[capture_count..total_params]
                    .copy_from_slice(&parent_task.registers.regs[..arg_len]);
                match parent_task.snapshot_child_ancestor_handlers_inline(&mut inline_handlers) {
                    Some(n) => n,
                    None => {
                        spill_handlers = Some(parent_task.snapshot_child_ancestor_handlers());
                        0
                    }
                }
            };
            let handler_slice: &[EffectHandler] = match &spill_handlers {
                Some(vec) => vec.as_slice(),
                None => &inline_handlers[..inline_len],
            };

            let child_task_id = vm.scheduler().alloc_unscheduled_task_from_slice(
                handler_func,
                Some(task_id),
                Some(future_id),
                handler_slice,
                |child_task| {
                    child_task.registers.regs[..total_params]
                        .copy_from_slice(&param_buf[..total_params]);
                },
            );

            // SAFETY: `future_val` is a freshly allocated, initialized heap wrapper
            // published into `parent_task.registers[0]` before any safepoint; conditional
            // root callbacks forward/clear `owner` during every GC weak pass.
            let owner =
                unsafe { WeakObject::new(future_val.as_heap_ptr().expect("future heap wrapper")) };
            vm.roots().promises.shard(future_id).lock().insert(
                future_id,
                PromiseEntry {
                    parent_task: task_id,
                    handler_scope_id,
                    child_task: child_task_id,
                    state: PromiseState::Pending,
                    waiters: Vec::new(),
                    all_waiters: Vec::new(),
                    observed_failure: false,
                    in_handler_scope: true,
                    owner: Some(owner),
                    traced: false,
                },
            );
            vm.roots()
                .handler_scopes
                .shard(handler_scope_id)
                .lock()
                .entry(handler_scope_id)
                .or_default()
                .insert(future_id);
            vm.task_mut(task_id)
                .expect("dispatched parent task exists")
                .registers
                .set(Reg(0), future_val);

            vm.ensure_aux_workers_started();
            vm.scheduler()
                .publish_spawned_task(vm.local_queue(), child_task_id);
            Ok(DispatchResult::Continue)
        })
        .unwrap_or_else(DispatchResult::Error)
    }

    /// Atomically inspect `fids` across their promise shards; if any promise is still `Pending`,
    /// register `waiter` on that promise before releasing the shard locks and return `Some(fid)`.
    pub(crate) fn try_wait_on_first_pending(&self, fids: &[u64], waiter: TaskId) -> Option<u64> {
        if fids.is_empty() {
            return None;
        }
        let mut promises = self.roots().promises.lock_fids(fids);
        for &fid in fids {
            if let Some(entry) = promises.get_mut(fid)
                && matches!(entry.state, PromiseState::Pending)
            {
                if !entry.waiters.contains(&waiter) {
                    entry.waiters.push(waiter);
                }
                return Some(fid);
            }
        }
        None
    }

    fn detach_promise_from_handler_scope(&self, handler_scope_id: u64, future_id: u64) {
        let mut scopes = self.roots().handler_scopes.shard(handler_scope_id).lock();
        let remove_scope = if let Some(scope_set) = scopes.get_mut(&handler_scope_id) {
            scope_set.remove(&future_id);
            scope_set.is_empty()
        } else {
            false
        };
        if remove_scope {
            scopes.remove(&handler_scope_id);
        }
    }

    pub(crate) fn mark_promise_failure_observed(&self, future_id: u64) {
        let detached = {
            let mut promises = self.roots().promises.shard(future_id).lock();
            if let Some(entry) = promises.get_mut(&future_id) {
                entry.observed_failure = true;
                if entry.in_handler_scope {
                    entry.in_handler_scope = false;
                    Some((entry.handler_scope_id, entry.owner.is_some()))
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some((scope_id, has_owner)) = detached {
            self.detach_promise_from_handler_scope(scope_id, future_id);
            if has_owner {
                self.roots()
                    .uncollected_terminal_promises
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn release_handler_scope(&self, scope_id: u64) {
        let removed_fids = self
            .roots()
            .handler_scopes
            .shard(scope_id)
            .lock()
            .remove(&scope_id);
        if let Some(fids) = removed_fids {
            let mut newly_reclaimable = 0usize;
            for fid in fids {
                let mut shard = self.roots().promises.shard(fid).lock();
                if let Some(entry) = shard.get_mut(&fid) {
                    entry.in_handler_scope = false;
                    if entry.owner.is_some() {
                        newly_reclaimable += 1;
                    }
                }
            }
            if newly_reclaimable > 0 {
                self.roots()
                    .uncollected_terminal_promises
                    .fetch_add(newly_reclaimable, Ordering::Relaxed);
            }
        }
    }

    pub(super) fn complete_promise(&mut self, future_id: u64, value: TaggedValue) {
        let (parent_task, handler_scope_id, has_owner, waiters, all_waiters) = {
            let mut promises = self.roots().promises.shard(future_id).lock();
            if let Some(entry) = promises.get_mut(&future_id)
                && matches!(entry.state, PromiseState::Pending)
            {
                entry.state = PromiseState::Completed(value);
                entry.in_handler_scope = false;
                (
                    entry.parent_task,
                    entry.handler_scope_id,
                    entry.owner.is_some(),
                    std::mem::take(&mut entry.waiters),
                    std::mem::take(&mut entry.all_waiters),
                )
            } else {
                return;
            }
        };
        self.scheduler().remove_task_promise(parent_task, future_id);
        self.detach_promise_from_handler_scope(handler_scope_id, future_id);
        if has_owner {
            self.roots()
                .uncollected_terminal_promises
                .fetch_add(1, Ordering::Relaxed);
        }
        let local_q = self.local_queue();
        for waiter in waiters {
            self.scheduler().wake_with_queue(local_q, waiter);
        }
        for aw in all_waiters {
            if aw.remaining.fetch_sub(1, Ordering::AcqRel) == 1
                && !aw.early_exit.load(Ordering::Acquire)
            {
                self.scheduler().wake_with_queue(local_q, aw.task_id);
            }
        }
    }

    pub(super) fn fail_promise(&mut self, future_id: u64, error: VmError) {
        let (parent_task, has_owner, in_scope, waiters, all_waiters) = {
            let mut promises = self.roots().promises.shard(future_id).lock();
            if let Some(entry) = promises.get_mut(&future_id)
                && matches!(entry.state, PromiseState::Pending)
            {
                entry.state = PromiseState::Failed(error);
                (
                    entry.parent_task,
                    entry.owner.is_some(),
                    entry.in_handler_scope,
                    std::mem::take(&mut entry.waiters),
                    std::mem::take(&mut entry.all_waiters),
                )
            } else {
                return;
            }
        };
        self.scheduler().remove_task_promise(parent_task, future_id);
        if !in_scope && has_owner {
            self.roots()
                .uncollected_terminal_promises
                .fetch_add(1, Ordering::Relaxed);
        }
        let local_q = self.local_queue();
        for waiter in waiters {
            self.scheduler().wake_with_queue(local_q, waiter);
        }
        for aw in all_waiters {
            if !aw.early_exit.swap(true, Ordering::AcqRel) {
                self.scheduler().wake_with_queue(local_q, aw.task_id);
            }
        }
    }

    pub(super) fn finish_cancelled_promise(&mut self, future_id: u64) {
        let (parent_task, handler_scope_id, has_owner, waiters, all_waiters) = {
            let mut promises = self.roots().promises.shard(future_id).lock();
            if let Some(entry) = promises.get_mut(&future_id)
                && matches!(entry.state, PromiseState::Pending)
            {
                entry.state = PromiseState::Cancelled;
                entry.in_handler_scope = false;
                (
                    entry.parent_task,
                    entry.handler_scope_id,
                    entry.owner.is_some(),
                    std::mem::take(&mut entry.waiters),
                    std::mem::take(&mut entry.all_waiters),
                )
            } else {
                return;
            }
        };
        self.scheduler().remove_task_promise(parent_task, future_id);
        self.detach_promise_from_handler_scope(handler_scope_id, future_id);
        if has_owner {
            self.roots()
                .uncollected_terminal_promises
                .fetch_add(1, Ordering::Relaxed);
        }
        let local_q = self.local_queue();
        for waiter in waiters {
            self.scheduler().wake_with_queue(local_q, waiter);
        }
        for aw in all_waiters {
            if !aw.early_exit.swap(true, Ordering::AcqRel) {
                self.scheduler().wake_with_queue(local_q, aw.task_id);
            }
        }
    }

    /// Cooperatively request cancellation on a future's task subtree without
    /// discarding stacks before `defer` blocks have unwound.
    pub(crate) fn cancel_future(&mut self, future_id: u64) {
        let child_task = self
            .roots()
            .promises
            .shard(future_id)
            .lock()
            .get(&future_id)
            .and_then(|entry| {
                matches!(entry.state, PromiseState::Pending).then_some(entry.child_task)
            });
        if let Some(child) = child_task {
            self.request_cancel_task_subtree(child);
        }
    }

    pub(super) fn request_cancel_task_subtree(&mut self, root_task: TaskId) {
        let mut pending_tasks = vec![root_task];
        let mut seen_tasks = std::collections::HashSet::new();

        while let Some(next_root) = pending_tasks.pop() {
            if !seen_tasks.insert(next_root) {
                continue;
            }
            let visited = self.scheduler().request_cancel_subtree(next_root);
            let mut child_fids = Vec::new();
            for tid in visited {
                seen_tasks.insert(tid);
                child_fids.extend(self.scheduler().task_promises_snapshot(tid));
            }
            for fid in child_fids {
                let shard = self.roots().promises.shard(fid).lock();
                if let Some(entry) = shard.get(&fid)
                    && matches!(entry.state, PromiseState::Pending)
                {
                    pending_tasks.push(entry.child_task);
                }
            }
        }
    }

    /// Advance cooperative cancellation unwinding for `task_id`:
    /// unwinds inner handler scopes (cancelling and awaiting children so inner
    /// child `defer`s complete before outer `defer`s) and executes frame `defer`
    /// blocks in strict LIFO order from innermost call frame to task root.
    pub(super) fn step_cancel_unwind(&mut self, task_id: TaskId) -> CancelUnwindAction {
        let children = self.scheduler().task_children(task_id);
        let promise_fids = self.scheduler().task_promises_snapshot(task_id);
        for child in children {
            self.request_cancel_task_subtree(child);
        }
        for &fid in &promise_fids {
            self.cancel_future(fid);
        }
        if self
            .try_wait_on_first_pending(&promise_fids, task_id)
            .is_some()
        {
            if let Some(task) = self.task_mut(task_id) {
                task.paused_in_unwind_step = true;
            }
            return CancelUnwindAction::Suspend;
        }

        loop {
            let top_entry = self
                .task_ref(task_id)
                .and_then(|t| t.unwind_stack.last().copied());

            match top_entry {
                Some(UnwindEntry::DeferPc(resume_pc)) => {
                    let task = self.task_mut(task_id).expect("unwinding task exists");
                    task.unwind_stack.pop();
                    task.is_unwind_target = true;
                    task.registers.set(Reg(0), TaggedValue::TRUE);
                    task.pc = resume_pc;
                    return CancelUnwindAction::ContinueBytecode;
                }
                Some(UnwindEntry::HandlerScope(scope_id)) => {
                    let fids: Vec<u64> = self
                        .roots()
                        .handler_scopes
                        .shard(scope_id)
                        .lock()
                        .get(&scope_id)
                        .map(|s| s.iter().copied().collect())
                        .unwrap_or_default();
                    for &fid in &fids {
                        self.cancel_future(fid);
                    }
                    if self.try_wait_on_first_pending(&fids, task_id).is_some() {
                        if let Some(task) = self.task_mut(task_id) {
                            task.paused_in_unwind_step = true;
                        }
                        return CancelUnwindAction::Suspend;
                    }
                    {
                        let task = self.task_mut(task_id).expect("unwinding task exists");
                        task.unwind_stack.pop();
                        if let Some(pos) = task
                            .handler_stack
                            .iter()
                            .rposition(|h| h.scope_id == scope_id)
                        {
                            task.handler_stack.remove(pos);
                        }
                    }
                    self.release_handler_scope(scope_id);
                }
                None => {
                    let popped_caller_or_delim = {
                        let task = self.task_mut(task_id).expect("unwinding task exists");
                        if let Some(frame) = task.call_stack.pop() {
                            task.pc = frame.return_pc;
                            task.current_func = frame.func_id;
                            let old_slots =
                                std::mem::replace(&mut task.local_slots, frame.local_slots);
                            task.display_state = frame.display_state;
                            task.unwind_stack = frame.unwind_stack;
                            task.is_unwind_target = false;
                            for (reg, saved_val) in &frame.saved_regs {
                                task.registers.set(*reg, *saved_val);
                            }
                            self.recycle_frame_slots(old_slots);
                            true
                        } else if task.stacks.return_from_delimiter(TaggedValue::UNIT) {
                            task.is_unwind_target = false;
                            true
                        } else {
                            false
                        }
                    };
                    if popped_caller_or_delim {
                        continue;
                    }

                    let children = self.scheduler().task_children(task_id);
                    let promise_fids = self.scheduler().task_promises_snapshot(task_id);
                    for child in children {
                        self.request_cancel_task_subtree(child);
                    }
                    for &fid in &promise_fids {
                        self.cancel_future(fid);
                    }
                    if self
                        .try_wait_on_first_pending(&promise_fids, task_id)
                        .is_some()
                    {
                        if let Some(task) = self.task_mut(task_id) {
                            task.paused_in_unwind_step = true;
                        }
                        return CancelUnwindAction::Suspend;
                    }
                    return CancelUnwindAction::Done;
                }
            }
        }
    }

    pub(super) fn dispatch_await_future(
        &mut self,
        task_id: TaskId,
        instr: &Instruction,
    ) -> DispatchResult {
        let payload = e_payload(instr);
        if payload & 0xFFF != 0 {
            return DispatchResult::Error(VmError::InvalidInstruction(instr.encode()));
        }
        let (dst, src) = Instruction::e_control_regs(payload);
        let future_val = self
            .task_ref(task_id)
            .expect("dispatched task exists")
            .registers
            .get(src);
        let future_id = match self.decode_future_id(future_val) {
            Ok(id) => id,
            Err(err) => return DispatchResult::Error(err),
        };

        enum AwaitDecision {
            Suspend,
            Completed(TaggedValue),
            Failed(VmError, Option<(u64, bool)>),
            Cancelled,
        }

        let decision = {
            let mut promises = self.roots().promises.shard(future_id).lock();
            let Some(entry) = promises.get_mut(&future_id) else {
                return DispatchResult::Error(VmError::TypeError);
            };
            match &entry.state {
                PromiseState::Pending => {
                    if !entry.waiters.contains(&task_id) {
                        entry.waiters.push(task_id);
                    }
                    AwaitDecision::Suspend
                }
                PromiseState::Completed(val) => AwaitDecision::Completed(*val),
                PromiseState::Failed(err) => {
                    let cloned_err = err.clone();
                    entry.observed_failure = true;
                    let detach = if entry.in_handler_scope {
                        entry.in_handler_scope = false;
                        Some((entry.handler_scope_id, entry.owner.is_some()))
                    } else {
                        None
                    };
                    AwaitDecision::Failed(cloned_err, detach)
                }
                PromiseState::Cancelled => AwaitDecision::Cancelled,
            }
        };

        match decision {
            AwaitDecision::Suspend => DispatchResult::SuspendRetry,
            AwaitDecision::Completed(result) => {
                self.task_mut(task_id)
                    .expect("dispatched task exists")
                    .registers
                    .set(dst, result);
                DispatchResult::Continue
            }
            AwaitDecision::Failed(err, detach) => {
                if let Some((scope_id, has_owner)) = detach {
                    self.detach_promise_from_handler_scope(scope_id, future_id);
                    if has_owner {
                        self.roots()
                            .uncollected_terminal_promises
                            .fetch_add(1, Ordering::Relaxed);
                    }
                }
                DispatchResult::Error(err)
            }
            AwaitDecision::Cancelled => DispatchResult::Error(VmError::TaskCancelled),
        }
    }

    pub(super) fn dispatch_pop_handler(&mut self, task_id: TaskId) -> DispatchResult {
        let Some(scope_id) = self
            .task_ref(task_id)
            .expect("dispatched task exists")
            .handler_stack
            .last()
            .map(|h| h.scope_id)
        else {
            return DispatchResult::Continue;
        };

        let fids: Vec<u64> = self
            .roots()
            .handler_scopes
            .shard(scope_id)
            .lock()
            .get(&scope_id)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        for &fid in &fids {
            self.cancel_future(fid);
        }
        if self.try_wait_on_first_pending(&fids, task_id).is_some() {
            return DispatchResult::SuspendRetry;
        }

        {
            let task = self.task_mut(task_id).expect("dispatched task exists");
            task.handler_stack.pop();
            if let Some(pos) = task
                .unwind_stack
                .iter()
                .rposition(|e| *e == UnwindEntry::HandlerScope(scope_id))
            {
                task.unwind_stack.remove(pos);
            }
        }
        self.release_handler_scope(scope_id);
        DispatchResult::Continue
    }

    pub(super) fn dispatch_await_pop_handler(&mut self, task_id: TaskId) -> DispatchResult {
        let Some(scope_id) = self
            .task_ref(task_id)
            .expect("dispatched task exists")
            .handler_stack
            .last()
            .map(|h| h.scope_id)
        else {
            return DispatchResult::Continue;
        };

        let fids: Vec<u64> = self
            .roots()
            .handler_scopes
            .shard(scope_id)
            .lock()
            .get(&scope_id)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        if self.try_wait_on_first_pending(&fids, task_id).is_some() {
            return DispatchResult::SuspendRetry;
        }

        {
            let task = self.task_mut(task_id).expect("dispatched task exists");
            let _ = task.handler_stack.pop();
            if let Some(pos) = task
                .unwind_stack
                .iter()
                .rposition(|e| *e == UnwindEntry::HandlerScope(scope_id))
            {
                task.unwind_stack.remove(pos);
            }
        }

        let removed = self
            .roots()
            .handler_scopes
            .shard(scope_id)
            .lock()
            .remove(&scope_id);
        let mut unobserved_error = None;
        if let Some(fids) = removed {
            let mut newly_reclaimable = 0usize;
            for fid in fids {
                let mut shard = self.roots().promises.shard(fid).lock();
                if let Some(entry) = shard.get_mut(&fid) {
                    entry.in_handler_scope = false;
                    if entry.owner.is_some() {
                        newly_reclaimable += 1;
                    }
                    if unobserved_error.is_none()
                        && let PromiseState::Failed(err) = &entry.state
                        && !entry.observed_failure
                    {
                        unobserved_error = Some(err.clone());
                        entry.observed_failure = true;
                    }
                }
            }
            if newly_reclaimable > 0 {
                self.roots()
                    .uncollected_terminal_promises
                    .fetch_add(newly_reclaimable, Ordering::Relaxed);
            }
        }
        if let Some(err) = unobserved_error {
            return DispatchResult::Error(err);
        }
        DispatchResult::Continue
    }

    pub(super) fn check_task_root_return(
        &mut self,
        task_id: TaskId,
        val: TaggedValue,
    ) -> Result<Option<u64>, VmError> {
        let promise_set = self.scheduler().task_promises_snapshot(task_id);
        if promise_set.is_empty() {
            return Ok(None);
        }
        let is_err = self
            .error_layout(val, 0)?
            .is_some_and(|layout| layout.tag != TypeId::ZERO);
        if is_err {
            for &fid in &promise_set {
                self.cancel_future(fid);
            }
        }
        Ok(self.try_wait_on_first_pending(&promise_set, task_id))
    }
}
