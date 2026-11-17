//! End-to-end integration tests for compact GC leaf `Bytes`, capability-gated C FFI,
//! native-frame effect/continuation guards, weak GC finalizers, and the FFI-backed `regex` package.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use driver::{Driver, RunResult};

struct TempPackage(PathBuf);

impl TempPackage {
    fn new(manifest: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = loop {
            let candidate = std::env::temp_dir().join(format!(
                "nessa-ffi-regex-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("failed to create temp package dir: {e}"),
            }
        };
        let pkg = Self(dir);
        pkg.write("package.toml", manifest);
        pkg
    }

    fn write(&self, rel: &str, contents: &str) {
        let path = self.0.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }
}

impl Drop for TempPackage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_pkg_i64(pkg: &TempPackage) -> i64 {
    let result = Driver::new().compile_package(&pkg.0);
    assert!(!result.has_errors, "diagnostics: {:?}", result.diagnostics);
    let artifact = result.into_artifact().unwrap();
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .expect("package must have an entry point");
    let task = engine.vm_mut().spawn_root(entry);
    match engine.vm_mut().run() {
        interpreter::VmResult::Finished => {
            assert_eq!(engine.vm.active_stack_count(), 0);
            engine
                .vm_mut()
                .task_result_number(task)
                .unwrap()
                .to_i64_checked()
                .unwrap()
        }
        interpreter::VmResult::Error(err) => panic!("unexpected VM error: {err:?}"),
    }
}

fn scan_cdylib_in_ancestors(exe: &Path, filename: &str) -> Option<PathBuf> {
    for ancestor in exe.ancestors() {
        let direct = ancestor.join(filename);
        if direct.is_file() {
            return Some(direct);
        }
        let in_deps = ancestor.join("deps").join(filename);
        if in_deps.is_file() {
            return Some(in_deps);
        }
    }
    None
}

fn find_regex_cdylib() -> PathBuf {
    let filename = format!(
        "{}nessa_regex_ffi{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let exe = std::env::current_exe().expect("current_exe");
    if let Some(found) = scan_cdylib_in_ancestors(&exe, &filename) {
        return found;
    }

    // When `cargo test -p nessa-test` runs standalone after `cargo clean`, Cargo only builds
    // the `rlib` target of `regex_ffi`. Link its `cdylib` target on demand into the same target dir.
    let target_dir = exe
        .ancestors()
        .find(|p| {
            matches!(
                p.file_name().and_then(|n| n.to_str()),
                Some("debug" | "release")
            )
        })
        .and_then(Path::parent)
        .expect("target directory parent of test binary");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = std::process::Command::new(cargo)
        .args(["build", "-p", "regex_ffi", "--lib"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("CARGO_TARGET_DIR", target_dir)
        .status()
        .expect("spawn cargo build -p regex_ffi");
    assert!(
        status.success(),
        "cargo build -p regex_ffi failed: {status}"
    );

    if let Some(found) = scan_cdylib_in_ancestors(&exe, &filename) {
        return found;
    }
    panic!(
        "compiled cdylib `{filename}` not found in ancestors of `{}`",
        exe.display()
    );
}

// ---------------------------------------------------------------------------
// 1. Compact GC Leaf `Bytes` Tests
// ---------------------------------------------------------------------------

#[test]
fn bytes_zero_copy_slice_cow_utf8_find_and_u64_le() {
    let source = r#"
use std.bytes.*

fn main() -> i64 {
    let base = Bytes.from_string("hello world!")
    if base.len() != 12 { return -1 }

    -- Zero-copy O(1) slice shares buffer until mutated
    let mid = base.slice(6, 11)
    if mid.to_utf8() != "world" { return -2 }

    -- Mutating slice detaches via COW without altering `base`
    mid.set(0, 87)
    if mid.to_utf8() != "World" { return -3 }
    if base.to_utf8() != "hello world!" { return -4 }

    -- Mutating `base` after taking another slice detaches `base` without altering `sub2`
    let sub2 = base.slice(0, 5)
    base.set(0, 72)
    base.push(33)
    if sub2.to_utf8() != "hello" { return -5 }
    if base.to_utf8() != "Hello world!!" { return -6 }

    -- `extend` / `append`, `clear`, `copy`, `from_list`, `to_list`, `find`, `read_u64_le`
    let buf = Bytes.with_capacity(16)
    buf.extend_str("abc")
    buf.`extend`(Bytes.from_list([100, 101]))
    buf.append(Bytes.from_list([102, 103, 104]))
    if buf.len() != 8 { return -7 }
    if buf.find_str("def") != 3 { return -8 }
    if buf.find(Bytes.from_string("gh"), start = 4) != 6 { return -9 }
    if buf.find_str("zzz") != null { return -10 }

    let word = buf.read_u64_le(0)
    -- 'a'=0x61, 'b'=0x62, 'c'=0x63, 'd'=0x64, 'e'=0x65, 'f'=0x66, 'g'=0x67, 'h'=0x68
    if word != 0x6867666564636261 { return -11 }

    let cloned = buf.copy()
    buf.clear()
    if not buf.is_empty() { return -12 }
    if cloned.len() != 8 { return -13 }
    let as_list = cloned.slice(0, 3).to_list()
    if as_list.len() != 3 or as_list.get(0).as(i64) != 97 { return -14 }

    -- Invalid UTF-8 returns null in `to_utf8()` and U+FFFD in `to_utf8_lossy()`
    let raw_bin = Bytes.from_list([0xff, 0x41, 0xfe])
    if raw_bin.to_utf8() != null { return -15 }
    if not raw_bin.to_utf8_lossy().contains("A") { return -16 }

    42
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn bytes_los_survives_32mib_gc_churn_and_preserves_traits_and_legacy_helpers() {
    let source = r#"
use std.bytes.*
use std.collections.HashMap
use std.runtime.collect_garbage

fn main() -> i64 {
    -- Allocate > 1.2 MiB LOS ByteBuffer filled with pointer-like bit patterns
    let big_len: usize = 1310720
    let big = Bytes.zeroed(big_len)
    var i: usize = 0
    while i < big_len {
        big.set(i, ((i * 73 + 19) % 251).as(u8))
        i += 4096
    }
    big.set(big_len - 1, 0xab)

    -- Churn > 36 MiB across Immix + LOS spaces and force multiple full collections
    var round: i64 = 0
    while round < 36 {
        let scratch = Bytes.zeroed(1048576)
        scratch.set(0, round.as(u8))
        scratch.set(1048575, 0xfe)
        if round % 9 == 8 {
            let _ = collect_garbage()
        }
        round += 1
    }
    let _ = collect_garbage()

    -- Verify the 1.25 MiB LOS buffer survived completely intact
    var check_i: usize = 0
    while check_i < big_len {
        let expected = ((check_i * 73 + 19) % 251).as(u8)
        if big.get(check_i) != expected {
            return -1
        }
        check_i += 4096
    }
    if big.get(big_len - 1) != 0xab { return -2 }

    -- Verify Display, Eq, PartialEq, and Hash (`HashMap` key lookup across slices!)
    if big.to_string() != "Bytes(len=1310720)" { return -3 }
    let m = HashMap.new()
    let k1 = Bytes.from_string("alpha_beta_gamma")
    let k2 = Bytes.from_string("xx_alpha_beta_gamma_yy").slice(3, 19)
    if k1 != k2 { return -4 }
    m.insert(k1, 99)
    if m.get(k2).as(i64) != 99 { return -5 }

    -- Verify `std.bytes` pack/unpack/hex/base64 helpers
    let packed = pack("<I2I4", 513, 0x12345678)! { _ => panic("pack failed") }
    if (pack_size("<I2I4")! { _ => -1 }) != 6 { return -7 }
    let unpacked = unpack("<I2I4", packed)! { _ => panic("unpack failed") }
    let vals = unpacked.0
    let next_pos = unpacked.1
    if next_pos != (6).as(usize) or vals.get(0).as(i64) != 513 or vals.get(1).as(i64) != 0x12345678 {
        return -9
    }
    let hex = to_hex(Bytes.from_list([0xde, 0xad, 0xbe, 0xef]))
    if hex != "deadbeef" { return -10 }
    let unhex = from_hex(hex)! { _ => Bytes.new() }
    if unhex.len() != (4).as(usize) { return -11 }
    let b64 = to_base64(Bytes.from_list([78, 101, 115, 115, 97]))
    if b64 != "TmVzc2E=" { return -12 }
    let unb64 = from_base64(b64)! { _ => Bytes.new() }
    if to_utf8(unb64) != "Nessa" { return -13 }

    42
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

// ---------------------------------------------------------------------------
// 2. General-Purpose Safe C FFI, Manifest Gating & Guard Tests
// ---------------------------------------------------------------------------

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
#[test]
fn ffi_real_cdylib_dlopen_and_handle_validation() {
    let cdylib_path = find_regex_cdylib();
    let escaped_path = cdylib_path.to_string_lossy().replace('\\', "\\\\");

    let pkg = TempPackage::new(
        r#"[package]
name = "cdylib_app"
domain = "org.test"
version = "0.1.0"
type = "exe"

[ffi]
allow = true
"#,
    );
    pkg.write(
        "src/main.ns",
        &format!(
            r#"use std.bytes.Bytes

const ffi_dlopen: fn(String) -> i64 = .__ffi_dlopen'builtin
const ffi_dlsym: fn(i64, String, String) -> i64 = .__ffi_dlsym'builtin
const ffi_call6: fn(i64, String, u32, usize, usize, Bytes) -> i64 = .__ffi_call6'builtin
const ffi_call4: fn(i64, i64, String, usize, u32) -> i64 = .__ffi_call4'builtin
const ffi_call_list: fn(i64, List) -> Unit = .__ffi_call'builtin

fn main() -> i64 {{
    -- Missing library returns negative code cleanly without panic
    if ffi_dlopen("/nonexistent/path/libdefinitely_missing_xyz.so") >= 0 {{
        return -1
    }}

    -- Real OS dlopen on compiled `libnessa_regex_ffi` cdylib!
    let lib = ffi_dlopen("{escaped_path}")
    if lib <= 0 {{
        return -2
    }}
    let sym_compile = ffi_dlsym(lib, "nessa_regex_compile", "slice_ro,u32,usize,usize,buf_mut->i64")
    let sym_is_match = ffi_dlsym(lib, "nessa_regex_is_match", "i64,slice_ro,usize,u32->i32")
    let sym_free = ffi_dlsym(lib, "nessa_regex_free", "i64->void")
    if sym_compile <= 0 or sym_is_match <= 0 or sym_free <= 0 {{
        return -3
    }}

    -- Non-existent symbol returns negative code cleanly
    if ffi_dlsym(lib, "symbol_that_does_not_exist_12345", "i64->i64") >= 0 {{
        return -4
    }}

    let err_buf = Bytes.zeroed(256)
    let re_handle = ffi_call6(sym_compile, "^cdylib-[0-9]+$", 8, 1048576, 1048576, err_buf)
    if re_handle <= 0 {{
        return -5
    }}
    let matched = ffi_call4(sym_is_match, re_handle, "cdylib-2026", 0, 0)
    let not_matched = ffi_call4(sym_is_match, re_handle, "cdylib-nope", 0, 0)
    ffi_call_list(sym_free, [re_handle])
    -- Calling after free returns error code (-1) cleanly instead of crashing
    let after_free = ffi_call4(sym_is_match, re_handle, "cdylib-2026", 0, 0)
    if matched == 1 and not_matched == 0 and after_free == -1 {{
        42
    }} else {{
        -6
    }}
}}
"#
        ),
    );
    assert_eq!(run_pkg_i64(&pkg), 42);

    // Forged symbol handle in `__ffi_call` must fail with a clean runtime error, never UB.
    let bad_pkg = TempPackage::new(
        r#"[package]
name = "forged_handle"
domain = "org.test"
version = "0.1.0"
type = "exe"

[ffi]
allow = true
"#,
    );
    bad_pkg.write(
        "src/main.ns",
        r#"
const ffi_call: fn(i64, List) -> i64 = .__ffi_call'builtin
fn main() -> i64 {
    ffi_call(999999, [1])
}
"#,
    );
    match Driver::new().run_package(&bad_pkg.0) {
        RunResult::RuntimeError(msg) => {
            assert!(
                msg.contains("invalid")
                    || msg.contains("InvalidArgument")
                    || msg.contains("symbol"),
                "unexpected runtime error message: {msg}"
            );
        }
        other => panic!("expected clean RuntimeError on forged FFI handle, got: {other:?}"),
    }
}

#[test]
fn ffi_manifest_capability_and_library_allowlist_gating() {
    // 1. Package without `[ffi]` cannot access `.__ffi_dlopen'builtin`
    let unprivileged = TempPackage::new(
        r#"[package]
name = "unpriv"
domain = "org.test"
version = "0.1.0"
type = "exe"
"#,
    );
    unprivileged.write(
        "src/main.ns",
        r#"
const dlopen: fn(String) -> i64 = .__ffi_dlopen'builtin
fn main() -> i64 { dlopen("nessa_regex_ffi") }
"#,
    );
    let res1 = Driver::new().compile_package(&unprivileged.0);
    assert!(res1.has_errors, "unprivileged package must be rejected");
    assert!(
        res1.diagnostics
            .iter()
            .any(|d| d.message.contains("[ffi] allow = true")),
        "expected `[ffi] allow = true` diagnostic, got: {:?}",
        res1.diagnostics
    );

    // 2. Package WITH `[ffi] allow = true` STILL cannot access non-FFI `'builtin` views
    let non_ffi_builtin = TempPackage::new(
        r#"[package]
name = "only_ffi"
domain = "org.test"
version = "0.1.0"
type = "exe"

[ffi]
allow = true
"#,
    );
    non_ffi_builtin.write(
        "src/main.ns",
        r#"
const raw_bytes_new: fn(usize) -> Any = .__bytes_new'builtin
fn main() -> i64 { 0 }
"#,
    );
    let res2 = Driver::new().compile_package(&non_ffi_builtin.0);
    assert!(
        res2.has_errors,
        "non-FFI `'builtin` must remain restricted to std"
    );

    // 3. Package with `[ffi] libraries = ["nessa_regex_ffi"]` rejects unauthorized libraries
    let restricted = TempPackage::new(
        r#"[package]
name = "restricted_ffi"
domain = "org.test"
version = "0.1.0"
type = "exe"

[ffi]
allow = true
libraries = ["nessa_regex_ffi"]
"#,
    );
    restricted.write(
        "src/main.ns",
        r#"
private const dlopen: fn(String) -> i64 = .__ffi_dlopen'builtin
fn main() -> i64 { dlopen("libc.so.6") }
"#,
    );
    let res3 = Driver::new().compile_package(&restricted.0);
    assert!(res3.has_errors, "unlisted library must be rejected");
    assert!(
        res3.diagnostics
            .iter()
            .any(|d| d.message.contains("not permitted by `[ffi] libraries`")),
        "expected allowlist diagnostic, got: {:?}",
        res3.diagnostics
    );
}

#[test]
fn native_frame_guard_rejects_effect_dispatch_and_continuation_capture() {
    // 1. Non-capturing algebraic effect dispatch across active native frame -> `EffectAcrossNativeFrame`
    let effect_src = r#"
effect ask() -> i64

fn main() -> i64 {
    ask()# { ask() => 42 }
}
"#;
    let compiled = Driver::new().compile(effect_src);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .unwrap();
    let _task = engine.vm_mut().spawn_root(entry);
    let guarded = engine.vm_mut().with_native_frame(|vm| match vm.run() {
        interpreter::VmResult::Finished => Ok(()),
        interpreter::VmResult::Error(err) => Err(err),
    });
    assert!(
        matches!(guarded, Err(interpreter::VmError::EffectAcrossNativeFrame)),
        "expected EffectAcrossNativeFrame, got {guarded:?}"
    );
    assert_eq!(engine.vm.native_frame_depth(), 0);

    // 2. Continuation-capturing coroutine yield across active native frame -> `EffectAcrossNativeFrame`
    let cont_src = r#"
use std.coroutine.*

fn main() -> i64 {
    let co = Coroutine.create(|_: Any| -> Any {
        yield_val(value = 42)
    })
    (co.try_resume() ! { _ => -1 }).as(i64)
}
"#;
    let compiled_cont = Driver::new().compile(cont_src);
    assert!(!compiled_cont.has_errors, "{:?}", compiled_cont.diagnostics);
    let cont_artifact = compiled_cont.into_artifact().unwrap();
    let mut engine2 = initialization::Engine::with_defaults();
    let entry2 = driver::install_artifact(engine2.vm_mut(), cont_artifact)
        .unwrap()
        .unwrap();
    let _task2 = engine2.vm_mut().spawn_root(entry2);
    let guarded_cont = engine2.vm_mut().with_native_frame(|vm| match vm.run() {
        interpreter::VmResult::Finished => Ok(()),
        interpreter::VmResult::Error(err) => Err(err),
    });
    assert!(
        matches!(
            guarded_cont,
            Err(interpreter::VmError::EffectAcrossNativeFrame)
        ),
        "expected EffectAcrossNativeFrame on continuation capture, got {guarded_cont:?}"
    );
    assert_eq!(engine2.vm.native_frame_depth(), 0);
}

// ---------------------------------------------------------------------------
// 3. Zero-Privilege FFI `regex` Library Tests
// ---------------------------------------------------------------------------

#[test]
fn regex_single_file_features_utf8_captures_flags_split_replace_and_batch_iter() {
    let source = r##"
use regex.*
use std.bytes.Bytes

fn main() -> i64 {
    -- 1. Alternations, greedy vs non-greedy `+?`
    let ng = Regex.compile("<.+?>")! { _ => panic("ng") }
    let m1 = ng.find("<a><b>")
    if m1 == null or m1.as(SpanMatch).text != "<a>" { return -2 }

    let alt = Regex.compile("cat|dog|fox")! { _ => panic("alt") }
    if not alt.is_match("quick brown fox") or alt.is_match("lazy bear") { return -4 }

    -- 2. Named captures & positional captures
    let date_re = Regex.compile("(?P<year>\\d{{4}})-(?P<month>\\d{{2}})-(?P<day>\\d{{2}})")! {
        _ => panic("date_re")
    }
    let caps_opt = date_re.captures("release: 2026-11-15!")
    if caps_opt == null { return -6 }
    let caps = caps_opt.as(Captures)
    if caps.get(0) != "2026-11-15" { return -7 }
    if caps.name("year") != "2026" or caps.name("month") != "11" or caps.name("day") != "15" {
        return -8
    }
    if caps.name("nonexistent") != null { return -9 }

    -- 3. Flags: case_insensitive, multi_line, dot_matches_new_line
    let ci = Regex.compile("^hello.*world$", case_insensitive = true, multi_line = true, dot_matches_new_line = true)! {
        _ => panic("ci")
    }
    if not ci.is_match("prefix\nHeLLo\n\nWoRLd\nsuffix") { return -11 }

    -- 4. Multi-byte UTF-8 byte offsets ("é"=2B, "日本語"=9B, "🦀"=4B)
    let utf8_re = Regex.compile("🦀(\\d+)")! { _ => panic("utf8_re") }
    let text = "é日本語🦀42!"
    -- byte offsets: "é"(2) + "日本語"(9) = 11; "🦀42" is 11..17
    let um = utf8_re.find(text)
    if um == null { return -13 }
    let u_span = um.as(SpanMatch)
    if u_span.start != 11 or u_span.end != 17 or u_span.text != "🦀42" { return -14 }

    -- 5. > 100 matches crossing the 64-match Span Buffer FFI batch boundary!
    var many_src = ""
    var idx: i64 = 0
    while idx < 150 {
        many_src = many_src ++ "item" ++ idx.to_string() ++ ","
        idx += 1
    }
    let num_re = Regex.compile("\\d+")! { _ => panic("num_re") }
    let found = num_re.find_all(many_src).to_list()
    if found.len() != 150 { return -16 }
    if found.get(0).as(SpanMatch).text != "0" { return -17 }
    if found.get(63).as(SpanMatch).text != "63" { return -18 }
    if found.get(64).as(SpanMatch).text != "64" { return -19 }
    if found.get(149).as(SpanMatch).text != "149" { return -20 }

    -- 6. `split` (unbounded and bounded `limit`)
    let sep = Regex.compile("\\s*,\\s*")! { _ => panic("sep") }
    let parts = sep.split("a, b ,  c,d")
    if parts.len() != 4 or parts.get(1).as(String) != "b" or parts.get(3).as(String) != "d" {
        return -22
    }
    let limited = sep.split("a, b, c, d", limit = 2)
    if limited.len() != 2 or limited.get(1).as(String) != "b, c, d" { return -23 }

    -- 7. `replace` and `replace_all` with `$1`, `${year}`, `$$`
    let rep_named = date_re.replace_all(
        "2025-01-09 & 2026-11-15",
        "$3/$2/${{year}}[$$]"
    )
    if rep_named != "09/01/2025[$] & 15/11/2026[$]" { return -24 }
    let first_only = num_re.replace("v1 and v2", "#$0")
    if first_only != "v#1 and v2" { return -25 }

    -- 8. Binary `Bytes` matching (`unicode = false` with arbitrary non-UTF-8 bytes)
    let bin_re = Regex.compile("\\xff\\x00[A-Z]+", unicode = false)! { _ => panic("bin_re") }
    let payload = Bytes.from_list([0x10, 0xff, 0x00, 65, 66, 67, 0xfe])
    if not bin_re.is_match_bytes(payload) { return -27 }
    let bm_opt = bin_re.find_bytes(payload)
    if bm_opt == null { return -28 }
    let bm = bm_opt.as(ByteMatch)
    if bm.start != 1 or bm.end != 6 or bm.len() != 5 { return -29 }

    -- 9. Syntax error & `CompiledTooLarge` error classification
    let bad_syn = Regex.compile("[unclosed") match {
        _! => 0,
        error RegexError.InvalidSyntax(_) => 1,
        error RegexError.CompiledTooLarge(_) => 0,
    }
    if bad_syn != 1 { return -30 }

    let too_big = Regex.compile("\\w{{200}}", size_limit = 256) match {
        _! => 0,
        error RegexError.CompiledTooLarge(lim) => if lim == 256 { 1 } else { 0 },
        error RegexError.InvalidSyntax(_) => 0,
    }
    if too_big != 1 { return -31 }

    42
}
"##;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn regex_redos_linear_time_immunity() {
    // Compile once, then time matching `(a+)+b` against `"a".repeat(50_000)`!
    let source = r#"
use regex.Regex
use std.time.Instant

fn main() -> i64 {
    let re = Regex.compile("(a+)+b")! { _ => panic("compile failed") }
    let hay = "a".repeat(50000)
    let t0 = Instant.now()
    let matched = re.is_match(hay)
    let elapsed_ms = t0.elapsed().as_millis()
    if matched { return -2 }
    elapsed_ms
}
"#;
    let start = Instant::now();
    let in_vm_ms = common::run_value(source).unwrap();
    let total_wall = start.elapsed();
    assert!(
        (0..100).contains(&in_vm_ms),
        "ReDoS pattern `(a+)+b` on 50,000 'a's must finish in < 100ms, took {in_vm_ms}ms (wall {total_wall:?})"
    );
}

#[test]
fn regex_package_dependency_gc_weak_finalizer_and_standalone_nsbc_archive() {
    // 1. Verify weak GC finalizer reclaims unclosed native regex handles during live VM GC!
    let freed_before = nessa_regex_ffi::freed_handle_count();
    let gc_src = r#"
use regex.Regex
use std.bytes.Bytes
use std.runtime.collect_garbage

fn allocate_ephemeral_regexes() -> i64 {
    var count: i64 = 0
    var i: i64 = 0
    while i < 40 {
        let re = Regex.compile("item_" ++ i.to_string())! { _ => panic("compile failed") }
        if re.is_match("prefix item_" ++ i.to_string()) {
            count += 1
        }
        i += 1
    }
    count
}

fn main() -> i64 {
    let matched = allocate_ephemeral_regexes()
    -- Churn heap and force GC so `_RegexGuard` objects become unreachable and finalize
    var round: i64 = 0
    while round < 16 {
        let _ = Bytes.zeroed(524288)
        round += 1
    }
    let _ = collect_garbage()
    let _ = collect_garbage()
    matched
}
"#;
    let compiled = driver::Driver::new().compile(gc_src);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), compiled.into_artifact().unwrap())
        .unwrap()
        .unwrap();
    let task = engine.vm_mut().spawn_root(entry);
    assert!(matches!(
        engine.vm_mut().run(),
        interpreter::VmResult::Finished
    ));
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 40);
    let freed_during_live_vm_gc = nessa_regex_ffi::freed_handle_count() - freed_before;
    assert!(
        freed_during_live_vm_gc >= 38,
        "weak GC finalizers must reclaim ephemeral regex handles before VM drop (reclaimed={freed_during_live_vm_gc})"
    );
    drop(engine);

    // 2. Verify `package.toml` with `[dependencies] "nessa/regex" = "0.1.0"` AND
    // standalone `.nsbc` archive execution after deleting ALL `.ns` source files!
    let pkg = TempPackage::new(
        r#"[package]
name = "regex_consumer"
domain = "org.test"
version = "0.1.0"
type = "exe"

[dependencies]
"nessa/regex" = "0.1.0"
"#,
    );
    pkg.write(
        "src/main.ns",
        r#"use regex.*

fn run_app() -> !RegexError i64 {
    let re = Regex.compile("(?P<k>[a-z]+)=(\\d+)")!
    let out = re.replace_all("x=10, y=32", "${{k}}:$2")
    if out != "x:10, y:32" { return -2 }
    let caps = re.captures("total=42").as(Captures)
    let val = caps.get(2).as(String).parse_i64().as(i64)
    re.close()
    val
}

fn main() -> i64 {
    run_app()! { _ => -1 }
}
"#,
    );

    let archive_path = pkg.0.join("standalone_regex_app.nsbc");
    Driver::new()
        .compile_package_to_archive(&pkg.0, &archive_path)
        .expect("compile_package_to_archive must succeed");

    // Delete `src/` and `package.toml` so ONLY the `.nsbc` archive remains!
    std::fs::remove_dir_all(pkg.0.join("src")).unwrap();
    std::fs::remove_file(pkg.0.join("package.toml")).unwrap();

    let archive_bytes = std::fs::read(&archive_path).unwrap();
    let loaded_artifact = nsbc_io::read_artifact(&archive_bytes).unwrap();
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), loaded_artifact)
        .unwrap()
        .unwrap();
    let task = engine.vm_mut().spawn_root(entry);
    assert!(matches!(
        engine.vm_mut().run(),
        interpreter::VmResult::Finished
    ));
    let answer = engine
        .vm_mut()
        .task_result_number(task)
        .unwrap()
        .to_i64_checked()
        .unwrap();
    assert_eq!(answer, 42);

    // 3. Verify dependency isolation: a `package.toml` WITHOUT `"nessa/regex"` cannot `use regex.*`
    let isolated = TempPackage::new(
        r#"[package]
name = "isolated_pkg"
domain = "org.test"
version = "0.1.0"
type = "exe"
"#,
    );
    isolated.write(
        "src/main.ns",
        r#"use regex.*
fn main() -> i64 { 0 }
"#,
    );
    let isolated_res = Driver::new().compile_package(&isolated.0);
    assert!(
        isolated_res.has_errors,
        "package omitting `nessa/regex` from [dependencies] must not resolve `use regex.*`"
    );
}
