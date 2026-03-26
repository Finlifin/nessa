# newtype 与 typealias

## typealias

`typealias` 创建一个类型的别名，别名与原类型完全等价，可互换使用：

```nessa
typealias UserId = u64
typealias Callback = fn(Request) -> Response
typealias StringMap = Map
```

别名不创建新类型——`UserId` 和 `u64` 是同一个类型，所有 `u64` 的 trait 实现自动适用于 `UserId`。

## newtype

`newtype` 创建一个与原类型具有相同运行时表示但在类型系统中不同的新类型：

```nessa
newtype Meters = f64
newtype Seconds = f64
newtype Email = String
```

`Meters` 和 `Seconds` 虽然底层都是 `f64`，但它们是不同的类型，不能直接互换，从而在编译期防止单位混淆等错误。

## newtype 与 trait 继承

> TODO: newtype 是否应该自动继承原类型的 trait 实现？这是一个待决定的设计问题。
>
> - 自动继承：方便，减少样板代码，但可能引入不期望的行为
> - 不继承：安全，但需要手动 impl 或 derive 所需的 trait
> - 选择性继承：通过某种语法声明要继承哪些 trait

## 何时用 typealias，何时用 newtype

| 场景 | 推荐 |
|------|------|
| 缩短复杂类型签名 | `typealias` |
| 提高可读性但不需要类型安全 | `typealias` |
| 需要类型安全的区分 | `newtype` |
| 防止不同语义的值混用 | `newtype` |

## 避免使用 const 绑定类型

即使 nessa 中类型是一等公民（类型为 `Type`），也应避免用 `const` 来引用类型：

```nessa
-- 不推荐
const MyId = u64

-- 推荐
typealias MyId = u64
-- 或
newtype MyId = u64
```

`typealias` 和 `newtype` 在语义上更清晰，且编译器可以对它们做更好的优化和诊断。

## 语法参考

```ebnf
typealias -> typealias id = expr
newtype -> newtype id = expr
```
