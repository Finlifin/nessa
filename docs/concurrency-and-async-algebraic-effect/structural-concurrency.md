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

## 取消机制与确定性 `defer` 展开

### 取消的实现

当 task 因父作用域提前 `return`、错误 `!` 传播、`await_any` / `await_first` 竞速淘汰或 `task.with_timeout` 到期而被标记取消时，它会在下一个 **safe-point** 或挂起原语（`libuv` I/O、`std.sync` 等待队列、`.await`）处立即感知取消信号，并进入受保护的 **`UnwindingCancel` 展开阶段**：

1. **停止正常控制流**并唤醒/撤销当前正在等待的定时器、子进程、TCP/UDP 套接字或 `Channel`/`Mutex`/`Semaphore` 队列条目
2. **级联取消并等待所有子 task 收敛**：确保子任务的资源清理先于父任务完成（严格内层先于外层析构顺序）
3. **自顶向下按 LIFO 逆序执行当前任务所有活跃栈帧中的 `defer` 块与 handler 弹出清理**（期间屏蔽重复取消中断，确保 `defer { file.close() }`、`defer { sock.close() }`、`Mutex.with_lock` 解锁等 RAII 逻辑 100% 完整执行）
4. **归还栈段至 `StackPool`**，状态收敛为 `Cancelled`

```
[被取消的 Task]
    │
    ├── [safe-point / 挂起原语唤醒] 检测到 cancelled == true
    │   └── 撤销未决 I/O 句柄 / 同步等待队列节点（若有活跃子进程则终止并回收）
    │
    ├── 向下级联取消所有子 task，并等待子 task 全部完成清理
    │
    ├── 进入 UnwindingCancel：沿调用栈按 LIFO 顺序执行全部已注册 defer 块
    │
    └── 弹出 handler 帧、归还栈段 → 状态变为 Cancelled
```

### 安全取消

由于取消只在 safe-point 或显式任务挂起点生效，且严格执行 `defer` 栈展开，任务不会在半条字节码或容器扩容中途被强杀（区别于裸线程 `pthread_cancel`），也不会泄漏锁、文件描述符、套接字或外部子进程。

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

可以直接使用标准库 `std.task.with_timeout(ms, thunk)`（内部基于 `libuv` 非阻塞定时器与结构化子任务自动取消），或使用 `task.await_first` 自定义竞速：

```nessa
use std.task
use std.task.TimeoutError

async effect fetch(url: String) -> !NetErr Response

fn fetch_with_timeout(url: String, ms: i64) -> #fetch ![TimeoutError, NetErr] Response {
    -- 若超过 ms 毫秒未完成，内部工作子任务及其打开的套接字/子任务会被自动级联取消并执行 defer
    task.with_timeout(ms, || -> !NetErr Response {
        fetch(url)#.await!
    })!
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