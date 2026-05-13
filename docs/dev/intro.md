# Nessa 引擎开发文档

本目录包含 Nessa 引擎各子系统的宏观设计文档。

## 文档索引

| 文档 | 内容 |
|------|------|
| [architecture.md](architecture.md) | 系统总体架构、编译管线、运行时架构、Crate 组织 |
| [compiler-interview-feature-guide.md](compiler-interview-feature-guide.md) | 面向编译岗位面试的语言特性讲解顺序与回答模板 |
| [runtime-representation/](runtime-representation/) | Tagged Pointer、堆对象布局、值的运行时表示 |
| [memory-management/](memory-management/) | MMTK + LXR GC、Task Stack Pool、外部对象管理 |
| [nessa-bytecode/](nessa-bytecode/) | 64-bit 指令集设计、NSBC Archive 格式、Safe Point |
| [normalied-intermediate-representation/](normalied-intermediate-representation/) | NIR 设计：脱糖、基本块、闭包转换 |
| [static-and-dynamic-decision-procedures.md/](static-and-dynamic-decision-procedures.md/) | 名称解析、类型推断、Effect 静态/动态分派决策 |
| [engine-initialization/](engine-initialization/) | 引擎启动流程、TypePool 预填充、GC/Scheduler 初始化 |

## 编译管线概览

```
Source (.ns)
  → Lexer (TokenStream)
  → Parser (AST)
  → Resolution (Resolved AST + SymbolTable + TypePool)
  → NIR Lowering (Normalized IR)
  → Codegen (Bytecode + StackMaps)
  → Archive Emission (.nsbc)
  → Interpreter (执行)
```

## 各 Crate 实现状态

| Crate | 状态 |
|-------|------|
| `str_interner` | 已完成 |
| `diagnostic` | 已完成 |
| `ast` | 基本完成，需同步最新设计 |
| `lexer` | 已完成 |
| `parser` | 基本完成，需同步最新设计 |
| `resolution` | 待实现 |
| `type_pool` | 待实现 |
| `nir` | 待实现 |
| `codegen` | 待实现 |
| `instruction` | 待实现 |
| `interpreter` | 待实现 |
| `runtime` | 待实现 |
| `gc` | 待实现 |
| `scheduler` | 待实现 |
| `nsbc_io` | 待实现 |
| `initialization` | 待实现 |
| `driver` | 待实现 |
| `pkg_manager` | 待实现 |
| `nessa` | 待实现 |
