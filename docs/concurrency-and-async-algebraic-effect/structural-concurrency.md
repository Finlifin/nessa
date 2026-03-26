# 结构化并发 (Structured Concurrency)

既然所有 task 已经通过 async effect handler 形成了一棵 [task 树](task-tree.md)，结构化并发就是这棵树的自然推论。

## 核心原则

结构化并发的核心保证是：**子 task 的生命周期严格嵌套在父 task 的作用域内**。不存在"野 task"——每个 task 都有明确的归属和确定性的生命周期。

```
fn scope() {
    let f1 = async_op_1()#    ── 创建子 task #1
    let f2 = async_op_2()#    ── 创建子 task #2

    -- ... 其他代码 ...

    f1.await                  ── 等待子 task #1
    f2.await                  ── 等待子 task #2
}   ── scope 退出时，所有子 task 必须已完成或被取消
```

## 生命周期嵌套

### 自动等待

当一个 handler 作用域（消除块 `# { ... }` 或 `handles` 语句的作用域）结束时，如果仍有未完成的子 task，父 task 会**自动等待**它们完成：

```nessa
fn main() {
    fetch_all()# {
        async fetch(url) => http_lib.get(url)
    }
    -- ↑ 消除块结束时，所有由 fetch handler spawn 的子 task
    --   必须已完成。如果有未 await 的 future，在此隐式等待。

    println("all done!")
}
```

### 自动取消

如果父 task 因异常或 `return` 提前退出，所有未完成的子 task 会被**自动取消**：

```nessa
fn main() {
    fetch_data()# {
        async fetch(url) => http_lib.get(url)
    }! {
        -- 如果在处理过程中出现错误并提前退出
        error e => {
            println("error: {e}")
            -- ← 所有仍在运行的 fetch 子 task 会被自动取消
            return
        }
    }
}
```

取消的传播是**向下级联的**：当 task A 被取消时，A 的所有子 task、孙 task 也会被取消：

```
[Task A] ← 被取消
├── [Task B] ← 自动取消
│   └── [Task D] ← 自动取消
└── [Task C] ← 自动取消
```

## 取消机制

### 取消的实现

task 被取消时，不会立刻终止——它会在下一个 **safe-point** 处检查取消标志，然后：

1. 停止执行当前逻辑
2. 取消所有子 task（级联取消）
3. 等待子 task 全部停止
4. 清理资源，进入 `Cancelled` 状态

```
[被取消的 Task]
    │
    ├── 继续执行...
    │
    ├── [safe-point] 检查取消标志 → 发现被取消
    │
    ├── 向所有子 task 发送取消信号
    │
    ├── 等待子 task 停止
    │
    └── 清理资源 → 状态变为 Cancelled
```

### 安全取消

由于取消只发生在 safe-point，task 的状态在取消时是**一致的**。不会在一个操作执行到一半时被强制终止（区别于线程的 `kill`）。

## 与 Task 树的关系

结构化并发的保证完全建立在 task 树之上：

| 保证 | 机制 |
|------|------|
| 没有野 task | task 只能通过 async effect handler 创建，必须有父 task |
| 生命周期嵌套 | 子 task 在父 task 的 handler 作用域结束前必须完成 |
| 错误传播 | 子 task 的错误通过 promise 传回父 task |
| 级联取消 | 取消沿 task 树向下传播 |
| 资源安全 | handler 作用域是 RAII 作用域，退出时清理 |

## 并发模式

### 并发 Map

对列表中的每个元素并发执行一个操作：

```nessa
async effect process(item: Any) -> Result

fn concurrent_map(items: List) -> #process List {
    let futures = items.map do |item|:
        process(item)#

    futures.map do |f|: f.await
}

fn main() {
    let results = concurrent_map([1, 2, 3, 4, 5])# {
        async process(item) => heavy_computation(item)
    }
}
```

### 竞速 (Race)

发出多个 async effect call，取最先完成的结果：

```nessa
async effect fetch(url: String) -> Response

fn race_fetch(urls: List) -> #fetch Response {
    let futures = urls.map do |url|: fetch(url)#

    -- await_any 返回第一个完成的 future 的结果
    -- 其余 future 对应的子 task 被自动取消
    futures.await_any()
}
```

### 超时

结合 async effect 实现超时控制：

```nessa
async effect timeout(duration: Duration) -> Unit
async effect fetch(url: String) -> !NetErr Response

fn fetch_with_timeout(url: String, dur: Duration) -> #[fetch, timeout] !TimeoutErr Response {
    let data_future = fetch(url)#
    let timeout_future = timeout(dur)#

    -- 哪个先完成就用哪个的结果
    -- 如果 timeout 先完成，fetch 的子 task 被取消
    await_first(data_future, timeout_future) match {
        Left(response) => response!,
        Right(_) => error TimeoutErr.Expired,
    }
}
```

## 与传统并发模型的对比

| 特性 | nessa (async effect) | Go (goroutine) | Rust (tokio) | Java (virtual thread) |
|------|---------------------|----------------|--------------|----------------------|
| 并发原语 | async effect | go func() | async fn | Thread.startVirtualThread |
| 结构化并发 | ✅ 内建 (task 树) | ❌ 需要手动管理 | ⚠️ 需要 scope | ⚠️ 需要 StructuredTaskScope |
| 取消传播 | ✅ 自动级联 | ❌ 需要 context | ⚠️ 需要 CancellationToken | ⚠️ 需要 shutdown() |
| 函数着色 | ❌ 无 | ❌ 无 | ✅ async fn | ❌ 无 |
| 调度模型 | safe-point 抢占 | 抢占式 | 协作式 | 抢占式 |

nessa 通过代数效应的框架，以一种统一且类型安全的方式实现了许多其他语言需要多种不同机制才能达到的并发特性。