# Multi-Prompt Delimited Continuation

Multi-prompt delimited continuation 是 nessa 中代数效应的**保守实现路径**。当默认的 [in-place handler application](in-place-handler-application.md) 策略不足以满足需求时——例如需要多次恢复、延迟恢复或放弃恢复——可以通过显式捕获 continuation 获得对控制流的完全掌控。

在 nessa 中，这是接触 delimited continuation 的**唯一途径**。

## 基本概念

### 什么是 Delimited Continuation

Delimited continuation 是"从当前执行点到最近的 handler boundary"之间的**剩余计算**。它可以被捕获为一个值，之后可以调用它来恢复该计算。

```
handler boundary ─────────────────────────── handler block
       │
       │  ← delimited continuation (captured as k)
       │
effect call point ──── yield(42)# ──── 计算在此暂停
```

### 为什么是 Multi-Prompt

"Multi-prompt" 指的是多个 handler 可以同时存在，每个 handler 对应一个**独立的 prompt（分界点）**。continuation 的捕获是相对于特定 prompt 的，不同效应各自捕获自己的 continuation 而不互相干扰。

## 语法：`catch k` 参数

在 effect 声明中使用 `catch k` 参数来启用显式 continuation 捕获：

```nessa
effect yield(data: Any, catch k: Continuation)
```

`catch` 是一个特殊的参数修饰符，表示该参数在 handler 中接收被捕获的 continuation。

### 关键区别

| 特性 | 无 `catch k` (默认) | 有 `catch k` |
|------|---------------------|-------------|
| Handler 执行 | In-place，直接调用 | 暂停当前计算，转移控制流 |
| Resume 行为 | 自动（handler 表达式值） | 手动（调用 `k(value)`） |
| Continuation | 不捕获 | 捕获为 `k` |
| 性能 | 零开销 | 有 continuation 捕获开销 |

## 使用方式

### 基本示例

```nessa
effect yield(data: Any, catch k: Continuation)

fn generate(list: List) -> #yield Unit {
    list.foreach do |x|:
        yield(x)#
}

fn main() {
    generate([1, 2, 3])# {
        yield(data, k) => {
            -- 显式捕获 continuation 后，不会默认 resume
            println("got: {data}")

            -- 显式调用 k 来恢复计算
            -- k(Unit) 将 Unit 作为 yield 调用的返回值，恢复 generate 的执行
            k(())
        }
    }
}
```

### 提前终止（不 resume）

handler 可以选择**不**调用 `k`，从而丢弃剩余计算：

```nessa
effect yield(data: Any, catch k: Continuation)

fn main() {
    -- 只取第一个元素
    let first = generate([1, 2, 3])# {
        yield(data, k) => {
            -- 不调用 k，剩余计算被丢弃
            data
        }
    }
    -- first == 1
}
```

### 多次恢复

continuation 可以被**多次调用**，实现非确定性计算等高级模式：

```nessa
effect choose(options: List, catch k: Continuation)

fn pythagorean(n: i32) -> #choose List {
    let a = choose(1..=n)#
    let b = choose(a..=n)#
    let c = choose(b..=n)#

    if a * a + b * b == c * c {
        [(a, b, c)]
    } else {
        []
    }
}

fn main() {
    let results = pythagorean(20)# {
        choose(options, k) => {
            -- 对每个选项恢复一次计算，收集所有结果
            options.flat_map do |opt|:
                k(opt)
        }
    }
    -- results: [(3,4,5), (5,12,13), (6,8,10), (8,15,17), (9,12,15), (12,16,20)]
}
```

### 存储和延迟恢复

continuation 可以被存储起来稍后调用：

```nessa
effect suspend(catch k: Continuation)

struct Coroutine {
    continuation: ?Continuation,
}

impl Coroutine {
    fn step(self) -> ?Any {
        let k = self.continuation else { return null }
        self.continuation = null

        k(()) match {
            -- ... 处理结果
        }
    }
}

fn main() {
    var coroutine = Coroutine { continuation: null }

    some_computation()# {
        suspend(k) => {
            coroutine.continuation = k
        }
    }

    -- 稍后手动恢复
    coroutine.step()
    coroutine.step()
}
```

## Continuation 类型

`Continuation` 是一个内建类型，表示被捕获的 delimited continuation。其行为类似于一个函数：

```nessa
-- Continuation 可以像函数一样调用
k(value)       -- 恢复计算，value 作为效应调用的返回值

-- 可以多次调用
k(1)
k(2)

-- 可以存储到变量中
let saved_k = k

-- 可以显式 clone
let k2 = k.clone()
```

### Continuation 的类型参数

`Continuation` 的完整类型签名反映了效应的返回类型：

```nessa
-- 如果 effect yield(data: Any) -> Unit
-- 那么 k 的类型为 Continuation，调用时接受 Unit，返回效应被消除后的整体结果类型
k(())  -- 传入 Unit，因为 yield 的返回类型是 Unit
```

### 安全性

显式操作 continuation 是**不安全的**——用户必须对自己的行为负责：

- **多次调用**可能导致副作用被重复执行
- **不调用**会导致资源泄漏（如果 continuation 中持有需要释放的资源）
- **跨 task 传递**可能导致未定义行为
- **clone** 会复制整个计算上下文，内存开销可能很大

> **最佳实践**：仅在确实需要非线性控制流时使用 `catch k`。绝大多数场景下，默认的 in-place handler application 更加安全和高效。

## 与 In-Place Handler Application 的关系

两种策略形成互补：

```
                    ┌───────────────────────────┐
                    │     效应调用 (expr#)       │
                    └───────────┬───────────────┘
                                │
                    ┌───────────┴───────────────┐
                    │   handler entry 有 catch k? │
                    └───────────┬───────────────┘
                       │                │
                      No               Yes
                       │                │
              ┌────────┴───────┐  ┌────┴────────────────┐
              │  In-Place      │  │ Multi-Prompt         │
              │  直接调用闭包  │  │ 捕获 continuation    │
              │  resume=return │  │ 显式控制恢复         │
              └────────────────┘  └─────────────────────┘
```

编译器根据 effect 声明中是否有 `catch` 参数来决定使用哪种策略。如果一个效应没有 `catch` 参数，所有对它的处理都使用 in-place 策略；如果有 `catch` 参数，则使用 continuation 捕获策略。

## 实际应用场景

| 场景 | 模式 |
|------|------|
| 生成器 / 迭代器 | `yield(data, catch k)` — 逐步生成值 |
| 非确定性计算 | `choose(options, catch k)` — 探索所有分支 |
| 协程 | `suspend(catch k)` — 手动调度 |
| 回溯搜索 | `try(catch k)` — 失败时尝试下一个 |
| 事务 | `checkpoint(catch k)` — 回滚到检查点 |