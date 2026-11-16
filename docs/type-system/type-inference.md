# 类型推导 (Type Inference)

nessa 尽可能通过类型推导来获取足够多的类型信息，减少显式类型标注的负担。

## 基本推导

```nessa
let x = 42                    -- 推导为整数字面量类型，根据上下文确定具体类型
let s = "hello"               -- String
let list = [1, 2, 3]          -- List
let pair = (true, 3.14)       -- (bool, f64)
```

## 函数返回值推导

单表达式函数体可以省略返回类型标注：

```nessa
fn double(x: i32) -> i32 = x * 2     -- 显式标注
fn double(x: i32) = x * 2            -- 推导返回类型为 i32
```

静态已知的无环调用依赖先推导被调用方，再检查调用方，因此无注解函数的
返回类型不依赖声明顺序。函数别名、返回函数的 factory 和静态投影也使用
已完成的结果类型；未使用的 effect 默认值同样检查，不能以临时 `Any`
放过已知的类型不匹配。

块内具名函数在整个块中前向可见；较晚的同名函数也会遮蔽较早使用的外层
函数。局部变量仍按源码顺序可见。具名函数不能捕获外层局部值，需要捕获
时使用 lambda。

显式返回类型可以为递归提供检查边界。无返回注解的循环函数目前保留
渐进 `Any`，尚未求解循环内的返回类型方程；显式 `Any` 和异构返回值也
保留运行时检查。实现与验证状态见[依赖推断记录](../dev/function-inference-audit.md)。

## 闭包参数与同构容器推导

### 1. 同构动态容器的元素与键值流向推导

尽管 `List`、`Set`、`Deque`、`Map` 与 `HashMap` 在运行时保持统一动态容器表示以兼顾渐进类型互操作，编译器会在词法范围内追踪未逃逸容器的元素/键值同构来源（如字面量 `[1, 2, 3]`、空容器 `List()` / `Set.new()` / `Deque.new()` / `Map()` 后续同构的 `.push` / `.insert` / `.push_back` / `.set`、`String.split` / `.chars` / `.bytes`、`Bytes.to_list` 等），自动将具体类型注入下游回调与循环绑定而无需 `.as(T)`：

```nessa
let sum = [1, 2, 3, 4, 5]
    .map(|x| x * 3)               -- |x| 自动推导为 i64，返回同构 List(i64)
    .filter(|x| x > 5)            -- |x| 自动推导为 i64
    .fold(0, |acc, x| acc + x)    -- 归纳推导：acc: i64, x: i64，整体返回 i64

let words = List()
words.push("nessa")
words.push("lang")
for w in words {                  -- w 在循环体内自动窄化为 String
    println(w.len())
}
```

对于异构容器（如 `[1, "two"]`）或已跨未知边界逃逸的容器，回调与循环绑定平滑回退为渐进 `Any` 并保留运行时边界检查。

### 2. 局部未标注闭包的调用点反向参数推导

当局部未标注 `let f = |a, b| ...` 在同一块内仅作为直接被调函数使用且未被重赋值时，编译器会汇总块内调用点的实参公共超类型反向推导闭包形参类型：

```nessa
let combine = |s, scale| s.len() * scale
let r = combine("nessa", 6) + combine("ok", 6)   -- s: String, scale: i64, 返回 i64
```

## 控制流敏感类型窄化与延迟 `null` 局部变量推导

### 1. 延迟 `null` 初始化推导

未显式标注类型且初始值为 `null` 的局部变量（`let` / `var`），会根据所属块后续兼容赋值自动升级为 `?T`，同时对不兼容的异构赋值静态报错：

```nessa
var found = null          -- 根据后续赋值推导为 ?i64
for i in 1..10 {
    if i == 7 {
        found = i * 6
    }
}
```

### 2. 控制流敏感窄化（Smart Casts）

对未被闭包可变捕获的局部绑定与形参，编译器在以下控制流路径中自动窄化其静态类型：

- `x != null` / `x == null` 在 `if`/`else`、`while`、`when` 分支、`and`/`or` 短路右操作数，以及早退守卫（`if x == null { return ... }`）之后的支配路径中将 `?T` 窄化为 `T`；
- 对 `?T` 变量直接赋非空兼容值（`x = 12`）后，在后续顺次语句中将其窄化为 `T`，直至发生可空重赋值或汇合失效；
- `if v matches _: T` 与 `if opt matches _: _?` 在真分支中将 `v` 窄化为目标类型 `T` 或非空载荷类型。

## 双向期望传播与枚举变体简写

- **效应与错误修饰穿透**：当函数、闭包、分支或算术表达式处于 `#Effect T`、`!Err T` 或 `?T` 期望上下文中时，内层载荷字面量与子表达式自动接收剥离修饰后的目标类型 `T`（例如 `fn compute() -> #Audit i32` 函数体尾部的 `42` 直接推导为 `i32`）。
- **目标类型驱动的 `.variant` 简写**：在已知目标枚举类型 `E`（包括变量标注、函数实参、`return`、`!Err` 错误构造 `error .variant`、`match` / `matches` 模式）的上下文中，可直接书写 `.variant` 或 `.variant(args)` 代替 `E.variant`。
- **`usize` 区间边界安全转换**：区间字面量 `0..buf.len()` 支持 `usize` 边界并插入受检 `i64` 转换。

## 类型标注与 as cast

当推导信息不足时，可以通过类型标注或 `as` cast 提供提示：

```nessa
-- 类型标注
let x: f64 = 0

-- as cast 用于类型提升
let result = some_fn().as(!ParseErr !HttpErr String)
```

nessa 中有较多依赖类型推导的类型提升风格，熟练使用 `as` cast 很重要。

## 推导的边界

以下场景仍需要显式类型标注或转型：

- 具名函数参数类型（始终需要显式标注）
- 无返回类型注解的递归/互递归函数环（按语言契约保留渐进 `Any` 返回类型，显式标注任意环锚点返回类型即可静态固定整条调用链）
- 无目标枚举上下文的独立 `.name` 符号字面量
- `Any` 类型在非受控分支下的显式向下转换
