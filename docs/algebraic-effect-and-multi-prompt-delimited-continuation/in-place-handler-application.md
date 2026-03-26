# In-Place Handler Application

In-place handler application 是 nessa 中代数效应的**默认执行策略**。其核心思想是：handler 在定义处为每个 handler entry 生成一个闭包，当效应被调用时，直接在发出 effect call 的上下文中调用该闭包，而不是将控制流转移到 handler 定义处。

## 设计动机

传统代数效应的实现通常基于 continuation：

1. 在效应调用点捕获当前 continuation
2. 将控制流转移到 handler
3. handler 处理后通过 resume 恢复 continuation

这种方式功能强大，但 continuation 的捕获和恢复开销不低。在绝大多数实际场景中，handler 只是执行一些逻辑然后让计算继续——并不需要真正捕获 continuation。

In-place handler application 针对这种常见场景进行了优化。

## 工作原理

### 1. 闭包生成

在 handler 定义处（消除块或 `handles` 语句），编译器为每个 handler entry 生成一个闭包：

```nessa
-- 源码
foo()# {
    yield(data) => println(data),
    log(msg) => println("[LOG] {msg}"),
}

-- 编译器生成的等价伪代码
let __handler_yield = |data: Any| -> Unit {
    println(data)
}
let __handler_log = |msg: String| -> Unit {
    println("[LOG] {msg}")
}
foo(__ev_yield = Evidence(__handler_yield),
    __ev_log = Evidence(__handler_log))
```

### 2. In-Place 调用

当函数内部发出效应时，通过 evidence 找到 handler 闭包，直接在**当前执行上下文**中调用它：

```nessa
-- foo 的内部
fn foo() -> #[yield, log] Unit {
    yield(42)#     -- 实际上: __ev_yield.call(42)
    log("hello")#  -- 实际上: __ev_log.call("hello")
}
```

关键点在于：handler 闭包是在发出 effect call 的 task 中、在当前调用栈上直接被调用的。它**不会**挂起当前计算，也**不会**切换执行上下文。

### 3. `resume` = `return`

在 in-place 策略下，handler 闭包的返回值直接成为效应调用表达式的结果。因此 `resume` 语义等价于 `return`：

```nessa
effect get_value(key: String) -> i32

fn compute() -> #get_value i32 {
    let x = get_value("a")#   -- handler 闭包返回 10
    let y = get_value("b")#   -- handler 闭包返回 20
    x + y
}

fn main() {
    let result = compute()# {
        get_value(key) => {
            -- 此处的表达式值就是 get_value 调用的结果
            -- resume 就是 return
            resume 10 if key == "a"
            20
        }
    }
    -- result == 30
}
```

## 闭包的环境捕获

Handler 闭包可以捕获定义处的变量，这使得 handler 可以携带状态：

```nessa
fn main() {
    var count = 0

    generate_items()# {
        yield(item) => {
            count = count + 1
            println("#{count}: {item}")
        }
    }

    println("total: {count}")
}
```

闭包捕获了 `count` 变量，每次效应调用时都可以访问和修改它。这是 in-place 策略的自然结果——闭包就在定义它的作用域中执行。

## 适用场景

In-place handler application 适用于（也是默认用于）以下场景：

| 场景 | 示例 |
|------|------|
| 日志记录 | `log(msg) => println(msg)` |
| 配置读取 | `get_config(key) => config_map.get(key)` |
| 事件发出 | `emit(event) => event_list.push(event)` |
| 依赖注入 | `get_service() => mock_service` |
| 验证 | `validate(data) => data.is_valid()` |

共同特征：handler 处理效应后，计算**总是继续**。

## 不适用的场景

当需要以下能力时，in-place 策略不够用，需要显式捕获 continuation：

- **多次 resume**：同一个效应调用需要被恢复多次（如非确定性计算）
- **延迟 resume**：将 continuation 存储起来稍后调用
- **放弃 resume**：handler 决定不恢复计算（提前终止）
- **分叉计算**：将 continuation 克隆后分别恢复

这些场景需要使用 [multi-prompt delimited continuation](multi-prompt-delimited-continuation.md) 的 `catch k` 机制。

## 与 Nessa 其他特性的配合

### 与异步效应的配合

对于 `async effect`，处理方式有所不同——async handler 不使用 in-place 策略，而是 spawn 一个子 task。详见 [异步代数效应](../concurrency-and-async-algebraic-effect/async-algebraic-effect.md)。

### 与错误效应的协同

handler 闭包自身也可以发出错误：

```nessa
effect fetch(url: String) -> !NetErr Response

fn crawl() -> #fetch !NetErr Unit {
    let resp = fetch("https://example.com")#
    -- ...
}

fn main() {
    crawl()# {
        fetch(url) => {
            http.get(url)!  -- 错误传播到 crawl 的上下文中
        }
    }
}
```

由于 handler 闭包是在 caller 的上下文中 in-place 执行的，它的错误会自然地传播到该上下文中。