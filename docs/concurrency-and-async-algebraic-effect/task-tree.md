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

## Promise 集

每个 task 维护一个 **promise 集**，记录所有由它发出的 async effect call 对应的 promise：

```nessa
-- task 内部结构 (概念模型)
struct Task {
    id: TaskId,
    parent: ?TaskId,
    children: List<TaskId>,
    promise_set: Map<EffectCallId, Promise>,
    state: TaskState,
    -- ...
}
```

### Effect Call ID

每个 async effect call 都有一个唯一的 ID，用于：
- 在 promise 集中标识对应的 promise
- 将子 task 的结果路由到正确的 promise
- 将 future 与 promise 关联

## 调度模型

nessa 使用**基于 safe-point 的抢占式**调度模型：

### Safe-Point

编译器在以下位置自动插入 safe-point：

1. **循环回边** (loop back-edges)：`while` 和 `for` 循环每次迭代之前
2. **函数调用点**：每次函数调用前

在 safe-point，task 会执行以下检查：

```
[safe-point 检查]
    │
    ├── 有待处理的中断？
    │   ├── Yes → 处理中断（填充 promise、更新状态等）
    │   └── No  → 继续执行
    │
    ├── 调度器要求让出？
    │   ├── Yes → 让出执行权，task 进入就绪队列
    │   └── No  → 继续执行
    │
    └── 继续当前 task 的执行
```

### 为什么是 Safe-Point 而非完全抢占

完全抢占（如 OS 线程）可以在任意指令处暂停，但这需要保存完整的 CPU 状态，开销大且实现复杂。Safe-point 方式：

- **可预测**：只在已知位置暂停，VM 状态一致
- **轻量**：不需要保存/恢复完整的机器状态
- **足够频繁**：循环回边 + 函数调用覆盖了几乎所有长时间运行的代码路径
- **低延迟**：在任何有循环或函数调用的代码中，两个 safe-point 之间的间隔很短

## 动态效应查找与 Task 树

当 evidence 为 `null`（[动态效应调用](../algebraic-effect-and-multi-prompt-delimited-continuation/dynamic-effect-call.md) 场景）时，运行时沿 task 树向上查找 handler：

```
[Root Task] ← handles(log)
├── [Task A]
│   └── [Task B]
│       └── [Task C] ← log("hello")# 动态查找
│           向上搜索: C → B → A → Root ← 找到 handler!
```

Task 树为动态 handler 查找提供了明确的搜索路径，从发出效应的 task 逐级向上直到 root task。

## Task 树的可视化

在调试模式下，可以查看当前的 task 树状态：

```
[Root Task #0] Running
├── [Task #1] Running  ← fetch("/api/users")
│   └── [Task #3] Running  ← tcp_connect("api.example.com:443")
├── [Task #2] Completed  ← fetch("/api/posts") → Response(200)
└── [Task #4] Running  ← parse(data)
```

这种清晰的层次结构使得并发程序的行为更容易理解和调试。