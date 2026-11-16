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


## 当前实现边界

- **比较派生（`Eq` / `PartialEq` / `Ord` / `PartialOrd`）**：支持 `struct`、`enum`、`newtype` 和 tuple `typealias`，生成普通受检函数并写入 `.nsbc` 归档，执行无需原源码。`Ord` 要求已具备 `Eq`，`PartialOrd` 要求已具备 `PartialEq`。`Any`、`List`、`Map`、闭包及缺少对应全局 trait 的字段在编译期诊断拒绝。
- **哈希派生（`Hash`）**：支持 `struct`、`enum`、`newtype` 和 tuple `typealias`，组合各字段的全局 `Hash.hash(self) -> u64`（FNV-1a 64-bit 混合变体标签与字段哈希），与标量内建哈希（正规化 `-0.0` 与跨整数位宽数值一致性）保持一致，供 `std.collections.{HashMap, Set}` 使用。
- **显示派生（`Display`）**：`struct`、`enum`、`newtype` 与 tuple `typealias` 统一采用组合式受检函数降级，逐字段调用全局 `Display.to_string(self) -> String`（字符串字段带引号与转义，自定义 `Display` 直接拼接；`struct` 格式为 `Name { field: value, ... }`，`enum` 为 `E.variant(...)`，`newtype` 为 `Name(inner)`），具备 128 层深度预算、`<cycle>` 环检测与 1 MiB 输出上限。
- **自定义 Trait 派生（`derive UserTrait for T`）**：当目标 trait 的所有方法均由 `derive fn` 提供默认实现，且目标类型已满足该 trait 的全部父 trait（supertraits）时，可通过 `derive` 直接生成受检实现；若仍含未实现的 `def fn` 必填方法或缺少父 trait 实现，则在编译期明确报错。


## Enum与Tuple的Display派生

Enum和Tuple typealias可派生普通 `fn(Self)->String`，逐字段调用实际全局
Display实现，检查真实返回值为String。先登记全部派生签名，再检查前置条件，
支持前向及递归声明；局部extend不能充当全局派生证据。缺少字段Display时诊断，
不回退到通用Any formatter。字段方法所属模块也加入初始化依赖，确保方法读取
全局值前该模块已初始化。

Enum保留 `E.none` / `E.some(value, ...)` 格式；Tuple使用括号，单元素带尾逗号。
原始String字段加引号及转义；自定义Display返回的文本直接插入，不再次加引号。
Optional的null显示为 `null`，非空值调用内部类型的Display。嵌套Tuple优先使用
显式全局Display实现，否则递归组合；嵌套struct沿用上述旧派生行为。

显示状态归调用帧所有：实际Display调用及内联Tuple计入128层预算，普通辅助
函数不增加深度；当前路径的堆聚合环显示为 `<cycle>`。活跃路径加入GC根，
效应捕获保存所在栈段的状态，多次恢复的clone各自复制状态。逻辑输出上限为
1 MiB，拼接、转义及实际方法返回均检查预算。当前String对象仍受u16 payload
字数限制，约524 KB时可能先返回ObjectTooLarge；逻辑上限不表示能分配1 MiB
String，未扩大String或GC布局。

生成函数的Display owner写入NSAM6，读取及安装检查全局派生身份、具体签名和
Value入口ABI。builtin ABI7提供转义和内联Tuple进入/退出辅助函数；旧ID120
及通用to_string语义保持。删除源码后的归档执行、损坏元数据拒绝、真实GC及
continuation多次恢复均有回归覆盖。

## Ord与PartialOrd派生

新源码可为Struct、Enum和Tuple typealias派生Ord/PartialOrd，生成普通函数，
返回Ordering/?Ordering。Ord需已有Eq，PartialOrd需已有PartialEq。所有组成
字段需全局目标排序实现；PartialOrd字段可以使用Ord实现。禁止把局部扩展
证据提升为全局derive契约，Any/集合/闭包等不满足前置要求。

比较遵循字段声明顺序，遇第一个less/greater立即返回；PartialOrd遇第一个
null立即返回，不再求后续字段。Enum先按variant声明顺序，再比较同variant
字段。Optional的null排在nonnull之前，两个null相等，nonnull递归字段接口。
递归与前向声明先登记派生签名再检查前置条件；有自定义字段方法时执行真实
用户函数、效应及GC路径，不能退回host按地址或布局的比较。Ord默认关系
方法独立注入，与用户覆盖相容。递归循环对象的比较预算仍需后续完善。

旧归档的struct native Ord sentinel维持原整数协议，不能假装已经拥有新
Ordering接口；新源码不会发布该sentinel为排序目标。
