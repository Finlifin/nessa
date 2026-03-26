# 派生 (derive)

`derive` 用于自动为类型生成 trait 实现，减少样板代码。

## 基本语法

```nessa
derive Eq, Show for Point
derive Ord, Hash for UserId
```

一条 `derive` 语句可以同时派生多个 trait。

## 工作原理

编译器根据类型的结构（字段类型、variant 等）自动生成 trait 方法的实现。例如，为一个 struct 派生 `Eq` 时，编译器会生成逐字段比较的 `eq` 方法。

```nessa
struct Point {
    x: f64,
    y: f64,
}

derive Eq for Point
-- 等价于：
-- impl Eq for Point {
--     fn eq(self, other: Point) -> bool {
--         self.x == other.x and self.y == other.y
--     }
-- }
```

## 前置条件

派生要求类型的所有组成部分（字段类型、variant 参数类型）都已实现目标 trait。例如，`derive Eq for Point` 要求 `f64` 已实现 `Eq`。

## 语法参考

```ebnf
derive_def -> derive expr* for expr
```
