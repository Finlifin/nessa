# 实现 (impl)

`impl` 用于两个目的：为类型实现 trait，以及向类型的关联作用域追加符号。

## Trait 实现

```nessa
impl Show for Point {
    fn show(self) -> String = "({self.x}, {self.y})"
}

impl Eq for Point {
    fn eq(self, other: Point) -> bool {
        self.x == other.x and self.y == other.y
    }
}
```

实现 trait 时，必须提供所有 `def fn` 声明的方法。`derive fn` 声明的方法如果不提供，则使用默认实现。

## 关联类型实现

```nessa
impl Iterator for Range {
    assoc Item: Type = i32

    fn next(self) -> ?i32 {
        if self.current < self.end {
            let val = self.current
            self.current = self.current + 1
            val
        } else {
            null
        }
    }
}
```

## 直属 impl 块

不带 `for` 的 `impl` 块向类型的关联作用域追加符号：

```nessa
impl Point {
    fn manhattan_distance(self) -> f64 = abs(self.x) + abs(self.y)
}
```

这等价于在 `struct Point { ... }` 定义体内直接写，但允许在其他位置（如其他文件中）补充定义。

## 孤儿规则 (Orphan Rule)

`impl Trait for Type` 要求当前包至少拥有 `Trait` 或 `Type` 之一。这防止了不同包对同一 trait-type 组合提供冲突的实现。

如果需要为外部类型实现外部 trait，可以使用 `extend`（参见 [扩展](extension.md)）。

## 语法参考

```ebnf
impl_def -> impl expr { statement* }
impl_trait_def -> impl expr for expr { statement* }
```
