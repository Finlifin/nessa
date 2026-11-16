# WebAssembly Support & Binding Guide (`engine/wasm` & `std.wasm`)

Nessa provides end-to-end WebAssembly 2.0 (`+ simd128`) support across four integrated subsystems:

1. **Dynamic Runtime API (`std.wasm`)**: Validate, inspect, compile, and instantiate `.wasm` modules at runtime with linear-memory/global access, `v128` SIMD (`Bytes(16)`), fuel and memory sandboxing, GC-rooted Nessa closure imports, and virtualized WASI Preview 1 execution (`wasm.run_wasi` / `wasm.run_wasi_file`).
2. **Declarative Package Embedding (`[[ffi.wasm]]`)**: Embed `.wasm` binaries directly into self-contained `.nsbc` archives (`SectionKind::Wasm = 7`) and call exported functions via `Opcode::CallWasm` (`0x8B`) across the interpreter, Tier-1 baseline JIT, and Tier-2 Cranelift JIT.
3. **Nessa-to-WebAssembly AOT Compiler (`nessa build --target ...`)**: Compile Nessa programs and libraries directly into standalone `.wasm` binaries targeting `wasm32-wasi` (WASI Preview 1 command modules) or `wasm32-freestanding` (zero-import pure computation modules), executable in `nessa run`, Node.js/V8, Wasmtime, and browsers.
4. **CLI Inspection & Execution**: `nessa build`, `nessa run *.wasm`, and `nessa disasm *.wasm`.

---

## 1. Dynamic Runtime API (`std.wasm`)

`std.wasm` requires no `[ffi]` capability flag because every WebAssembly module executes inside Nessa's memory-isolated, bounds-checked, stack- and fuel-limited virtual machine (`engine/wasm`), without raw host pointers or OS syscalls.

```nessa
use std.wasm
use std.bytes.*

fn main() {
    -- Load and compile a `.wasm` binary from disk or in-memory `Bytes`
    let mod_handle = wasm.compile_file("kernels.wasm")!{ e => panic(e.to_string()) }

    -- Supply GC-rooted Nessa closures as typed host imports ("module::name" or nested Map)
    let imports = Map()
    imports.set("env::scale", |x: i64| -> i64 { x * 3 })

    let inst = mod_handle.instantiate(
        imports = imports,
        max_memory_pages = 64,
        fuel = 500_000,
    )!{ e => panic(e.to_string()) }

    -- Single-value & multi-value calls
    let sum = inst.call1("add_scaled", [10, 4])!{ e => panic(e.to_string()) }
    println(sum)

    -- Linear memory and mutable global access
    let written = inst.write_str(256, "hello wasm")!{ e => panic(e.to_string()) }
    let text = inst.read_str(256, written)!{ e => panic(e.to_string()) }
    println(text)
}
```

### Key `std.wasm` APIs

| Function / Method | Signature | Description |
| --- | --- | --- |
| `wasm.validate(bytes)` | `fn(Bytes) -> !WasmError Unit` | Full W3C WebAssembly 2.0 (`+ simd128`, `bulk-memory`, `multi-value`, `tail-call`, `reference-types`) binary & bytecode validation. |
| `wasm.inspect(bytes)` | `fn(Bytes) -> !WasmError String` | Human-readable section, type, import, export, memory, global, and code summary. |
| `wasm.compile(bytes)` / `wasm.compile_file(path)` | `fn(...) -> !WasmError WasmModule` | Parse and validate a reusable `WasmModule` handle. |
| `wasm.run_wasi(bytes, ...)` / `wasm.run_wasi_file(path, ...)` | `fn(...) -> !WasmError WasiResult` | Execute a `wasi_snapshot_preview1` `_start` module with virtualized `args`, `env`, `stdin`, captured `stdout`/`stderr`, `max_memory_pages`, and `fuel`. |
| `WasmModule.exports(self)` / `imports(self)` | `fn(self) -> List` | Inspect `WasmExport` and `WasmImport` descriptors. |
| `WasmModule.instantiate(self, ...)` | `fn(self, .imports: ?Map = null, .max_memory_pages: i64 = 1024, .max_call_depth: i64 = 512, .fuel: i64 = -1) -> !WasmError WasmInstance` | Instantiate with optional Nessa host callbacks and deterministic resource limits. |
| `WasmInstance.call(self, name, .args)` / `call1` | `fn(...) -> !WasmError List` / `Any` | Invoke exported Wasm functions (`i32`/`i64` $\leftrightarrow$ `i64`, `f32`/`f64` $\leftrightarrow$ `f64`, `v128` $\leftrightarrow$ 16-byte `Bytes`). |
| `WasmInstance.read_memory` / `write_memory` / `read_str` / `write_str` / `grow_memory` / `memory_pages` | Bounds-checked linear memory access (64 KiB pages). |
| `WasmInstance.get_global` / `set_global` | Read exported globals and mutate exported `mut` globals. |
| `WasmInstance.fuel_remaining` / `close` | Query remaining instruction fuel (`?i64`) or release the instance handle early. |

### GC & Continuation Safety for Host Imports

- Nessa closures passed in `imports` are stored in `VmRoots::wasm_host_imports` and traced/updated by MMTk across moving Immix collections — even if a host callback triggers garbage collection while Wasm frames are active on the Rust stack.
- Host callbacks execute inside `Vm::with_native_frame` (`native_frame_depth > 0`) while saving and restoring caller VM registers. Capturing, cloning, or resuming a delimited continuation or raising an unhandled algebraic effect across an active Wasm frame is deterministically rejected with `VmError::EffectAcrossNativeFrame`.

---

## 2. Declarative `[[ffi.wasm]]` Package Bindings

Packages with `[ffi] allow = true` can embed `.wasm` modules directly into `.nsbc` archives via `package.toml`:

```toml
[package]
name = "crypto_demo"
domain = "example"
version = "0.1.0"
type = "exe"

[ffi]
allow = true

[[ffi.wasm]]
name = "kernels"
file = "wasm/kernels.wasm"

[ffi.wasm.exports]
add_bump = { export = "add_bump", params = ["i32", "i32"], ret = "i32" }
checksum = { export = "sum_bytes", params = ["Bytes"], ret = "i32" }
checksum_str = { export = "sum_bytes", params = ["String"], ret = "i32" }
```

- **Compile-Time Validation**: `nessa check`, `nessa build`, and `nessa run` validate the `.wasm` binary, verify that every bound export exists, and check exact Wasm parameter/result types at compile time. `.wasm` assets are also included in `package.lock` SHA-256 integrity hashes.
- **Self-Contained `.nsbc` Archives**: `nessa build` embeds the `.wasm` payload into `SectionKind::Wasm` (`7`) and compiles generated `pub mod <name>` wrappers down to `[CallWasm, Return]` (`Opcode::CallWasm = 0x8B`), supported across interpreter, Tier-1 JIT, and Tier-2 JIT.
- **Slice Marshaling (`String` & `Bytes`)**: A parameter declared as `String` or `Bytes` lowers to `(ptr: i32, len: i32)` in Wasm. If the module exports `nessa_alloc(i32) -> i32` or `alloc(i32) -> i32`, Nessa allocates via that export; otherwise it writes into a dedicated grown linear-memory scratch page.

---

## 3. Standalone Nessa-to-WebAssembly AOT Compilation

Compile Nessa source files or packages directly to standard WebAssembly binaries:

```sh
# Build a standalone WASI Preview 1 command binary (`_start` entry point)
nessa build app.ns --target wasm32-wasi -O3 -o app.wasm

# Auto-infers `--target wasm32-wasi` when `-o` ends with `.wasm`
nessa build app.ns -O2 -o app.wasm

# Build a zero-import freestanding WebAssembly library (exports `pub fn`s)
nessa build math.ns --target wasm32-freestanding -O2 -o math.wasm

# Inspect sections, imports, exports, and code sizes
nessa disasm app.wasm

# Execute a WASI `.wasm` binary in Nessa's sandboxed WASI host
nessa run app.wasm -- arg1 arg2
```

### Target Capabilities & Explicit Boundaries

- **Supported Language Surface**: Scalars (`Unit`, `bool`, `char`, `i8`..`i64`, `u8`..`u64`, `isize`, `usize`, `f32`, `f64`), gradual `Any` NaN-boxed dynamic values, `String`, `Bytes`, `Tuple`, `Struct`, payload and C-style `Enum`s, `List`, `Map`, closures (`CallIndirect`), traits & dynamic vtable dispatch (`TraitCall`, `Display`, `PartialEq`, `Ord`, `Hash`), `!Error` propagation/matching, `-O0`..`-O3` NIR optimizations, and WASI `print`/`println`/`eprint`/`eprintln`/`std.env.args`/`exit`/`panic`.
- **Export ABI**: Top-level `pub fn`s with scalar signatures (`i32`/`u32`/`bool`/`char` $\rightarrow$ `i32`, `i64`/`u64`/`isize`/`usize` $\rightarrow$ `i64`, `f32` $\rightarrow$ `f32`, `f64` $\rightarrow$ `f64`) are exported directly with native Wasm scalar ABIs (automatically running module initialization once before user code). Every module also exports `memory` and `nessa_alloc(size: i32) -> i32`.
- **Explicit Compile-Time Rejections**: Features requiring OS threads, multi-segment delimited continuations, native `dlopen` C FFI, or `i128`/`u128` constants are rejected at compile time with an explicit `WasmCompileError` diagnostic rather than emitting silent stubs.
