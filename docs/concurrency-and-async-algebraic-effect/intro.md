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
- **Future**：效应调用的惰性结果引用
- **Task**：并发执行的调度单位
- **Task 树**：task 的父子关系形成的树结构，是结构化并发的基础
- **Safe-point**：编译器插入的调度检查点

## 本章内容

- [异步代数效应](async-algebraic-effect.md)：async effect 的声明、调用、handler 执行模型
- [Task 树](task-tree.md)：task 的生命周期、父子关系与调度
- [结构化并发](structural-concurrency.md)：基于 task 树的结构化并发保证

## 与同步效应的对比

| 特性 | 同步效应 (`effect`) | 异步效应 (`async effect`) |
|------|---------------------|-------------------------|
| 声明 | `effect log(msg: String)` | `async effect fetch(url: String)` |
| 调用 | `log("hi")#` | `fetch(url)#` |
| 返回值 | 立即得到结果 | 返回 `Future` |
| Handler 执行 | In-place（同一 task） | Spawn 子 task |
| 传播语法 | `#effect_name` | `#effect_name` |
| 消除语法 | `# { ... }` / `.use()` | `# { async ... }` |