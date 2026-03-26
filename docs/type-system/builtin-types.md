# 内建类型 (Builtin Types)

## Intrinsic 机制

nessa 的内建类型（如 `u32`、`f64`、`bool`、`String` 等）并非语言层面的特殊存在，而是通过 intrinsic 机制在标准库中定义的普通类型。

访问 intrinsic 的唯一方式是对一个 symbol 字面量取 intrinsic view，使用 `'intrinsic` 语法。这种操作**仅在 `std` 包中允许**，用户代码无法直接使用。

```nessa
-- std.builtin 中的定义方式
pub typealias u32 = .u32'intrinsic
pub typealias f64 = .f64'intrinsic
pub typealias bool = .bool'intrinsic
pub typealias String = .String'intrinsic
```

## 通过 impl 注入方法与常量

内建类型定义后，通过 `impl` 块向其关联作用域注入方法和常量，与用户自定义类型完全一致：

```nessa
impl u32 {
    pub const MAX: u32 = 0xFFFFFFFF
    pub const MIN: u32 = 0

    pub fn to_string(self) -> String { ... }
    pub fn checked_add(self, other: u32) -> ?u32 { ... }
}
```

## Intrinsic 函数

除了类型，标准库中的内建函数也通过 intrinsic view 获取：

```nessa
-- std.math
pub const sin: fn(f64) -> f64 = .sin'intrinsic
pub const cos: fn(f64) -> f64 = .cos'intrinsic
pub const sqrt: fn(f64) -> f64 = .sqrt'intrinsic
```

## 设计意图

这种设计使得：
- 内建类型与用户定义类型在语言层面没有本质区别，都遵循相同的类型系统规则
- 标准库是可审查的——所有内建类型的公开接口都在 `std` 源码中可见
- intrinsic 的使用被严格限制在 `std` 包内，防止用户代码依赖不稳定的底层实现

## 常见内建类型一览

| 类型 | 说明 |
|------|------|
| `bool` | 布尔值，`true` / `false` |
| `u8`, `u16`, `u32`, `u64`, `u128` | 无符号整数 |
| `i8`, `i16`, `i32`, `i64`, `i128` | 有符号整数 |
| `usize`, `isize` | 平台相关大小的整数 |
| `f32`, `f64` | 浮点数 |
| `String` | UTF-8 字符串 |
| `char` | Unicode 字符 |
| `Unit` | 无信息类型，字面量为 `()` |
| `NoReturn` | 永不返回类型（类型格下界） |
| `Any` | 所有类型的超类型（类型格上界） |
