//! Conditional ownership of detached language continuation stacks.
//!
//! Raw ABI captures remain strong roots. Language templates become roots only
//! after their weak wrapper is reached; repeated weak passes compute chains of
//! owners without retaining isolated cycles.

use std::sync::atomic::Ordering;

use gc::{ConditionalRootCallbacks, NessaSlot, WeakObject};
use runtime::{ContinuationId, StackContext, StackError, TaggedValue, TaskId};
use type_pool::TypeIndex;

use crate::{Vm, VmError, VmRoots};

#[derive(Clone, Copy)]
pub(crate) struct ContinuationInputContract {
    pub input: TypeIndex,
    pub scope: Option<u32>,
}

pub(crate) struct ManagedContinuation {
    task: TaskId,
    owner: WeakObject,
    traced: bool,
    contract: ContinuationInputContract,
}

pub(crate) struct WeakFinalizer {
    owner: WeakObject,
    reachable: bool,
    cleanup: Option<Box<dyn FnOnce() + Send>>,
}

impl WeakFinalizer {
    pub(crate) fn run_cleanup(&mut self) {
        if let Some(cb) = self.cleanup.take() {
            cb();
        }
    }
}

pub(super) const CALLBACKS: ConditionalRootCallbacks = ConditionalRootCallbacks {
    prepare,
    trace,
    sweep,
    forward,
};

impl Vm {
    pub(super) fn continuation_input_contract(
        &self,
        task: TaskId,
        handle: ContinuationId,
    ) -> Result<ContinuationInputContract, VmError> {
        let invalid = || VmError::Stack(StackError::InvalidContinuation);
        let (owner_task, contract) = {
            let guard = self.roots().continuations.lock();
            let continuation = guard.get(&handle).ok_or_else(invalid)?;
            (continuation.task, continuation.contract)
        };
        if owner_task != task
            || !self
                .task_ref(task)
                .is_some_and(|task| task.stacks.has_capture(handle))
        {
            return Err(invalid());
        }
        Ok(contract)
    }

    pub(super) fn own_continuation(
        &mut self,
        task: TaskId,
        handle: ContinuationId,
        value: TaggedValue,
        contract: ContinuationInputContract,
    ) {
        self.task_mut(task)
            .expect("continuation belongs to a live task")
            .stacks
            .promote_language_capture(handle)
            .expect("only a live pending capture is published");
        // SAFETY: the wrapper is initialized, and this registry removes dead
        // owners at every weak fixed point before mutators resume.
        let owner = unsafe { WeakObject::new(value.as_heap_ptr().expect("continuation wrapper")) };
        let previous = self.roots().continuations.lock().insert(
            handle,
            ManagedContinuation {
                task,
                owner,
                traced: false,
                contract,
            },
        );
        assert!(previous.is_none(), "capture has exactly one language owner");
    }

    pub(crate) fn register_weak_finalizer(
        &mut self,
        target: TaggedValue,
        cleanup: impl FnOnce() + Send + 'static,
    ) -> Result<(), VmError> {
        let ptr = target.as_heap_ptr().ok_or(VmError::TypeError)?;
        // SAFETY: `target` is a live rooted heap object during the active mutator operation;
        // `sweep` / `VmRoots::drop` removes unreachable entries before mutators resume.
        let owner = unsafe { WeakObject::new(ptr) };
        self.roots().weak_finalizers.lock().push(WeakFinalizer {
            owner,
            reachable: false,
            cleanup: Some(Box::new(cleanup)),
        });
        Ok(())
    }
}

unsafe fn prepare(context: *const ()) {
    // SAFETY: the registered UnsafeCell is stable; STW gives exclusive access,
    // and preparation runs before ordinary root scanning starts.
    let roots = unsafe { &mut *context.cast::<VmRoots>().cast_mut() };
    let continuations = unsafe { &mut *roots.continuations.data_ptr() };
    for continuation in continuations.values_mut() {
        continuation.traced = false;
    }
    let finalizers = unsafe { &mut *roots.weak_finalizers.data_ptr() };
    for finalizer in finalizers.iter_mut() {
        finalizer.reachable = false;
    }
    // SAFETY: STW preparation runs with all mutators stopped and no shard locks held.
    unsafe {
        roots.promises.for_each_shard_stopped_mut(|promises| {
            for entry in promises.values_mut() {
                entry.traced = false;
            }
        });
    }
}

unsafe fn trace(context: *const (), visitor: &mut dyn FnMut(NessaSlot)) -> bool {
    // SAFETY: weak processing runs after all ordinary tracing work under STW;
    // the registration lock keeps this UnsafeCell backing state alive.
    let roots = unsafe { &mut *context.cast::<VmRoots>().cast_mut() };
    let scheduler = &mut roots.scheduler;
    let continuations = unsafe { &mut *roots.continuations.data_ptr() };
    let mut changed = false;
    continuations.retain(|&handle, continuation| {
        let Some(task) = scheduler.get_task_mut(continuation.task) else {
            return false;
        };
        if !task.stacks.has_capture(handle) {
            return false;
        };
        if !continuation.traced {
            // SAFETY: the current closure is complete and unswept owners have
            // valid old addresses. Forward only owners already reached.
            if unsafe { continuation.owner.is_reachable() } {
                unsafe { continuation.owner.forward() };
                continuation.traced = true;
                task.stacks
                    .visit_captured_contexts_mut(handle, |stack| visit_stack_slots(stack, visitor))
                    .expect("owner's capture checked under exclusive access");
                changed = true;
            }
        }
        true
    });
    // SAFETY: exclusive STW weak tracing pass with all mutators parked.
    unsafe {
        roots.promises.for_each_shard_stopped_mut(|promises| {
            for entry in promises.values_mut() {
                if !entry.traced
                    && let Some(ref mut owner) = entry.owner
                {
                    // SAFETY: current closure is complete; unswept `Future` owners hold
                    // valid pre-forwarding heap addresses under exclusive STW access.
                    if owner.is_reachable() {
                        owner.forward();
                        entry.traced = true;
                        if entry.waiters.is_empty()
                            && entry.all_waiters.is_empty()
                            && let crate::futures::PromiseState::Completed(ref mut val) =
                                entry.state
                        {
                            // SAFETY: exclusive STW slot visit updates the promise value in place.
                            visitor(NessaSlot::from_raw_ptr(
                                (val as *mut TaggedValue).cast::<u64>(),
                            ));
                        }
                        changed = true;
                    }
                }
            }
        });
    }
    let finalizers = unsafe { &mut *roots.weak_finalizers.data_ptr() };
    for finalizer in finalizers.iter_mut() {
        if !finalizer.reachable {
            // SAFETY: `owner` tracks an unswept heap object under STW after closure completion.
            if unsafe { finalizer.owner.is_reachable() } {
                unsafe { finalizer.owner.forward() };
                finalizer.reachable = true;
            }
        }
    }
    changed
}

unsafe fn sweep(context: *const ()) {
    // SAFETY: no new owner was activated in the last completed closure pass;
    // no tracing work retains slots in the chains discarded below.
    let roots = unsafe { &mut *context.cast::<VmRoots>().cast_mut() };
    let scheduler = &mut roots.scheduler;
    let continuations = unsafe { &mut *roots.continuations.data_ptr() };
    continuations.retain(|&handle, continuation| {
        if continuation.traced {
            return true;
        }
        if let Some(task) = scheduler.get_task_mut(continuation.task)
            && task.stacks.has_capture(handle)
        {
            task.stacks
                .discard(handle)
                .expect("checked unreachable capture");
        }
        false
    });
    let finalizers = unsafe { &mut *roots.weak_finalizers.data_ptr() };
    finalizers.retain_mut(|finalizer| {
        if finalizer.reachable {
            true
        } else {
            finalizer.run_cleanup();
            false
        }
    });
    // SAFETY: exclusive STW sweep pass with all mutators parked.
    unsafe {
        roots.promises.for_each_shard_stopped_mut(|promises| {
            promises.retain(|_, entry| {
                if !entry.traced {
                    entry.owner = None;
                }
                entry.traced
                    || matches!(entry.state, crate::futures::PromiseState::Pending)
                    || !entry.waiters.is_empty()
                    || !entry.all_waiters.is_empty()
                    || (entry.in_handler_scope
                        && matches!(entry.state, crate::futures::PromiseState::Failed(_))
                        && !entry.observed_failure)
            });
        });
    }
    roots
        .uncollected_terminal_promises
        .store(0, Ordering::Relaxed);
}

unsafe fn forward(context: *const (), visitor: &mut dyn FnMut(NessaSlot)) {
    // SAFETY: the collector's forwarding phase follows weak closure/sweep,
    // retaining only reachable owners, with exclusive stopped state.
    let roots = unsafe { &mut *context.cast::<VmRoots>().cast_mut() };
    let continuations = unsafe { &mut *roots.continuations.data_ptr() };
    for (&handle, continuation) in continuations.iter_mut() {
        unsafe { continuation.owner.forward() };
        if let Some(task) = roots.scheduler.get_task_mut(continuation.task)
            && task.stacks.has_capture(handle)
        {
            task.stacks
                .visit_captured_contexts_mut(handle, |stack| visit_stack_slots(stack, visitor))
                .expect("retained owner has a capture");
        }
    }
    let finalizers = unsafe { &mut *roots.weak_finalizers.data_ptr() };
    for finalizer in finalizers.iter_mut() {
        // SAFETY: only reachable finalizers survived `sweep`.
        unsafe { finalizer.owner.forward() };
    }
    // SAFETY: exclusive STW forwarding pass with all mutators parked.
    unsafe {
        roots.promises.for_each_shard_stopped_mut(|promises| {
            for entry in promises.values_mut() {
                if let Some(ref mut owner) = entry.owner {
                    // SAFETY: only reachable owners remain `Some` after `sweep`.
                    owner.forward();
                }
                if entry.traced
                    && entry.waiters.is_empty()
                    && entry.all_waiters.is_empty()
                    && let crate::futures::PromiseState::Completed(ref mut val) = entry.state
                {
                    // SAFETY: exclusive STW forwarding pass updates retained slot in place.
                    visitor(NessaSlot::from_raw_ptr(
                        (val as *mut TaggedValue).cast::<u64>(),
                    ));
                }
            }
        });
    }
}

/// Weak tracing writes forwarded addresses synchronously through mutable slots.
/// Slots cannot escape this visit; only heap objects are queued for later work.
pub(crate) fn visit_stack_slots(context: &mut StackContext, visitor: &mut dyn FnMut(NessaSlot)) {
    context.visit_slots_mut(|value| {
        // SAFETY: mutable access to an aligned transparent u64 slot is exclusive
        // during STW. The GC visitor completes load/store before returning.
        visitor(unsafe { NessaSlot::from_raw_ptr((value as *mut TaggedValue).cast::<u64>()) });
    });
}
