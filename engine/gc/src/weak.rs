//! Non-owning handles used only by the VM's stopped weak-reference protocol.

#[cfg(unix)]
use mmtk::util::{Address, ObjectReference};

#[cfg(unix)]
pub struct WeakObject(ObjectReference);

#[cfg(windows)]
pub struct WeakObject(*const u8);

impl WeakObject {
    /// # Safety
    /// `payload` must be a non-null aligned payload of an initialized managed
    /// object. The caller must process this handle during every collection and
    /// remove it if unreachable, before mutators resume.
    pub unsafe fn new(payload: *const u8) -> Self {
        #[cfg(unix)]
        {
            // SAFETY: the caller supplies a valid initialized managed payload.
            let address = unsafe { Address::from_usize(payload as usize) };
            Self(ObjectReference::from_raw_address(address).expect("managed payload is non-null"))
        }
        #[cfg(windows)]
        {
            assert!(!payload.is_null(), "managed payload is non-null");
            Self(payload)
        }
    }

    /// # Safety
    /// Query only during weak processing after the current tracing closure.
    pub unsafe fn is_reachable(&self) -> bool {
        #[cfg(unix)]
        {
            self.0.is_reachable()
        }
        #[cfg(windows)]
        {
            // SAFETY: Queried under STW during `sweep_conditional_roots` before unmarked sweep.
            let header = unsafe { crate::ObjectHeader::from_payload_ptr(self.0) };
            (header.gc_meta & crate::win_gc::MARK_BIT) != 0
        }
    }

    /// # Safety
    /// Update only during weak processing/forwarding of a reachable object,
    /// before any collection can reclaim its old address.
    pub unsafe fn forward(&mut self) {
        #[cfg(unix)]
        {
            self.0 = self.0.get_forwarded_object().unwrap_or(self.0);
        }
        #[cfg(windows)]
        {
            // Mark-sweep retains object addresses in place.
        }
    }
}
