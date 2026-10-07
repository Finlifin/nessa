//! Lifetime-bound root registrations for independently owned VM states.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::NessaSlot;

/// Enumerate slots in a registered, stopped VM state.
///
/// The context must remain valid for the duration of the callback. Callbacks
/// must not register or unregister roots, because scanning holds the registry
/// lock to keep every context alive until enumeration completes.
pub type RootScanCallback = unsafe fn(*const (), &mut dyn FnMut(NessaSlot));

/// Callbacks for out-of-line roots whose owner is weakly referenced.
///
/// All callbacks run with mutators stopped and the registration lock held.
/// `prepare` starts a collection; `trace` activates newly reachable owners and
/// returns whether it exposed new edges. `sweep` runs only after that closure
/// reaches a fixed point. `forward` updates retained edges in two-phase plans.
/// Callbacks must not register/unregister roots or retain slot borrows.
#[derive(Clone, Copy)]
pub struct ConditionalRootCallbacks {
    pub prepare: unsafe fn(*const ()),
    pub trace: unsafe fn(*const (), &mut dyn FnMut(NessaSlot)) -> bool,
    pub sweep: unsafe fn(*const ()),
    pub forward: RootScanCallback,
}

struct RootSource {
    scan: RootScanCallback,
    conditional: Option<ConditionalRootCallbacks>,
}

static ROOTS: Mutex<BTreeMap<usize, RootSource>> = Mutex::new(BTreeMap::new());

/// Removes a VM's root source before its backing state is destroyed.
pub struct RootRegistration {
    context: usize,
}

impl RootRegistration {
    /// Register a stable VM state, including while its engine is parked.
    ///
    /// # Safety
    /// `context` must point to the state expected by `scan` at a stable address
    /// until this registration is dropped. The registration must be dropped
    /// before the state. Mutations must be stopped during collection.
    pub unsafe fn new(context: *const (), scan: RootScanCallback) -> Self {
        // SAFETY: this constructor has the same lifetime and barrier contract.
        unsafe { Self::register(context, scan, None) }
    }

    /// Register a VM with conditional, out-of-line roots.
    ///
    /// # Safety
    /// The requirements of `new` apply to every callback. Conditional callbacks
    /// may mutate their state exclusively during stop-the-world collection.
    /// The backing state must permit that access through interior mutability;
    /// no mutator may retain an overlapping borrow during callbacks.
    pub unsafe fn new_with_conditional_roots(
        context: *const (),
        scan: RootScanCallback,
        conditional: ConditionalRootCallbacks,
    ) -> Self {
        // SAFETY: the caller supplies the callback and context guarantees.
        unsafe { Self::register(context, scan, Some(conditional)) }
    }

    unsafe fn register(
        context: *const (),
        scan: RootScanCallback,
        conditional: Option<ConditionalRootCallbacks>,
    ) -> Self {
        let context = context as usize;
        assert_ne!(context, 0, "root context must be non-null");
        let previous = ROOTS
            .lock()
            .unwrap()
            .insert(context, RootSource { scan, conditional });
        assert!(previous.is_none(), "root context registered twice");
        Self { context }
    }
}

impl Drop for RootRegistration {
    fn drop(&mut self) {
        ROOTS.lock().unwrap().remove(&self.context);
    }
}

/// # Safety
/// All registered VM states must be stopped for the entire visit.
pub(crate) unsafe fn scan_roots(visitor: &mut dyn FnMut(NessaSlot)) {
    let roots = ROOTS.lock().unwrap();
    for (&context, source) in roots.iter() {
        // SAFETY: registration ensures address/lifetime stability, the caller
        // supplies the collection barrier, and the lock prevents removal.
        unsafe { (source.scan)(context as *const (), visitor) };
    }
}

/// # Safety
/// All registered states must remain stopped throughout these callbacks.
pub(crate) unsafe fn prepare_conditional_roots() {
    let roots = ROOTS.lock().unwrap();
    for (&context, source) in roots.iter() {
        if let Some(callbacks) = source.conditional {
            // SAFETY: the registry lock preserves the stopped backing state.
            unsafe { (callbacks.prepare)(context as *const ()) };
        }
    }
}

/// # Safety
/// Called after the current transitive closure with exclusive stopped state.
pub(crate) unsafe fn trace_conditional_roots(visitor: &mut dyn FnMut(NessaSlot)) -> bool {
    let roots = ROOTS.lock().unwrap();
    let mut changed = false;
    for (&context, source) in roots.iter() {
        if let Some(callbacks) = source.conditional {
            // SAFETY: the registry lock preserves the stopped backing state.
            changed |= unsafe { (callbacks.trace)(context as *const (), visitor) };
        }
    }
    changed
}

/// # Safety
/// No conditional owner exposed new edges in the last completed closure pass.
pub(crate) unsafe fn sweep_conditional_roots() {
    let roots = ROOTS.lock().unwrap();
    for (&context, source) in roots.iter() {
        if let Some(callbacks) = source.conditional {
            // SAFETY: the registry lock preserves the stopped backing state.
            unsafe { (callbacks.sweep)(context as *const ()) };
        }
    }
}

/// # Safety
/// Called in a stopped forwarding phase after unreachable owners were swept.
pub(crate) unsafe fn forward_conditional_roots(visitor: &mut dyn FnMut(NessaSlot)) {
    let roots = ROOTS.lock().unwrap();
    for (&context, source) in roots.iter() {
        if let Some(callbacks) = source.conditional {
            // SAFETY: the registry lock preserves the stopped backing state.
            unsafe { (callbacks.forward)(context as *const (), visitor) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn scan_word(context: *const (), visitor: &mut dyn FnMut(NessaSlot)) {
        // SAFETY: test registrations own a boxed aligned u64 until removal.
        visitor(unsafe { NessaSlot::from_raw_ptr(context.cast::<u64>()) });
    }

    #[test]
    fn registrations_survive_owner_moves_and_remove_independently() {
        struct Owner {
            // Drop the registration before freeing the context.
            registration: RootRegistration,
            word: Box<u64>,
        }
        fn owner(value: u64) -> Owner {
            let word = Box::new(value);
            // SAFETY: Box retains its address when Owner moves; field order
            // removes the registration before the word is freed.
            let registration =
                unsafe { RootRegistration::new((&*word as *const u64).cast(), scan_word) };
            Owner { registration, word }
        }
        let first = owner(8);
        let second = owner(16);
        let first_address = first.registration.context;
        let second_address = second.registration.context;
        let moved = vec![first, second];
        let mut slots = Vec::new();
        // SAFETY: both owners are idle and their boxes remain live.
        unsafe { scan_roots(&mut |slot| slots.push(slot)) };
        assert!(slots.contains(&unsafe { NessaSlot::from_raw_ptr(first_address as *const u64) }));
        assert!(slots.contains(&unsafe { NessaSlot::from_raw_ptr(second_address as *const u64) }));
        assert_eq!(*moved[1].word, 16);
        let mut moved = moved;
        drop(moved.remove(0));
        assert!(!ROOTS.lock().unwrap().contains_key(&first_address));
        assert!(ROOTS.lock().unwrap().contains_key(&second_address));
        drop(moved);
        assert!(!ROOTS.lock().unwrap().contains_key(&second_address));
    }
}
