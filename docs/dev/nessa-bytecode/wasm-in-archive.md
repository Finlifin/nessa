# WASM 模块嵌入

## 概述

Nessa 支持在 NSBC Archive 中嵌入 WASM（WebAssembly）模块，作为 FFI 的一种形态。这使得 Nessa 可以直接调用 WASM 编译的库，无需额外的原生编译步骤。

## WASM Section 结构

```
WASM section:
  module_count:  [4 bytes]  u32
  entries:       [module_count × WasmModuleEntry]

WasmModuleEntry:
  name:          [4 bytes]  u32     → string pool index (模块名)
  wasm_offset:   [8 bytes]  u64    → WASM 字节码在 section 内的偏移
  wasm_size:     [8 bytes]  u64    → WASM 字节码大小
  binding_count: [4 bytes]  u32    → 绑定数量
  bindings:      [binding_count × WasmBinding]

WasmBinding:
  nessa_name:    [4 bytes]  u32    → string pool index (Nessa 侧函数名)
  wasm_export:   [4 bytes]  u32    → string pool index (WASM 侧导出名)
  signature:     [4 bytes]  u32    → TypeIndex (函数签名类型)
```

## 调用流程

```
Nessa code                  WASM Runtime
┌──────────┐               ┌──────────────┐
│ CALL_WASM│──marshal──────▶│ wasm_func()  │
│ r0, @fn  │               │              │
│          │◀──unmarshal───│ return val   │
└──────────┘               └──────────────┘
```

1. 遇到 WASM 绑定的函数调用时，解释器发出 `CALL_WASM` 指令
2. 参数从 TaggedValue 编组为 WASM 原始类型 (i32/i64/f32/f64)
3. 调用嵌入的 WASM runtime 执行
4. 返回值从 WASM 类型反编组为 TaggedValue

## 类型映射

| Nessa 类型 | WASM 类型 |
|-----------|-----------|
| i32 / u32 | i32 |
| i64 / u64 | i64 |
| f32 | f32 |
| f64 | f64 |
| bool | i32 (0/1) |
| 其他 | 通过线性内存传递 |

## 使用场景

- 复用已有的 WASM 生态库（如密码学、图像处理）
- 跨语言代码共享（C/C++/Rust → WASM → Nessa）
- 沙箱执行不受信任的代码（WASM 内存隔离）