# 内建类型 (Builtin Types)

## Builtin 机制

nessa 的内建类型（如 `u32`、`f64`、`bool`、`String` 等）并非语言层面的特殊存在，而是通过 builtin 机制在标准库中定义的普通类型。

访问 builtin 的唯一方式是对一个 symbol 字面量取 builtin view，使用 `'builtin` 语法。这种操作**仅在 `std`（及 `core` / `alloc`）等特权包中允许**，用户代码无法直接使用。

```nessa
-- std.builtin 中的定义方式
pub typealias u32 = .u32'builtin
pub typealias f64 = .f64'builtin
pub typealias bool = .bool'builtin
pub typealias String = .String'builtin
```

## 通过 impl 注入方法与常量

内建类型定义后，通过 `impl` 块向其关联作用域注入方法和常量，与用户自定义类型完全一致。**只有特权包可以对 builtin 类型做 `impl`。**

```nessa
impl u32 {
    pub const MAX: u32 = 0xFFFFFFFF
    pub const MIN: u32 = 0

    pub fn to_string(self) -> String { ... }
    pub fn checked_add(self, other: u32) -> ?u32 { ... }
}
```

## Builtin 函数

标准库中的内建函数通过同一 `'builtin` view 绑定，再由带类型的 `const` / `fn` 包装：

```nessa
-- std.math
pub const sin: fn(f64) -> f64 = .sin'builtin
pub const cos: fn(f64) -> f64 = .cos'builtin
pub const sqrt: fn(f64) -> f64 = .sqrt'builtin
```

引擎侧通过 `CallBuiltin` 与共享的 `BuiltinCatalog` 分发；注册时不附带静态类型，动态检查在 builtin 实现内完成。

## 设计意图

- 内建类型与用户定义类型在语言层面没有本质区别
- 标准库可审查——公开接口都在 `std` 源码中可见
- `'builtin` 被严格限制在特权包内，防止用户依赖不稳定底层实现

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


## 集合的当前实现

`std.builtin`暴露独立引擎类型角色`List`和`Map`；它们不是新增Intrinsic，
不会改变已有primitive/trait/null索引。`std.collections`提供受检方法。
Map当前接受String键和Any值，字符串按内容匹配，缺失get/remove返回null；
contains区分缺失和存储null。构造用`Map()`或`Map.new()`，索引语法与List
的apply/update一致：

```nessa
let entries = Map()
entries("answer") = 42
let value: i64 = entries("answer")
```

普通用户代码不能通过impl/字段写入伪造集合内部布局。Map容量目前上限8192，
动态String键在运行时检查。泛型Map参数、任意Hash/Eq键尚未实现，
`{property | expr}`的匿名Object语法也不作为Map字面量。

## 基础类型 trait 的当前实现

`std.traits`为全部整数、`f32`/`f64`、`bool`、`char`、`String`、`Unit`和
`Type`提供显式的`Eq`、`PartialEq`及`Display`实现。实现是普通有类型源码函数，
其具体Self签名、函数ABI、impl记录和vtable随NSBC归档保存；载入旧归档不会
补造这些实现。Eq与PartialEq复用VM的标量比较规则，String按内容比较，128位
整数保持精度。浮点沿用既有NaN不等于自身、正负零相等的语义；这里的Eq接口
不额外承诺浮点比较的自反性。Ord/Ordering排序见后文；Hash及集合trait仍未补齐。

内部`__scalar_eq`避免在Eq函数体内用`==`递归调用自身；Any仅是private native
helper的参数类型，并不会为Any、闭包、Continuation或集合注册上述trait。
字符字面量产生真实Unicode字符immediate，而非String；归档通过独立Char常量
保存Unicode标量。


## 基础排序接口

标准库另提供ordinary Ordering enum及全整数、bool、char、String、Unit的
Ord/PartialOrd源码实现；float只实现PartialOrd。内部`__scalar_cmp`返回
受检?i64符号，由私有std函数映射到Ordering/?Ordering，没有在native ABI
编码type-pool index。helper按精确-1/0/1做Eq比较，避免调用Ord造成自身递归。
bool遵false<true，char按Unicode标量，String按UTF-8字典序，Unit恒equal。
Ord固定cmp返回Ordering，PartialOrd的NaN结果为null，所有关系运算false。
Type、Any、集合及闭包未注册排序能力。详见trait-definition与derivation。

## List 的迭代

List 的 Item 是准确声明的 Any，包含 null、Unit 和其他合法值。
`values.into_iter()` 返回 `std.collections.ListIterator`，`next()` 返回
`IterationStep(Any)`；yielded(null) 与 done 是不同分支。`for` 使用相同协议。

每次 into_iter 以 O(n) 成本复制元素引用并创建独立游标；原列表后续增删改不
改变该次遍历内容。元素对象仍共享，迭代器、快照及载荷均保持正常 GC 根。
重复 next 在耗尽后持续返回 done。泛型 List 尚未接入，不能据此声称支持 List(T)。

## Map 的快照与迭代

`keys()`、`values()`、`entries()` 分别返回键、值、`(String, Any)` pair 的独立
List 浅快照；键为 String，值包含 null、Unit。List 本身的 Item 仍是 Any，
读取 entries 的元素时可以显式检查为 `(String, Any)`。

`map.into_iter()` 返回 `std.collections.MapIterator`，其准确 Item 是
`(String, Any)`，next 返回 `IterationStep((String, Any))`。`for (key,value) in map`
因此直接得到 String 类型的 key 和 Any 类型的 value。每次转换创建独立游标和
pair 快照，之后 Map 的新增、删除、替换不影响遍历；pair 中的对象引用仍共享。
耗尽后 next 持续返回 done。遍历顺序不作保证，不保证插入顺序。

快照的成本为 O(capacity + entries)，需要复制键或元素引用；entries 和迭代器
还为每个条目创建 pair。没有用户回调参与快照构造。旧 Map 身份和布局不改变。
