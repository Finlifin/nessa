//! Native C-ABI regular expression backend (`libnessa_regex_ffi`).
//!
//! Built on `regex-automata`'s `meta::Regex` engine (literal prefilters + bounded Lazy DFA +
//! PikeVM capture fallback), guaranteeing strict $O(N)$ linear-time matching with zero
//! catastrophic backtracking (ReDoS immunity). Exposes coarse-grained C-ABI symbols so
//! `library/regex` operates as a zero-privilege FFI package with zero compiler or VM regex magic.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use regex_automata::Input;
use regex_automata::meta::{self, BuildError, Regex as MetaRegex};
use regex_automata::util::syntax;

/// Case-insensitive matching (`(?i)`).
pub const FLAG_CASE_INSENSITIVE: u32 = 1 << 0;
/// Multi-line anchors `^` and `$` (`(?m)`).
pub const FLAG_MULTI_LINE: u32 = 1 << 1;
/// Dot `.` matches `\n` (`(?s)`).
pub const FLAG_DOT_MATCHES_NEW_LINE: u32 = 1 << 2;
/// Unicode character classes and case folding (`(?u)`).
pub const FLAG_UNICODE: u32 = 1 << 3;

const DEFAULT_SIZE_LIMIT: usize = 10 * 1024 * 1024;
const DEFAULT_DFA_SIZE_LIMIT: usize = 2 * 1024 * 1024;

struct CompiledRegex {
    utf8_re: Option<MetaRegex>,
    bytes_re: MetaRegex,
    group_count: usize,
    named_groups: HashMap<String, usize>,
}

static NEXT_REGEX_HANDLE: AtomicI64 = AtomicI64::new(1);
static FREED_REGEX_COUNT: AtomicUsize = AtomicUsize::new(0);

fn registry() -> &'static Mutex<HashMap<i64, Arc<CompiledRegex>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<i64, Arc<CompiledRegex>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Return the number of currently live compiled regex handles in the native registry.
pub fn live_handle_count() -> usize {
    registry().lock().map(|guard| guard.len()).unwrap_or(0)
}

/// Return the cumulative number of compiled regex handles reclaimed so far.
pub fn freed_handle_count() -> usize {
    FREED_REGEX_COUNT.load(Ordering::Relaxed)
}

fn get_handle(handle: i64) -> Option<Arc<CompiledRegex>> {
    if handle <= 0 {
        return None;
    }
    registry().lock().ok()?.get(&handle).cloned()
}

/// Return the host C-ABI symbol table for registering `"nessa_regex_ffi"` in `engine/ffi`.
pub fn host_symbols() -> Vec<(&'static str, *const ())> {
    vec![
        ("nessa_regex_compile", nessa_regex_compile as *const ()),
        ("nessa_regex_free", nessa_regex_free as *const ()),
        (
            "nessa_regex_group_count",
            nessa_regex_group_count as *const (),
        ),
        (
            "nessa_regex_group_index_by_name",
            nessa_regex_group_index_by_name as *const (),
        ),
        ("nessa_regex_is_match", nessa_regex_is_match as *const ()),
        ("nessa_regex_captures", nessa_regex_captures as *const ()),
        (
            "nessa_regex_find_batch",
            nessa_regex_find_batch as *const (),
        ),
        (
            "nessa_regex_find_bytes",
            nessa_regex_find_bytes as *const (),
        ),
    ]
}

unsafe fn write_err_message(err_ptr: *mut u8, err_cap: usize, msg: &str) {
    if err_ptr.is_null() || err_cap < 8 {
        return;
    }
    let max_payload = err_cap - 8;
    let mut copy_len = msg.len().min(max_payload);
    while copy_len > 0 && !msg.is_char_boundary(copy_len) {
        copy_len -= 1;
    }
    let len_bytes = (copy_len as u64).to_le_bytes();
    // SAFETY: Caller guarantees `err_ptr` is valid for `err_cap` writable bytes (`err_cap >= 8 + copy_len`).
    unsafe {
        std::ptr::copy_nonoverlapping(len_bytes.as_ptr(), err_ptr, 8);
        if copy_len > 0 {
            std::ptr::copy_nonoverlapping(msg.as_ptr(), err_ptr.add(8), copy_len);
        }
    }
}

fn build_meta_regex(
    pattern: &str,
    flags: u32,
    utf8_mode: bool,
    size_limit: usize,
    dfa_size_limit: usize,
) -> Result<MetaRegex, Box<BuildError>> {
    let syntax_cfg = syntax::Config::new()
        .case_insensitive((flags & FLAG_CASE_INSENSITIVE) != 0)
        .multi_line((flags & FLAG_MULTI_LINE) != 0)
        .dot_matches_new_line((flags & FLAG_DOT_MATCHES_NEW_LINE) != 0)
        .unicode((flags & FLAG_UNICODE) != 0)
        .utf8(utf8_mode);

    let meta_cfg = meta::Config::new()
        .utf8_empty(utf8_mode)
        .nfa_size_limit(Some(size_limit))
        .dfa_size_limit(Some(dfa_size_limit))
        .hybrid_cache_capacity(dfa_size_limit);

    MetaRegex::builder()
        .syntax(syntax_cfg)
        .configure(meta_cfg)
        .build(pattern)
        .map_err(Box::new)
}

/// Compile a regular expression pattern and return a positive monotonic handle.
///
/// Returns `-1` on syntax/UTF-8 error (writing `[len_u64_le][utf8_msg]` into `err_ptr`),
/// or `-2` when `size_limit` is exceeded (writing `[len_u64_le][decimal_limit]` into `err_ptr`).
///
/// # Safety
/// - If `pat_len > 0`, `pat_ptr` must point to `pat_len` readable bytes.
/// - If `err_cap > 0`, `err_ptr` must point to `err_cap` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nessa_regex_compile(
    pat_ptr: *const u8,
    pat_len: usize,
    flags: u32,
    size_limit: usize,
    dfa_size_limit: usize,
    err_ptr: *mut u8,
    err_cap: usize,
) -> i64 {
    let pat_bytes = if pat_len == 0 {
        &[][..]
    } else if pat_ptr.is_null() {
        // SAFETY: `write_err_message` validates `err_ptr` and `err_cap`.
        unsafe { write_err_message(err_ptr, err_cap, "null pattern pointer") };
        return -1;
    } else {
        // SAFETY: Caller guarantees `pat_ptr` is valid for `pat_len` readable bytes.
        unsafe { std::slice::from_raw_parts(pat_ptr, pat_len) }
    };

    let Ok(pattern) = std::str::from_utf8(pat_bytes) else {
        // SAFETY: `write_err_message` validates `err_ptr` and `err_cap`.
        unsafe { write_err_message(err_ptr, err_cap, "pattern is not valid UTF-8") };
        return -1;
    };

    let eff_size_limit = if size_limit == 0 {
        DEFAULT_SIZE_LIMIT
    } else {
        size_limit
    };
    let eff_dfa_limit = if dfa_size_limit == 0 {
        DEFAULT_DFA_SIZE_LIMIT
    } else {
        dfa_size_limit
    };

    let bytes_re = match build_meta_regex(pattern, flags, false, eff_size_limit, eff_dfa_limit) {
        Ok(re) => re,
        Err(err) => {
            if let Some(limit) = err.size_limit() {
                let limit_text = limit.to_string();
                // SAFETY: `write_err_message` validates `err_ptr` and `err_cap`.
                unsafe { write_err_message(err_ptr, err_cap, &limit_text) };
                return -2;
            }
            let msg = err.to_string();
            // SAFETY: `write_err_message` validates `err_ptr` and `err_cap`.
            unsafe { write_err_message(err_ptr, err_cap, &msg) };
            return -1;
        }
    };

    // Build UTF-8 guaranteed automaton when the pattern cannot match invalid UTF-8.
    // Patterns with `(?-u)` raw byte escapes like `\xff` still succeed in `bytes_re`
    // for `Bytes` matching while `utf8_re` stays `None`.
    let utf8_re = match build_meta_regex(pattern, flags, true, eff_size_limit, eff_dfa_limit) {
        Ok(re) => Some(re),
        Err(err) => {
            if let Some(limit) = err.size_limit() {
                let limit_text = limit.to_string();
                // SAFETY: `write_err_message` validates `err_ptr` and `err_cap`.
                unsafe { write_err_message(err_ptr, err_cap, &limit_text) };
                return -2;
            }
            None
        }
    };

    let group_info = bytes_re.group_info();
    let group_count = group_info.all_group_len();
    let mut named_groups = HashMap::new();
    for (idx, opt_name) in group_info
        .pattern_names(regex_automata::PatternID::ZERO)
        .enumerate()
    {
        if let Some(name) = opt_name {
            named_groups.insert(name.to_owned(), idx);
        }
    }

    let compiled = Arc::new(CompiledRegex {
        utf8_re,
        bytes_re,
        group_count,
        named_groups,
    });

    let handle = NEXT_REGEX_HANDLE.fetch_add(1, Ordering::Relaxed);
    let Ok(mut guard) = registry().lock() else {
        // SAFETY: `write_err_message` validates `err_ptr` and `err_cap`.
        unsafe { write_err_message(err_ptr, err_cap, "regex registry poisoned") };
        return -1;
    };
    guard.insert(handle, compiled);
    handle
}

/// Idempotently release a compiled regex handle. Safe to call multiple times or from GC cleanup.
#[unsafe(no_mangle)]
pub extern "C" fn nessa_regex_free(handle: i64) {
    if handle <= 0 {
        return;
    }
    if let Ok(mut guard) = registry().lock()
        && guard.remove(&handle).is_some()
    {
        FREED_REGEX_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

/// Return the capture group count (including group `0`), or `-1` on invalid/closed handle.
#[unsafe(no_mangle)]
pub extern "C" fn nessa_regex_group_count(handle: i64) -> isize {
    match get_handle(handle) {
        Some(re) => re.group_count as isize,
        None => -1,
    }
}

/// Resolve a named capture group (`(?P<name>...)` or `(?<name>...)`) to its group index,
/// returning `-1` if absent or `-2` if `handle` is invalid/closed.
///
/// # Safety
/// If `name_len > 0`, `name_ptr` must point to `name_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nessa_regex_group_index_by_name(
    handle: i64,
    name_ptr: *const u8,
    name_len: usize,
) -> isize {
    let Some(re) = get_handle(handle) else {
        return -2;
    };
    let name_bytes = if name_len == 0 {
        &[][..]
    } else if name_ptr.is_null() {
        return -1;
    } else {
        // SAFETY: Caller guarantees `name_ptr` points to `name_len` readable bytes.
        unsafe { std::slice::from_raw_parts(name_ptr, name_len) }
    };
    let Ok(name) = std::str::from_utf8(name_bytes) else {
        return -1;
    };
    match re.named_groups.get(name) {
        Some(&idx) => idx as isize,
        None => -1,
    }
}

/// Fast boolean match check using the bounded Lazy DFA / prefilter without capture slot overhead.
///
/// When `bytes_mode == 0`, validates `start` as a UTF-8 boundary and uses the UTF-8 automaton;
/// when `bytes_mode != 0`, matches arbitrary raw bytes via `bytes_re`.
/// Returns `1` on match, `0` on non-match, `-1` on invalid handle or invalid start/UTF-8 mode,
/// or `-2` if `utf8_re` is unavailable (raw non-UTF-8 byte pattern used on `String`).
///
/// # Safety
/// If `data_len > 0`, `data_ptr` must point to `data_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nessa_regex_is_match(
    handle: i64,
    data_ptr: *const u8,
    data_len: usize,
    start: usize,
    bytes_mode: u32,
) -> i32 {
    let Some(re) = get_handle(handle) else {
        return -1;
    };
    if start > data_len {
        return -1;
    }
    let data = if data_len == 0 {
        &[][..]
    } else if data_ptr.is_null() {
        return -1;
    } else {
        // SAFETY: Caller guarantees `data_ptr` points to `data_len` readable bytes.
        unsafe { std::slice::from_raw_parts(data_ptr, data_len) }
    };

    if bytes_mode == 0 {
        let Ok(text) = std::str::from_utf8(data) else {
            return -1;
        };
        if !text.is_char_boundary(start) {
            return -1;
        }
        let Some(utf8_re) = &re.utf8_re else {
            return -2;
        };
        i32::from(utf8_re.is_match(Input::new(text).span(start..data_len)))
    } else {
        i32::from(re.bytes_re.is_match(Input::new(data).span(start..data_len)))
    }
}

/// Match capture groups starting at `start`, writing `group_count` pairs of `[start_u64_le, end_u64_le]`
/// into `out_ptr` (`u64::MAX` for unmatched optional groups).
///
/// Returns `1` if matched, `0` if unmatched, `-1` on invalid handle/offset/buffer, or `-2` if
/// the pattern can match invalid UTF-8 and cannot be run on `String`.
///
/// # Safety
/// - If `text_len > 0`, `text_ptr` must point to `text_len` readable bytes.
/// - If `out_cap > 0`, `out_ptr` must point to `out_cap` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nessa_regex_captures(
    handle: i64,
    text_ptr: *const u8,
    text_len: usize,
    start: usize,
    out_ptr: *mut u8,
    out_cap: usize,
) -> i32 {
    let Some(re) = get_handle(handle) else {
        return -1;
    };
    let Some(utf8_re) = &re.utf8_re else {
        return -2;
    };
    if start > text_len {
        return -1;
    }
    let needed_bytes = re.group_count.saturating_mul(16);
    if out_cap < needed_bytes || (needed_bytes > 0 && out_ptr.is_null()) {
        return -1;
    }
    let data = if text_len == 0 {
        &[][..]
    } else if text_ptr.is_null() {
        return -1;
    } else {
        // SAFETY: Caller guarantees `text_ptr` points to `text_len` readable bytes.
        unsafe { std::slice::from_raw_parts(text_ptr, text_len) }
    };
    let Ok(text) = std::str::from_utf8(data) else {
        return -1;
    };
    if !text.is_char_boundary(start) {
        return -1;
    }

    let mut caps = utf8_re.create_captures();
    utf8_re.search_captures(&Input::new(text).span(start..text_len), &mut caps);
    if !caps.is_match() {
        return 0;
    }

    for group_idx in 0..re.group_count {
        let (s, e) = match caps.get_group(group_idx) {
            Some(span) => (span.start as u64, span.end as u64),
            None => (u64::MAX, u64::MAX),
        };
        let s_bytes = s.to_le_bytes();
        let e_bytes = e.to_le_bytes();
        // SAFETY: `out_cap >= re.group_count * 16` and `out_ptr` is valid and non-null.
        unsafe {
            let slot_ptr = out_ptr.add(group_idx * 16);
            std::ptr::copy_nonoverlapping(s_bytes.as_ptr(), slot_ptr, 8);
            std::ptr::copy_nonoverlapping(e_bytes.as_ptr(), slot_ptr.add(8), 8);
        }
    }
    1
}

fn utf8_char_step(rest: &str) -> usize {
    rest.chars().next().map_or(1, char::len_utf8)
}

/// Batch global UTF-8 match search writing up to `out_cap / 16` pairs of `[start_u64_le, end_u64_le]`
/// into `out_ptr` in a single C-ABI call while preserving full look-around context (`^`, `$`, `\b`)
/// and exact empty-match progression across batch boundaries.
///
/// Pass `start = 0, last_match_end = -1` on the first batch, and `start = prev_end, last_match_end = prev_end as isize`
/// on subsequent batches.
/// Returns the number of matches written (`0..=max_matches`), `-1` on invalid handle/offset/buffer,
/// or `-2` if the pattern cannot run on UTF-8 `String`.
///
/// # Safety
/// - If `text_len > 0`, `text_ptr` must point to `text_len` readable bytes.
/// - If `out_cap > 0`, `out_ptr` must point to `out_cap` writable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nessa_regex_find_batch(
    handle: i64,
    text_ptr: *const u8,
    text_len: usize,
    start: usize,
    last_match_end: isize,
    out_ptr: *mut u8,
    out_cap: usize,
) -> isize {
    let Some(re) = get_handle(handle) else {
        return -1;
    };
    let Some(utf8_re) = &re.utf8_re else {
        return -2;
    };
    let max_matches = out_cap / 16;
    if max_matches == 0 || out_ptr.is_null() || start > text_len {
        return -1;
    }
    let data = if text_len == 0 {
        &[][..]
    } else if text_ptr.is_null() {
        return -1;
    } else {
        // SAFETY: Caller guarantees `text_ptr` points to `text_len` readable bytes.
        unsafe { std::slice::from_raw_parts(text_ptr, text_len) }
    };
    let Ok(text) = std::str::from_utf8(data) else {
        return -1;
    };
    if !text.is_char_boundary(start) {
        return -1;
    }

    let mut pos = start;
    let mut prev_end = last_match_end;
    let mut written = 0usize;

    while written < max_matches && pos <= text_len {
        let Some(mut m) = utf8_re.find(Input::new(text).span(pos..text_len)) else {
            break;
        };
        if m.is_empty() && prev_end >= 0 && m.end() == prev_end as usize {
            if pos >= text_len {
                break;
            }
            pos += utf8_char_step(&text[pos..]);
            let Some(next_m) = utf8_re.find(Input::new(text).span(pos..text_len)) else {
                break;
            };
            m = next_m;
        }

        let s_bytes = (m.start() as u64).to_le_bytes();
        let e_bytes = (m.end() as u64).to_le_bytes();
        // SAFETY: `written < max_matches == out_cap / 16` and `out_ptr` is non-null.
        unsafe {
            let pair_ptr = out_ptr.add(written * 16);
            std::ptr::copy_nonoverlapping(s_bytes.as_ptr(), pair_ptr, 8);
            std::ptr::copy_nonoverlapping(e_bytes.as_ptr(), pair_ptr.add(8), 8);
        }
        written += 1;
        prev_end = m.end() as isize;
        pos = m.end();
    }

    written as isize
}

/// Find the first match in arbitrary binary `Bytes` starting at `start`, writing
/// `[start_u64_le, end_u64_le]` into `out_ptr`.
///
/// Returns `1` if matched, `0` if unmatched, or `-1` on invalid handle/offset/buffer.
///
/// # Safety
/// - If `data_len > 0`, `data_ptr` must point to `data_len` readable bytes.
/// - `out_ptr` must point to at least `16` writable bytes (`out_cap >= 16`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nessa_regex_find_bytes(
    handle: i64,
    data_ptr: *const u8,
    data_len: usize,
    start: usize,
    out_ptr: *mut u8,
    out_cap: usize,
) -> i32 {
    let Some(re) = get_handle(handle) else {
        return -1;
    };
    if start > data_len || out_cap < 16 || out_ptr.is_null() {
        return -1;
    }
    let data = if data_len == 0 {
        &[][..]
    } else if data_ptr.is_null() {
        return -1;
    } else {
        // SAFETY: Caller guarantees `data_ptr` points to `data_len` readable bytes.
        unsafe { std::slice::from_raw_parts(data_ptr, data_len) }
    };

    let Some(m) = re.bytes_re.find(Input::new(data).span(start..data_len)) else {
        return 0;
    };
    let s_bytes = (m.start() as u64).to_le_bytes();
    let e_bytes = (m.end() as u64).to_le_bytes();
    // SAFETY: `out_cap >= 16` and `out_ptr` is non-null.
    unsafe {
        std::ptr::copy_nonoverlapping(s_bytes.as_ptr(), out_ptr, 8);
        std::ptr::copy_nonoverlapping(e_bytes.as_ptr(), out_ptr.add(8), 8);
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn compile_batch_captures_and_redos_linear_time() {
        let pat = b"(?P<year>\\d{4})-(?P<month>\\d{2})";
        let mut err = [0u8; 256];
        let handle = unsafe {
            nessa_regex_compile(
                pat.as_ptr(),
                pat.len(),
                FLAG_UNICODE,
                0,
                0,
                err.as_mut_ptr(),
                err.len(),
            )
        };
        assert!(handle > 0);
        assert_eq!(nessa_regex_group_count(handle), 3);
        assert_eq!(
            unsafe { nessa_regex_group_index_by_name(handle, b"year".as_ptr(), 4) },
            1
        );
        assert_eq!(
            unsafe { nessa_regex_group_index_by_name(handle, b"month".as_ptr(), 5) },
            2
        );
        assert_eq!(
            unsafe { nessa_regex_group_index_by_name(handle, b"nope".as_ptr(), 4) },
            -1
        );

        let text = b"dates: 2025-03 and 2026-11!";
        let mut caps_buf = [0u8; 48];
        let rc = unsafe {
            nessa_regex_captures(
                handle,
                text.as_ptr(),
                text.len(),
                0,
                caps_buf.as_mut_ptr(),
                caps_buf.len(),
            )
        };
        assert_eq!(rc, 1);
        assert_eq!(u64::from_le_bytes(caps_buf[0..8].try_into().unwrap()), 7);
        assert_eq!(u64::from_le_bytes(caps_buf[8..16].try_into().unwrap()), 14);
        assert_eq!(u64::from_le_bytes(caps_buf[16..24].try_into().unwrap()), 7);
        assert_eq!(u64::from_le_bytes(caps_buf[24..32].try_into().unwrap()), 11);

        // Batch of size 1 across multiple matches
        let mut one_pair = [0u8; 16];
        let n1 = unsafe {
            nessa_regex_find_batch(
                handle,
                text.as_ptr(),
                text.len(),
                0,
                -1,
                one_pair.as_mut_ptr(),
                one_pair.len(),
            )
        };
        assert_eq!(n1, 1);
        let end1 = u64::from_le_bytes(one_pair[8..16].try_into().unwrap()) as usize;
        assert_eq!(end1, 14);
        let n2 = unsafe {
            nessa_regex_find_batch(
                handle,
                text.as_ptr(),
                text.len(),
                end1,
                end1 as isize,
                one_pair.as_mut_ptr(),
                one_pair.len(),
            )
        };
        assert_eq!(n2, 1);
        let s2 = u64::from_le_bytes(one_pair[0..8].try_into().unwrap()) as usize;
        let e2 = u64::from_le_bytes(one_pair[8..16].try_into().unwrap()) as usize;
        assert_eq!((s2, e2), (19, 26));

        nessa_regex_free(handle);
        assert_eq!(nessa_regex_group_count(handle), -1);

        // ReDoS linear-time immunity check
        let redos_pat = b"(a+)+b";
        let h_redos = unsafe {
            nessa_regex_compile(
                redos_pat.as_ptr(),
                redos_pat.len(),
                FLAG_UNICODE,
                0,
                0,
                err.as_mut_ptr(),
                err.len(),
            )
        };
        assert!(h_redos > 0);
        let input = vec![b'a'; 50_000];
        let t0 = Instant::now();
        let matched = unsafe { nessa_regex_is_match(h_redos, input.as_ptr(), input.len(), 0, 0) };
        let elapsed = t0.elapsed();
        assert_eq!(matched, 0);
        assert!(elapsed.as_millis() < 100, "ReDoS took {elapsed:?}");
        nessa_regex_free(h_redos);
    }

    #[test]
    fn size_limit_returns_compiled_too_large() {
        let pat = b"[a-z]{200}";
        let mut err = [0u8; 128];
        let handle = unsafe {
            nessa_regex_compile(
                pat.as_ptr(),
                pat.len(),
                FLAG_UNICODE,
                256,
                256,
                err.as_mut_ptr(),
                err.len(),
            )
        };
        assert_eq!(handle, -2);
        let len = u64::from_le_bytes(err[0..8].try_into().unwrap()) as usize;
        let msg = std::str::from_utf8(&err[8..8 + len]).unwrap();
        assert_eq!(msg, "256");
    }
}
