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

Eq/PartialEq 支持 struct、enum 和 tuple typealias，生成普通受检函数。Enum 先
比较 variant，再按声明顺序短路比较字段；嵌套 struct/enum 调用目标 trait 的
`eq(self, other: T) -> bool` 实现，PartialEq 字段可回退 Eq。Tuple 和 Optional
字段递归组合；数值、bool、字符串、Unit 与 Type 使用原生值比较。所有派生先
登记，再检查字段先决条件，因此前向引用与递归 enum 不依赖声明次序。

Any、List、闭包及缺少目标 trait 的字段不能派生比较；无效方法签名会诊断。
生成函数保留正常调用帧、GC 根与效应处理路径，真实函数身份写入 NSBC，加载
无需原源码。循环对象的递归比较预算尚未实现。

Hash 派生目前明确诊断未支持，不返回占位 hash。Display及Ord/PartialOrd
派生见后文，已有路径不代表完整trait设计完成。
Any 的动态比较尚未选择用户 trait。


新源码的 struct Display 派生也发布普通有类型函数 `fn(Self)->String`，
可通过 `Display` 参数证明调用。其内部使用 builtin ABI4 的 ID120，保持
原 `Name { field: value, ... }` 字段格式与 Unicode 文本，不以通用 Any
to_string 替代。入口检查 receiver，返回前检查实际 String；native 验证
执行函数与全局派生 Display 记录身份、真实签名和布局。删除源码后的归档
及真实 GC/continuation 多次恢复已验证。旧 sentinel metadata 不自动升级
为证明可执行目标。此旧字段 formatter 不调用字段的用户 Display 方法。

用户选择暂时保留struct的旧派生行为：struct仍使用ID120，不新增字段Display
前置要求，Any等已有字段仍沿旧formatter；不把struct内部字段迁移为用户方法调用。
Enum/Tuple新增派生路径单独实现，调用嵌套struct的Display时仍尊重这个旧实现。


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
