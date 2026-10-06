# 类型系统 (Type System)

nessa 拥有一个静态类型系统，支持类型推导、代数数据类型、trait 多态、限定类型 (qualified types) 以及渐进类型 (gradual typing)。

## 类型格 (Type Lattice)

nessa 的所有类型构成一个格 (lattice)：

- 上界 `Any`：所有类型的超类型，用于渐进类型场景
- 下界 `NoReturn`：所有类型的子类型，表示永不返回（如无限循环、panic）
- `Unit`：无信息类型，类似于其他语言中的 `void`，字面量为 `()`

```
        Any
       / | \
     ...类型...
       \ | /
      NoReturn
```

## 类型是一等公民

在 nessa 中，类型本身也是值，其类型为 `Type`。这意味着类型可以被绑定到变量、作为参数传递。不过，即使类型是一等的，也应优先使用 `typealias` 和 `newtype` 来定义类型别名和新类型，而非直接用 `const` 绑定。

## 本章内容

### 基础类型
- [内建类型](builtin-types.md)：builtin 类型的定义方式
- [数据类型 — struct](data-types/struct.md)
- [数据类型 — enum](data-types/enum.md)
- [数据类型 — tuple](data-types/tuple.md)
- [newtype 与 typealias](newtype-and-typealias.md)

### 类型特性
- [Type — 类型的类型](the-type-of-types.md)
- [Any 与渐进类型](any-the-type.md)
- [类型 ID](type-id.md)
- [类型推导](type-inference.md)
- [数值类型提升](numeric-type-lifting.md)

### 函数与效应类型
- [函数、闭包与效应的类型](the-types-of-functions-closures-and-effects.md)

### 限定类型 (Qualified Types)
- [Error 限定类型](qualified-types/error-qualified-type.md)
- [Optional 类型](qualified-types/optional-type.md)
- [Effect 限定类型](qualified-types/effect-qualified-type.md)

### Trait、实现与扩展
- [概述](trait-implementation-extension/intro.md)
- [Trait 定义](trait-implementation-extension/trait-definition.md)
- [实现 (impl)](trait-implementation-extension/implementation.md)
- [扩展 (extend)](trait-implementation-extension/extension.md)
- [派生 (derive)](trait-implementation-extension/derivation.md)
- [动态分派](trait-implementation-extension/dynamic-dispatching.md)

### 关联作用域
- [关联作用域](associated-scope.md)
