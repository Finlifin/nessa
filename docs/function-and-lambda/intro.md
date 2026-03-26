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
fn log_all(...args: List) {
    args.each(|a| println(a.as(String)))
}

log_all("a", "b", "c", "d")
```

### 双变参 / 扩展变参函数

当函数同时有两个变参时，表达式部分收集进 `children: List`，属性对收集进 `properties: Map`，占两个元数。暂时不允许有其他参数：

```nessa
fn div(...children: List, ...properties: Map) -> Component { ... }
```

扩展变参函数只能用 extended application 语法调用。结构体的构造语法也是 extended application，只是通常忽视 children 部分：

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
