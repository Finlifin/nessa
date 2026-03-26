# 包与类型在字节码中的组织

## 核心原则：Type ↔ Scope 双射

Nessa 中 **不存在独立的 "模块" 概念**。`mod` 只是一种没有实例化能力的命名类型，与 `struct`、`enum` 在作用域行为上完全等价。每个 `.ns` 源文件自动定义一个 `mod` 类型。因此字节码中不需要 ModuleTable——所有类型（包括 mod）统一存储在 METADATA 的 TypeTable 中。

## 编译单元

每个 `.ns` 源文件对应一个 `mod` 类型。编译后，一个包 (package) 的所有类型合并到单个 NSBC Archive 中。

```
Package (domain/name)
├── src/main.ns     → mod root 
├── src/utils.ns    → mod utils 
├── src/net/mod.ns  → mod net 
├── src/net/http.ns → mod http
└── src/net/tcp.ns  → mod tcp
         ↓
    单个 .nsbc archive
```

类型嵌套关系直接反映在 TypeEntry 的 `parent_type` 和 `children` 字段中。

## 类型在 Archive 中的表示

所有类型（mod / struct / enum / trait）统一存储在 METADATA section 的 TypeTable 中。每种 kind 的类型作用域行为一致，仅实例化能力不同。

```
TypeTable (在 METADATA section):
  type_count:  [4 bytes]   u32
  entries:     [type_count × TypeEntry]

TypeEntry:
  type_id:      [16 bytes]  u128    → 完整 TypeID
  type_index:   [4 bytes]   u32     → 运行时 TypeIndex
  parent_type:  [4 bytes]   u32     → 父类型的 TypeIndex (0xFFFFFFFF=根)
  child_count:  [2 bytes]   u16     → 子类型数量
  children:     [child_count × u32] → 子类型的 TypeIndex 数组
  field_count:  [2 bytes]   u16     → (Struct/Enum 有效)
  method_count: [2 bytes]   u16
  func_range:   [8 bytes]   u32 × 2 → [start_func_id, end_func_id) 直属函数范围
  init_func:    [4 bytes]   u32     → __init__ 函数的 FuncId (0xFFFFFFFF=无)
  visibility:   [1 byte]    u8      → pub / package / private
  fields:       [field_count × FieldDef]
  methods:      [method_count × MethodRef]
```

## 类型初始化顺序

`__init__` 函数，在类型首次加载时自动调用。
IMPORTS section:
  import_count:   [4 bytes]  u32
  entries:        [import_count × ImportEntry]

ImportEntry:
  package_domain: [4 bytes]  u32   → string pool index (如 "com.example")
  package_name:   [4 bytes]  u32   → string pool index (如 "utils")
  version:        [4 bytes]  u32   → string pool index (如 "^1.0.0")
  symbol_count:   [4 bytes]  u32
  symbols:        [symbol_count × ImportSymbol]

ImportSymbol:
  name:           [4 bytes]  u32   → string pool index
  kind:           [1 byte]   u8    → Func|Type|Trait|Effect
  resolved_id:    [4 bytes]  u32   → 链接时填充的运行时 ID
```

运行时加载时，`resolved_id` 通过查找已加载的包来填充，实现跨包链接。

## 导出符号

EXPORTS section 声明本包对外可见的符号：

```
EXPORTS section:
  export_count:  [4 bytes]  u32
  entries:       [export_count × ExportEntry]

ExportEntry:
  name:        [4 bytes]  u32   → string pool index (全限定名)
  kind:        [1 byte]   u8    → Func|Type|Trait|Effect
  visibility:  [1 byte]   u8    → pub only (non-pub 不导出)
  internal_id: [4 bytes]  u32   → 包内 FuncId / TypeIndex / ...
```

只有标记为 `pub` 的符号才会出现在 EXPORTS 中。