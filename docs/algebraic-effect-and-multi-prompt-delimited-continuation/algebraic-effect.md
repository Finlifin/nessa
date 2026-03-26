# 代数效应 (Algebraic Effects)

代数效应是 nessa 中声明和处理可控副作用的核心机制。一个效应描述了一种操作的**签名**，而不规定其**实现**——实现由调用者通过 handler 提供。

## 效应声明

使用 `effect` 关键字声明一个效应：

```nessa
effect log(msg: String) -> Unit
effect read_line() -> String
effect yield(data: Any)
```

效应声明定义了：
- 效应名称（作为标识符）
- 参数列表（与函数参数语法一致）
- 返回类型（省略时默认为 `Unit`）

效应声明可以出现在模块作用域中，并遵循与函数相同的可见性规则：

```nessa
mod io:
    pub effect read_file(path: String) -> !IoErr String
    pub effect write_file(path: String, content: String) -> !IoErr Unit
```

### 语法参考

```ebnf
effect_def -> async? effect id (param*) (-> expr)?
```

> **注意**：`async effect` 用于异步效应，详见 [并发与异步代数效应](../concurrency-and-async-algebraic-effect/intro.md)。

## 效应调用

使用 `#` 后缀运算符发出效应调用：

```nessa
effect log(msg: String) -> Unit

fn greet(name: String) -> #log Unit {
    log("greeting: {name}")#
    println("hello, {name}")
}
```

`expr#` 中的 `expr` 可以是：
- 直接的效应调用（如 `log("hello")#`）
- 返回效应限定类型的函数调用（如 `some_effectful_fn()#`）

`#` 运算符的作用与 `!`（错误传播）完全对称：它将效应向上传播给调用者处理。

### 效应调用的返回值

效应调用会返回一个值——这个值由 handler 提供。例如：

```nessa
effect read_line() -> String

fn ask_name() -> #read_line String {
    -- read_line()# 的值由 handler 决定
    let name = read_line()#
    "hello, {name}"
}
```

handler 通过 `resume` 语义（handler 块最后一个表达式的值，或显式 `resume value`）将值返回给效应调用点。

## 效应传播

函数的返回类型可以用 effect qualified type 标注其可能发出的效应：

```nessa
-- 单个效应
fn foo() -> #log Unit { ... }

-- 多个效应
fn bar() -> #[log, read_line] String { ... }
```

当函数 A 调用了可能发出效应的函数 B 时，A 必须：
1. **传播**效应（在返回类型中声明该效应）
2. 或**消除**效应（使用 handler 处理该效应）

```nessa
effect log(msg: String) -> Unit
effect read_line() -> String

fn interactive() -> #[log, read_line] Unit {
    -- 传播 log 和 read_line 效应
    let name = ask_name()#
    log("user: {name}")#
}
```

效应传播的类型规则与错误传播完全对称：

```
forall t.           t == #[] t              -- 无效应
forall e, t.        #e t == #[e] t          -- 单元素等价
forall e1, e2, t.   #[e1, e2] t == #[e2, e1] t  -- 交换律
```

## 效应消除

### 消除块

使用 `# { ... }` 后缀表达式来消除效应：

```nessa
effect yield(data: Any)

fn generate(list: List) -> #yield Unit {
    list.foreach do |x|:
        yield(x)#
}

fn main() {
    let list = [1, 2, 3, 4, 5]
    generate(list)# {
        yield(data) => println(data),
    }
}
```

消除块中的每个分支 (case arm) 对应一个效应的处理逻辑。当被调用函数发出对应的效应时，该分支中的代码会在**发出效应的上下文中**被执行（参见 [In-Place Handler Application](in-place-handler-application.md)）。

### Handler Application (`.use()`)

使用 `.use()` 中缀表达式应用一个 handler：

```nessa
fn main() {
    let logger = Logger()
    some_effectful_fn().use(logger.log_handler)
}
```

`.use()` 是一种更灵活的消除方式，特别适合与 [capability](capability.md) 模式配合使用。

### 语法参考

```ebnf
effect_propagation  -> expr #
effect_elimination  -> expr # { case_arm* }
handler_application -> expr.use(expr)
```

## `handles` 语句

`handles` 语句在词法作用域中为特定效应绑定 handler。在该作用域内，所有传播出来的对应效应都会自动使用该 handler：

```nessa
fn main() {
    handles log(msg, level) => println("{level}: {msg}");

    -- 后续所有 log 效应调用都会使用上面的 handler
    log("hello", LogLevel.info)#
    log("world", LogLevel.debug)#
    do_something_with_logging()#
}
```

`handles` 语句为当前词法作用域建立了一个 handler 绑定，使得该作用域内传播出来的效应不需要显式消除。

### 语法

```ebnf
handles_statement -> handles pattern => (block | statement)
```

## `resume` 语义

在 handler 块中，`resume` 的行为类似于 `return`——它将一个值传回效应调用点，使被暂停的计算继续执行。

### 隐式 resume

handler arm 的最后一个表达式会被自动 resume：

```nessa
effect get_config(key: String) -> String

fn main() {
    load_config()# {
        -- "default_value" 自动作为 get_config 的返回值
        get_config(key) => "default_value",
    }
}
```

### 显式 resume

使用 `resume value` 可以提前恢复计算，类似于 handler 块中的提前 return：

```nessa
effect validate(data: Any) -> bool

fn main() {
    process_data()# {
        validate(data) => {
            resume true if data.is_valid()
            -- 进行更多检查...
            let result = deep_validate(data)
            result
        },
    }
}
```

`resume` 支持 `if` 条件后缀，与 `return`、`break`、`continue` 保持一致：

```nessa
resume value if condition
```

> **重要**：在没有显式捕获 continuation 的情况下（即无 `catch k`），handler 块完成后计算会自动继续。`resume` 就是 handler 块中的 `return`。关于显式 continuation 捕获的行为差异，参见 [Multi-Prompt Delimited Continuation](multi-prompt-delimited-continuation.md)。

## 完整示例

```nessa
-- 声明效应
effect ask(prompt: String) -> String
effect tell(msg: String)

-- 使用效应的函数
fn conversation() -> #[ask, tell] Unit {
    tell("Welcome!")#
    let name = ask("What is your name?")#
    tell("Hello, {name}!")#
    let age = ask("How old are you?")#
    tell("{name} is {age} years old.")#
}

-- 消除效应：交互式终端实现
fn main() {
    conversation()# {
        ask(prompt) => {
            print(prompt ++ " ")
            read_stdin_line()
        },
        tell(msg) => println(msg),
    }
}

-- 消除效应：测试实现（纯函数式）
fn test_conversation() {
    var responses = ["Alice", "30"]
    var output = []

    conversation()# {
        ask(_prompt) => {
            let (first, ...rest) = responses
            responses = rest
            first
        },
        tell(msg) => {
            output = output ++ [msg]
        },
    }

    assert(output == ["Welcome!", "Hello, Alice!", "Alice is 30 years old."])
}
```

这个例子展示了代数效应的核心价值：`conversation` 函数不关心 I/O 如何实现，同一段代码可以用于真实交互，也可以用于纯函数式测试。