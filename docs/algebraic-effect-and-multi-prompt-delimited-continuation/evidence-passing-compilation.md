# Evidence-Passing 编译策略

Evidence-passing 是 nessa 编译器用于高效传递 handler 引用的核心编译策略。当一个函数声明了可能发出的效应时，编译器会为其隐式地添加 **evidence 参数**，使得 handler 的查找在绝大多数情况下是零开销的静态传递，而非运行时的动态查找。

## 基本原理

### 问题

考虑以下代码：

```nessa
effect log(msg: String) -> Unit

fn a() -> #log Unit {
    log("from a")#
}

fn b() -> #log Unit {
    a()#
}

fn main() {
    b()# {
        log(msg) => println("[LOG] {msg}")
    }
}
```

当 `a()` 内部发出 `log` 效应时，运行时需要找到 `main` 中定义的 handler。如果每次都通过动态调用栈搜索 handler，代价高昂。

### 解决方案：Evidence 参数

编译器为每个声明了效应的函数隐式添加一个 **evidence 参数**——指向 handler 的引用。调用链上的每个函数都会将 evidence 传递下去：

```nessa
-- 源码
fn a() -> #log Unit {
    log("from a")#
}

fn b() -> #log Unit {
    a()#
}

-- 编译后的等价伪代码
fn a(__ev_log: Evidence<log>) -> Unit {
    __ev_log.call("from a")
}

fn b(__ev_log: Evidence<log>) -> Unit {
    a(__ev_log)
}
```

当 `main` 中通过消除块或 `.use()` 处理效应时，编译器会将 handler 打包为 evidence，作为隐式参数传入：

```nessa
fn main() {
    let __ev_log = Evidence { handler: |msg| println("[LOG] {msg}") }
    b(__ev_log)
}
```

## 多效应的 Evidence

当一个函数声明了多个效应时，编译器会为每个效应添加一个独立的 evidence 参数：

```nessa
-- 源码
fn interactive() -> #[log, read_line] String { ... }

-- 编译后
fn interactive(__ev_log: Evidence<log>, __ev_read_line: Evidence<read_line>) -> String { ... }
```

## Evidence 的传递规则

### 1. 静态传递（默认路径）

在正常的编译流程中，evidence 通过参数逐层传递，形成一条从 handler 定义处到效应调用处的**静态链**。这是零开销的：

```nessa
fn c() -> #log Unit { log("c")# }
fn b() -> #log Unit { c()# }
fn a() -> #log Unit { b()# }

fn main() {
    a()# { log(msg) => println(msg) }
}

-- evidence 传递链: main -> a -> b -> c
```

### 2. 部分消除

如果中间的某个函数消除了部分效应，它会为已消除的效应提供新的 evidence，同时继续传递未消除的：

```nessa
fn inner() -> #[log, read_line] String { ... }

fn middle() -> #read_line String {
    -- 消除 log 效应，但传播 read_line
    inner()# {
        log(msg) => println(msg),
    }
}

-- middle 只接收 __ev_read_line
-- middle 为 log 创建本地 evidence，但将 __ev_read_line 传递给 inner
```

### 3. Null Evidence（动态退化）

当编译器无法静态确定 callee 的效应签名时（主要因为涉及 `Any` 类型），evidence 会被设为 `null`，运行时转为动态查找：

```nessa
-- callee 类型为 Any 时，无法传递 evidence
let f: Any = some_closure
f()  -- 如果 f 内部发出效应，只能走动态查找
```

详见 [动态效应调用](dynamic-effect-call.md)。

## `handles` 语句与 Evidence

`handles` 语句会在当前作用域建立一个 evidence 绑定：

```nessa
fn main() {
    handles log(msg, level) => println("{level}: {msg}");

    -- 编译器知道此作用域内 log 的 evidence 由上面的 handles 提供
    do_something()#  -- evidence 自动传入
}
```

`handles(effect_name) let` 也同样创建 evidence 绑定：

```nessa
handles(log) let logger = Logger { ... }
-- 此后 log 的 evidence 指向 logger 的 log_handler
```

## 性能特性

Evidence-passing 策略的性能优势：

| 操作 | 开销 |
|------|------|
| Evidence 传递 | 与普通参数传递相同（指针大小） |
| Effect 调用 | 间接函数调用（通过 evidence 指针） |
| Handler 查找 | 零开销（编译时已解析） |
| 动态退化 | 运行时栈搜索（仅 `Any` 类型场景） |

与基于 continuation 的传统实现相比，evidence-passing 避免了：
- 运行时 handler 栈的维护
- 每次效应调用时的栈搜索
- 不必要的 continuation 捕获和恢复

## 与 In-Place Handler Application 的协同

Evidence-passing 提供了 handler 的**查找**机制，而 [in-place handler application](in-place-handler-application.md) 提供了 handler 的**执行**机制。两者协同工作：

1. 编译器通过 evidence 参数将 handler 的闭包传递到效应调用点
2. 在效应调用点，直接在当前上下文中调用 handler 闭包（in-place application）
3. handler 闭包的返回值即为效应调用的结果（`resume` ≈ `return`）

这两个策略共同确保了简单效应处理场景下的最优性能。