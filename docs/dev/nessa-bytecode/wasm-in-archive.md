# WASM 模块嵌入 (`SectionKind::Wasm`)

## 概述

Nessa 支持在 NSBC Archive（`.nsbc`）中直接嵌入 WebAssembly 2.0（`+ simd128`）模块（`SectionKind::Wasm = 7`），并通过字节码指令 `Opcode::CallWasm`（`0x8B`）在解释器、Tier-1 基线 JIT 与 Tier-2 Cranelift JIT 中直接调用导出的 WASM 函数，无需外部原生编译器或动态链接库。

## WASM Section 二进制结构 (`SectionKind::Wasm = 7`)

```
WASM section:
  module_count:  [4 bytes]  u32
  entries:       [module_count × WasmModuleEntry]
  string_count:  [4 bytes]  u32
  strings:       [string_count × (len: u32, utf8_bytes: [u8; len])]
  payloads:      concatenated raw `.wasm` binaries (dense, non-overlapping)

WasmModuleEntry:
  name:          [4 bytes]  u32     → section string pool index (模块名)
  wasm_offset:   [8 bytes]  u64     → `.wasm` 字节流在 section 内的字节偏移
  wasm_size:     [8 bytes]  u64     → `.wasm` 字节流长度
  binding_count: [4 bytes]  u32     → 绑定数量
  bindings:      [binding_count × WasmBinding]

WasmBinding:
  nessa_name:    [4 bytes]  u32     → section string pool index (Nessa 侧包装函数名)
  wasm_export:   [4 bytes]  u32     → section string pool index (WASM 侧导出函数名)
  signature:     [4 bytes]  u32     → TypeIndex (`TypeKind::Function` 签名类型)
```

`nsbc::validate_artifact` 在写入和加载归档时完整校验：
1. 每个内嵌 `.wasm` 负载通过 W3C WebAssembly 2.0 二进制与字节码校验；
2. 每个 `WasmBinding` 的 `wasm_export` 在对应模块中存在且为函数导出；
3. `signature` 在 `TypePool` 中为合法 `TypeKind::Function`，且与 WASM 导出的参数/返回值类型精确匹配；
4. 所有 `CallWasm` 指令引用的全局 `binding_id` 与 `argc` 均在有效范围内。

## 调用流程

```
Nessa code                  Embedded WASM Runtime (`engine/wasm`)
┌──────────────────┐        ┌──────────────────────────┐
│ CallWasm id, n   │──marshal──▶│ WasmInstance::call() │
│ (r0..r{n-1})     │        │   (with_native_frame)    │
│ r0 ◀─────────────│◀─unmarshal─│ return WasmVal       │
└──────────────────┘        └──────────────────────────┘
```

1. 包声明 `[[ffi.wasm]]` 时，编译器生成类型化模块包装函数，并在代码生成后将其重写为 `[CallWasm, Return/ReturnUnit]`；
2. 执行 `CallWasm` 时，参数从 `r0..r{argc-1}` 的 `TaggedValue` 编组为 WASM 原始类型或线性内存切片；
3. 在 `Vm::with_native_frame` 保护下执行沙箱化 `WasmInstance`（阻止跨原生/WASM 栈帧捕获或恢复限定延续）；
4. 返回值反编组回 `TaggedValue` 并写入 `r0`。

## 类型映射

| Nessa 类型 | WASM 参数类型 | WASM 返回类型 |
|-----------|--------------|--------------|
| `i8` / `u8` / `i16` / `u16` / `i32` / `u32` / `bool` / `char` | `i32` | `i32` |
| `i64` / `u64` / `isize` / `usize` | `i64` | `i64` |
| `f32` | `f32` | `f32` |
| `f64` | `f64` | `f64` |
| `Unit` | — | `()`（无返回值） |
| `String` / `Bytes` | `(ptr: i32, len: i32)`（优先调用导出的 `nessa_alloc`/`alloc`，否则使用线性内存暂存页） | — |

## 使用场景

- 零外部依赖复用加密、压缩、编解码、数值计算等 `.wasm` 库，随单个 `.nsbc` 文件分发；
- 跨语言确定性计算（Rust / C / C++ / Zig $\rightarrow$ `.wasm` $\rightarrow$ Nessa）；
- 线性内存隔离沙箱执行不受信任的插件代码。
