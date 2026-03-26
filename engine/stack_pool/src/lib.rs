//! Task Stack Pool — mmap'd segmented stack management.
//!
//! Each Nessa task gets an 8 MB virtual memory slot for its call stack.
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
}

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
            libc::madvise(
                base as *mut libc::c_void,
                mmap_size,
                libc::MADV_DONTNEED,
            );
        }

        Self {
            base,
            slot_count,
            mmap_size,
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

        let slot_id = inner.free_list.pop().expect("free list should not be empty after grow");
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

static SIGACTION_INIT: Once = Once::new();
static mut PREV_SIGACTION: libc::sigaction = unsafe { std::mem::zeroed() };

/// Wrapper to allow storing a raw pointer in a `Mutex` inside a `static`.
struct PoolPtr(*const StackPool);
unsafe impl Send for PoolPtr {}

static STACK_POOL_PTR: Mutex<Option<PoolPtr>> = Mutex::new(None);

/// Install the SIGSEGV handler for stack overflow detection.
///
/// This must be called once during engine initialization after creating
/// the `StackPool`.  The `pool` reference must live for the program's
/// lifetime (typically `&'static StackPool` or leaked `Box`).
///
/// # Safety
/// The `pool` pointer must remain valid for the lifetime of the program.
pub unsafe fn install_guard_page_handler(pool: &StackPool) {
    SIGACTION_INIT.call_once(|| {
        *STACK_POOL_PTR.lock().unwrap() = Some(PoolPtr(pool as *const StackPool));

        // Set up an alternate signal stack so we can handle overflow
        // even when the regular stack is exhausted.
        let alt_stack_size = libc::SIGSTKSZ * 2;
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
        assert!(alt_stack_mem != libc::MAP_FAILED, "mmap for alt stack failed");

        let ss = libc::stack_t {
            ss_sp: alt_stack_mem,
            ss_flags: 0,
            ss_size: alt_stack_size,
        };
        let rc = unsafe { libc::sigaltstack(&ss, ptr::null_mut()) };
        assert!(rc == 0, "sigaltstack failed");

        // Install SIGSEGV handler.
        let mut sa: libc::sigaction = unsafe { std::mem::zeroed() };
        sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
        sa.sa_sigaction = guard_page_sigsegv_handler as usize;
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
    let fault_addr = unsafe { (*info).si_addr() as usize };

    let is_guard = {
        let pool_opt = STACK_POOL_PTR.lock().ok();
        pool_opt
            .and_then(|guard| {
                (*guard).as_ref().map(|pp| {
                    let pool = unsafe { &*pp.0 };
                    pool.is_guard_page(fault_addr)
                })
            })
            .unwrap_or(false)
    };

    if is_guard {
        // Stack overflow detected — abort with a clear message.
        let msg = b"fatal: Nessa task stack overflow (guard page hit)\n";
        unsafe {
            libc::write(2, msg.as_ptr() as *const libc::c_void, msg.len());
            libc::abort();
        }
    }

    // Not our guard page — chain to previous handler.
    unsafe {
        let prev = &*(&raw const PREV_SIGACTION);
        if prev.sa_flags & libc::SA_SIGINFO != 0 {
            let handler: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) =
                std::mem::transmute(prev.sa_sigaction);
            handler(sig, info, ctx);
        } else if prev.sa_sigaction == libc::SIG_DFL {
            // Re-raise with default action.
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
    // Cache the page size to avoid repeated syscalls.
    static PAGE_SIZE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *PAGE_SIZE.get_or_init(|| unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(pool.total_slots(), INITIAL_SEGMENT_SLOTS + INITIAL_SEGMENT_SLOTS * 2);

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
