//! Non-owning handles used only by the VM's stopped weak-reference protocol.

use mmtk::util::{Address, ObjectReference};

/// A managed object reference that is deliberately absent from the root set.
/// The owner must remove it before a collection frees the referenced object.
pub struct WeakObject(ObjectReference);

impl WeakObject {
    /// # Safety
    /// `payload` must be a non-null aligned payload of an initialized managed
    /// object. The caller must process this handle during every collection and
    /// remove it if unreachable, before mutators resume.
    pub unsafe fn new(payload: *const u8) -> Self {
        // SAFETY: the caller supplies a valid initialized managed payload.
        let address = unsafe { Address::from_usize(payload as usize) };
        Self(ObjectReference::from_raw_address(address).expect("managed payload is non-null"))
    }

    /// # Safety
    /// Query only during weak processing after the current tracing closure.
    pub unsafe fn is_reachable(&self) -> bool {
        self.0.is_reachable()
    }

    /// # Safety
    /// Update only during weak processing/forwarding of a reachable object,
    /// before any collection can reclaim its old address.
    pub unsafe fn forward(&mut self) {
        self.0 = self.0.get_forwarded_object().unwrap_or(self.0);
    }
}
