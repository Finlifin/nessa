//! Task Stack Pool — mmap'd segmented stack management.
//!
//! Each Nessa stack segment gets an 8 MB virtual memory slot. A task may own
//! several segments, including detached continuation stacks.
//! Physical pages are demand-paged by the OS; only the first 8 KB is
//! pre-committed via `madvise(WILLNEED)`.
//!
//! The pool grows in segments that double in capacity (64 → 128 → 256 …).
//! Each segment is an independent `mmap` region with guard pages between
//! adjacent stack slots.
//!
//! # Architecture
//!
//! ```text
//! Segment 0 (64 slots × 8 MB = 512 MB VA)
//! ┌──────┬──────┬──────┬─ ─ ─ ─┬──────┐
//! │Slot 0│Slot 1│Slot 2│  ...  │Slot63│
//! └──────┴──────┴──────┴─ ─ ─ ─┴──────┘
//!
//! Segment 1 (128 slots × 8 MB = 1 GB VA)    ← mmap'd on demand
//! ┌──────┬──────┬─ ─ ─ ─┬───────┐
//! │Slot64│Slot65│  ...  │Slot191│
//! └──────┴──────┴─ ─ ─ ─┴───────┘
//!
//! Each slot:
//!   [ 8 MB − PAGE_SIZE usable stack ][ guard page (PROT_NONE) ]
//! ```

use std::ptr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Size of a single stack slot (8 MB).
const SLOT_SIZE: usize = 8 * 1024 * 1024;

/// Number of bytes pre-committed on allocation (8 KB).
const PRE_COMMIT_SIZE: usize = 8 * 1024;

/// Initial segment capacity (number of slots).
const INITIAL_SEGMENT_SLOTS: usize = 64;

// ---------------------------------------------------------------------------
// SlotId — unique identifier for an allocated stack slot
// ---------------------------------------------------------------------------

/// Identifies a stack slot within the pool.
///
/// Encodes the segment index and the slot offset within that segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SlotId {
    /// Segment index in the segment list.
    segment: u16,
    /// Slot offset within the segment.
    offset: u32,
}

impl SlotId {
    fn new(segment: usize, offset: usize) -> Self {
        Self {
            segment: segment as u16,
            offset: offset as u32,
        }
    }
}

// ---------------------------------------------------------------------------
// StackHandle — user-facing handle to an allocated stack
// ---------------------------------------------------------------------------

/// A handle to an allocated stack slot.
///
/// The stack grows downward from `top` toward `base`.
/// `guard` points to the guard page at the bottom of the slot.
#[derive(Debug)]
pub struct StackHandle {
    /// ID of this slot (used to return it to the pool).
    pub id: SlotId,
    /// Bottom of the usable stack region (just above the guard page).
    pub base: *mut u8,
    /// Top of the stack region (highest address; initial SP).
    pub top: *mut u8,
    /// Start of the guard page (PROT_NONE).
    pub guard: *mut u8,
}

impl StackHandle {
    /// Usable stack size in bytes.
    pub fn usable_size(&self) -> usize {
        self.top as usize - self.base as usize
    }
}

// SAFETY: StackHandle is a plain pointer bundle; only one task uses it at a time.
unsafe impl Send for StackHandle {}
unsafe impl Sync for StackHandle {}

// ---------------------------------------------------------------------------
// Segment — one mmap'd region holding N stack slots
// ---------------------------------------------------------------------------

struct Segment {
    /// Base address of the mmap'd region.
    base: *mut u8,
    /// Number of slots in this segment.
    slot_count: usize,
    /// Total mmap'd size in bytes.
    mmap_size: usize,
    /// Slot in the lock-free signal guard table.
    guard_slot: Option<usize>,
}

// SAFETY: Segment exclusively owns its mmap reservation, which has no thread
// affinity. StackPool only accesses segment metadata while holding its mutex;
// live StackHandles keep the owning pool alive through the runtime's OwnedStack.
// Moving the metadata does not move the mapping or dereference stack contents.
unsafe impl Send for Segment {}

impl Segment {
    /// Create a new segment with `slot_count` slots.
    fn new(slot_count: usize) -> Self {
        let mmap_size = slot_count * SLOT_SIZE;
        let base = unsafe {
            libc::mmap(
                ptr::null_mut(),
                mmap_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert!(
            base != libc::MAP_FAILED,
            "mmap failed for stack segment ({mmap_size} bytes)"
        );
        let base = base as *mut u8;

        // Set up guard pages at the bottom of every slot.
        let page_size = page_size();
        for i in 0..slot_count {
            let guard_addr = unsafe { base.add(i * SLOT_SIZE) };
            let rc = unsafe {
                libc::mprotect(guard_addr as *mut libc::c_void, page_size, libc::PROT_NONE)
            };
            assert!(rc == 0, "mprotect guard page failed for slot {i}");
        }

        // Advise the OS that most of the region won't be touched soon.
        unsafe {
            libc::madvise(base as *mut libc::c_void, mmap_size, libc::MADV_DONTNEED);
        }

        let guard_slot = register_guard_segment(base as usize, mmap_size);

        Self {
            base,
            slot_count,
            mmap_size,
            guard_slot,
        }
    }

    /// Get the memory layout for slot `offset` within this segment.
    fn slot_layout(&self, offset: usize) -> StackHandle {
        debug_assert!(offset < self.slot_count);
        let page_size = page_size();
        let slot_base = unsafe { self.base.add(offset * SLOT_SIZE) };
        let guard = slot_base;
        let base = unsafe { slot_base.add(page_size) }; // usable starts after guard
        let top = unsafe { slot_base.add(SLOT_SIZE) }; // top of slot

        StackHandle {
            id: SlotId::new(0, 0), // caller fills in correct id
            base,
            top,
            guard,
        }
    }
}

impl Drop for Segment {
    fn drop(&mut self) {
        unregister_guard_segment(self.guard_slot);
        unsafe {
            libc::munmap(self.base as *mut libc::c_void, self.mmap_size);
        }
    }
}

// ---------------------------------------------------------------------------
// StackPool — the main pool managing segments and free list
// ---------------------------------------------------------------------------

/// A pool of mmap'd task stacks organized as a segmented list.
///
/// Thread-safe: all operations are protected by an internal mutex.
pub struct StackPool {
    inner: Mutex<PoolInner>,
}

struct PoolInner {
    /// Allocated segments (grows by doubling capacity).
    segments: Vec<Segment>,
    /// Free slot identifiers ready for reuse.
    free_list: Vec<SlotId>,
    /// Total slots ever allocated across all segments.
    total_slots: usize,
    /// Currently in-use slot count.
    active_count: usize,
}

impl StackPool {
    /// Create a new StackPool with an initial segment.
    pub fn new() -> Self {
        let seg = Segment::new(INITIAL_SEGMENT_SLOTS);
        let mut free_list = Vec::with_capacity(INITIAL_SEGMENT_SLOTS);
        // Populate free list (all slots available).
        // Push in reverse so that slot 0 is popped first.
        for i in (0..INITIAL_SEGMENT_SLOTS).rev() {
            free_list.push(SlotId::new(0, i));
        }

        Self {
            inner: Mutex::new(PoolInner {
                segments: vec![seg],
                free_list,
                total_slots: INITIAL_SEGMENT_SLOTS,
                active_count: 0,
            }),
        }
    }

    /// Allocate a stack slot from the pool.
    ///
    /// Returns a `StackHandle` with the guard page, usable base, and top.
    /// Pre-commits the first 8 KB of the stack for immediate use.
    pub fn alloc(&self) -> StackHandle {
        let mut inner = self.inner.lock().unwrap();

        // If free list is empty, grow by adding a new segment.
        if inner.free_list.is_empty() {
            inner.grow();
        }

        let slot_id = inner
            .free_list
            .pop()
            .expect("free list should not be empty after grow");
        inner.active_count += 1;

        let seg = &inner.segments[slot_id.segment as usize];
        let mut handle = seg.slot_layout(slot_id.offset as usize);
        handle.id = slot_id;

        // Pre-commit the top 8 KB of the stack (stack grows downward).
        let commit_start = unsafe { handle.top.sub(PRE_COMMIT_SIZE) };
        unsafe {
            libc::madvise(
                commit_start as *mut libc::c_void,
                PRE_COMMIT_SIZE,
                libc::MADV_WILLNEED,
            );
        }

        handle
    }

    /// Return a stack slot to the pool for reuse.
    ///
    /// Releases physical pages via `madvise(DONTNEED)` but keeps the
    /// virtual address reservation and guard page intact.
    pub fn dealloc(&self, handle: &StackHandle) {
        let mut inner = self.inner.lock().unwrap();
        let seg = &inner.segments[handle.id.segment as usize];
        let slot_base = unsafe { seg.base.add(handle.id.offset as usize * SLOT_SIZE) };
        let page_size = page_size();

        // Release all physical pages in the usable region (above guard page).
        let usable_start = unsafe { slot_base.add(page_size) };
        let usable_size = SLOT_SIZE - page_size;
        unsafe {
            libc::madvise(
                usable_start as *mut libc::c_void,
                usable_size,
                libc::MADV_DONTNEED,
            );
        }

        inner.active_count -= 1;
        inner.free_list.push(handle.id);
    }

    /// Check if a faulting address falls within a guard page.
    ///
    /// This is called from the SIGSEGV handler to distinguish stack
    /// overflow from other segfaults.  Returns `true` if `addr` is in
    /// a known guard page region.
    pub fn is_guard_page(&self, addr: usize) -> bool {
        let inner = self.inner.lock().unwrap();
        let page_size = page_size();
        for seg in &inner.segments {
            let seg_base = seg.base as usize;
            let seg_end = seg_base + seg.mmap_size;
            if addr >= seg_base && addr < seg_end {
                // Offset within the segment.
                let offset = addr - seg_base;
                let slot_offset = offset % SLOT_SIZE;
                // Guard page is at the bottom of each slot (first `page_size` bytes).
                return slot_offset < page_size;
            }
        }
        false
    }

    /// Number of currently active (in-use) slots.
    pub fn active_count(&self) -> usize {
        self.inner.lock().unwrap().active_count
    }

    /// Total number of slots across all segments.
    pub fn total_slots(&self) -> usize {
        self.inner.lock().unwrap().total_slots
    }

    /// Number of segments.
    pub fn segment_count(&self) -> usize {
        self.inner.lock().unwrap().segments.len()
    }
}

impl Default for StackPool {
    fn default() -> Self {
        Self::new()
    }
}

impl PoolInner {
    /// Grow the pool by adding a new segment with doubled capacity.
    fn grow(&mut self) {
        let new_slot_count = match self.segments.last() {
            Some(last) => last.slot_count * 2,
            None => INITIAL_SEGMENT_SLOTS,
        };
        let seg_idx = self.segments.len();
        let seg = Segment::new(new_slot_count);

        // Add all new slots to the free list (in reverse for LIFO ordering).
        for i in (0..new_slot_count).rev() {
            self.free_list.push(SlotId::new(seg_idx, i));
        }

        self.total_slots += new_slot_count;
        self.segments.push(seg);
    }
}

// ---------------------------------------------------------------------------
// Signal handler for stack overflow detection
// ---------------------------------------------------------------------------

use std::sync::Once;

const MAX_GUARD_SEGMENTS: usize = 1024;

struct GuardRegion {
    base: AtomicUsize,
    size: AtomicUsize,
}

impl GuardRegion {
    const fn empty() -> Self {
        Self {
            base: AtomicUsize::new(0),
            size: AtomicUsize::new(0),
        }
    }
}

static GUARD_REGIONS: [GuardRegion; MAX_GUARD_SEGMENTS] =
    [const { GuardRegion::empty() }; MAX_GUARD_SEGMENTS];
static CACHED_PAGE_SIZE: AtomicUsize = AtomicUsize::new(0);
static SIGACTION_INIT: Once = Once::new();
static mut PREV_SIGACTION: libc::sigaction = unsafe { std::mem::zeroed() };

fn register_guard_segment(base: usize, size: usize) -> Option<usize> {
    if base == 0 {
        return None;
    }
    for (idx, region) in GUARD_REGIONS.iter().enumerate() {
        if region
            .base
            .compare_exchange(0, usize::MAX, Ordering::AcqRel, Ordering::Relaxed)
            .is_ok()
        {
            region.size.store(size, Ordering::Relaxed);
            region.base.store(base, Ordering::Release);
            return Some(idx);
        }
    }
    None
}

fn unregister_guard_segment(slot: Option<usize>) {
    if let Some(idx) = slot {
        let region = &GUARD_REGIONS[idx];
        region.size.store(0, Ordering::Relaxed);
        region.base.store(0, Ordering::Release);
    }
}

/// Async-signal-safe check across all live registered stack segments.
fn is_registered_guard_page(addr: usize) -> bool {
    let ps = CACHED_PAGE_SIZE.load(Ordering::Relaxed);
    if ps == 0 {
        return false;
    }
    for region in &GUARD_REGIONS {
        let base = region.base.load(Ordering::Acquire);
        if base == 0 || base == usize::MAX {
            continue;
        }
        let size = region.size.load(Ordering::Relaxed);
        if let Some(end) = base.checked_add(size)
            && addr >= base
            && addr < end
        {
            let slot_offset = (addr - base) % SLOT_SIZE;
            return slot_offset < ps;
        }
    }
    false
}

/// Install the SIGSEGV handler for stack overflow guard-page detection.
///
/// Idempotent across multiple engines; live `StackPool` segments register and
/// unregister themselves automatically in the lock-free guard region table.
pub fn install_guard_page_handler() {
    let _ = page_size();
    SIGACTION_INIT.call_once(|| {
        // Set up an alternate signal stack so we can handle overflow
        // even when the regular stack is exhausted.
        let alt_stack_size = libc::SIGSTKSZ * 2;
        // SAFETY: anonymous private mapping for alternate signal stack.
        let alt_stack_mem = unsafe {
            libc::mmap(
                ptr::null_mut(),
                alt_stack_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert!(
            alt_stack_mem != libc::MAP_FAILED,
            "mmap for alt stack failed"
        );

        let ss = libc::stack_t {
            ss_sp: alt_stack_mem,
            ss_flags: 0,
            ss_size: alt_stack_size,
        };
        // SAFETY: `ss` points to a valid freshly-mapped region of `alt_stack_size` bytes.
        let rc = unsafe { libc::sigaltstack(&ss, ptr::null_mut()) };
        assert!(rc == 0, "sigaltstack failed");

        // Install SIGSEGV handler.
        // SAFETY: `sigaction` struct is zero-initialized and populated with valid flags/handler;
        // `Once` guarantees `PREV_SIGACTION` is written before any signal handler reads it.
        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
        sa.sa_sigaction = guard_page_sigsegv_handler as *const () as libc::sighandler_t;
        unsafe { libc::sigemptyset(&mut sa.sa_mask) };

        let rc = unsafe { libc::sigaction(libc::SIGSEGV, &sa, &raw mut PREV_SIGACTION) };
        assert!(rc == 0, "sigaction failed");
    });
}

extern "C" fn guard_page_sigsegv_handler(
    sig: libc::c_int,
    info: *mut libc::siginfo_t,
    ctx: *mut libc::c_void,
) {
    // SAFETY: `SA_SIGINFO` guarantees `info` is non-null when delivered by the kernel.
    let fault_addr = unsafe { (*info).si_addr() as usize };

    if is_registered_guard_page(fault_addr) {
        // Stack overflow detected — abort with an async-signal-safe write.
        let msg = b"fatal: Nessa task stack overflow (guard page hit)\n";
        // SAFETY: `write` and `abort` are async-signal-safe POSIX primitives.
        unsafe {
            libc::write(2, msg.as_ptr() as *const libc::c_void, msg.len());
            libc::abort();
        }
    }

    // Not our guard page — chain to previous handler.
    // SAFETY: `PREV_SIGACTION` was initialized once before this handler was installed.
    unsafe {
        let prev = (&raw const PREV_SIGACTION).read();
        if prev.sa_flags & libc::SA_SIGINFO != 0 {
            let handler: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) =
                std::mem::transmute(prev.sa_sigaction);
            handler(sig, info, ctx);
        } else if prev.sa_sigaction == libc::SIG_DFL {
            libc::signal(libc::SIGSEGV, libc::SIG_DFL);
            libc::raise(libc::SIGSEGV);
        } else if prev.sa_sigaction != libc::SIG_IGN {
            let handler: extern "C" fn(libc::c_int) = std::mem::transmute(prev.sa_sigaction);
            handler(sig);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn page_size() -> usize {
    let cached = CACHED_PAGE_SIZE.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    // SAFETY: `sysconf(_SC_PAGESIZE)` has no preconditions.
    let ps = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
    CACHED_PAGE_SIZE.store(ps, Ordering::Relaxed);
    ps
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_pool_allocates_distinct_slots_across_threads() {
        use std::collections::HashSet;
        use std::sync::Arc;

        let pool = Arc::new(StackPool::new());
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let pool = Arc::clone(&pool);
                std::thread::spawn(move || (0..8).map(|_| pool.alloc()).collect::<Vec<_>>())
            })
            .collect();
        let handles: Vec<_> = workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect();
        let ids: HashSet<_> = handles.iter().map(|handle| handle.id).collect();
        assert_eq!(ids.len(), 32);
        assert_eq!(pool.active_count(), 32);
        for handle in handles {
            pool.dealloc(&handle);
        }
        assert_eq!(pool.active_count(), 0);
    }

    #[test]
    fn page_size_is_reasonable() {
        let ps = page_size();
        assert!(ps >= 4096);
        assert!(ps.is_power_of_two());
    }

    #[test]
    fn slot_id_roundtrip() {
        let id = SlotId::new(3, 42);
        assert_eq!(id.segment, 3);
        assert_eq!(id.offset, 42);
    }

    #[test]
    fn alloc_and_dealloc_single() {
        let pool = StackPool::new();
        assert_eq!(pool.active_count(), 0);
        assert_eq!(pool.total_slots(), INITIAL_SEGMENT_SLOTS);

        let handle = pool.alloc();
        assert_eq!(pool.active_count(), 1);
        assert!(handle.usable_size() > 0);
        assert!(handle.usable_size() < SLOT_SIZE);

        // The stack should be writable at the top.
        unsafe {
            let top_byte = handle.top.sub(1);
            ptr::write_volatile(top_byte, 0xAB);
            assert_eq!(ptr::read_volatile(top_byte), 0xAB);
        }

        pool.dealloc(&handle);
        assert_eq!(pool.active_count(), 0);
    }

    #[test]
    fn alloc_all_initial_slots_triggers_growth() {
        let pool = StackPool::new();
        assert_eq!(pool.segment_count(), 1);

        let mut handles = Vec::new();
        for _ in 0..INITIAL_SEGMENT_SLOTS {
            handles.push(pool.alloc());
        }
        assert_eq!(pool.active_count(), INITIAL_SEGMENT_SLOTS);
        assert_eq!(pool.segment_count(), 1);

        // One more should trigger growth.
        let extra = pool.alloc();
        assert_eq!(pool.segment_count(), 2);
        assert_eq!(
            pool.total_slots(),
            INITIAL_SEGMENT_SLOTS + INITIAL_SEGMENT_SLOTS * 2
        );

        pool.dealloc(&extra);
        for h in &handles {
            pool.dealloc(h);
        }
        assert_eq!(pool.active_count(), 0);
    }

    #[test]
    fn guard_page_detection() {
        let pool = StackPool::new();
        let handle = pool.alloc();

        // The guard address should be detected.
        assert!(pool.is_guard_page(handle.guard as usize));
        // The usable base should NOT be a guard page.
        assert!(!pool.is_guard_page(handle.base as usize));
        // The top should NOT be a guard page.
        assert!(!pool.is_guard_page(handle.top as usize - 1));
        // An unrelated address should not match.
        assert!(!pool.is_guard_page(0x1234_5678));

        pool.dealloc(&handle);
    }

    #[test]
    fn multiple_pools_register_and_unregister_guard_pages_on_drop() {
        let pool1 = StackPool::new();
        let pool2 = StackPool::new();
        let h1 = pool1.alloc();
        let h2 = pool2.alloc();
        let guard1 = h1.guard as usize;
        let guard2 = h2.guard as usize;
        let base1 = h1.base as usize;

        assert!(is_registered_guard_page(guard1));
        assert!(is_registered_guard_page(guard2));
        assert!(!is_registered_guard_page(base1));

        pool1.dealloc(&h1);
        drop(pool1);
        assert!(!is_registered_guard_page(guard1));
        assert!(is_registered_guard_page(guard2));

        pool2.dealloc(&h2);
        drop(pool2);
        assert!(!is_registered_guard_page(guard2));
    }

    #[test]
    fn reuse_after_dealloc() {
        let pool = StackPool::new();
        let h1 = pool.alloc();
        let id1 = h1.id;
        pool.dealloc(&h1);

        // Should reuse the same slot.
        let h2 = pool.alloc();
        assert_eq!(h2.id, id1);
        pool.dealloc(&h2);
    }

    #[test]
    fn stack_handle_layout() {
        let pool = StackPool::new();
        let handle = pool.alloc();
        let ps = page_size();

        // Guard is at the base of the slot.
        // Usable base is guard + page_size.
        assert_eq!(handle.base as usize, handle.guard as usize + ps);
        // Usable size = SLOT_SIZE - page_size.
        assert_eq!(handle.usable_size(), SLOT_SIZE - ps);

        pool.dealloc(&handle);
    }
}
