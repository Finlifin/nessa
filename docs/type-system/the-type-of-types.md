# Type — 类型的类型

在 nessa 中，类型本身也是值，其类型为 `Type`。`Type`类型不可动态构造。

## 基本概念

```nessa
-- 类型可以作为值使用
let t: Type = i32
let u: Type = String

-- 类型可以作为参数传递
fn size_of(T: Type) -> usize { ... }
size_of(i32)
```

`Type` 是 nessa 类型系统中的一种特殊类型——它是所有类型的类型。当你写下 `i32` 时，它既是一个类型（用于类型标注），也是一个 `Type` 类型的值（用于值上下文）。

## 与 typealias / newtype 的关系

虽然类型是一等公民，但定义类型别名或新类型时应使用专门的语法：

```nessa
-- 推荐
typealias Id = u64
newtype Meters = f64

-- 不推荐（虽然合法）
const Id: Type = u64
```

详见 [newtype 与 typealias](newtype-and-typealias.md)。

## view 语法获取类型

可以通过 `'type` view 在运行时获取值的类型：

```nessa
let x = 42
let t = x'type    -- t: Type, 值为 i32
```
