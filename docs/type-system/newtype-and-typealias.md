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

## newtype 构造、转换、模式匹配与 trait 继承

`newtype` 采用**显式不自动继承**策略：

- **名义类型身份**：每个 `newtype` 拥有独立的名义 `TypeId` 与关联作用域，运行时值包装并保留名义身份，`type_of(Meters(1.0)) == Meters` 且不等于 `f64` 或 `Seconds`。禁止直接或间接自引用循环（如 `newtype A = A`），编译期报告循环诊断。
- **构造与显式转换**：使用 `Meters(3.5)` 构造实例；通过 `m.as(f64)` 解包为底层类型，或通过 `(3.5).as(Meters)` 显式包装，不允许隐式退化为底层类型。
- **模式匹配**：支持构造器模式解构，如 `m match { Meters(v) => v }`。
- **Trait 实现与派生**：不自动继承底层类型的 trait 实现，防止意外暴露底层运算；支持显式 `impl` / `extend`，以及通过 `derive Eq, PartialEq, Ord, PartialOrd, Display, Hash` 或自定义无必填方法 trait 的 `derive` 按需派生（`Display` 输出 `Name(inner)` 格式）。

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
