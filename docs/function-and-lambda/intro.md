# 函数与 Lambda (Functions & Lambdas)

nessa 中函数是一等公民，函数值和 lambda 共享同一个类型签名 `fn(...) -> T`。函数定义支持多种参数形式，包括可选参数、变参、trait bound 参数和 lambda 参数。

## 函数定义

```nessa
-- 单表达式函数体，使用 = 语法
fn double(x: i32) -> i32 = x * 2

-- 多语句函数体，使用 block
fn greet(name: String) -> String {
    let msg = "hello, " ++ name
    msg
}

-- 无返回值（返回 Unit）
fn log(msg: String) {
    println("[LOG] " ++ msg)
}
```

## 参数形式

### 普通参数 (param_typed)

```nessa
fn add(a: i32, b: i32) -> i32 = a + b
```

### self 参数

方法的第一个参数为 `self`，表示调用者实例：

```nessa
struct Counter {
    value: i32,

    fn increment(self) -> Counter {
        Counter { value: self.value + 1 }
    }
}
```

### 可选参数 (param_optional)

以 `.` 开头，带默认值。调用时使用命名参数语法：

```nessa
fn man(.msg: String = "") -> String = "man: " ++ msg

man()                -- "man: "
man(msg = "alright") -- "man: alright"
```

显式实参按源码顺序求值，再绑定到声明中的参数槽；命名实参不会改写同名局部变量。
省略的默认值随后按参数声明顺序求值，可以引用之前已绑定的参数，不能引用自身或后续参数。
默认表达式使用函数声明处的名称作用域，显式提供值时不执行默认表达式。
未知参数名、重复绑定、缺少必填参数及类型不匹配均产生编译诊断。

当前实现对已知函数声明及直接 lambda 调用生成完整参数列表。仅有 `fn(...) -> T`
类型的函数值不携带参数名或默认表达式，调用时须提供完整的位置参数。
当前默认值不支持跨表达式边界的 `return` / `resume`，会产生编译诊断；
嵌套函数自己的返回不受此限制。单 List 变参与双 List/Map 变参已实现；
一般方法的完整默认/命名参数绑定仍待补齐。仅有 FnType 的函数值不自动附加
声明默认值或变参打包。

### Trait bound 参数

使用 `pattern : TraitName` 语法约束参数必须实现某个 trait（与普通类型标注语法一致）：

```nessa
fn print_it(x: Show) {
    println(x.show())
}
```

### 变参 (param_varargs)

使用 `...` 前缀，所有额外参数收集进一个 `List`。变参只占一个元数 (arity)：

```nessa
fn count(...args: List) -> i64 { args.len() }

count("a", "b", "c", "d") -- 4
```

当前单变参收集动态 Any 元素，须标注 List 或其透明别名；它占一个实际参数槽。
已知声明及直接 lambda 才使用此绑定计划；只有函数类型的间接值须显式传入 List。
允许变参前的固定位置参数和变参后的可选命名参数，默认值可以引用已打包的 List。
按参数名直接供给变参、变参后的普通固定参数及非 List 注解会诊断。
超过32个源码变参不会挤入32寄存器参数窗口，按源序保存后打包。
泛型元素约束尚未实现。具体 List 已支持 Iterator(Item=Any) 及 map/filter/fold/each/foreach。

### 双变参 / 扩展变参函数

已知声明的双变参调用将表达式打包为 List、属性打包为 Map。具体 Map 已实现动态键值操作及快照迭代。

当函数同时有两个变参时，表达式部分收集进 `children: List`，属性对收集进 `properties: Map`，占两个元数。暂时不允许有其他参数：

```nessa
fn div(...children: List, ...properties: Map) -> Component { ... }
```

已知双变参函数只能用 extended application 语法调用：

```nessa
fn gather(...children: List, ...properties: Map) -> i64 {
    children(0).as(i64) + properties("answer").as(i64)
}

gather { 40, answer: 1, answer: 2 } -- 42，最后一个 answer 覆盖此前的值
```

先求值 callee 或 receiver，再按混合源码顺序求值所有 children 和属性值，各一次；
随后创建两个独立容器。重复属性的所有值表达式都会执行，最后一个值生效。
属性名转为字符串键，不读取同名变量。List 与 Map 必须按此顺序声明，允许
透明类型别名；隐式 self 与 effect 注入的 catch 不占这两个调用者参数槽。
其它调用者参数、倒置容器类型及未使用的非法声明都会被拒绝。

已知函数、模块/import 别名、直接 lambda、静态/实例/trait 默认方法及 effect
支持此协议。仅有 `fn(List, Map) -> T` 类型的间接值不能使用扩展打包语法，
但仍可通过普通调用显式传入两个完整容器。双变参 enum 分支尚未支持。

结构体的字段构造也使用 extended application，按结构体构造规则处理。
以下 Component 和匿名 Object 嵌套示例表达更完整的设计目标；匿名 Object
尚未实现，不能将它视为已经可编译的 Map 字面量：

```nessa
let component = div {
    h1 { "hello world" },
    section { ... },

    -- 这是属性，value 是 object
    -- extended application 的后半部分就是 object 语法，不要搞混
    layout: {
        margin: "2em",
        ...
    },
}
```

### Lambda 参数 (param_lambda)

用 `lambda` 关键字限定的参数，调用时适用 tacit lambda 写法。`_0`、`_1` 等是隐式参数占位符：

```nessa
fn sort(list: List, lambda f: fn(Any, Any) -> math.Order) -> List { ... }

sort([234, 34, 3, 452, 3], _0 < _1)
-- 等价于：
sort([234, 34, 3, 452, 3], |a, b| a < b)
```

## Lambda 表达式

Lambda 使用 `|params|` 语法定义：

```nessa
let add = |a: i32, b: i32| a + b
let greet = |name: String| {
    println("hello, " ++ name)
}
```

Lambda 的参数类型通常可以从上下文推导，无需显式标注：

```nessa
let numbers = [3, 1, 4, 1, 5]
numbers.map(|x| x * 2)
numbers.filter(|x| x.as(i32) > 2)
```

当前 List 的元素是 Any。高阶方法使用以下准确契约：

| 方法 | 回调类型 | 结果 |
| --- | --- | --- |
| map | fn(Any) -> Any | List |
| filter | fn(Any) -> bool | List |
| fold | fn(Any, Any) -> Any | Any |
| each / foreach | fn(Any) -> Unit | Unit |

这些方法按原槽位顺序遍历调用开始时的浅快照；回调 push/pop/set 原列表不会
改变本次输入，元素对象仍共享。null 和 Unit 均是正常元素，空 fold 返回 initial。
上下文 lambda 检查参数数目和结果类型；具名或 Any 回调在间接调用时检查实际
函数的参数，返回值满足调用位置的契约。闭包按值捕获标量，捕获的堆对象保持共享。
continuation 多次恢复各自保留栈上的遍历位置与标量 accumulator，堆对象不会复制。
这些高阶方法支持尾随回调，以下三种调用等价：

```nessa
numbers.map(|x| x * 2)
numbers.map() do |x| x * 2
numbers.map do |x| x * 2
```

`numbers.fold(0) do |acc, x| acc + x` 将闭包追加到已有实参。
`invoke do { return 42 }` 传入零参数闭包；return 返回到 invoke 的回调调用处。
上下文类型、默认值、命名参数和变参使用普通调用的规则。连接后续调用时，可以
用括号或局部变量明确 lambda 体的范围。
泛型 List(T) 与完整 tacit lambda 仍待实现。

## 函数类型

函数和 lambda 共享同一个类型表示：

```nessa
fn(i32) -> i32
fn(String, String) -> bool
fn() -> Unit
```

详见 [函数、闭包与效应的类型](../type-system/the-types-of-functions-closures-and-effects.md)。

## 本章内容

- [特殊函数 (new / apply / update)](special-fns.md)
- [Lambda 捕获规则](lambda-capturing-rules.md)

## 语法参考

```ebnf
function_def -> fn id ((param*)) (-> expr)? (handles expr)? ((= expr) | block)
lambda -> |param*| (-> type)? (block | statement)

param ->
    param_optional |     -- .id : expr = expr
    param_typed |        -- pattern : expr
    param_self |         -- self
    param_varargs |      -- ...id (: expr)?
    param_lambda         -- lambda pattern (: expr)?
```
