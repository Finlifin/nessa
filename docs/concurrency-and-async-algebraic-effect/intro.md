# 并发与异步代数效应 (Concurrency & Async Algebraic Effects)

nessa 完全放弃了 `async fn` 和传统的 async/await 模型。并发编程完全依赖于**异步代数效应** (async algebraic effects)。

## 设计哲学

传统的 async/await 将异步性编码进函数签名（"函数着色问题"），导致同步代码和异步代码不能互换。nessa 选择了不同的路径：

1. **没有函数着色**：函数本身没有 async/sync 之分。异步性是一种**效应**，由 handler 决定
2. **统一模型**：同步效应和异步效应使用相同的声明与传播语法，区别仅在于 handler 的执行方式
3. **结构化并发**：所有 task 形成一棵树，生命周期严格嵌套，避免野 task 泄漏
4. **抢占式调度**：基于 safe-point 的抢占式调度模型，避免协作式调度的饥饿问题

## 核心概念

```
async effect 声明
       │
       ├── 发出 effect call → 返回 Future
       │
       ├── handler 接收 → spawn 子 Task 执行 handler 逻辑
       │
       ├── 子 Task 完成 → 中断父 Task，填充 Promise
       │
       └── 父 Task await Future → 获取结果
```

- **async effect**：异步效应声明，表示该效应的 handler 将在独立 task 中执行
- **Future(T)**：效应调用的一等静态类型惰性结果引用
- **Task**：并发执行的调度单位，由多核 Work-Stealing 调度器在 $M:N$ Worker 线程池上并行执行
- **Task 树**：task 的父子关系形成的树结构，是结构化并发、级联取消与 `defer` 确定性清理的基础
- **Safe-point**：编译器在循环回边与函数入口插入的低开销调度与移动式 GC 协作检查点
- **I/O Reactor**：基于 `libuv` 事件循环的非阻塞定时器、子进程、文件系统与网络事件驱动器

## 本章内容

- [异步代数效应](async-algebraic-effect.md)：async effect 的声明、调用、handler 执行模型与组合子静态类型推导
- [Task 树](task-tree.md)：代际 Task 槽表、生命周期、Work-Stealing 调度、I/O Reactor 与跨任务效应边界
- [结构化并发](structural-concurrency.md)：自动等待、级联取消、`defer` 展开清理与竞速/超时模式
- 标准库并发与网络模块：
  - [`std.task`](../standard-library/task.md)：任务让出、非阻塞睡眠、强类型 `Future` 组合子、超时控制与任务树快照
  - [`std.sync`](../standard-library/sync.md)：任务级非阻塞 `Channel`、`Mutex`、`RwLock`、`Semaphore`、`Once`、`Lazy`、`Barrier` 与原子类型
  - [`std.net`](../standard-library/net.md)：基于 `libuv` 的非阻塞 TCP、UDP 与异步 DNS 解析

## 与同步效应的对比

| 特性 | 同步效应 (`effect`) | 异步效应 (`async effect`) |
|------|---------------------|-------------------------|
| 声明 | `effect log(msg: String)` | `async effect fetch(url: String)` |
| 调用 | `log("hi")#` | `fetch(url)#` |
| 返回值 | 立即得到结果 | 返回 `Future` |
| Handler 执行 | In-place（同一 task） | Spawn 子 task |
| 传播语法 | `#effect_name` | `#effect_name` |
| 消除语法 | `# { ... }` / `.use()` | `# { async ... }` |