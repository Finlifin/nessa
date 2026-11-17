//! End-to-end tests for the reactivated `std.ffi` declarative & imperative C-ABI API
//! (`docs/ffi.md`), compile-time capability/allowlist/signature validation, mixed GP+XMM
//! hardware dispatch, `CStr`/`Ptr`/`SliceRo`/`BufMut` marshalling, GC weak finalizers,
//! and standalone `.nsbc` archive roundtrips.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use driver::Driver;

struct TempPackage(PathBuf);

impl TempPackage {
    fn new(manifest: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = loop {
            let candidate = std::env::temp_dir().join(format!(
                "nessa-stdlib-ffi-test-{}-{}",
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
    let bytes = nsbc_io::write_artifact(&artifact).expect("serialize .nsbc");
    let reloaded = nsbc_io::read_artifact(&bytes).expect("deserialize .nsbc");
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), reloaded)
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

fn find_regex_cdylib() -> PathBuf {
    let filename = format!(
        "{}nessa_regex_ffi{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let exe = std::env::current_exe().expect("current_exe");
    for ancestor in exe.ancestors() {
        let direct = ancestor.join(&filename);
        if direct.is_file() {
            return direct;
        }
        let in_deps = ancestor.join("deps").join(&filename);
        if in_deps.is_file() {
            return in_deps;
        }
    }
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
    for ancestor in exe.ancestors() {
        let direct = ancestor.join(&filename);
        if direct.is_file() {
            return direct;
        }
        let in_deps = ancestor.join("deps").join(&filename);
        if in_deps.is_file() {
            return in_deps;
        }
    }
    panic!("could not find {filename}");
}

#[test]
fn declarative_ffi_load_and_must_load_cover_mixed_gp_xmm_cstr_ptr_and_nsbc_roundtrip() {
    let pkg = TempPackage::new(
        r#"[package]
name = "ffi_decl"
domain = "acme"
version = "0.1.0"
type = "exe"

[ffi]
allow = true
libraries = ["__nessa_ffi_test"]
"#,
    );
    pkg.write(
        "src/main.ns",
        r#"use std.ffi
use std.ffi.types.{CStr, Ptr}

const host = ffi.must_load("__nessa_ffi_test") {
    test_add_i32: fn(i32, i32) -> i32,
    test_narrow_mix: ffi.func(fn(i8, u8, i16, u16) -> i32, callconv = .c),
    test_f32_math: fn(f32, f32) -> f32,
    test_f64_mix: ffi.func(fn(i32, f64, f32, i64) -> f64, callconv = .system),
    test_cstr_len: fn(CStr) -> usize,
    test_cstr_greeting: fn() -> CStr,
    test_ptr_offset: fn(Ptr, isize) -> Ptr,
    test_many_args: fn(i64, f64, i32, f32, i64, f64, i32, i64) -> f64,
}

fn run_decl_checks() -> !ffi.FfiError i64 {
    let sum32: i32 = host.test_add_i32((19).as(i32), (23).as(i32))
    assert_eq(sum32, (42).as(i32))

    let narrow: i32 = host.test_narrow_mix((-5).as(i8), (200).as(u8), (-100).as(i16), (1000).as(u16))
    assert_eq(narrow, (1095).as(i32))

    let hyp: f32 = host.test_f32_math((3.0).as(f32), (4.0).as(f32))
    assert_eq(hyp, (5.0).as(f32))

    let mix: f64 = host.test_f64_mix((4).as(i32), 2.5, (1.5).as(f32), 10)
    assert(mix.approx_eq(21.5))

    let n_bytes: usize = host.test_cstr_len(CStr.new("nessa-ffi"))
    assert_eq(n_bytes, (9).as(usize))

    let greet: CStr = host.test_cstr_greeting()
    assert_eq(greet.as_str(), "hello from c")

    let base = Ptr.from_addr((4096).as(usize))
    let moved: Ptr = host.test_ptr_offset(base, (-96).as(isize))
    assert_eq(moved.addr, (4000).as(usize))

    assert(Ptr.NULL.is_null())
    let null_err = Ptr.NULL.read_cstr() ! {
        ffi.FfiError.NullPointer(_) => "caught-null",
        _ => "wrong-err",
    }
    assert_eq(null_err, "caught-null")

    let spill: f64 = host.test_many_args(1, 2.5, (3).as(i32), (4.5).as(f32), 5, 6.0, (7).as(i32), 8)
    assert(spill.approx_eq(37.0))

    let checked = ffi.load("__nessa_ffi_test") {
        test_add_i32: fn(i32, i32) -> i32,
    }!
    assert(checked.is_open())
    assert(checked.has_symbol("test_add_i32"))
    let dyn_add = checked.bind("test_add_i32", fn(i32, i32) -> i32)!
    let r2: i32 = dyn_add((10).as(i32), (32).as(i32))
    checked.close()
    assert(not checked.is_open())
    r2.as(i64)
}

fn main() -> i64 {
    run_decl_checks() ! { _ => -1 }
}
"#,
    );
    assert_eq!(run_pkg_i64(&pkg), 42);
}

#[test]
fn std_ffi_slice_buffers_and_gc_weak_finalizers_work_against_real_cdylib() {
    let cdylib = find_regex_cdylib();
    let cdylib_escaped = cdylib.to_string_lossy().replace('\\', "/");

    let pkg = TempPackage::new(&format!(
        r#"[package]
name = "ffi_cdylib"
domain = "acme"
version = "0.1.0"
type = "exe"

[ffi]
allow = true
libraries = ["{cdylib_escaped}"]
"#
    ));
    pkg.write(
        "src/main.ns",
        &format!(
            r#"use std.ffi
use std.ffi.types.{{BufMut, SliceRo}}
use std.runtime

const regex_lib = ffi.must_load("{cdylib_escaped}") {{
    nessa_regex_compile: fn(SliceRo, u32, usize, usize, BufMut) -> i64,
    nessa_regex_is_match: fn(i64, SliceRo, usize, u32) -> i32,
    nessa_regex_group_count: fn(i64) -> isize,
}}

struct RegexBox {{
    raw: i64,
}}

fn alloc_temp_regex() -> i64 {{
    let err_buf = Bytes.zeroed(128)
    let pat = "ab+c".to_bytes()
    let lim: usize = 1048576
    let handle = regex_lib.nessa_regex_compile(SliceRo.new(pat), (0).as(u32), lim, lim, BufMut.new(err_buf))
    assert(handle > 0)
    let matched = regex_lib.nessa_regex_is_match(handle, SliceRo.new("xxabbbcyy"), (0).as(usize), (0).as(u32))
    assert_eq(matched, (1).as(i32))
    let boxed = RegexBox {{ raw: handle }}
    regex_lib.attach_finalizer(boxed, "nessa_regex_free", handle) ! {{
        _ => panic("attach_finalizer failed"),
    }}
    handle
}}

fn run_cdylib_checks() -> !ffi.FfiError i64 {{
    let checked_lib = ffi.load("{cdylib_escaped}") {{
        nessa_regex_group_count: fn(i64) -> isize,
    }}!
    let temp_handle = alloc_temp_regex()
    assert_eq(checked_lib.nessa_regex_group_count(temp_handle), (1).as(isize))
    let _ = runtime.collect_garbage()
    let _ = runtime.collect_garbage()
    assert_eq(checked_lib.nessa_regex_group_count(temp_handle), (-1).as(isize))
    let _ = checked_lib.close()
    99
}}

fn main() -> i64 {{
    run_cdylib_checks() ! {{ _ => -1 }}
}}
"#
        ),
    );
    assert_eq!(run_pkg_i64(&pkg), 99);

    let missing_pkg = TempPackage::new(
        r#"[package]
name = "ffi_missing_lib"
domain = "acme"
version = "0.1.0"
type = "exe"

[ffi]
allow = true
"#,
    );
    missing_pkg.write(
        "src/main.ns",
        r#"use std.ffi
fn main() -> i64 {
    let missing = ffi.load("__nonexistent_nessa_lib_xyz__") {
        foo: fn() -> i32,
    } ! {
        ffi.FfiError.LibraryNotFound(_) => null,
        _ => panic("expected LibraryNotFound"),
    }
    assert_eq(missing, null)
    7
}
"#,
    );
    assert_eq!(run_pkg_i64(&missing_pkg), 7);
}

#[test]
fn std_ffi_enforces_static_capability_allowlist_signatures_and_bound_member_checks() {
    // 1. Missing `[ffi] allow = true` rejected at compile time.
    let no_cap = TempPackage::new(
        r#"[package]
name = "no_ffi"
domain = "acme"
version = "0.1.0"
type = "exe"
"#,
    );
    no_cap.write(
        "src/main.ns",
        r#"use std.ffi
fn main() {
    let _ = ffi.must_load("__nessa_ffi_test") {
        test_add_i32: fn(i32, i32) -> i32,
    }
}
"#,
    );
    let res1 = Driver::new().compile_package(&no_cap.0);
    assert!(res1.has_errors);
    assert!(
        res1.diagnostics
            .iter()
            .any(|d| d.message.contains("[ffi] allow = true")),
        "expected capability diagnostic, got: {:?}",
        res1.diagnostics
    );

    // 2. Unlisted library rejected by `[ffi] libraries` allowlist, and escaping `ffi.load` value rejected.
    let allowlist_pkg = TempPackage::new(
        r#"[package]
name = "restricted_ffi"
domain = "acme"
version = "0.1.0"
type = "exe"

[ffi]
allow = true
libraries = ["__nessa_ffi_test"]
"#,
    );
    allowlist_pkg.write(
        "src/main.ns",
        r#"use std.ffi
fn main() {
    let escaped = ffi.load
    let _ = ffi.must_load("unauthorized_lib") {
        test_add_i32: fn(i32, i32) -> i32,
    }
    let _ = escaped
}
"#,
    );
    let res2 = Driver::new().compile_package(&allowlist_pkg.0);
    assert!(res2.has_errors);
    assert!(
        res2.diagnostics
            .iter()
            .any(|d| d.message.contains("allowlist")),
        "expected allowlist diagnostic, got: {:?}",
        res2.diagnostics
    );
    assert!(
        res2.diagnostics
            .iter()
            .any(|d| d.message.contains("cannot escape")),
        "expected value-escape diagnostic, got: {:?}",
        res2.diagnostics
    );

    // 3. Invalid Nessa GC type (`List`) in FFI signature, unknown bound symbol `c.typo`, and wrong arg type.
    let bad_sig = TempPackage::new(
        r#"[package]
name = "bad_sig"
domain = "acme"
version = "0.1.0"
type = "exe"

[ffi]
allow = true
"#,
    );
    bad_sig.write(
        "src/main.ns",
        r#"use std.ffi
fn main() {
    let c = ffi.must_load("__nessa_ffi_test") {
        test_add_i32: fn(i32, i32) -> i32,
        bad_fn: fn(List) -> i32,
    }
    let _ = c.typo((1).as(i32))
    let _ = c.test_add_i32("wrong", (2).as(i32))
}
"#,
    );
    let res3 = Driver::new().compile_package(&bad_sig.0);
    assert!(res3.has_errors);
    assert!(
        res3.diagnostics
            .iter()
            .any(|d| d.message.contains("unsupported FFI parameter")),
        "expected unsupported FFI parameter diagnostic, got: {:?}",
        res3.diagnostics
    );
    assert!(
        res3.diagnostics
            .iter()
            .any(|d| d.message.contains("unknown FFI symbol `typo`")),
        "expected unknown FFI symbol diagnostic, got: {:?}",
        res3.diagnostics
    );
    assert!(
        res3.diagnostics
            .iter()
            .any(|d| d.message.contains("type mismatch")),
        "expected type mismatch diagnostic, got: {:?}",
        res3.diagnostics
    );
}
