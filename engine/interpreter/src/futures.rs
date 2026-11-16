//! Async effect promises, `Future` heap handles, task-tree cancellation, and `await` dispatch.

use gc::ObjectHeader;
use nsbc::{Instruction, Reg};
use runtime::{ClosureEnv, EffectHandler, TaggedValue, TaskId};
use type_pool::{Intrinsic, TypeId, TypeIndex, TypeKind};

use crate::{DispatchResult, Vm, VmError, e_payload};

#[derive(Debug, Clone)]
pub(crate) enum PromiseState {
    Pending,
    Completed(TaggedValue),
    Failed(VmError),
    Cancelled,
}

#[derive(Debug, Clone)]
pub(crate) struct PromiseEntry {
    pub child_task: TaskId,
    pub state: PromiseState,
    pub waiters: Vec<TaskId>,
    pub observed_failure: bool,
}

impl Vm {
    pub(super) fn alloc_future_wrapper(
        &mut self,
        inner_ty: TypeIndex,
        future_id: u64,
    ) -> Option<TaggedValue> {
        let future_ty = self
            .state
            .type_pool
            .find_future(inner_ty)
            .unwrap_or_else(|| self.state.type_pool.intern_future(inner_ty));
        // SAFETY: the caller holds a mutator operation and publishes or roots
        // the wrapper before the next safepoint.
        let pointer = unsafe { self.state.heap.alloc_object(future_ty, 1)? };
        // Encode as an unsigned immediate so the conservative payload word scanner
        // never mistakes an 8-aligned handle ID for a heap pointer.
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
        let canon = self.canonical_type(header.type_index)?;
        if !matches!(
            self.state.type_pool.get(canon).kind,
            TypeKind::Future { .. }
        ) {
            return Err(VmError::TypeError);
        }
        // SAFETY: payload_words() == 1 proves the single u64 word is initialized.
        let raw = unsafe { pointer.cast::<u64>().read() };
        TaggedValue::from_raw(raw)
            .as_u64()
            .ok_or(VmError::TypeError)
    }

    pub(super) fn spawn_async_effect(
        &mut self,
        task_id: TaskId,
        handler: &EffectHandler,
        ret_type: TypeIndex,
        args: &[TaggedValue],
    ) -> DispatchResult {
        let handler_func = handler.handler_func;
        let handler_scope_id = handler.scope_id;
        let mut initial_roots = args.to_vec();
        initial_roots.push(handler.closure_env);
        let base_outer = self.roots.temporary_values.borrow().len();
        let expanded = self.with_temporary_roots(&initial_roots, |vm| {
            let physical = vm.expand_logical_arguments(handler_func, args)?;
            let environment = vm.roots.temporary_values.borrow()[base_outer + args.len()];
            vm.check_entry_environment(handler_func, Some(environment))?;
            vm.check_physical_arguments(handler_func, &physical)?;
            Ok((physical, environment))
        });
        let (physical, environment) = match expanded {
            Ok(pair) => pair,
            Err(err) => return DispatchResult::Error(err),
        };

        let phys_len = physical.len();
        let mut spawn_roots = physical;
        spawn_roots.push(environment);
        let base_inner = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&spawn_roots, |vm| {
            let future_id = vm.state.next_future_id;
            vm.state.next_future_id = future_id.checked_add(1).ok_or(VmError::OutOfMemory)?;
            let future_ty = vm
                .canonical_type(ret_type)
                .unwrap_or(Intrinsic::Any.type_index());
            let future_val = vm
                .alloc_future_wrapper(future_ty, future_id)
                .ok_or(VmError::OutOfMemory)?;

            let roots = vm.roots.temporary_values.borrow();
            let physical_args = roots[base_inner..base_inner + phys_len].to_vec();
            let environment = roots[base_inner + phys_len];
            drop(roots);

            let layout = Self::closure_layout(environment)?;
            let capture_count = layout.capture_count;
            if capture_count as usize + physical_args.len() > runtime::GP_REGISTER_COUNT {
                return Err(VmError::TypeError);
            }
            let mut parameters = Vec::with_capacity(capture_count as usize + physical_args.len());
            for index in 0..capture_count {
                // SAFETY: index is bounded by the validated closure capture count.
                parameters.push(unsafe { ClosureEnv::get_capture(layout.pointer, index) });
            }
            parameters.extend_from_slice(&physical_args);

            let Some(code) = vm.state.bytecode.try_get_function(handler_func) else {
                return Err(VmError::InvalidFunction(handler_func));
            };
            if code.param_count as usize != parameters.len() {
                return Err(VmError::ArityError {
                    expected: code.param_count,
                    got: parameters.len() as u8,
                });
            }

            let child_task_id = vm.roots.scheduler.spawn(handler_func, Some(task_id));
            {
                let child_task = vm
                    .roots
                    .scheduler
                    .get_task_mut(child_task_id)
                    .expect("freshly spawned child task exists");
                child_task.registers.regs[..parameters.len()].copy_from_slice(&parameters);
                child_task.spawned_promise = Some(future_id);
            }

            vm.roots.promises.insert(
                future_id,
                PromiseEntry {
                    child_task: child_task_id,
                    state: PromiseState::Pending,
                    waiters: Vec::new(),
                    observed_failure: false,
                },
            );
            vm.roots
                .handler_scopes
                .entry(handler_scope_id)
                .or_default()
                .push(future_id);
            let parent_task = vm
                .roots
                .scheduler
                .get_task_mut(task_id)
                .expect("dispatched parent task exists");
            parent_task.promise_set.push(future_id);
            parent_task.registers.set(Reg(0), future_val);
            Ok(DispatchResult::Continue)
        })
        .unwrap_or_else(DispatchResult::Error)
    }

    pub(crate) fn register_promise_waiter(&mut self, future_id: u64, waiter: TaskId) {
        if let Some(entry) = self.roots.promises.get_mut(&future_id)
            && !entry.waiters.contains(&waiter)
        {
            entry.waiters.push(waiter);
        }
    }

    pub(super) fn complete_promise(&mut self, future_id: u64, value: TaggedValue) {
        let waiters = if let Some(entry) = self.roots.promises.get_mut(&future_id)
            && matches!(entry.state, PromiseState::Pending)
        {
            entry.state = PromiseState::Completed(value);
            std::mem::take(&mut entry.waiters)
        } else {
            return;
        };
        for waiter in waiters {
            self.roots.scheduler.resume(waiter);
        }
    }

    pub(super) fn fail_promise(&mut self, future_id: u64, error: VmError) {
        let waiters = if let Some(entry) = self.roots.promises.get_mut(&future_id)
            && matches!(entry.state, PromiseState::Pending)
        {
            entry.state = PromiseState::Failed(error);
            std::mem::take(&mut entry.waiters)
        } else {
            return;
        };
        for waiter in waiters {
            self.roots.scheduler.resume(waiter);
        }
    }

    pub(crate) fn cancel_future(&mut self, future_id: u64) {
        let Some(child_task) = self.roots.promises.get(&future_id).and_then(|entry| {
            matches!(entry.state, PromiseState::Pending).then_some(entry.child_task)
        }) else {
            return;
        };
        self.cancel_task_subtree(child_task);
    }

    pub(super) fn cancel_task_subtree(&mut self, root_task: TaskId) {
        let mut pending_tasks = vec![root_task];
        let mut seen_tasks = std::collections::HashSet::new();
        let mut cancelled_promises = Vec::new();

        while let Some(next_root) = pending_tasks.pop() {
            if !seen_tasks.insert(next_root) {
                continue;
            }
            let visited = self.roots.scheduler.cancel_subtree(next_root);
            for tid in visited {
                seen_tasks.insert(tid);
                if let Some(task) = self.roots.scheduler.get_task(tid) {
                    if let Some(fid) = task.spawned_promise {
                        cancelled_promises.push(fid);
                    }
                    for &child_fid in &task.promise_set {
                        cancelled_promises.push(child_fid);
                        if let Some(entry) = self.roots.promises.get(&child_fid)
                            && matches!(entry.state, PromiseState::Pending)
                        {
                            pending_tasks.push(entry.child_task);
                        }
                    }
                }
            }
        }

        let mut waiters_to_wake = Vec::new();
        for fid in cancelled_promises {
            if let Some(entry) = self.roots.promises.get_mut(&fid)
                && matches!(entry.state, PromiseState::Pending)
            {
                entry.state = PromiseState::Cancelled;
                waiters_to_wake.extend(std::mem::take(&mut entry.waiters));
            }
        }
        for waiter in waiters_to_wake {
            self.roots.scheduler.resume(waiter);
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
            .roots
            .scheduler
            .get_task(task_id)
            .expect("dispatched task exists")
            .registers
            .get(src);
        let future_id = match self.decode_future_id(future_val) {
            Ok(id) => id,
            Err(err) => return DispatchResult::Error(err),
        };
        let Some(entry) = self.roots.promises.get_mut(&future_id) else {
            return DispatchResult::Error(VmError::TypeError);
        };
        match &entry.state {
            PromiseState::Pending => {
                self.register_promise_waiter(future_id, task_id);
                DispatchResult::SuspendRetry
            }
            PromiseState::Completed(val) => {
                let result = *val;
                self.roots
                    .scheduler
                    .get_task_mut(task_id)
                    .expect("dispatched task exists")
                    .registers
                    .set(dst, result);
                DispatchResult::Continue
            }
            PromiseState::Failed(err) => {
                let err = err.clone();
                entry.observed_failure = true;
                DispatchResult::Error(err)
            }
            PromiseState::Cancelled => DispatchResult::Error(VmError::TaskCancelled),
        }
    }

    pub(super) fn dispatch_pop_handler(&mut self, task_id: TaskId) -> DispatchResult {
        let popped = self
            .roots
            .scheduler
            .get_task_mut(task_id)
            .expect("dispatched task exists")
            .handler_stack
            .pop();
        if let Some(handler) = popped
            && let Some(fids) = self.roots.handler_scopes.remove(&handler.scope_id)
        {
            for fid in fids {
                self.cancel_future(fid);
            }
        }
        DispatchResult::Continue
    }

    pub(super) fn dispatch_await_pop_handler(&mut self, task_id: TaskId) -> DispatchResult {
        let Some(scope_id) = self
            .roots
            .scheduler
            .get_task(task_id)
            .expect("dispatched task exists")
            .handler_stack
            .last()
            .map(|h| h.scope_id)
        else {
            return DispatchResult::Continue;
        };
        if let Some(pending_fid) = self.roots.handler_scopes.get(&scope_id).and_then(|fids| {
            fids.iter().copied().find(|fid| {
                self.roots
                    .promises
                    .get(fid)
                    .is_some_and(|e| matches!(e.state, PromiseState::Pending))
            })
        }) {
            self.register_promise_waiter(pending_fid, task_id);
            return DispatchResult::SuspendRetry;
        }

        let _ = self
            .roots
            .scheduler
            .get_task_mut(task_id)
            .expect("dispatched task exists")
            .handler_stack
            .pop();
        if let Some(fids) = self.roots.handler_scopes.remove(&scope_id) {
            for fid in fids {
                if let Some(entry) = self.roots.promises.get_mut(&fid)
                    && let PromiseState::Failed(err) = &entry.state
                    && !entry.observed_failure
                {
                    let err = err.clone();
                    entry.observed_failure = true;
                    return DispatchResult::Error(err);
                }
            }
        }
        DispatchResult::Continue
    }

    /// Before a task completes at its root frame, ensure structured concurrency:
    /// if returning an `Err` envelope, cancel any pending child promises; otherwise
    /// if any child promise in `promise_set` is still `Pending`, register as a waiter
    /// and return `Some(fid)` so the caller suspends and retries the return.
    pub(super) fn check_task_root_return(
        &mut self,
        task_id: TaskId,
        val: TaggedValue,
    ) -> Result<Option<u64>, VmError> {
        let promise_set = self
            .roots
            .scheduler
            .get_task(task_id)
            .ok_or(VmError::InvalidTask)?
            .promise_set
            .clone();
        if promise_set.is_empty() {
            return Ok(None);
        }
        let is_err = self
            .error_layout(val, 0)?
            .is_some_and(|layout| layout.tag != TypeId::ZERO);
        if is_err {
            for fid in promise_set {
                self.cancel_future(fid);
            }
            return Ok(None);
        }
        for fid in promise_set {
            if self
                .roots
                .promises
                .get(&fid)
                .is_some_and(|entry| matches!(entry.state, PromiseState::Pending))
            {
                self.register_promise_waiter(fid, task_id);
                return Ok(Some(fid));
            }
        }
        Ok(None)
    }
}
