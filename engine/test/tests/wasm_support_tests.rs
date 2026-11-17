//! Comprehensive end-to-end integration tests for WebAssembly support across Nessa:
//! 1. `std.wasm` (validation, inspection, Wasm 2.0 multi-value, tail calls, `simd128`,
//!    linear memory, globals, fuel/memory sandboxing, and `run_wasi`).
//! 2. GC-rooted Nessa closure host imports across moving MMTk collections &
//!    `EffectAcrossNativeFrame` continuation-capture protection.
//! 3. `[[ffi.wasm]]` package manifests, `SectionKind::Wasm = 7` `.nsbc` archive roundtrips,
//!    `Opcode::CallWasm = 0x8B` disassembly, `-O3` optimization, and Tier-1/Tier-2 JIT execution.
//! 4. Nessa-to-WebAssembly AOT compilation (`wasm32-wasi` & `wasm32-freestanding`), CLI
//!    `nessa build --target` / `nessa run *.wasm` / `nessa disasm *.wasm`, and live cross-runtime
//!    verification in Node.js (V8 WebAssembly + `node:wasi`).

use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use driver::{Driver, JitMode, OptLevel, RunResult, WasmCompileOptions, WasmTarget};
use wasm_encoder::{
    CodeSection, ConstExpr, ExportKind, ExportSection, Function, FunctionSection, GlobalSection,
    GlobalType, ImportSection, Instruction as WInstr, MemArg, MemorySection, MemoryType, Module,
    TypeSection, ValType,
};

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        loop {
            let dir = std::env::temp_dir().join(format!(
                "nessa-wasm-test-{prefix}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir_all(&dir) {
                Ok(()) => return Self(dir),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("failed to create temp dir: {e}"),
            }
        }
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Build a feature-rich Wasm 2.0 + `simd128` + `tail-call` + `multi-value` test module:
/// - memory: 1..4 pages (exported as `"memory"`)
/// - globals: `mut counter: i32 = 10`, `const magic: i64 = 99`
/// - exports:
///   - `add_bump(a: i32, b: i32) -> i32`: `(a + b) + (++counter)`
///   - `divmod(a: i64, b: i64) -> (i64, i64)`: multi-value `(a / b, a % b)`
///   - `tail_sum(n: i64, acc: i64) -> i64`: tail-recursive `return_call` sum `1..=n`
///   - `dot_i32x4(a: v128, b: v128) -> i32`: `simd128` 4-lane dot product
///   - `sum_bytes(ptr: i32, len: i32) -> i32`: sums `[ptr .. ptr + len]` in linear memory
fn build_rich_wasm_module() -> Vec<u8> {
    let mut m = Module::new();

    let mut types = TypeSection::new();
    // 0: (i32, i32) -> i32
    types
        .ty()
        .function([ValType::I32, ValType::I32], [ValType::I32]);
    // 1: (i64, i64) -> (i64, i64)
    types
        .ty()
        .function([ValType::I64, ValType::I64], [ValType::I64, ValType::I64]);
    // 2: (i64, i64) -> i64
    types
        .ty()
        .function([ValType::I64, ValType::I64], [ValType::I64]);
    // 3: (v128, v128) -> i32
    types
        .ty()
        .function([ValType::V128, ValType::V128], [ValType::I32]);
    m.section(&types);

    let mut funcs = FunctionSection::new();
    funcs.function(0); // 0: add_bump
    funcs.function(1); // 1: divmod
    funcs.function(2); // 2: tail_sum
    funcs.function(3); // 3: dot_i32x4
    funcs.function(0); // 4: sum_bytes
    m.section(&funcs);

    let mut mems = MemorySection::new();
    mems.memory(MemoryType {
        minimum: 1,
        maximum: Some(4),
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    m.section(&mems);

    let mut globals = GlobalSection::new();
    globals.global(
        GlobalType {
            val_type: ValType::I32,
            mutable: true,
            shared: false,
        },
        &ConstExpr::i32_const(10),
    );
    globals.global(
        GlobalType {
            val_type: ValType::I64,
            mutable: false,
            shared: false,
        },
        &ConstExpr::i64_const(99),
    );
    m.section(&globals);

    let mut exports = ExportSection::new();
    exports.export("memory", ExportKind::Memory, 0);
    exports.export("counter", ExportKind::Global, 0);
    exports.export("magic", ExportKind::Global, 1);
    exports.export("add_bump", ExportKind::Func, 0);
    exports.export("divmod", ExportKind::Func, 1);
    exports.export("tail_sum", ExportKind::Func, 2);
    exports.export("dot_i32x4", ExportKind::Func, 3);
    exports.export("sum_bytes", ExportKind::Func, 4);
    m.section(&exports);

    let mut codes = CodeSection::new();

    // 0: add_bump(a, b) -> (a + b) + (++counter)
    {
        let mut f = Function::new([]);
        f.instruction(&WInstr::GlobalGet(0));
        f.instruction(&WInstr::I32Const(1));
        f.instruction(&WInstr::I32Add);
        f.instruction(&WInstr::GlobalSet(0));
        f.instruction(&WInstr::LocalGet(0));
        f.instruction(&WInstr::LocalGet(1));
        f.instruction(&WInstr::I32Add);
        f.instruction(&WInstr::GlobalGet(0));
        f.instruction(&WInstr::I32Add);
        f.instruction(&WInstr::End);
        codes.function(&f);
    }

    // 1: divmod(a, b) -> (a / b, a % b)
    {
        let mut f = Function::new([]);
        f.instruction(&WInstr::LocalGet(0));
        f.instruction(&WInstr::LocalGet(1));
        f.instruction(&WInstr::I64DivS);
        f.instruction(&WInstr::LocalGet(0));
        f.instruction(&WInstr::LocalGet(1));
        f.instruction(&WInstr::I64RemS);
        f.instruction(&WInstr::End);
        codes.function(&f);
    }

    // 2: tail_sum(n, acc) -> if n <= 0 { acc } else { return_call tail_sum(n - 1, acc + n) }
    {
        let mut f = Function::new([]);
        f.instruction(&WInstr::LocalGet(0));
        f.instruction(&WInstr::I64Const(0));
        f.instruction(&WInstr::I64LeS);
        f.instruction(&WInstr::If(wasm_encoder::BlockType::Result(ValType::I64)));
        f.instruction(&WInstr::LocalGet(1));
        f.instruction(&WInstr::Else);
        f.instruction(&WInstr::LocalGet(0));
        f.instruction(&WInstr::I64Const(1));
        f.instruction(&WInstr::I64Sub);
        f.instruction(&WInstr::LocalGet(1));
        f.instruction(&WInstr::LocalGet(0));
        f.instruction(&WInstr::I64Add);
        f.instruction(&WInstr::ReturnCall(2));
        f.instruction(&WInstr::End);
        f.instruction(&WInstr::End);
        codes.function(&f);
    }

    // 3: dot_i32x4(a: v128, b: v128) -> sum of 4 lane products
    {
        let mut f = Function::new([(1, ValType::V128)]);
        f.instruction(&WInstr::LocalGet(0));
        f.instruction(&WInstr::LocalGet(1));
        f.instruction(&WInstr::I32x4Mul);
        f.instruction(&WInstr::LocalSet(2));
        f.instruction(&WInstr::LocalGet(2));
        f.instruction(&WInstr::I32x4ExtractLane(0));
        f.instruction(&WInstr::LocalGet(2));
        f.instruction(&WInstr::I32x4ExtractLane(1));
        f.instruction(&WInstr::I32Add);
        f.instruction(&WInstr::LocalGet(2));
        f.instruction(&WInstr::I32x4ExtractLane(2));
        f.instruction(&WInstr::I32Add);
        f.instruction(&WInstr::LocalGet(2));
        f.instruction(&WInstr::I32x4ExtractLane(3));
        f.instruction(&WInstr::I32Add);
        f.instruction(&WInstr::End);
        codes.function(&f);
    }

    // 4: sum_bytes(ptr: i32, len: i32) -> i32
    {
        let mut f = Function::new([(2, ValType::I32)]); // local 2 = i, local 3 = acc
        f.instruction(&WInstr::Block(wasm_encoder::BlockType::Empty));
        f.instruction(&WInstr::Loop(wasm_encoder::BlockType::Empty));
        f.instruction(&WInstr::LocalGet(2));
        f.instruction(&WInstr::LocalGet(1));
        f.instruction(&WInstr::I32GeU);
        f.instruction(&WInstr::BrIf(1));
        f.instruction(&WInstr::LocalGet(3));
        f.instruction(&WInstr::LocalGet(0));
        f.instruction(&WInstr::LocalGet(2));
        f.instruction(&WInstr::I32Add);
        f.instruction(&WInstr::I32Load8U(MemArg {
            offset: 0,
            align: 0,
            memory_index: 0,
        }));
        f.instruction(&WInstr::I32Add);
        f.instruction(&WInstr::LocalSet(3));
        f.instruction(&WInstr::LocalGet(2));
        f.instruction(&WInstr::I32Const(1));
        f.instruction(&WInstr::I32Add);
        f.instruction(&WInstr::LocalSet(2));
        f.instruction(&WInstr::Br(0));
        f.instruction(&WInstr::End);
        f.instruction(&WInstr::End);
        f.instruction(&WInstr::LocalGet(3));
        f.instruction(&WInstr::End);
        codes.function(&f);
    }

    m.section(&codes);
    m.finish()
}

/// Build a Wasm module importing `"env" "step" (i64) -> i64` and exporting `drive(n: i64) -> i64`
/// which calls `step(i)` for `i = 1..=n` and sums the results.
fn build_host_callback_wasm_module() -> Vec<u8> {
    let mut m = Module::new();

    let mut types = TypeSection::new();
    types.ty().function([ValType::I64], [ValType::I64]);
    m.section(&types);

    let mut imports = ImportSection::new();
    imports.import("env", "step", wasm_encoder::EntityType::Function(0));
    m.section(&imports);

    let mut funcs = FunctionSection::new();
    funcs.function(0);
    m.section(&funcs);

    let mut exports = ExportSection::new();
    exports.export("drive", ExportKind::Func, 1);
    m.section(&exports);

    let mut codes = CodeSection::new();
    let mut f = Function::new([(2, ValType::I64)]); // local 1 = i, local 2 = sum
    f.instruction(&WInstr::I64Const(1));
    f.instruction(&WInstr::LocalSet(1));
    f.instruction(&WInstr::Block(wasm_encoder::BlockType::Empty));
    f.instruction(&WInstr::Loop(wasm_encoder::BlockType::Empty));
    f.instruction(&WInstr::LocalGet(1));
    f.instruction(&WInstr::LocalGet(0));
    f.instruction(&WInstr::I64GtS);
    f.instruction(&WInstr::BrIf(1));
    f.instruction(&WInstr::LocalGet(2));
    f.instruction(&WInstr::LocalGet(1));
    f.instruction(&WInstr::Call(0));
    f.instruction(&WInstr::I64Add);
    f.instruction(&WInstr::LocalSet(2));
    f.instruction(&WInstr::LocalGet(1));
    f.instruction(&WInstr::I64Const(1));
    f.instruction(&WInstr::I64Add);
    f.instruction(&WInstr::LocalSet(1));
    f.instruction(&WInstr::Br(0));
    f.instruction(&WInstr::End);
    f.instruction(&WInstr::End);
    f.instruction(&WInstr::LocalGet(2));
    f.instruction(&WInstr::End);
    codes.function(&f);

    m.section(&codes);
    m.finish()
}

#[test]
fn std_wasm_validates_executes_simd_multivalue_tailcalls_memory_globals_and_limits() {
    let wasm_hex = hex_encode(&build_rich_wasm_module());
    let script = format!(
        r#"
use std.wasm
use std.bytes

fn assert_eq(a: Any, b: Any) {{
    if a != b {{
        panic("assertion failed: " ++ to_string(a) ++ " != " ++ to_string(b))
    }}
}}

fn main() {{
    let bin = Bytes.from_list(bytes.from_hex("{wasm_hex}").as(List))
    wasm.validate(bin)!{{ e => panic(e.to_string()) }}

    let bad = Bytes.from_list([0, 97, 115, 109, 99, 0, 0, 0])
    var bad_ok = false
    wasm.validate(bad)!{{
        wasm.WasmError.Validation(_) => {{ bad_ok = true }},
        _ => {{}},
    }}
    assert_eq(bad_ok, true)

    let summary = wasm.inspect(bin)!{{ e => panic(e.to_string()) }}
    assert_eq(summary.contains("add_bump"), true)

    let mod_handle = wasm.compile(bin)!{{ e => panic(e.to_string()) }}
    let exps = mod_handle.exports()
    assert_eq(exps.len(), 8)

    let inst = mod_handle.instantiate(max_memory_pages = 2, max_call_depth = 16, fuel = 500000)!{{ e => panic(e.to_string()) }}
    assert_eq(inst.get_global("magic")!{{ e => panic(e.to_string()) }}, 99)
    assert_eq(inst.get_global("counter")!{{ e => panic(e.to_string()) }}, 10)

    let r1 = inst.call_i64("add_bump", args = [20, 11])!{{ e => panic(e.to_string()) }}
    assert_eq(r1, 42)
    assert_eq(inst.get_global("counter")!{{ e => panic(e.to_string()) }}, 11)

    inst.set_global("counter", 100)!{{ e => panic(e.to_string()) }}
    assert_eq(inst.get_global("counter")!{{ e => panic(e.to_string()) }}, 100)

    var immut_rejected = false
    inst.set_global("magic", 7)!{{
        wasm.WasmError.ImmutableGlobal(_) => {{ immut_rejected = true }},
        _ => {{}},
    }}
    assert_eq(immut_rejected, true)

    -- Multi-value return `(q, r)`
    let qr = inst.call_multi("divmod", args = [47, 5])!{{ e => panic(e.to_string()) }}
    assert_eq(qr.len(), 2)
    assert_eq(qr(0), 9)
    assert_eq(qr(1), 2)

    -- Division by zero traps deterministically without panicking the Nessa VM
    var div_zero_trapped = false
    let _ = inst.call_multi("divmod", args = [10, 0])!{{
        wasm.WasmError.Trap(_) => {{ div_zero_trapped = true; [] }},
        _ => [],
    }}
    assert_eq(div_zero_trapped, true)

    -- Tail-call `return_call` runs in O(1) stack frames (depth 2000 with max_call_depth = 16!)
    let tsum = inst.call_i64("tail_sum", args = [2000, 0])!{{ e => panic(e.to_string()) }}
    assert_eq(tsum, 2001000)

    -- `simd128` 4-lane i32 dot product: [1, 2, 3, 4] . [10, 20, 30, 40] = 300
    let v_a = Bytes.from_list([1,0,0,0, 2,0,0,0, 3,0,0,0, 4,0,0,0])
    let v_b = Bytes.from_list([10,0,0,0, 20,0,0,0, 30,0,0,0, 40,0,0,0])
    let dot = inst.call_i64("dot_i32x4", args = [v_a, v_b])!{{ e => panic(e.to_string()) }}
    assert_eq(dot, 300)

    -- Linear memory UTF-8 & raw byte read/write + sandbox page ceiling
    let written = inst.write_str(256, "Nessa+WASM")!{{ e => panic(e.to_string()) }}
    assert_eq(written, 10)
    assert_eq(inst.read_str(256, 10)!{{ e => panic(e.to_string()) }}, "Nessa+WASM")
    let csum = inst.call_i64("sum_bytes", args = [256, 5])!{{ e => panic(e.to_string()) }}
    -- 'N'(78) + 'e'(101) + 's'(115) + 's'(115) + 'a'(97) = 506
    assert_eq(csum, 506)

    assert_eq(inst.memory_pages(), 1)
    let prev_pages = inst.memory_grow(1)!{{ e => panic(e.to_string()) }}
    assert_eq(prev_pages, 1)
    assert_eq(inst.memory_pages(), 2)
    var limit_denied = false
    let _ = inst.memory_grow(1)!{{
        wasm.WasmError.MemoryLimit(_) => {{ limit_denied = true; -1 }},
        _ => -1,
    }}
    assert_eq(limit_denied, true)

    -- Deterministic fuel exhaustion
    let tight = mod_handle.instantiate(fuel = 20)!{{ e => panic(e.to_string()) }}
    var out_of_fuel = false
    let _ = tight.call_i64("tail_sum", args = [500, 0])!{{
        wasm.WasmError.OutOfFuel(_) => {{ out_of_fuel = true; 0 }},
        _ => 0,
    }}
    assert_eq(out_of_fuel, true)
    assert_eq(tight.close(), true)
    assert_eq(inst.close(), true)
    assert_eq(mod_handle.close(), true)
}}
"#
    );

    let res = Driver::new().without_cache().run(&script);
    assert!(
        matches!(res, RunResult::Ok),
        "std.wasm test failed: {res:?}"
    );
}

#[test]
fn std_wasm_host_closure_imports_survive_moving_gc_and_block_cross_frame_continuations() {
    let cb_hex = hex_encode(&build_host_callback_wasm_module());

    // 1. Captured heap objects inside Nessa host-import closures survive forced moving GC!
    let gc_script = format!(
        r#"
use std.wasm
use std.bytes
use std.runtime

fn main() {{
    let bin = Bytes.from_list(bytes.from_hex("{cb_hex}").as(List))
    let m = wasm.compile(bin)!{{ e => panic(e.to_string()) }}
    let prefix = "heap_captured_" ++ "tag"
    let weights = [10, 20, 30]
    let imp = Map()
    imp.set("env::step", |x: i64| -> i64 {{
        -- Allocate heap garbage and force GC inside the Wasm->Nessa host callback!
        let tmp = prefix ++ ":" ++ to_string(x)
        let _ = runtime.collect_garbage()
        if tmp.len() < 15 {{ panic("corrupted captured string") }}
        x * weights(0).as(i64) + weights(2).as(i64)
    }})
    let inst = m.instantiate(imports = imp)!{{ e => panic(e.to_string()) }}

    -- Sum for x = 1..=8 of (10*x + 30) = 10*36 + 240 = 600
    let total = inst.call_i64("drive", args = [8])!{{ e => panic(e.to_string()) }}
    if total != 600 {{
        panic("unexpected callback total: " ++ to_string(total))
    }}
    let _ = inst.close()
    let _ = m.close()
}}
"#
    );
    let res = Driver::new().without_cache().run(&gc_script);
    assert!(
        matches!(res, RunResult::Ok),
        "GC host import callback test failed: {res:?}"
    );

    // 2. Continuation capture (`coroutine.yield_val`) across an active Wasm stack frame raises `EffectAcrossNativeFrame`!
    let cont_script = format!(
        r#"
use std.wasm
use std.bytes
use std.coroutine

fn main() {{
    let bin = Bytes.from_list(bytes.from_hex("{cb_hex}").as(List))
    let m = wasm.compile(bin)!{{ e => panic(e.to_string()) }}
    let co = coroutine.create(|_: Any| -> Any {{
        let imp = Map()
        imp.set("env::step", |x: i64| -> i64 {{
            let _ = coroutine.yield_val(value = x)
            x
        }})
        let inst = m.instantiate(imports = imp)!{{ e => panic(e.to_string()) }}
        inst.call_i64("drive", args = [2])!{{ e => panic(e.to_string()) }}
    }})
    let _ = coroutine.step(co)
}}
"#
    );
    let cont_res = Driver::new().without_cache().run(&cont_script);
    assert!(
        matches!(cont_res, RunResult::RuntimeError(ref msg) if msg.contains("EffectAcrossNativeFrame")),
        "expected EffectAcrossNativeFrame across Wasm frame, got {cont_res:?}"
    );
}

#[test]
fn ffi_wasm_package_manifest_embeds_wasm_section_and_executes_call_wasm_across_jit_tiers() {
    let tmp = TempDir::new("pkg-ffi-wasm");
    let pkg_dir = tmp.path().join("app");
    std::fs::create_dir_all(pkg_dir.join("src")).unwrap();
    std::fs::create_dir_all(pkg_dir.join("wasm")).unwrap();

    let wasm_bytes = build_rich_wasm_module();
    std::fs::write(pkg_dir.join("wasm/kernels.wasm"), &wasm_bytes).unwrap();
    std::fs::write(
        pkg_dir.join("package.toml"),
        r#"[package]
name = "app"
domain = "test.wasm"
version = "0.1.0"
type = "exe"

[ffi]
allow = true

[[ffi.wasm]]
name = "kernels"
file = "wasm/kernels.wasm"

[ffi.wasm.exports]
add_bump = { export = "add_bump", params = ["i32", "i32"], ret = "i32" }
tail_sum = { export = "tail_sum", params = ["i64", "i64"], ret = "i64" }
checksum = { export = "sum_bytes", params = ["Bytes"], ret = "i32" }
checksum_str = { export = "sum_bytes", params = ["String"], ret = "i32" }
"#,
    )
    .unwrap();

    std::fs::write(
        pkg_dir.join("src/main.ns"),
        r#"
use std.bytes.*

fn main() {
    -- Hot loop (>1500 iterations) exercises Tier-1 & Tier-2 JIT over `Opcode::CallWasm`!
    var acc: i64 = 0
    var i: i64 = 1
    while i <= 1500 {
        acc += kernels.tail_sum(4, 0)
        i += 1
    }
    if acc != 15000 {
        panic("hot CallWasm loop mismatch: " ++ to_string(acc))
    }

    let b = Bytes.from_list([10, 20, 30, 40])
    if kernels.checksum(b) != (100).as(i32) {
        panic("Bytes slice CallWasm mismatch")
    }
    if kernels.checksum_str("ABC") != (65 + 66 + 67).as(i32) {
        panic("String slice CallWasm mismatch")
    }
    if kernels.add_bump((5).as(i32), (6).as(i32)) != (22).as(i32) {
        panic("add_bump CallWasm mismatch")
    }
}
"#,
    )
    .unwrap();

    let archive_path = tmp.path().join("standalone.nsbc");
    Driver::new()
        .without_cache()
        .with_opt_level(OptLevel::O3)
        .compile_package_to_archive(&pkg_dir, &archive_path)
        .expect("package with [[ffi.wasm]] must compile to .nsbc");

    // Delete original `wasm/kernels.wasm` so runtime execution proves `standalone.nsbc` is 100% self-contained!
    std::fs::remove_dir_all(pkg_dir.join("wasm")).unwrap();

    let disasm = Driver::new()
        .disassemble_archive_file(&archive_path)
        .expect("disassemble .nsbc with SectionKind::Wasm");
    assert!(
        disasm.contains("section .wasm"),
        "expected `.wasm` section in disassembly:\n{disasm}"
    );
    assert!(
        disasm.contains("CallWasm"),
        "expected `CallWasm` opcode in disassembly:\n{disasm}"
    );

    for jit_mode in [JitMode::Off, JitMode::Tier1, JitMode::Tier2] {
        let run_res = Driver::new()
            .with_jit_mode(jit_mode)
            .run_archive_file(&archive_path);
        assert!(
            matches!(run_res, RunResult::Ok),
            "standalone .nsbc execution failed under {jit_mode:?}: {run_res:?}"
        );
    }

    // Negative check: mismatched Wasm export signature is rejected at compile time!
    let bad_dir = tmp.path().join("bad_sig");
    std::fs::create_dir_all(bad_dir.join("src")).unwrap();
    std::fs::create_dir_all(bad_dir.join("wasm")).unwrap();
    std::fs::write(bad_dir.join("wasm/kernels.wasm"), &wasm_bytes).unwrap();
    std::fs::write(
        bad_dir.join("package.toml"),
        r#"[package]
name = "bad_sig"
domain = "test.wasm"
version = "0.1.0"
type = "exe"

[ffi]
allow = true

[[ffi.wasm]]
name = "kernels"
file = "wasm/kernels.wasm"

[ffi.wasm.exports]
wrong = { export = "add_bump", params = ["i64", "i64"], ret = "i64" }
"#,
    )
    .unwrap();
    std::fs::write(
        bad_dir.join("src/main.ns"),
        "fn main() { println(kernels.wrong(1, 2)) }",
    )
    .unwrap();
    let bad_compiled = Driver::new().without_cache().compile_package(&bad_dir);
    assert!(
        bad_compiled.has_errors,
        "expected compile-time rejection on Wasm signature mismatch"
    );
}

#[test]
fn aot_wasm32_wasi_and_freestanding_execute_in_nessa_cli_and_nodejs_v8() {
    let tmp = TempDir::new("aot-wasm");

    // 1. Rich Nessa program compiled to `wasm32-wasi` at `-O3`:
    //    structs, payload enums, traits + `Display`, closures, `List`, `Map`, `!Error` handling, `println`.
    let wasi_src_path = tmp.path().join("app.ns");
    let wasi_out_path = tmp.path().join("app.wasm");
    let wasi_source = r#"
enum Expr {
    num(v: i64),
    add(a: i64, b: i64),
}

trait Summarizable {
    def fn summary(self) -> String
}

struct Report {
    title: String,
    score: i64,
}

impl Summarizable for Report {
    pub fn summary(self) -> String {
        self.title ++ "=" ++ to_string(self.score)
    }
}

fn eval_expr(e: Expr) -> i64 {
    e match {
        Expr.num(v) => v,
        Expr.add(a, b) => a + b,
    }
}

fn checked_div(a: i64, b: i64) -> !String i64 {
    if b == 0 {
        error "div0"
    } else {
        a / b
    }
}

fn render(item: Summarizable) -> String {
    item.summary()
}

fn main() {
    let scale = 3
    let f = |x: i64| -> i64 { x * scale + 1 }
    let xs = [f(3), f(4), eval_expr(Expr.add(10, 5))]
    let m = Map()
    m.set("k", xs(0).as(i64) + xs(1).as(i64) + xs(2).as(i64))
    let q = checked_div(m("k").as(i64), 2)!{ _ => 0 }
    let rep = Report { title: "wasi_ok", score: q }
    println(render(rep))
}
"#;
    std::fs::write(&wasi_src_path, wasi_source).unwrap();

    // Compile via Driver (`OptLevel::O3`, `WasmTarget::WasiPreview1`) and inspect!
    Driver::new()
        .without_cache()
        .with_opt_level(OptLevel::O3)
        .compile_to_wasm(
            wasi_source,
            &wasi_out_path,
            WasmCompileOptions {
                target: WasmTarget::WasiPreview1,
                export_all_pub: true,
            },
        )
        .expect("compile_to_wasm wasm32-wasi");
    assert!(wasi_out_path.is_file());

    let disasm_text = Driver::new()
        .disassemble_wasm_file(&wasi_out_path)
        .expect("disassemble_wasm_file");
    assert!(disasm_text.contains("_start"), "{disasm_text}");

    // Verify expected result: `xs = [10, 13, 15]`, sum = `38`, `38 / 2 = 19` -> `"wasi_ok=19\n"`
    let wasi_bytes = std::fs::read(&wasi_out_path).unwrap();
    let wasi_out = wasm::run_wasi_bytes(&wasi_bytes, wasm::WasiConfig::default())
        .expect("run_wasi_bytes on Nessa-compiled app.wasm");
    assert_eq!(wasi_out.exit_code, 0);
    assert_eq!(String::from_utf8_lossy(&wasi_out.stdout), "wasi_ok=19\n");

    // Also run the compiled `app.wasm` inside Nessa's `std.wasm.run_wasi_file`!
    let escaped_wasi_path = wasi_out_path.to_string_lossy().replace('\\', "/");
    let host_wasi_script = format!(
        r#"
use std.wasm
fn main() {{
    let out = wasm.run_wasi_file("{escaped_wasi_path}")!{{ e => panic(e.to_string()) }}
    if out.exit_code != 0 or out.stdout_str() != "wasi_ok=19\n" {{
        panic("unexpected std.wasm.run_wasi_file output: " ++ out.stdout_str())
    }}
}}
"#
    );
    let host_run = Driver::new().without_cache().run(&host_wasi_script);
    assert!(matches!(host_run, RunResult::Ok), "{host_run:?}");

    // 2. Compile a pure computation module to `wasm32-freestanding` (zero WASI imports!).
    let free_source = r#"
pub fn fib(n: i64) -> i64 {
    if n <= 1 {
        n
    } else {
        fib(n - 1) + fib(n - 2)
    }
}

pub fn hypot_sq(a: f64, b: f64) -> f64 {
    a * a + b * b
}
"#;
    let free_wasm_path = tmp.path().join("math_free.wasm");
    Driver::new()
        .without_cache()
        .with_opt_level(OptLevel::O2)
        .compile_to_wasm(
            free_source,
            &free_wasm_path,
            WasmCompileOptions {
                target: WasmTarget::Freestanding,
                export_all_pub: true,
            },
        )
        .expect("compile wasm32-freestanding");

    let free_bytes = std::fs::read(&free_wasm_path).unwrap();
    let parsed_free = wasm::WasmModule::from_bytes(&free_bytes).unwrap();
    assert!(
        parsed_free.imports().is_empty(),
        "wasm32-freestanding must emit zero host imports, got {:?}",
        parsed_free.imports()
    );
    let mut free_inst = wasm::WasmInstance::instantiate(
        std::sync::Arc::new(parsed_free),
        wasm::WasmConfig::default(),
        std::collections::HashMap::new(),
        &std::collections::HashMap::new(),
    )
    .unwrap();
    assert_eq!(
        free_inst
            .call_export("fib", &[wasm::WasmVal::I64(20)])
            .unwrap(),
        vec![wasm::WasmVal::I64(6765)]
    );
    assert_eq!(
        free_inst
            .call_export(
                "hypot_sq",
                &[wasm::WasmVal::f64(3.0), wasm::WasmVal::f64(4.0)]
            )
            .unwrap(),
        vec![wasm::WasmVal::f64(25.0)]
    );

    // 3. Cross-runtime verification against Node.js (V8 WebAssembly + `node:wasi`) on Unix hosts with `/usr/bin/node`!
    #[cfg(unix)]
    if Path::new("/usr/bin/node").is_file() {
        let node_script = format!(
            r#"
const fs = require("node:fs");
const {{ WASI }} = require("node:wasi");

(async () => {{
  // (a) Verify `wasm32-wasi` binary under V8 + node:wasi
  const wasi = new WASI({{ version: "preview1", args: ["app.wasm"], env: {{}} }});
  const wasiBytes = fs.readFileSync({wasi_path:?});
  const wasiMod = await WebAssembly.compile(wasiBytes);
  const wasiInst = await WebAssembly.instantiate(wasiMod, wasi.getImportObject());
  const code = wasi.start(wasiInst);
  if (code !== 0) throw new Error("unexpected exit code " + code);

  // (b) Verify `wasm32-freestanding` exports under V8 with empty import object
  const freeBytes = fs.readFileSync({free_path:?});
  const {{ instance }} = await WebAssembly.instantiate(freeBytes, {{}});
  const fib20 = instance.exports.fib(20n);
  const h = instance.exports.hypot_sq(3.0, 4.0);
  if (fib20 !== 6765n) throw new Error("fib(20) mismatch: " + fib20);
  if (h !== 25.0) throw new Error("hypot_sq mismatch: " + h);
  process.stdout.write("NODE_V8_OK\n");
}})().catch((e) => {{
  console.error(e);
  process.exit(1);
}});
"#,
            wasi_path = wasi_out_path.to_str().unwrap(),
            free_path = free_wasm_path.to_str().unwrap(),
        );
        let node_out = Command::new("/usr/bin/node")
            .args(["-e", &node_script])
            .output()
            .expect("invoke /usr/bin/node");
        assert!(
            node_out.status.success(),
            "Node.js V8 cross-runtime verification failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&node_out.stdout),
            String::from_utf8_lossy(&node_out.stderr)
        );
        let stdout = String::from_utf8_lossy(&node_out.stdout);
        assert_eq!(stdout, "wasi_ok=19\nNODE_V8_OK\n");
    }

    // 4. Reject unsupported continuation/OS operations on `wasm32-*` with an explicit compile error!
    let unsupported_src = r#"
use std.coroutine
fn main() {
    let co = coroutine.create(|_: Any| -> Any { let _ = coroutine.yield_val(value = 1); 2 })
    println(coroutine.step(co))
}
"#;
    let err = Driver::new()
        .without_cache()
        .compile_to_wasm_bytes(
            unsupported_src,
            WasmCompileOptions {
                target: WasmTarget::WasiPreview1,
                export_all_pub: false,
            },
        )
        .expect_err("coroutines must be rejected by wasm32 AOT backend");
    assert!(
        err.to_string().contains("wasm compilation failed"),
        "unexpected error: {err}"
    );
}
