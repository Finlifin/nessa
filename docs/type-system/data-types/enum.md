# enum

`enum` 是 nessa 中的代数数据类型 (ADT)，用于定义一组互斥的变体 (variant)。每个 variant 可以携带命名参数，`enum` 同样拥有关联作用域。

## 定义

```nessa
enum Direction {
    north,
    south,
    east,
    west,
}

enum Ast {
    `null`,
    id(name: String),
    add(lhs: Ast, rhs: Ast),
}
```

当 variant 名与关键字冲突时，使用反引号转义（如 `` `null` ``）。

## Variant 命名规范

- Error 用途的 enum：variant 使用大驼峰命名法 (`PascalCase`)
- 非 Error 用途的 enum：variant 使用蛇形命名法 (`snake_case`)

```nessa
-- Error enum：PascalCase variant
enum HttpError {
    Timeout(time: Duration),
    ConnectionReseted,
    NotFound(url: String),
}

-- 普通 enum：snake_case variant
enum IpAddr {
    v4(a: u8, b: u8, c: u8, d: u8),
    v6(addr: String),
}
```

## 构造与模式匹配

```nessa
-- 构造
let addr = IpAddr.v4(127, 0, 0, 1)
let tree = Ast.add(Ast.id("x"), Ast.id("y"))

-- 模式匹配
addr match {
    IpAddr.v4(a, b, c, d) => println("{a}.{b}.{c}.{d}"),
    IpAddr.v6(addr) => println("{addr}"),
}

tree match {
    Ast.`null` => println("null"),
    Ast.id(name) => println("id: {name}"),
    Ast.add(lhs, rhs) => println("add"),
}
```

## 关联作用域

```nessa
enum Shape {
    circle(radius: f64),
    rect(width: f64, height: f64),

    fn area(self) -> f64 {
        self match {
            circle(r) => 3.14159 * r * r,
            rect(w, h) => w * h,
        }
    }

    fn is_circle(self) -> bool {
        self matches circle(_)
    }
}
```

## 递归类型

enum 天然支持递归定义（如上面的 `Ast` 示例），运行时通过引用实现。

## 语法参考

```ebnf
enum_def -> enum id { (enum_variant | statement)* }
enum_variant -> id ((param*))?
```


## 当前实现与限制（2026-10-07）

无载荷variant与携带值variant已贯穿源码、运行时和独立归档。运行值携带所属
Enum类型及variant tag；不同Enum即使tag相同也不是同一个值，反射返回实际
Enum类型。透明typealias、前向声明和递归Enum字段保留名义身份。

无载荷variant可直接用`E.none`；带载荷variant用`E.some(value)`构造，支持
位置及具名实参。先按源码顺序求值并保存，再按声明字段顺序组装；未知/重复/
缺必填字段、数量或类型错误诊断。expected字段类型与Any边界检查贯穿构造，
类型不符产生TypeError。字段必须具名并带有效类型注解；可选默认字段和单个List
变参字段遵循下面的参数规则，解构字段尚不支持。带载荷variant目前不能作为
普通一等constructor函数值。

```nessa
enum Packet {
    data(seed: i64, ...items: List, .answer: i64 = seed + items.len()),
}
let packet = Packet.data(40, "payload", null)
packet match { Packet.data(seed, items, answer) => println(answer) }
```

`.field: Type = expression` 只能用具名实参覆盖，省略时按声明顺序求值，默认
表达式可引用前面的字段，包括已打包的变参List。所有显式实参先按源码顺序
求值并保存，再补默认值；显式覆盖的默认表达式不执行。默认值在variant的
声明作用域解析，调用者同名局部变量不影响它；每个variant的参数作用域独立。
未使用的默认表达式也检查类型，不能引用自身或后面的字段。默认值内可创建
捕获前置字段的闭包，或运行有自己break/continue目标的循环；return/resume
不能跨越默认边界，递归默认展开在编译期拒绝。

单个`...items: List`（或透明alias）收集剩余位置实参，零实参生成空List；它
不能按名字提供，后面只能跟可选字段。实际payload与pattern中仍是一个List
槽，不把元素展开成不同variant布局。所有字段有默认时仍写`E.variant()`，
不是直接取`E.variant`。双变参及泛型variant尚未实现。

`match` / `matches`支持递归Enum constructor与静态已知shape的Tuple模式，
以及字面量、普通绑定、`_`和bool guard。Enum模式先检查类型/tag再提取当前
variant实际字段，递归模式和guard失败继续下一arm；绑定限于相应arm作用域。
静态输入为另一Enum时诊断，Any输入可用明确的Enum模式动态分派；Any没有
静态Tuple shape时仍不能使用Tuple模式。类型阶段检查所有arm及guard，不只
检查首arm。没有匹配项时报NoMatchingCase，不无条件执行末arm或返回伪造Unit。
`or` 和 `as` 共用上述匹配路径：备选分支共享准确同名绑定，`as` 保存整个
输入值，支持嵌套字段与 guard。规则和优先级见[模式语法](../../grammar/patterns.md)。

`matches`的绑定限于临时模式作用域，只能在guard中使用；失败、部分失败或
短路后均不能从外部读取，成功分支的flow-sensitive绑定仍待实现。match各arm
统一检查/转换到结果类型，数字分支按提升规则生成同一表示。
Enum本体的关联方法支持受检self调用，包括无载荷值；递归方法的函数身份也
可保存到独立归档。

显示形式为`E.none`或`E.some(42)`；载荷字符串带引号/转义，可嵌套Enum、
Tuple和List。共享显示器使用128层深度和1 MiB输出上限，递归环为`<cycle>`，
超限产生显式错误。载荷为TaggedValue槽，构造转换期间保留根；真实GC回归
包括只由Enum字段保留的字符串和暂停continuation中的Enum。

无载荷值用独立immediate subtag9表示，携带值用受检Enum堆布局。NSBC新常量
及指令保留类型/tag与payload，跨进程加载不依赖原源码或StrId；此处TypeIndex
仍是完整产物内的索引，不是已完成跨包稳定TypeId或链接。

当前相同Enum/tag的无载荷值相等；不同Enum或不同tag比较为false。相同
Enum/tag的携带值在静态类型已实现Eq或PartialEq时调用其比较方法；支持
`derive Eq for E`和`derive PartialEq for E`，按variant及声明字段顺序短路比较，
字段调用对应trait的用户实现或派生函数。未实现trait的携值比较仍明确返回
UnsupportedEnumEquality，不以地址代替字段语义。Any动态比较尚未接入trait分派；
Enum支持Ord/PartialOrd派生：先按variant声明顺序比较，再逐字段比较，
PartialOrd遇到不可比较字段返回null。Hash派生仍明确不支持。
List/struct等其余模式、泛型variant、完整穷尽性分析和trait规则
也未因本轮构造/匹配通过而完成；设计示例不代表这些路径已经支持。
字符值使用独立Char表示并可持久化到NSBC。字符字面量模式支持Unicode标量与
词法支持的转义，适用于match/matches及嵌套Enum/Tuple字段；Any中的String或
整数不会当作同字符匹配，静态不兼容字段会产生类型诊断。


Enum的Display派生已接通：按variant及字段声明顺序调用实际全局字段Display，
Optional支持null，原始String带引号，自定义Display结果不重复加引号。缺少字段
实现时诊断；全局初始化会包含字段方法模块的依赖。通用formatter与struct旧派生
保持原行为。递归预算、真实GC、多次恢复与独立归档的规则见
[派生](../trait-implementation-extension/derivation.md)。逻辑输出预算1 MiB不扩大
String对象的u16 payload限制，较大文本可能先返回ObjectTooLarge。
