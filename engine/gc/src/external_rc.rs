//! Off-heap reference-counted byte buffers paired with thin 3-word Immix GC shells
//! ([`PayloadRole::ExternalRc`]).
//!
//! ## Architecture
//!
//! Large byte-oriented values (`String` and `Bytes` backing buffers exceeding
//! [`INLINE_BYTES_THRESHOLD`]) allocate a compact 40-byte shell (`16B ObjectHeader`
//! followed by `[raw_ptr: u64, offset: u64, len: u64]`) in Immix while storing raw
//! bytes in an off-heap atomically reference-counted [`ExternalRcHandle`].
//!
//! - **Zero-copy slicing & sharing**: Multiple `ExternalRc` shells (e.g. `String`
//!   slices) or host/FFI clones share one [`ExternalRcHandle`] via atomic `refs`,
//!   without GC pinning or copying megabytes of text/binary data.
//! - **Moving-GC & continuation safety**: Managed code manipulates ordinary heap
//!   `TaggedValue` pointers to the 40-byte Immix shell, remaining 100% compatible
//!   with register copies, moving Immix defragmentation, and multi-shot
//!   delimited continuations (`k.clone()`).
//! - **Deterministic RC + STW sweep**: Each shell owns one strong count registered
//!   in [`EXTERNAL_RC_REGISTRY`] with a [`WeakObject`]. During every STW GC weak
//!   closure, unreachable shells release their strong count; when `refs` reaches
//!   zero, the off-heap allocation is freed immediately to the OS allocator.
//! - **External memory pressure accounting**: Newly allocated off-heap bytes
//!   accumulate in [`EXTERNAL_UNCOLLECTED_BYTES`]; crossing the configured
//!   threshold sets [`GC_FLAGS.gc_requested`] so the next instruction safepoint
//!   reclaims dead shells promptly even when 40-byte Immix shells barely move the
//!   managed heap cursor.

use std::alloc::{Layout, alloc, alloc_zeroed, dealloc};
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use type_pool::Intrinsic;

use crate::{GC_FLAGS, ObjectHeader, PayloadRole, WeakObject};

/// Maximum byte capacity of a single [`ExternalRcHandle`] allocation (`1 GiB`).
pub const MAX_EXTERNAL_RC_BYTES: usize = 1 << 30;

/// Payloads at or below this byte count (`8,168 B`, fitting inside the `8 KiB`
/// `LARGE_OBJECT_THRESHOLD` together with `16B ObjectHeader + 8B length/share_count`)
/// stay inline in thread-local Immix bump blocks; larger payloads use `ExternalRc`.
pub const INLINE_BYTES_THRESHOLD: usize = 8192 - ObjectHeader::SIZE - 8;

/// Number of `u64` payload words in every [`PayloadRole::ExternalRc`] shell:
/// `[raw_inner_ptr: u64, offset: u64, byte_len: u64]`.
pub const EXTERNAL_RC_SHELL_WORDS: u16 = 3;

const DEFAULT_PRESSURE_THRESHOLD: usize = 32 * 1024 * 1024;
const MIN_PRESSURE_THRESHOLD: usize = 2 * 1024 * 1024;
const MAX_PRESSURE_THRESHOLD: usize = 256 * 1024 * 1024;

static EXTERNAL_LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static EXTERNAL_UNCOLLECTED_BYTES: AtomicUsize = AtomicUsize::new(0);
static EXTERNAL_PRESSURE_THRESHOLD: AtomicUsize = AtomicUsize::new(DEFAULT_PRESSURE_THRESHOLD);
static EXTERNAL_GC_NEEDED: AtomicBool = AtomicBool::new(false);

#[repr(C, align(8))]
struct ExternalRcHeader {
    refs: AtomicUsize,
    share_count: AtomicU64,
    capacity: usize,
    _pad: usize,
}

const HEADER_BYTES: usize = std::mem::size_of::<ExternalRcHeader>();

#[inline]
fn buffer_layout(capacity: usize) -> Option<Layout> {
    if capacity == 0 || capacity > MAX_EXTERNAL_RC_BYTES {
        return None;
    }
    let total = HEADER_BYTES.checked_add(capacity)?;
    Layout::from_size_align(total, 8).ok()
}

/// Strong reference-counted handle to an off-heap byte buffer.
///
/// Cloning increments the atomic strong count in $O(1)$; dropping the last
/// handle (whether from a host guard or an unreachable GC `ExternalRc` shell)
/// immediately frees the backing OS memory and decrements [`external_live_bytes`].
pub struct ExternalRcHandle {
    ptr: NonNull<ExternalRcHeader>,
}

// SAFETY: `ExternalRcHeader` uses atomic reference and COW share counts; raw byte
// mutations in `Bytes` enforce exclusive ownership (`share_count == 1`) before writing.
unsafe impl Send for ExternalRcHandle {}
// SAFETY: Shared references only expose atomic metadata or immutable/synchronized bytes.
unsafe impl Sync for ExternalRcHandle {}

impl ExternalRcHandle {
    fn alloc_raw(capacity: usize, zeroed: bool) -> Option<Self> {
        let layout = buffer_layout(capacity)?;
        // SAFETY: `layout` has non-zero size (`HEADER_BYTES + capacity >= 33`) and align 8.
        // Returning `None` on null avoids process abort on OS allocation failure.
        let raw = unsafe {
            if zeroed {
                alloc_zeroed(layout)
            } else {
                alloc(layout)
            }
        };
        let hdr_ptr = NonNull::new(raw.cast::<ExternalRcHeader>())?;
        // SAFETY: `hdr_ptr` points to freshly allocated, 8-byte aligned writable memory.
        unsafe {
            hdr_ptr.as_ptr().write(ExternalRcHeader {
                refs: AtomicUsize::new(1),
                share_count: AtomicU64::new(1),
                capacity,
                _pad: 0,
            });
        }
        EXTERNAL_LIVE_BYTES.fetch_add(capacity, Ordering::Relaxed);
        let uncollected = EXTERNAL_UNCOLLECTED_BYTES
            .fetch_add(capacity, Ordering::Relaxed)
            .saturating_add(capacity);
        if uncollected >= EXTERNAL_PRESSURE_THRESHOLD.load(Ordering::Relaxed) {
            EXTERNAL_GC_NEEDED.store(true, Ordering::Release);
            GC_FLAGS.gc_requested.store(true, Ordering::Release);
        }
        Some(Self { ptr: hdr_ptr })
    }

    /// Allocate a zero-initialized off-heap buffer of `capacity` bytes (`1..=MAX_EXTERNAL_RC_BYTES`).
    pub fn new_zeroed(capacity: usize) -> Option<Self> {
        Self::alloc_raw(capacity, true)
    }

    /// Allocate an uninitialized off-heap buffer of `capacity` bytes for immediate overwrite.
    ///
    /// # Safety
    /// Caller must initialize all bytes that will ever be read before publishing any shell.
    pub unsafe fn new_uninit(capacity: usize) -> Option<Self> {
        Self::alloc_raw(capacity, false)
    }

    /// Allocate an off-heap buffer initialized from `bytes`.
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        let handle = Self::alloc_raw(bytes.len(), false)?;
        // SAFETY: `handle.data_mut_ptr()` owns `bytes.len()` writable disjoint bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), handle.data_mut_ptr(), bytes.len());
        }
        Some(handle)
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        // SAFETY: `self.ptr` is valid while `refs >= 1`.
        unsafe { self.ptr.as_ref().capacity }
    }

    #[inline]
    pub fn data_ptr(&self) -> *const u8 {
        // SAFETY: `HEADER_BYTES` bytes of `ExternalRcHeader` immediately precede the data tail.
        unsafe { self.ptr.as_ptr().cast::<u8>().add(HEADER_BYTES) }
    }

    #[inline]
    pub fn data_mut_ptr(&self) -> *mut u8 {
        // SAFETY: `HEADER_BYTES` bytes of `ExternalRcHeader` immediately precede the data tail.
        unsafe { self.ptr.as_ptr().cast::<u8>().add(HEADER_BYTES) }
    }

    #[inline]
    pub fn share_count_atom(&self) -> &AtomicU64 {
        // SAFETY: `self.ptr` is live for the lifetime of `&self`.
        unsafe { &self.ptr.as_ref().share_count }
    }

    #[inline]
    fn into_raw(self) -> NonNull<ExternalRcHeader> {
        let ptr = self.ptr;
        std::mem::forget(self);
        ptr
    }

    #[inline]
    unsafe fn clone_from_raw(ptr: NonNull<ExternalRcHeader>) -> Self {
        // SAFETY: Caller guarantees `ptr` has at least one live strong reference.
        let prev = unsafe { ptr.as_ref().refs.fetch_add(1, Ordering::Relaxed) };
        assert!(
            prev > 0 && prev < usize::MAX,
            "ExternalRc refcount overflow"
        );
        Self { ptr }
    }

    #[inline]
    unsafe fn drop_raw(ptr: NonNull<ExternalRcHeader>) {
        // SAFETY: Caller releases one owned strong reference to `ptr`.
        let prev = unsafe { ptr.as_ref().refs.fetch_sub(1, Ordering::Release) };
        debug_assert!(prev >= 1, "ExternalRc double-free");
        if prev == 1 {
            std::sync::atomic::fence(Ordering::Acquire);
            // SAFETY: Last reference dropped; `capacity` is immutable and matches `alloc_raw`.
            unsafe {
                let cap = ptr.as_ref().capacity;
                EXTERNAL_LIVE_BYTES.fetch_sub(cap, Ordering::Relaxed);
                let layout = buffer_layout(cap).expect("allocated capacity has valid layout");
                dealloc(ptr.as_ptr().cast::<u8>(), layout);
            }
        }
    }
}

impl Clone for ExternalRcHandle {
    #[inline]
    fn clone(&self) -> Self {
        // SAFETY: `self` holds a live strong reference.
        unsafe { Self::clone_from_raw(self.ptr) }
    }
}

impl Drop for ExternalRcHandle {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: `self` owns one strong reference.
        unsafe { Self::drop_raw(self.ptr) };
    }
}

struct RegistryEntry {
    shell: WeakObject,
    raw_handle: NonNull<ExternalRcHeader>,
    reachable: bool,
}

// SAFETY: `WeakObject` and `NonNull<ExternalRcHeader>` are synchronized via `REGISTRY` mutex
// during mutator registration and accessed exclusively under the STW GC barrier.
unsafe impl Send for RegistryEntry {}

static REGISTRY: Mutex<Vec<RegistryEntry>> = Mutex::new(Vec::new());

/// Configure the external allocation threshold that requests a safepoint GC.
pub(crate) fn init_pressure_threshold(heap_size: usize) {
    let threshold = (heap_size / 2).clamp(MIN_PRESSURE_THRESHOLD, MAX_PRESSURE_THRESHOLD);
    EXTERNAL_PRESSURE_THRESHOLD.store(threshold, Ordering::Relaxed);
}

#[inline]
pub(crate) fn take_gc_needed() -> bool {
    EXTERNAL_GC_NEEDED.swap(false, Ordering::AcqRel)
}

/// Total off-heap bytes currently alive across all [`ExternalRcHandle`] allocations.
pub fn external_live_bytes() -> usize {
    EXTERNAL_LIVE_BYTES.load(Ordering::Relaxed)
}

/// Number of GC-managed [`PayloadRole::ExternalRc`] shells currently tracked in the weak registry.
pub fn external_shell_count() -> usize {
    REGISTRY.lock().unwrap().len()
}

/// Register a newly initialized [`PayloadRole::ExternalRc`] shell, transferring
/// ownership of `handle`'s strong reference to the GC weak registry.
///
/// # Safety
/// `shell_payload` must point to an initialized 3-word `ExternalRc` heap payload
/// under an active mutator session.
pub(crate) unsafe fn register_shell(shell_payload: *mut u8, handle: ExternalRcHandle) {
    let raw_handle = handle.into_raw();
    // SAFETY: `shell_payload` is a non-null aligned payload of an initialized managed object;
    // unreachable entries are removed in `sweep_stw` before mutators resume.
    let shell = unsafe { WeakObject::new(shell_payload) };
    let words = shell_payload.cast::<u64>();
    // SAFETY: Caller allocated 3 payload words and holds exclusive ownership before publication.
    unsafe {
        words.write(raw_handle.as_ptr() as usize as u64);
    }
    REGISTRY.lock().unwrap().push(RegistryEntry {
        shell,
        raw_handle,
        reachable: false,
    });
}

/// Reset reachability flags at the start of a stop-the-world GC cycle.
///
/// # Safety
/// Must be called only while all mutators are stopped at the STW barrier.
pub(crate) unsafe fn prepare_stw() {
    let mut reg = REGISTRY.lock().unwrap();
    for entry in reg.iter_mut() {
        entry.reachable = false;
    }
}

/// Mark and forward reachable `ExternalRc` shells after transitive root closure.
///
/// # Safety
/// Must be called during STW weak-reference processing after closure completion.
pub(crate) unsafe fn trace_stw() {
    let mut reg = REGISTRY.lock().unwrap();
    for entry in reg.iter_mut() {
        if !entry.reachable {
            // SAFETY: Unswept `entry.shell` holds a valid pre-forwarding managed address under STW.
            if unsafe { entry.shell.is_reachable() } {
                unsafe { entry.shell.forward() };
                entry.reachable = true;
            }
        }
    }
}

/// Reclaim all unreachable `ExternalRc` shells at the weak fixed point, dropping
/// their strong reference counts and resetting uncollected external byte pressure.
///
/// # Safety
/// Must be called during STW weak sweep after the transitive closure fixed point.
pub(crate) unsafe fn sweep_stw() {
    let mut dead: Vec<NonNull<ExternalRcHeader>> = Vec::new();
    {
        let mut reg = REGISTRY.lock().unwrap();
        reg.retain(|entry| {
            if entry.reachable {
                true
            } else {
                dead.push(entry.raw_handle);
                false
            }
        });
    }
    EXTERNAL_UNCOLLECTED_BYTES.store(0, Ordering::Relaxed);
    EXTERNAL_GC_NEEDED.store(false, Ordering::Release);
    for raw in dead {
        // SAFETY: Each dead shell owned exactly one strong reference transferred in `register_shell`.
        unsafe { ExternalRcHandle::drop_raw(raw) };
    }
}

/// Forward surviving `ExternalRc` shell addresses in two-phase compacting plans.
///
/// # Safety
/// Must be called during STW `forward_weak_refs` after unreachable shells were swept.
pub(crate) unsafe fn forward_stw() {
    let mut reg = REGISTRY.lock().unwrap();
    for entry in reg.iter_mut() {
        // SAFETY: Only reachable shells remain in `REGISTRY` after `sweep_stw`.
        unsafe { entry.shell.forward() };
    }
}

/// Inspect a validated [`PayloadRole::ExternalRc`] payload pointer, returning a cloned
/// [`ExternalRcHandle`] together with `(offset, byte_len)`.
///
/// # Safety
/// `payload_ptr` must point to a live, rooted heap payload within an active mutator operation.
pub unsafe fn external_rc_clone_view(
    payload_ptr: *const u8,
) -> Option<(ExternalRcHandle, usize, usize)> {
    // SAFETY: Caller guarantees `payload_ptr` is a live managed object payload.
    let header = unsafe { ObjectHeader::from_payload_ptr(payload_ptr) };
    if header.payload_role() != Some(PayloadRole::ExternalRc)
        || header.payload_words() != EXTERNAL_RC_SHELL_WORDS as usize
    {
        return None;
    }
    let words = payload_ptr.cast::<u64>();
    // SAFETY: `payload_words() == 3` guarantees three aligned `u64` words.
    let (raw_addr, offset, len) = unsafe {
        (
            words.read() as usize,
            words.add(1).read() as usize,
            words.add(2).read() as usize,
        )
    };
    let hdr_ptr = NonNull::new(raw_addr as *mut ExternalRcHeader)?;
    // SAFETY: Live shell in `REGISTRY` holds `refs >= 1` for the duration of the mutator operation.
    let cap = unsafe { hdr_ptr.as_ref().capacity };
    let end = offset.checked_add(len)?;
    if end > cap {
        return None;
    }
    // SAFETY: Verified live shell owns a valid strong reference.
    let handle = unsafe { ExternalRcHandle::clone_from_raw(hdr_ptr) };
    Some((handle, offset, len))
}

/// Borrow the byte slice of a [`PayloadRole::ExternalRc`] shell without cloning its refcount.
///
/// # Safety
/// `payload_ptr` must be a live, rooted heap payload within an active mutator operation, and
/// no concurrent writer may mutate the slice while borrowed.
pub unsafe fn external_rc_bytes<'a>(payload_ptr: *const u8) -> Option<&'a [u8]> {
    // SAFETY: Caller supplies a live managed payload pointer.
    let header = unsafe { ObjectHeader::from_payload_ptr(payload_ptr) };
    if header.payload_role() != Some(PayloadRole::ExternalRc)
        || header.payload_words() != EXTERNAL_RC_SHELL_WORDS as usize
    {
        return None;
    }
    let words = payload_ptr.cast::<u64>();
    // SAFETY: `payload_words() == 3` guarantees three initialized words.
    let (raw_addr, offset, len) = unsafe {
        (
            words.read() as usize,
            words.add(1).read() as usize,
            words.add(2).read() as usize,
        )
    };
    let hdr_ptr = NonNull::new(raw_addr as *mut ExternalRcHeader)?;
    // SAFETY: Shell owns a strong reference keeping `hdr_ptr` alive across this mutator operation.
    let cap = unsafe { hdr_ptr.as_ref().capacity };
    let end = offset.checked_add(len)?;
    if end > cap {
        return None;
    }
    // SAFETY: `offset + len <= cap` lies within the `HEADER_BYTES`-prefixed data allocation.
    let data = unsafe { hdr_ptr.as_ptr().cast::<u8>().add(HEADER_BYTES + offset) };
    Some(unsafe { std::slice::from_raw_parts(data, len) })
}

/// Access `(share_count, data_mut_ptr, capacity)` of a [`PayloadRole::ExternalRc`] backing buffer
/// used by `Bytes`.
///
/// # Safety
/// `payload_ptr` must be a live, rooted `ExternalRc` shell within an active mutator operation.
pub unsafe fn external_rc_backing_parts(
    payload_ptr: *const u8,
) -> Option<(&'static AtomicU64, *mut u8, usize)> {
    // SAFETY: Caller supplies a live managed payload pointer.
    let header = unsafe { ObjectHeader::from_payload_ptr(payload_ptr) };
    if header.payload_role() != Some(PayloadRole::ExternalRc)
        || header.payload_words() != EXTERNAL_RC_SHELL_WORDS as usize
    {
        return None;
    }
    let words = payload_ptr.cast::<u64>();
    // SAFETY: `payload_words() == 3` guarantees three initialized words.
    let (raw_addr, offset, len) = unsafe {
        (
            words.read() as usize,
            words.add(1).read() as usize,
            words.add(2).read() as usize,
        )
    };
    let hdr_ptr = NonNull::new(raw_addr as *mut ExternalRcHeader)?;
    // SAFETY: Live shell keeps `hdr_ptr` valid for the duration of the mutator operation.
    let hdr_ref: &'static ExternalRcHeader = unsafe { hdr_ptr.as_ref() };
    if offset != 0 || len != hdr_ref.capacity {
        return None;
    }
    // SAFETY: `HEADER_BYTES` precedes `hdr_ref.capacity` writable bytes.
    let data_ptr = unsafe { hdr_ptr.as_ptr().cast::<u8>().add(HEADER_BYTES) };
    Some((&hdr_ref.share_count, data_ptr, hdr_ref.capacity))
}

/// Unified zero-copy byte reader for heap `String` objects, supporting both inline
/// [`PayloadRole::Ordinary`] strings and off-heap [`PayloadRole::ExternalRc`] strings.
///
/// # Safety
/// `payload_ptr` must point to a live, rooted heap payload within an active mutator operation
/// with no intervening GC allocation while the returned slice is held.
pub unsafe fn heap_string_bytes<'a>(payload_ptr: *const u8) -> Option<&'a [u8]> {
    // SAFETY: Caller supplies a live managed payload pointer.
    let header = unsafe { ObjectHeader::from_payload_ptr(payload_ptr) };
    if header.type_index != Intrinsic::Str.type_index() {
        return None;
    }
    match header.payload_role() {
        Some(PayloadRole::Ordinary) => {
            let words = header.payload_words();
            if words == 0 {
                return None;
            }
            // SAFETY: `words >= 1` guarantees an initialized 8-byte length prefix.
            let len = unsafe { *(payload_ptr as *const u64) } as usize;
            if len > words * 8 - 8 {
                return None;
            }
            // SAFETY: `8 + len <= words * 8` lies within this immutable `Str` payload.
            Some(unsafe { std::slice::from_raw_parts(payload_ptr.add(8), len) })
        }
        Some(PayloadRole::ExternalRc) => {
            // SAFETY: `payload_ptr` is a live `ExternalRc` `Str` shell.
            unsafe { external_rc_bytes(payload_ptr) }
        }
        _ => None,
    }
}
