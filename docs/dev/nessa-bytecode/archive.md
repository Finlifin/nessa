# NSBC Archive 文件格式

NSBC Archive (`.nsbc`) 是 Nessa 的编译产物文件。一个包 (package) 编译后产出一个 archive。

## 文件结构总览

```
┌──────────────────────────────────────────────────┐
│ File Header         (60 bytes, 固定大小)          │
├──────────────────────────────────────────────────┤
│ Section Table       (N × 32 bytes)               │
├──────────────────────────────────────────────────┤
│ Section 0: CODE                                  │
│   函数字节码流，按 FuncId 索引                     │
├──────────────────────────────────────────────────┤
│ Section 1: METADATA                              │
│   类型表 + 方法表 + 字段表 + 字符串池              │
├──────────────────────────────────────────────────┤
│ Section 2: STACK_MAPS                            │
│   每函数的 safe-point 位图                        │
├──────────────────────────────────────────────────┤
│ Section 3: CONSTANTS                             │
│   常量池 (数值、字符串字面量)                      │
├──────────────────────────────────────────────────┤
│ Section 4: DEBUG_INFO  (可剥离)                   │
│   源码映射、行号表                                │
├──────────────────────────────────────────────────┤
│ Section 5: IMPORTS                               │
│   外部依赖声明 (domain/name + version)            │
├──────────────────────────────────────────────────┤
│ Section 6: EXPORTS                               │
│   导出符号表                                      │
├──────────────────────────────────────────────────┤
│ Section 7: WASM  (可选)                           │
│   嵌入的 WASM 模块                                │
└──────────────────────────────────────────────────┘
```

## File Header

```
FileHeader (60 bytes):
  magic:          [4 bytes]   "NSBC"                            → 魔数
  version:        [4 bytes]   major.minor.patch (u16.u16.u16+pad) → archive 格式版本
  checksum:       [32 bytes]  SHA-256 of (header 以外所有数据)    → 完整性校验
  target_arch:    [2 bytes]   0=Any 1=X86_64 2=ARM64 3=RV64     → 目标架构
  target_os:      [2 bytes]   0=Any 1=Linux 2=Darwin 3=Win      → 目标 OS
  flags:          [4 bytes]   bit0=debug bit1=compress ...       → 标志位
  section_count:  [4 bytes]   u32                                → section 数量
  section_table:  [8 bytes]   u64                                → section table 在文件内的偏移
```

`target_arch` 和 `target_os` 为 `Any(0)` 时表示平台无关字节码。

## Section Table

Section table 位于 `section_table` 指定的偏移处，每个 entry 32 bytes：

```
SectionEntry (32 bytes):
  name_idx:    [4 bytes]   u32  → string pool 中的名称索引
  type:        [4 bytes]   u32  → section 类型枚举
  offset:      [8 bytes]   u64  → 该 section 数据在文件内的偏移
  size:        [8 bytes]   u64  → 数据大小 (bytes)
  alignment:   [4 bytes]   u32  → 对齐要求
  flags:       [4 bytes]   u32  → 标志位 (bit0=compressed, bit1=strippable)
```

Section type 枚举值：

| 值 | 名称 | 说明 |
|----|------|------|
| 0 | CODE | 字节码指令流 |
| 1 | METADATA | 类型表、方法表、字段表、字符串池 |
| 2 | STACK_MAPS | Safe-point 位图 + deopt 信息 |
| 3 | CONSTANTS | 常量池 |
| 4 | DEBUG_INFO | 源码映射（可剥离） |
| 5 | IMPORTS | 外部包依赖声明 |
| 6 | EXPORTS | 导出符号表 |
| 7 | WASM | 嵌入 WASM 模块 |

## CODE Section

按 FuncId 顺序排列每个函数的字节码：

```
CODE Section:
  FuncTable:
    func_count:  [4 bytes]  u32
    entries:     [func_count × FuncEntry]

  FuncEntry:
    func_id:         [4 bytes]  u32
    code_offset:     [4 bytes]  u32  → 相对于 CODE section 起始的偏移
    code_size:       [4 bytes]  u32  → 字节码大小 (bytes)
    register_count:  [1 byte]   u8   → 使用的寄存器数量
    param_count:     [1 byte]   u8   → 参数数量
    flags:           [2 bytes]  u16  → bit0=has_variadic, bit1=is_closure

  Data:
    [函数 0 的 64-bit 指令序列]
    [函数 1 的 64-bit 指令序列]
    ...
```

## METADATA Section

```
METADATA Section:
  TypeTable:
    type_count:  [4 bytes]  u32
    entries:     [type_count × TypeEntry]

  TypeEntry:
    type_id:     [16 bytes]  u128      → 完整 TypeID
    type_index:  [4 bytes]   u32       → 运行时 TypeIndex
    kind:        [1 byte]    u8        → Mod|Struct|Enum|Trait|Alias|...
    parent_type: [4 bytes]   u32       → 父类型的 TypeIndex (0xFFFFFFFF=根)
    child_count: [2 bytes]   u16       → 子类型数量
    children:    [child_count × u32]   → 子类型 TypeIndex 数组
    field_count: [2 bytes]   u16
    method_count:[2 bytes]   u16
    func_range:  [8 bytes]   u32 × 2   → [start_func_id, end_func_id) 直属函数
    init_func:   [4 bytes]   u32       → __init__ 的 FuncId (0xFFFFFFFF=无)
    visibility:  [1 byte]    u8        → pub / package / private
    fields:      [field_count × FieldDef]
    methods:     [method_count × MethodRef]

  StringPool:
    string_count: [4 bytes]  u32
    offsets:      [string_count × u32]   → 各字符串的偏移
    data:         [UTF-8 bytes]          → 连续存储的字符串数据
```

## STACK_MAPS Section

每个函数的 safe-point 信息（详见 [safe-point.md](safe-point.md)）：

```
STACK_MAPS Section:
  func_count:  [4 bytes]  u32
  per_func:
    func_id:        [4 bytes]  u32
    safepoint_count:[4 bytes]  u32
    entries:        [safepoint_count × StackMapEntry]

  StackMapEntry:
    pc_offset:   [4 bytes]  u32        → 指令偏移
    bitmap_size: [2 bytes]  u16        → 位图字节数
    bitmap:      [bitmap_size bytes]   → 引用类型位图
    deopt_id:    [4 bytes]  u32        → deoptimization ID (0=none)
```

## 双相格式

| 格式 | 扩展名 | 用途 |
|------|--------|------|
| 二进制 | `.nsbc` | 生产部署，sections 可选 zstd 压缩 |
| 文本 | `.nsbc.text` | 调试与审查，人类可读的反汇编格式 |

文本格式示例：

```
-- hello.nsbc.text
@func main [registers=3, params=0]
  0000: LOAD_CONST  r0, #str:0    -- "hello, world"
  0004: CALL        r1, @println, [r0]
  0008: RETURN_UNIT
```

## 两层组织

- **Archive**: 单个编译单元（一个 `.ns` 源文件或 `__init__` 模块）的产物
- **Package**: 多个 Archive + package.toml 元数据的组合，对应分发/依赖单元