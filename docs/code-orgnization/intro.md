# 代码组织 (Code Organization)

nessa 的代码组织体系围绕**包 (package)**、**模块 (module)** 和**关联作用域 (associated scope)** 三个层次展开。

## 核心理念

nessa 中，模块并非特殊的语言构造——它本质上是一种**没有实例化能力的命名类型**。每个命名类型（`struct`、`enum`、`mod`）都拥有自己的关联作用域，可以在其中定义函数、常量、子类型等符号。`mod` 只是一个专门用于代码组织的类型关键字，如果你愿意，完全可以把 `mod` 换成 `struct` 或 `enum`，它们在作用域能力上是等价的。

这意味着：
- `mod` 用于纯粹的命名空间组织
- `struct` 和 `enum` 在拥有数据定义能力的同时，也天然具备模块的组织能力
- 不存在"模块能做但类型不能做"的事情

## 本章内容

- [文件系统模块发现](fs-module-discovering.md)：源码文件如何映射为模块树
- [包管理](package-management.md)：`package.toml`、依赖管理与包唯一性
- [use 语句与路径](use-statement.md)：符号导入与路径解析规则
- [可见性](visiability.md)：三级可见性控制
- [main 与 \_\_init\_\_ 函数](main-and-__init__-fns.md)：程序入口与作用域初始化
