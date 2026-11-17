//! Strict W^X executable memory management with bounded per-VM code cache.
//!
//! Code pages are allocated `PROT_READ | PROT_WRITE` via `mmap`, populated with
//! machine code bytes while non-executable, and then sealed `PROT_READ | PROT_EXEC`
//! via `mprotect` before any function pointer can be obtained. Pages are never
//! simultaneously writable and executable.

use std::ptr::NonNull;

/// Default maximum executable memory budget per VM instance (16 MiB).
pub const DEFAULT_MAX_CODE_BYTES: usize = 16 * 1024 * 1024;

/// Errors returned during JIT compilation or executable page allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JitError {
    /// Compiled code would exceed the configured per-VM code-cache budget.
    CodeCacheFull {
        requested: usize,
        used: usize,
        limit: usize,
    },
    /// OS `mmap` or `mprotect` call failed.
    AllocationFailed,
    /// Target architecture or OS does not support native emission.
    UnsupportedTarget,
    /// Function or loop shape is not eligible for the requested tier.
    UnsupportedBytecode(&'static str),
}

/// Immutable, sealed `PROT_READ | PROT_EXEC` native machine-code buffer.
#[derive(Debug)]
pub struct ExecutableBuffer {
    ptr: NonNull<u8>,
    code_len: usize,
    mapped_len: usize,
}

// SAFETY: Once constructed, `ExecutableBuffer` is sealed `PROT_READ | PROT_EXEC`
// and never mutated again; `Drop` unmaps the private anonymous mapping only when
// exclusive ownership is released.
unsafe impl Send for ExecutableBuffer {}
// SAFETY: Sealed `PROT_READ | PROT_EXEC` bytes are immutable and safe for concurrent reads.
unsafe impl Sync for ExecutableBuffer {}

#[cfg(windows)]
mod win_mem {
    use std::ffi::c_void;

    pub const MEM_COMMIT: u32 = 0x0000_1000;
    pub const MEM_RESERVE: u32 = 0x0000_2000;
    pub const MEM_RELEASE: u32 = 0x0000_8000;
    pub const PAGE_READWRITE: u32 = 0x04;
    pub const PAGE_EXECUTE_READ: u32 = 0x20;

    #[repr(C)]
    pub struct SystemInfo {
        pub processor_architecture: u16,
        pub reserved: u16,
        pub page_size: u32,
        pub minimum_application_address: *mut c_void,
        pub maximum_application_address: *mut c_void,
        pub active_processor_mask: usize,
        pub number_of_processors: u32,
        pub processor_type: u32,
        pub allocation_granularity: u32,
        pub processor_level: u16,
        pub processor_revision: u16,
    }

    #[cfg(test)]
    #[repr(C)]
    pub struct MemoryBasicInformation {
        pub base_address: *mut c_void,
        pub allocation_base: *mut c_void,
        pub allocation_protect: u32,
        pub partition_id: u16,
        pub region_size: usize,
        pub state: u32,
        pub protect: u32,
        pub type_: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn VirtualAlloc(
            lp_address: *mut c_void,
            dw_size: usize,
            fl_allocation_type: u32,
            fl_protect: u32,
        ) -> *mut c_void;
        pub fn VirtualProtect(
            lp_address: *mut c_void,
            dw_size: usize,
            fl_new_protect: u32,
            lpfl_old_protect: *mut u32,
        ) -> i32;
        pub fn VirtualFree(lp_address: *mut c_void, dw_size: usize, dw_free_type: u32) -> i32;
        pub fn GetCurrentProcess() -> *mut c_void;
        pub fn FlushInstructionCache(
            h_process: *mut c_void,
            lp_base_address: *const c_void,
            dw_size: usize,
        ) -> i32;
        pub fn GetSystemInfo(lp_system_info: *mut SystemInfo);
        #[cfg(test)]
        pub fn VirtualQuery(
            lp_address: *const c_void,
            lp_buffer: *mut MemoryBasicInformation,
            dw_length: usize,
        ) -> usize;
    }
}

impl ExecutableBuffer {
    /// Allocate, populate, and W^X-seal a native code buffer.
    #[cfg(unix)]
    pub fn from_code(code: &[u8]) -> Result<Self, JitError> {
        if code.is_empty() {
            return Err(JitError::AllocationFailed);
        }
        let page_size = os_page_size();
        let mapped_len = align_up(code.len(), page_size).ok_or(JitError::AllocationFailed)?;

        // Phase 1: Map private anonymous pages as RW (writable, non-executable).
        // SAFETY: `mmap` with `MAP_PRIVATE | MAP_ANONYMOUS`, null hint address,
        // `fd = -1`, `offset = 0`, and non-zero `mapped_len` requests fresh OS pages.
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                mapped_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if raw == libc::MAP_FAILED || raw.is_null() {
            return Err(JitError::AllocationFailed);
        }
        let ptr = NonNull::new(raw.cast::<u8>()).ok_or(JitError::AllocationFailed)?;

        // Copy machine code into the RW buffer and fill trailing bytes with trap/int3 (0xCC on x86_64).
        // SAFETY: `ptr` points to a freshly mapped, exclusively owned RW region of `mapped_len >= code.len()` bytes.
        unsafe {
            #[cfg(target_arch = "x86_64")]
            std::ptr::write_bytes(ptr.as_ptr(), 0xCC, mapped_len);
            #[cfg(not(target_arch = "x86_64"))]
            std::ptr::write_bytes(ptr.as_ptr(), 0x00, mapped_len);

            std::ptr::copy_nonoverlapping(code.as_ptr(), ptr.as_ptr(), code.len());
        }

        // Phase 2: Transition RW -> RX (strict W^X: write permission removed before execute is granted).
        // SAFETY: `ptr` is page-aligned from `mmap` and covers `mapped_len` page-aligned bytes.
        let protect_rc = unsafe {
            libc::mprotect(
                ptr.as_ptr().cast::<libc::c_void>(),
                mapped_len,
                libc::PROT_READ | libc::PROT_EXEC,
            )
        };
        if protect_rc != 0 {
            // SAFETY: Roll back the mapping if `mprotect` fails so no pages leak.
            unsafe {
                let _ = libc::munmap(ptr.as_ptr().cast::<libc::c_void>(), mapped_len);
            }
            return Err(JitError::AllocationFailed);
        }

        flush_instruction_cache(ptr.as_ptr(), code.len());

        Ok(Self {
            ptr,
            code_len: code.len(),
            mapped_len,
        })
    }

    #[cfg(windows)]
    pub fn from_code(code: &[u8]) -> Result<Self, JitError> {
        if code.is_empty() {
            return Err(JitError::AllocationFailed);
        }
        let page_size = os_page_size();
        let mapped_len = align_up(code.len(), page_size).ok_or(JitError::AllocationFailed)?;

        // Phase 1: Allocate RW pages via `VirtualAlloc` (strict W^X: non-executable while writing).
        // SAFETY: `VirtualAlloc` with null hint and `MEM_COMMIT | MEM_RESERVE` returns fresh RW pages.
        let raw = unsafe {
            win_mem::VirtualAlloc(
                std::ptr::null_mut(),
                mapped_len,
                win_mem::MEM_COMMIT | win_mem::MEM_RESERVE,
                win_mem::PAGE_READWRITE,
            )
        };
        let ptr = NonNull::new(raw.cast::<u8>()).ok_or(JitError::AllocationFailed)?;

        // SAFETY: `ptr` points to a freshly allocated RW region of `mapped_len >= code.len()` bytes.
        unsafe {
            #[cfg(target_arch = "x86_64")]
            std::ptr::write_bytes(ptr.as_ptr(), 0xCC, mapped_len);
            #[cfg(not(target_arch = "x86_64"))]
            std::ptr::write_bytes(ptr.as_ptr(), 0x00, mapped_len);

            std::ptr::copy_nonoverlapping(code.as_ptr(), ptr.as_ptr(), code.len());
        }

        // Phase 2: Transition RW -> RX (`PAGE_EXECUTE_READ`) and flush icache.
        let mut old_protect = 0u32;
        // SAFETY: `ptr` and `mapped_len` match the `VirtualAlloc` region.
        let protect_ok = unsafe {
            win_mem::VirtualProtect(
                ptr.as_ptr().cast(),
                mapped_len,
                win_mem::PAGE_EXECUTE_READ,
                &raw mut old_protect,
            )
        };
        if protect_ok == 0 {
            // SAFETY: Free allocation on failure so no virtual pages leak.
            unsafe {
                let _ = win_mem::VirtualFree(ptr.as_ptr().cast(), 0, win_mem::MEM_RELEASE);
            }
            return Err(JitError::AllocationFailed);
        }

        flush_instruction_cache(ptr.as_ptr(), code.len());

        Ok(Self {
            ptr,
            code_len: code.len(),
            mapped_len,
        })
    }

    #[cfg(not(any(unix, windows)))]
    pub fn from_code(_code: &[u8]) -> Result<Self, JitError> {
        Err(JitError::UnsupportedTarget)
    }

    /// Pointer to the start of the sealed `PROT_READ | PROT_EXEC` code buffer.
    #[inline]
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr.as_ptr()
    }

    /// Exact machine-code payload size in bytes (excluding page rounding).
    #[inline]
    pub fn code_len(&self) -> usize {
        self.code_len
    }

    /// Total OS page-aligned bytes reserved by this mapping.
    #[inline]
    pub fn mapped_len(&self) -> usize {
        self.mapped_len
    }
}

impl Drop for ExecutableBuffer {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // SAFETY: `self.ptr` and `self.mapped_len` were obtained from a succeeded
            // `mmap` in `ExecutableBuffer::from_code` and are unmapped exactly once on drop.
            unsafe {
                let _ = libc::munmap(self.ptr.as_ptr().cast::<libc::c_void>(), self.mapped_len);
            }
        }
        #[cfg(windows)]
        {
            // SAFETY: `self.ptr` was returned by `VirtualAlloc` and is released once on drop.
            unsafe {
                let _ = win_mem::VirtualFree(self.ptr.as_ptr().cast(), 0, win_mem::MEM_RELEASE);
            }
        }
    }
}
/// Bounded per-VM executable code cache.
#[derive(Debug)]
pub struct CodeCache {
    buffers: Vec<ExecutableBuffer>,
    code_bytes: usize,
    mapped_bytes: usize,
    max_bytes: usize,
}

impl CodeCache {
    pub fn new(max_bytes: usize) -> Self {
        Self {
            buffers: Vec::new(),
            code_bytes: 0,
            mapped_bytes: 0,
            max_bytes,
        }
    }

    /// Allocate and seal a new native function buffer within the configured budget.
    pub fn allocate(&mut self, code: &[u8]) -> Result<&ExecutableBuffer, JitError> {
        if code.is_empty() {
            return Err(JitError::AllocationFailed);
        }
        let page_size = os_page_size();
        let needed_mapped = align_up(code.len(), page_size).ok_or(JitError::AllocationFailed)?;
        let next_mapped =
            self.mapped_bytes
                .checked_add(needed_mapped)
                .ok_or(JitError::CodeCacheFull {
                    requested: needed_mapped,
                    used: self.mapped_bytes,
                    limit: self.max_bytes,
                })?;
        if next_mapped > self.max_bytes {
            return Err(JitError::CodeCacheFull {
                requested: needed_mapped,
                used: self.mapped_bytes,
                limit: self.max_bytes,
            });
        }

        let buf = ExecutableBuffer::from_code(code)?;
        self.code_bytes += buf.code_len();
        self.mapped_bytes += buf.mapped_len();
        self.buffers.push(buf);
        Ok(self.buffers.last().expect("just pushed buffer"))
    }

    /// Total emitted machine-code bytes across all compiled functions in this cache.
    #[inline]
    pub fn code_bytes(&self) -> usize {
        self.code_bytes
    }

    /// Total OS mapped bytes across all compiled buffers in this cache.
    #[inline]
    pub fn mapped_bytes(&self) -> usize {
        self.mapped_bytes
    }

    /// Configured maximum mapped byte budget.
    #[inline]
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Release all compiled code buffers (used only when VM type metadata is replaced).
    pub fn clear(&mut self) {
        self.buffers.clear();
        self.code_bytes = 0;
        self.mapped_bytes = 0;
    }
}

#[cfg(unix)]
fn os_page_size() -> usize {
    // SAFETY: `sysconf(_SC_PAGESIZE)` is thread-safe and reads OS configuration.
    let raw = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if raw > 0 { raw as usize } else { 4096 }
}

#[cfg(windows)]
fn os_page_size() -> usize {
    let mut info: win_mem::SystemInfo = unsafe { std::mem::zeroed() };
    // SAFETY: `GetSystemInfo` writes into caller-allocated `SystemInfo`.
    unsafe { win_mem::GetSystemInfo(&raw mut info) };
    let ps = info.page_size as usize;
    if ps >= 4096 && ps.is_power_of_two() {
        ps
    } else {
        4096
    }
}

#[cfg(not(any(unix, windows)))]
fn os_page_size() -> usize {
    4096
}

fn align_up(len: usize, align: usize) -> Option<usize> {
    let mask = align.checked_sub(1)?;
    len.checked_add(mask).map(|v| v & !mask)
}

#[inline]
fn flush_instruction_cache(_ptr: *const u8, _len: usize) {
    // On x86_64, data/instruction caches are hardware-coherent across `mprotect`.
    // On AArch64 Linux, explicit icache invalidation is required before execution;
    // when an AArch64 emitter is enabled, synchronize via `sys_icache_invalidate` / `clear_cache`.
    #[cfg(all(target_arch = "aarch64", target_os = "linux"))]
    {
        unsafe extern "C" {
            fn __clear_cache(start: *mut libc::c_void, end: *mut libc::c_void);
        }
        // SAFETY: `_ptr .. _ptr + _len` is a valid mapped region owned by caller.
        unsafe {
            let start = _ptr.cast_mut().cast::<libc::c_void>();
            let end = _ptr.add(_len).cast_mut().cast::<libc::c_void>();
            __clear_cache(start, end);
        }
    }
    #[cfg(windows)]
    {
        // SAFETY: `_ptr .. _ptr + _len` is a valid `VirtualAlloc` region owned by caller.
        unsafe {
            let _ = win_mem::FlushInstructionCache(win_mem::GetCurrentProcess(), _ptr.cast(), _len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    fn mapping_permissions_for_addr(addr: usize) -> Option<String> {
        let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
        for line in maps.lines() {
            let mut parts = line.split_whitespace();
            let range = parts.next()?;
            let perms = parts.next()?;
            let (start_hex, end_hex) = range.split_once('-')?;
            let start = usize::from_str_radix(start_hex, 16).ok()?;
            let end = usize::from_str_radix(end_hex, 16).ok()?;
            if (start..end).contains(&addr) {
                return Some(perms.to_string());
            }
        }
        None
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn executable_buffer_enforces_strict_wx_permissions() {
        // Minimal `ret` instruction on x86_64 (or non-empty byte sequence).
        let buf = ExecutableBuffer::from_code(&[0xC3]).expect("executable alloc should succeed");
        assert_eq!(buf.code_len(), 1);
        assert!(buf.mapped_len() >= 4096);

        #[cfg(target_os = "linux")]
        {
            let perms = mapping_permissions_for_addr(buf.as_ptr() as usize)
                .expect("buffer address must appear in /proc/self/maps");
            assert_eq!(
                perms, "r-xp",
                "sealed JIT buffer must be readable+executable and NEVER writable (got {perms})"
            );
        }

        #[cfg(windows)]
        {
            let mut mbi: win_mem::MemoryBasicInformation = unsafe { std::mem::zeroed() };
            // SAFETY: `buf.as_ptr()` is a live page-aligned allocation and `mbi` is valid.
            let n = unsafe {
                win_mem::VirtualQuery(
                    buf.as_ptr().cast(),
                    &raw mut mbi,
                    std::mem::size_of::<win_mem::MemoryBasicInformation>(),
                )
            };
            assert!(n > 0);
            assert_eq!(mbi.protect, win_mem::PAGE_EXECUTE_READ);
        }
    }

    #[test]
    #[cfg(any(unix, windows))]
    fn code_cache_enforces_budget_and_falls_back_cleanly() {
        let page = os_page_size();
        let mut cache = CodeCache::new(page);
        let first = cache
            .allocate(&[0x90, 0xC3])
            .expect("first page fits within budget");
        assert_eq!(first.code_len(), 2);
        assert_eq!(cache.code_bytes(), 2);
        assert_eq!(cache.mapped_bytes(), page);

        let err = cache
            .allocate(&[0xC3])
            .expect_err("second page must exceed 1-page budget");
        assert!(matches!(err, JitError::CodeCacheFull { .. }));
        assert_eq!(cache.code_bytes(), 2);
    }
}
