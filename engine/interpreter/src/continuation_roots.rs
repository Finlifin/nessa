//! Conditional ownership of detached language continuation stacks.
//!
//! Raw ABI captures remain strong roots. Language templates become roots only
//! after their weak wrapper is reached; repeated weak passes compute chains of
//! owners without retaining isolated cycles.

use gc::{ConditionalRootCallbacks, NessaSlot, WeakObject};
use runtime::{ContinuationId, StackContext, StackError, TaggedValue, TaskId};
use type_pool::TypeIndex;

use crate::{Vm, VmError, VmRoots};

#[derive(Clone, Copy)]
pub(super) struct ContinuationInputContract {
    pub input: TypeIndex,
    pub scope: Option<u32>,
}

pub(super) struct ManagedContinuation {
    task: TaskId,
    owner: WeakObject,
    traced: bool,
    contract: ContinuationInputContract,
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
        let continuation = self.roots.continuations.get(&handle).ok_or_else(invalid)?;
        if continuation.task != task
            || !self
                .roots
                .scheduler
                .get_task(task)
                .is_some_and(|task| task.stacks.has_capture(handle))
        {
            return Err(invalid());
        }
        Ok(continuation.contract)
    }

    pub(super) fn own_continuation(
        &mut self,
        task: TaskId,
        handle: ContinuationId,
        value: TaggedValue,
        contract: ContinuationInputContract,
    ) {
        let roots = &mut *self.roots;
        roots
            .scheduler
            .get_task_mut(task)
            .expect("continuation belongs to a live task")
            .stacks
            .promote_language_capture(handle)
            .expect("only a live pending capture is published");
        // SAFETY: the wrapper is initialized, and this registry removes dead
        // owners at every weak fixed point before mutators resume.
        let owner = unsafe { WeakObject::new(value.as_heap_ptr().expect("continuation wrapper")) };
        let previous = roots.continuations.insert(
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
}

unsafe fn prepare(context: *const ()) {
    // SAFETY: the registered UnsafeCell is stable; STW gives exclusive access,
    // and preparation runs before ordinary root scanning starts.
    let roots = unsafe { &mut *context.cast::<VmRoots>().cast_mut() };
    for continuation in roots.continuations.values_mut() {
        continuation.traced = false;
    }
}

unsafe fn trace(context: *const (), visitor: &mut dyn FnMut(NessaSlot)) -> bool {
    // SAFETY: weak processing runs after all ordinary tracing work under STW;
    // the registration lock keeps this UnsafeCell backing state alive.
    let roots = unsafe { &mut *context.cast::<VmRoots>().cast_mut() };
    let scheduler = &mut roots.scheduler;
    let mut changed = false;
    roots.continuations.retain(|&handle, continuation| {
        let Some(task) = scheduler.get_task_mut(continuation.task) else {
            return false;
        };
        // Consumed/finished handles must be removed before consulting a weak
        // pointer that no longer owns any captured stack.
        if !task.stacks.has_capture(handle) {
            return false;
        }
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
    changed
}

unsafe fn sweep(context: *const ()) {
    // SAFETY: no new owner was activated in the last completed closure pass;
    // no tracing work retains slots in the chains discarded below.
    let roots = unsafe { &mut *context.cast::<VmRoots>().cast_mut() };
    let scheduler = &mut roots.scheduler;
    roots.continuations.retain(|&handle, continuation| {
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
}

unsafe fn forward(context: *const (), visitor: &mut dyn FnMut(NessaSlot)) {
    // SAFETY: the collector's forwarding phase follows weak closure/sweep,
    // retaining only reachable owners, with exclusive stopped state.
    let roots = unsafe { &mut *context.cast::<VmRoots>().cast_mut() };
    for (&handle, continuation) in &mut roots.continuations {
        unsafe { continuation.owner.forward() };
        if let Some(task) = roots.scheduler.get_task_mut(continuation.task)
            && task.stacks.has_capture(handle)
        {
            task.stacks
                .visit_captured_contexts_mut(handle, |stack| visit_stack_slots(stack, visitor))
                .expect("retained owner has a capture");
        }
    }
}

/// Weak tracing writes forwarded addresses synchronously through mutable slots.
/// Slots cannot escape this visit; only heap objects are queued for later work.
fn visit_stack_slots(context: &mut StackContext, visitor: &mut dyn FnMut(NessaSlot)) {
    let mut visit = |value: &mut TaggedValue| {
        // SAFETY: mutable access to an aligned transparent u64 slot is exclusive
        // during STW. The GC visitor completes load/store before returning.
        visitor(unsafe { NessaSlot::from_raw_ptr((value as *mut TaggedValue).cast::<u64>()) });
    };
    if let Some(state) = &mut context.display_state {
        for value in &mut state.ancestors {
            visit(value);
        }
    }
    if let Some(value) = &mut context.entry_closure_env {
        visit(value);
    }
    for handler in &mut context.handler_stack {
        if let Some(value) = &mut handler.closure_env {
            visit(value);
        }
    }
    for value in &mut context.local_slots {
        visit(value);
    }
    for value in &mut context.registers.regs {
        visit(value);
    }
    for frame in &mut context.call_stack {
        if let Some(state) = &mut frame.display_state {
            for value in &mut state.ancestors {
                visit(value);
            }
        }
        for value in &mut frame.local_slots {
            visit(value);
        }
        for (_, value) in &mut frame.saved_regs {
            visit(value);
        }
        if let Some(value) = &mut frame.closure_env {
            visit(value);
        }
    }
}
