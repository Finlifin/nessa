# 静态与动态决策过程

Resolution 阶段是编译管线中最复杂的阶段，负责将 AST 中的名称、类型、effect 全部解析为明确的引用。本文档描述 Resolution 的整体架构和各子过程。

## Resolution 总览

```
AST + SourceInfo
    │
    ▼
┌──────────────────────────────────────────────────────────────────┐
│ Resolution 阶段                                                  │
│                                                                  │
│  ┌────────────────┐                                              │
│  │ 3a. Name       │ → 构建作用域树，解析所有标识符绑定              │
│  │    Resolution  │   use 展开，可见性检查                         │
│  └───────┬────────┘                                              │
│          │                                                       │
│  ┌───────▼────────┐                                              │
│  │ 3b. Type       │ → 构建类型格，双向类型推断                     │
│  │    Resolution  │   Qualified type 展开，TypeID 计算             │
│  └───────┬────────┘                                              │
│          │                                                       │
│  ┌───────▼────────┐                                              │
│  │ 3c. Effect     │ → 收集 effect 声明，evidence passing 编译      │
│  │    Resolution  │   标记 static/dynamic dispatch                │
│  └───────┬────────┘                                              │
│          │                                                       │
│  ┌───────▼────────┐                                              │
│  │ 3d. Trait &    │ → trait 约束求解，方法查找                     │
│  │    Method Res. │   vtable 构建，特殊函数绑定                    │
│  └────────────────┘                                              │
│                                                                  │
└──────────────────────────────────────────────────────────────────┘
    │
    ▼
Resolved AST + SymbolTable + TypePool
```

## 静态 vs 动态决策

Nessa 是一个渐进类型 (gradual typing) 系统：大部分代码是静态类型的，但通过 `Any` 类型提供动态灵活性。这导致许多决策存在"静态路径"和"动态回退路径"：

| 决策 | 静态路径 (类型已知) | 动态路径 (涉及 Any) |
|------|-------------------|-------------------|
| 方法调用 | 编译时解析 FuncId，直接 CALL | 通过 vtable/TypeIndex 运行时查找 |
| 字段访问 | 编译时计算偏移量 | 通过 FieldIndex 运行时查找 |
| Effect handler | Evidence passing (O(1)) | 栈/task-tree 搜索 |
| 类型转换 | 编译时验证兼容性 | 运行时 TypeCheck + TypeCast |
| 模式匹配 | 编译时决策树 | 需运行时类型检查分支 |

## 3a. 名称解析 (Name Resolution)

详见 [name-resolution.md](name-resolution.md)。

核心过程：
1. 构建作用域树——基于 Type ↔ Scope 双射（类型 → 函数 → 块，层层嵌套）
2. 解析每个标识符到其定义点（DefId）
3. 处理 `use` 语句（展开、重命名、通配符、重导出）
4. 检查可见性约束（pub / package-private / private）
5. 统一解析 `.` 投影（`mod.fn` 与 `struct.method` 使用同一查找机制）

## 3b. 类型解析 (Type Resolution)

### 类型格构建

```
        Any                    ← 顶类型
       / | \
      /  |  \
   i32  String  List(i32)     ← 具体类型
      \  |  /
       \ | /
      NoReturn                 ← 底类型
```

所有类型在格中有明确的子类型关系：
- `NoReturn <: T <: Any` 对所有类型 T 成立

### TypeID 计算

每个类型分配 128-bit TypeID：
- 基础类型: 预定义常量 (u32 → 固定 ID)
- 用户类型: 基于完全限定名 + 包 hash 的确定性计算
- 泛型实例: TypeID(base) ⊕ TypeID(params...)

TypeIndex 是 TypeID 在当前 TypePool 中的压缩索引 (u32)。

## 3c. Effect 解析 (Effect Resolution)

### Evidence Passing 编译

对于静态已知类型的 effect 调用，编译器在函数签名中注入隐式 evidence 参数：

```
原始签名: fn greet() #ReadLine
编译后:   fn greet(_ev_read_line: ?&Handler_ReadLine)

调用处:
  greet() # { read_line() => "world" }
编译为:
  let _handler = Handler_ReadLine { impl: |_| "world" }
  greet(&_handler)
```

### 动态 Effect Dispatch 判定

当遇到以下情况时，effect 调用标记为 dynamic dispatch：
1. 调用对象类型为 `Any`（无法确定需要哪个 handler）
2. effect 调用穿越 `Any` 类型的函数边界
3. 通过 task-tree 跨 task 传递 handler

动态路径在运行时沿调用栈和 task-tree 搜索 handler。

## 3d. Trait 与方法解析 (Trait & Method Resolution)

### 方法查找顺序

1. 类型自身关联作用域中的方法
2. impl 块中定义的方法
3. trait 默认实现
4. extend 块中的扩展方法

### 特殊函数绑定

| 函数名 | 用途 | 调用场景 |
|--------|------|---------|
| `new` | 构造函数 | `Type(args...)` 语法 |
| `apply` | 函数调用语义 | `value(args...)` 对非函数类型 |
| `update` | 索引赋值 | `value[key] = val` |
| `iter` | 迭代器 | `for item in value` |

### vtable 构建

对于 trait object (动态分派)，编译时构建 vtable：
```
VTable:
  type_id:     u128
  method_count: u32
  methods:      [FuncPtr; method_count]    -- 按 trait 方法声明顺序排列
```

## 诊断

Resolution 阶段产生的典型错误：
- 未定义的标识符
- 类型不匹配
- 不可达的模式匹配分支
- 未处理的 effect
- 可见性违规
- 循环依赖

所有错误通过 `diagnostic` crate 报告，携带 Span 信息用于精确的源码定位。