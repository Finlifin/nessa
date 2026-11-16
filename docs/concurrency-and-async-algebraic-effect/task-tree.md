# Task 树 (Task Tree)

nessa 从一个 **root task** 开始执行。所有并发执行的 task 通过父子关系形成一棵**树**，这棵树是 nessa 并发模型和 [结构化并发](structural-concurrency.md) 保证的基础。

## Root Task

程序启动时，`main()` 函数在唯一的 root task 中执行：

```
[Root Task]
└── main()
```

Root task 是 task 树的根节点。当 root task 完成（`main()` 返回），整个程序退出。

## Task 的创建

在 nessa 中，task 的创建**只能**通过 async effect handler 发生。没有独立的 `spawn` 原语——这是一个刻意的设计选择，确保每个 task 都有明确的效应语义和 handler 归属。

```nessa
async effect fetch(url: String) -> !NetErr Response

fn main() {
    let f1 = fetch("/api/users")# {
        async fetch(url) => http_lib.get(url)
    }
    -- 此时 task 树:
    -- [Root Task: main]
    -- └── [Child Task #1: http_lib.get("/api/users")]

    let f2 = fetch("/api/posts")# {
        async fetch(url) => http_lib.get(url)
    }
    -- [Root Task: main]
    -- ├── [Child Task #1: http_lib.get("/api/users")]
    -- └── [Child Task #2: http_lib.get("/api/posts")]

    f1.await
    f2.await
    -- 两个子 task 完成后，树回到只有 root task
}
```

### 嵌套 Task

子 task 中也可以发出 async effect call，创建孙 task，形成更深的树结构：

```nessa
async effect fetch(url: String) -> Response
async effect parse(data: String) -> Document

fn process(url: String) -> #[fetch, parse] Document {
    let resp = fetch(url)#
    let doc = parse(resp.await.body)#
    doc.await
}

fn main() {
    let result = process("/page")# {
        async fetch(url) => {
            -- 这个 handler 自身也可以发出 async effect
            let raw = tcp_connect(url)#
            http_parse(raw.await)
        },
        async parse(data) => html_parser.parse(data),
    }# {
        async tcp_connect(url) => socket_lib.connect(url),
    }
}

-- Task 树 (运行时某一时刻):
-- [Root Task: main]
-- ├── [Task: fetch handler]
-- │   └── [Task: tcp_connect handler]
-- └── [Task: parse handler]
```

## Task 生命周期

每个 task 经历以下生命周期阶段：

```
Created → Running → Completed
                  → Failed
                  → Cancelled
```

### 状态说明

| 状态 | 说明 |
|------|------|
| **Created** | task 已创建，等待调度器分配执行时间 |
| **Running** | task 正在执行中 |
| **Completed** | task 正常完成，结果已填入 promise |
| **Failed** | task 因未处理的异常或错误而终止 |
| **Cancelled** | task 被父 task 或调度器取消 |

### 完成与通知

当子 task 完成时：

1. 将结果（成功值或错误）填入对应的 promise
2. 向父 task 发送**中断** (interrupt)
3. 父 task 在下一个 safe-point 处理该中断
4. 如果父 task 正在 `await` 该 future，则被唤醒

```
[Parent Task]                    [Child Task]
    │                                │
    ├── safe-point: 无中断，继续      │ 执行 handler 逻辑
    │                                │
    ├── future.await ── 挂起         │
    │                                │
    │                                ├── 完成! → 填充 promise
    │                                │        → 发送中断给 parent
    │                                │
    ├── ◄── 收到中断，唤醒 ──────────┘
    │   └── await 返回结果
    ▼
```

## Promise 注册表与代际 Task 槽表

运行时通过分片代际槽表（`TaskArena`）与双引用计数 `PromiseRegistry` 管理任务和异步结果生命周期：

- **代际 `TaskId`**：编码为 `[32-bit generation | 32-bit slot_index]`，任务检索为 $O(1)$ 数组下标访问并防止悬垂 `TaskId` 误唤醒（ABA）。
- **自动回收（零长稳泄漏）**：子任务完成后立即从父任务的活跃 `pending_children` 中压缩移除；当对应的 `Future` 句柄被消费/GC 回收且离开 handler 作用域后，`PromiseEntry` 与子任务槽位立即归还复用。

## 多核 Work-Stealing 调度与 I/O Reactor

nessa 运行时采用 **$M:N$ 多核 Work-Stealing 调度器 + 每 Worker 独立 MMTk Mutator + `libuv` 异步 I/O Reactor** 架构：

1. **Worker 线程池**：默认启动与逻辑 CPU 核数相等的 Worker 线程（可通过 `NESSA_WORKER_THREADS` 或 `SchedulerConfig` 配置为单线程确定性模式）。每个 Worker 持有本地双端队列（LIFO 亲和局部性 + 跨线程 FIFO 窃取）和独立的 MMTk bump-pointer 分配器。
2. **收敛 Safe-Point**：编译器在**循环回边**（`while` / `for`）与**函数入口**插入 `Safepoint` 指令。在 safe-point 处，Worker 通过单次原子标志与时间片预算检查协作响应移动式 GC Stop-The-World 握手、任务取消与时间片轮转让出。
3. **`libuv` I/O Reactor 与全局死锁检测**：定时器（`task.sleep` / `time.sleep_ms`）、子进程（`std.process`）、文件系统（`std.fs`）与网络套接字（`std.net`）统一通过后台 `libuv` Reactor 非阻塞驱动，绝不阻塞计算 Worker 线程。若所有就绪任务耗尽、存在挂起等待的任务且没有任何活跃 Reactor 句柄或外部唤醒源，运行时立即报告 `VmError::DeadlockDetected`（包含等待链信息），避免静默挂死或误退出。

### 为什么是 Safe-Point 而非硬件信号硬抢占

硬件信号完全抢占可以在任意机器指令处打断线程，但无法低成本维护精确移动式 GC 根映射。Safe-point 方式：

- **精确 GC 友好**：只在寄存器/槽位状态已知的位置响应 STW，支持 MMTk Immix 移动式压缩回收
- **超低热路径开销**：普通分支基本块不发射冗余 `Safepoint`，无 GC/抢占请求时仅需一次轻量检查
- **覆盖完整执行流**：循环回边 + 函数入口覆盖所有迭代与递归计算路径

## 动态效应查找与单 Task 定界延续边界

当效应调用需要动态分派（参见 [动态效应调用](../algebraic-effect-and-multi-prompt-delimited-continuation/dynamic-effect-call.md)）时，子 task 在派生时以 $O(1)$ 继承父 task 当前的**不可变祖先 Handler 链快照**，无需跨线程遍历正在运行的父 task 调用栈：

```
[Root Task] ← handles(log)
├── [Task A]
│   └── [Task B]
│       └── [Task C] ← log("hello")# 命中继承的祖先快照 → Root handler!
```

跨任务效应分派严格遵守以下并发安全边界：

1. **祖先 `async effect` Handler**：允许任意后代 task 触发，在调用方 task 下继续派生新的子 task。
2. **祖先无 `catch` 的同步 `effect` Handler**：允许后代 task 直接调用（共享堆容器与全局/捕获槽由运行时原子槽与对象锁保护，绝不产生数据竞争 UB）。
3. **定界延续（`catch k`）严格归属单 Task**：每个 task 可以独立使用局部 `catch k`、`k.clone()` 与 `std.coroutine` 有状态协程，栈段在各自 task 内安全切换与移动式 GC 扫描；但**禁止跨 task 捕获祖先栈段或跨 task 恢复他人的 `Continuation`**，违者确定性报错 `VmError::CrossTaskContinuationCapture`。

## Task 树内省与可视化

标准库 [`std.task`](../standard-library/task.md) 提供运行时可观测 API：

- `task.current_id() -> i64`：返回当前正在执行的代际 `TaskId`。
- `task.tree_snapshot() -> String`：生成当前活跃 task 树的层次状态快照（含父子缩进、`TaskId` 与运行/挂起/取消状态），便于诊断慢任务与并发拓扑：

```nessa
use std.task

println(task.tree_snapshot())
-- 示例输出:
-- [Task #1] Running
--   ├── [Task #2] Waiting
--   └── [Task #3] Running
```