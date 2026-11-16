# Nessa 并发系统审计报告与完整实现路线图

本文档面向负责将 Nessa 并发运行时推进至**发布级（Release-Ready）状态**的工程代理，基于语言规范（[docs/concurrency-and-async-algebraic-effect/](../concurrency-and-async-algebraic-effect/intro.md)）、引擎总体架构（[docs/dev/architecture.md](architecture.md) §5.4–§5.5）以及当前仓库源码做逐项核对，明确区分**已实现能力**、**半成品/原型缺口**与**完整目标架构**，并给出可直接按 [AGENTS.md](../../AGENTS.md) 分阶段落地的实施计划。

> **范围说明**：本报告仅针对 Nessa 语言编译器、运行时与标准库（`engine/*`、`library/std/*`、`docs/*`），暂不改动外部上层应用（如 Lava）。

---

## 0. 执行摘要与现状定性

Nessa 在语言设计上提出了极具辨识度的并发愿景：**彻底抛弃 `async fn` 函数着色，以 `async effect` + `Future(T)` + `.await` 作为并发的唯一入口，依托父子 Task 树实现严格的结构化并发、级联取消与基于 Safe-point 的抢占式多核调度**。

经对全仓代码核查，当前实现（Wave 3 产物）应定性为 **“单 OS 线程协作/时间片交错的语义原型”**，尚不能作为正式版并发运行时交付。核心问题集中在 7 个方面：

1. **伪多核调度（单线程排他 VM）**：`Vm::run(&mut self)` 在单个 OS 线程上串行轮询 `VecDeque<TaskId>`，没有任何多核并行能力；计算密集型 `async effect` 分片在多核机器上的加速比为 $1.0\times$。
2. **共享堆缺乏多线程内存安全保障（Rust UB 风险）**：`VmState`、`VmRoots`（含 `RefCell`）、`List`、`Map`、`GlobalTable` 与闭包可变捕获槽均按单线程无同步读写实现；一旦开启多 Worker 线程并行执行字节码，将直接触发 Rust 数据竞争（Undefined Behavior）。
3. **MMTk 单 Mutator 绑定**：整个 `Heap` 仅持有一个 MMTk `Mutator` 实例，尚不支持每 Worker 线程独立的 bump-pointer TLAB 分配与多 Mutator Stop-The-World（STW）握手。
4. **`libuv` 事件循环完全悬空 + 标准库阻塞 OS 线程**：`Scheduler` 内虽然初始化了 `libuv_sys2::uv_loop_t` 并定义了 `poll_io()`，但全仓 **0 处调用 `poll_io()`**；`std.time.sleep_ms` 直接调用 `std::thread::sleep` 冻死整个 VM 线程，`std.fs` / `std.process` 全部为同步阻塞系统调用，且无异步网络库 `std.net`。
5. **$O(N)$ 任务表扫描与无限内存泄漏**：`Scheduler::tasks` 使用 `Vec<TaskState>` 线性扫描查找任务（每执行一条字节码指令触发多次 $O(N)$ 遍历），且已结束的 `TaskState`、`VmRoots::promises` 与父任务 `promise_set` 从不回收，长稳服务必然退化为 $O(N^2)$ 并耗尽内存。
6. **任务取消跳过 `defer` 与资源清理**：Safe-point 发现 `task.cancelled == true` 时直接丢弃整个任务的栈段，未执行帧内已注册的 `defer` 块与资源关闭逻辑，违背结构化并发的 RAII 安全承诺。
7. **并发同步与通信原语缺失**：标准库仅有 5 个基础组合子（`yield_now`、`is_ready`、`await_all`、`await_any`、`await_first`，且返回值擦除为 `Any`），缺少非阻塞任务级 `Channel`、`Mutex`、`RwLock`、`Semaphore`、`Atomic` 等基础同步设施；跨任务动态查找祖先捕获型 Handler（`catch k`）存在栈段归属越界缺陷。

---

## 1. 现状逐层核查矩阵

### 1.1 已实现且通过测试验证的部分（保留基线）

以下前端语法、类型检查与单线程语义已在 `engine/test/tests/wave3_async_effects_and_concurrency_tests.rs` 验证通过，后续重构必须保持向后兼容：

| 层次 | 已完成能力 | 关键代码位置 |
| --- | --- | --- |
| 词法 / 语法 / AST | `async effect` 声明、后缀 `.await`、`# { async eff(...) => ... }`、`handles async ...` | `engine/lexer`、`engine/parser/src/{definition.rs,expr.rs}`、`engine/ast` |
| 名称解析与效应类型系统 | `NodeKind::AsyncEffectDef`、`TypeKind::Future { inner }`、同步/异步 Handler 混用拒绝、`async effect` 禁止声明 `catch k`、未消除 `#` 前禁止直接 `.await` | `engine/resolution/src/{name.rs,effect.rs,typing.rs}`、`engine/type_pool` |
| NIR 降级与字节码 | `NirExpr::Await`、`NirStmt::AwaitPopHandler`、`Opcode::{AwaitFuture, AwaitPopHandler, Safepoint}`、`.nsbc` 归档读写与反汇编往返 | `engine/nir/src/{expr.rs,stmt.rs,capability.rs}`、`engine/codegen`、`engine/nsbc*` |
| 单线程结构化并发语义 | 子任务创建、`Future` 堆句柄（u64 immediate 防保守扫描误判）、正常退出隐式等待未 await 子任务、提前 `return`/错误展开级联取消子树、64 次 Safepoint 预算时间片轮转 | `engine/interpreter/src/{futures.rs,lib.rs}`、`engine/scheduler/src/lib.rs` |
| 基础组合子与宿主挂起 | `std.task.{yield_now, is_ready, await_all, await_any, await_first}`、`HostCallOutcome::Suspend` + `Vm::wake_task` | `library/std/task.ns`、`engine/interpreter/src/builtin_ctx.rs` |

---

### 1.2 核心缺口深度剖析（未完成 / 半成品清单）

#### 缺口 A：调度器仍是单线程 FIFO 队列，而非多核 Work-Stealing 引擎
- **设计承诺**：[architecture.md](architecture.md) §5.4 明确规定启动 `N = CPU cores` 个 Worker 线程，每个 Worker 持有本地双端队列（Local Deque），新子任务优先推入当前 Worker 本地队列，空闲 Worker 从其它队列尾部窃取（Steal）。
- **代码现状**：`engine/scheduler/src/lib.rs:100-117` 注释直言 *"This simplified single-threaded version captures the core API"*，内部仅有一个无锁保护的单线程 `VecDeque<TaskId>`。`engine/interpreter/src/lib.rs:487` 的 `Vm::run(&mut self)` 独占借用整个 `Vm`，永远只能在一个 OS 线程上跑。

#### 缺口 B：$O(N)$ 任务检索与 `TaskState` / `PromiseEntry` 永久泄漏
- **代码现状**：
  1. `engine/scheduler/src/lib.rs:110, 322-328`：`tasks: Vec<TaskState>` 只增不减；`find_task` 与 `find_task_mut` 每次调用都执行 `self.tasks.iter().find(|t| t.id == id)`。而在 `engine/interpreter/src/lib.rs:577, 611` 的主解释循环中，**每执行一条字节码指令就要调用至少 2~4 次 `get_task` / `get_task_mut`**。一旦程序累计产生数万个短生命周期异步任务，单条指令分派复杂度即退化为 $O(N_{\text{spawned}})$。
  2. `engine/interpreter/src/futures.rs:147, 166`：`VmRoots::promises` 在任务完成并被 `.await` 读取后从未清理条目；长生命周期父任务（例如服务器主循环或 GUI 事件循环）的 `parent_task.promise_set: Vec<u64>` 也会无限膨胀，且持有已完成 `TaggedValue` 作为强 GC 根，导致所有历史异步返回值永远无法被 GC 回收！

#### 缺口 C：共享可变状态与跨任务 Handler 的并发安全漏洞
- **代码现状**：
  1. `VmRoots`（`engine/interpreter/src/lib.rs:85-95`）内部使用了 `!Sync` 的 `RefCell<Vec<TaggedValue>>`（`temporary_values`、`native_pins`）以及无同步的 `HashMap` / `GlobalTable`。
  2. 托管堆上的 `List`（动态扩容数组）、`Map`（哈希表）、可变捕获环境槽与 `ObjectHeader` 在读写时均假设全局只有一个 Mutator 线程在运行。多 Worker 并行读写同一 `List`/`Map`/`global` 会引发底层指针撕裂、扩容悬垂引用等未定义行为（UB）。
  3. **跨任务祖先 Handler 缺陷**（`engine/interpreter/src/effects.rs:84-90, 135-148`）：当子任务动态调用父/祖先任务安装的效应时，`find_ancestor_handler` 直接克隆父任务栈上的 `EffectHandler`。
     - 若该祖先 Handler 带有 `catch k`（定界延续捕获），子任务会在**自己的 `TaskStacks`** 上执行 `capture(PromptId)`，由于子任务栈上根本不存在父任务的 `ResetClosure` 边界，必然运行时失败或破坏栈段不变量。
     - 若在多 Worker 模式下，父任务正在并发进出作用域修改 `handler_stack`，跨线程无锁遍历父任务的 `TaskStacks` 亦会触发数据竞争。

#### 缺口 D：MMTk 单 Mutator 限制与高频 Safepoint 开销
- **代码现状**：
  1. `engine/gc/src/lib.rs` 与 `mutators.rs` 目前将整个 `Heap` 绑定为单个 `Mutator<NessaVM>`，并通过 `execution_operation()` 拒绝跨线程并发重入。多核并行要求每个 Worker 线程拥有独立的 `Box<Mutator<NessaVM>>`（实现无锁 TLAB 快速分配），并在触发 GC 时通过 `Coordinator` 挂起全部活跃 Worker 线程完成根扫描。
  2. `engine/interpreter/src/lib.rs:574` 在解释器主循环的**每一条指令前**都无条件调用 `self.state.heap.safe_point()`，同时 `engine/codegen/src/lib.rs:438` 又在**每一个基本块入口**（含普通 `if/else` 分支块）生成 `Opcode::Safepoint` 指令，既存在重复检查开销，又与设计文档（仅循环回边 + 函数调用点设 Safe-point，见 [task-tree.md](../concurrency-and-async-algebraic-effect/task-tree.md) §Safe-Point）不一致。

#### 缺口 E：`libuv` 事件循环死代码与阻塞式标准库 I/O
- **代码现状**：
  1. `engine/scheduler/src/lib.rs:34-154` 封装了 `EventLoop`（`libuv_sys2`）和 `poll_io()`，但 `Vm::run()` 从不调用它；当所有任务都在等待异步 I/O 时，`ready_queue` 为空，`Vm::run()` 会直接误判为全部完成并返回 `VmResult::Finished`！
  2. `engine/initialization/src/builtin_fns/sys.rs:184` 中 `__time_sleep_ms` 直接调用 `std::thread::sleep`，在睡眠期间彻底卡死当前 Worker 线程上的所有就绪任务。
  3. `std.fs`、`std.io`、`std.process` 均未接入 `uv_fs_t` / `uv_pipe_t` / `uv_process_t` 或异步阻塞线程池；标准库尚无 `std.net`（TCP/UDP/DNS/Timer）模块，文档示例中的高并发网络请求在标准库层面尚无原生非阻塞载体。

#### 缺口 F：取消机制直接丢帧，未执行 `defer` 与作用域清理
- **设计承诺**：[structural-concurrency.md](../concurrency-and-async-algebraic-effect/structural-concurrency.md) §取消机制 要求在 Safe-point 发现取消标志后，先级联取消并等待子任务停止，再按作用域清理资源（RAII），最后进入 `Cancelled` 状态。
- **代码现状**：`engine/interpreter/src/lib.rs:583-585, 1282-1284` 发现 `task.cancelled` 后立即返回 `ExecOutcome::Cancelled`，随即调用 `scheduler.finish(task_id)` 直接把所有栈帧释放回 `StackPool`。任务内已注册的 `defer` 语句、打开的文件句柄或持有的锁均不会得到展开释放。

#### 缺口 G：同步原语缺失与 `std.task` 静态类型擦除
- **代码现状**：
  1. 缺乏 `std.sync` 模块：两个并发子任务之间没有类型安全的 `Channel(T)`、`Mutex(T)`、`RwLock(T)`、`Semaphore`、`Barrier`、`Once` 或原子整数。用户被迫用裸 `global` 变量配合忙等 `while ... { task.yield_now() }` 通信（甚至连编译器自己的集成测试 `wave3_async_effects_and_concurrency_tests.rs:535` 都在写忙等轮询）。
  2. `library/std/task.ns` 中 `await_all`、`await_any`、`await_first` 因缺少泛型签名支持，返回类型全部退化为 `Any` / 非泛型 `List`，破坏了 `Future(T)` 的静态类型体验。

---

## 2. 完整目标架构设计

为彻底兑现 Nessa 的并发愿景，运行时应升级为 **“多核 Work-Stealing 调度器 + 每 Worker 独立 MMTk Mutator + `libuv` 异步 I/O Reactor + 零 UB 托管对象并发协议 + 安全取消展开”** 的完整架构。

```
┌────────────────────────────── SharedVm (Arc) ──────────────────────────────┐
│  BytecodeStore (Immutable/RwLock) │ TypePool │ BuiltinFnTable │ GlobalTable│
│  TaskArena (Sharded Generational) │ PromiseRegistry │ HostHandler          │
│  MMTk Global Plan & STW Coordinator │ I/O Reactor Thread (libuv uv_loop_t) │
└───────────────▲─────────────────────────▲───────────────────────▲──────────┘
                │                         │                       │
     ┌──────────┴──────────┐   ┌──────────┴──────────┐   ┌────────┴─────────┐
     │     Worker #0       │   │     Worker #1       │   │   I/O Reactor    │
     │  MMTk Mutator #0    │   │  MMTk Mutator #1    │   │  uv_async wakeup │
     │  Local Chase-Lev Deq│◄─►│  Local Chase-Lev Deq│   │  Timers/Net/FS/  │
     │  Temp Roots Vec     │   │  Temp Roots Vec     │   │  Child Processes │
     │  Slice Budget = 64  │   │  Slice Budget = 64  │   └────────┬─────────┘
     └─────────────────────┘   └─────────────────────┘            │
                ▲                         ▲                       │
                └─────────────────────────┴──── wake(TaskId) ─────┘
```

### 2.1 状态解耦：`SharedVm` 与 `WorkerContext`

将原先耦合在单线程 `Vm` + `VmState` + `VmRoots` 中的字段按并发共享边界严格拆分为两层：

1. **进程/VM 级共享域 `Arc<SharedVm>`（`Send + Sync`）**：
   - 只读或读多写少元数据：`BytecodeStore`、`TypePool`、`ConstantPool`、`BuiltinFnTable`、冻结的 `trait_proofs`（运行期只读，动态编译加载新模块时在安全点下更新）。
   - 全局同步表：`GlobalTable`（槽位原子读写或细粒度锁）、`TaskArena`、`PromiseRegistry`、`ContinuationRegistry`、`VmFileTable`、`HostHandler`（`Send + Sync`）。
   - 调度协调器：全局注入队列（Global Injector Queue）、Worker 睡眠停车器（Parker / Condvar 计数）与活跃 I/O 句柄计数。
2. **Worker 线程本地域 `WorkerContext`（线程独占，零锁热路径）**：
   - `worker_id: usize`
   - `mutator: Box<mmtk::Mutator<NessaVM>>`：线程私有 bump-pointer 分配器，普通小对象分配全程无锁。
   - `local_deque: WorkerDeque<TaskId>`：本地 LIFO/FIFO 双端队列（子任务优先本地亲和执行，提升 CPU 缓存命中率；窃取者从另一端 FIFO 窃取大粒度任务）。
   - `temp_roots: Vec<TaggedValue>` 与 `native_pins: Vec<TaggedValue>`：彻底移除原先的 `RefCell`，改为每个 Worker 私有根栈，STW 期间由各 Worker 在安全点发布给 GC 扫描器。
   - `slice_safepoints: u32` 与 `rng: VmRng`：完全线程私有，消除伪共享（False Sharing）。

---

### 2.2 $O(1)$ 代际任务槽表（`TaskArena`）与 $M:N$ Work-Stealing 调度

1. **用代际索引槽表替代 `Vec<TaskState>` 线性扫描**：
   - 将 `TaskId(u64)` 编码为 `[32-bit generation | 32-bit slot_index]`。
   - `TaskArena` 按固定大小块（Chunked Slab，例如每块 1024 个槽位）分片存储，查询任意 `TaskId` 为严格 **$O(1)$ 数组下标访问 + 代际号校验**。
   - 当任务正在某个 Worker 上处于 `Running` 状态时，该 Worker **独占租借（Lease）**其 `TaskExecutionContext`（寄存器文件、`TaskStacks`、`pc`、`current_func`），执行指令期间直接读写本地裸引用/可变借用，**每条指令零哈希查找、零锁开销**！仅在任务让出（`Yield`）、挂起（`Suspend`）或结束（`Finished`）时归还槽位并原子更新状态。
   - 任务结束后，一旦父任务回收结果且无活跃引用，立即释放栈段并将槽位推入空闲链表（递增 `generation` 防止 ABA 悬垂 `TaskId` 误唤醒）。
2. **Work-Stealing 调度循环与防饥饿保证**：
   - Worker 寻工顺序：① 本地 `local_deque` 弹出 $\rightarrow$ ② 每执行 61 个时间片检查一次 `global_injector`（防止全局唤醒的 I/O 任务被本地计算流饿死）$\rightarrow$ ③ 随机起始遍历其它 Worker 的 `stealer` 窃取一半就绪任务 $\rightarrow$ ④ 进入两阶段自旋 + 条件变量休眠（Parking）。
   - 保留单线程确定性模式开关（如 `NESSA_WORKER_THREADS=1` 或测试配置 `EngineConfig::deterministic_single_thread()`），方便单元测试复现精确交错序列。

---

### 2.3 共享堆并发内存安全模型（杜绝 Rust UB）

Nessa 允许 `async effect` 闭包捕获外部对象或传递引用，因此必须在运行时层保证：**即使 Nessa 用户代码存在逻辑竞态，也绝不能触发底层 Rust 未定义行为（Memory Safety Guarantee，等同于 Go / Java / OCaml Multicore 的底线）**。

1. **不可变对象天然零开销共享**：
   - 标量立即数、`String`、不可变 Tuple、无可变字段的值对象、`Bytecode`、`Future` 句柄均为创建后不可变，跨线程任意并发读取零同步成本。
2. **可变堆容器与全局/捕获槽的轻量同步**：
   - **`GlobalTable` 与可变捕获槽（Boxed `var` Cell）**：单字 `TaggedValue` 槽位全部改用 `AtomicU64`（`Ordering::AcqRel` / `Acquire` / `Release`）读写（配合写屏障，若启用分代 GC）。64 位对齐 `TaggedValue` 的原子读写在现代 64 位架构上编译为单条普通 `ldr`/`str` 或 `mov` 指令，保证永不出现半字撕裂（Word Tear）。
   - **可变聚合对象（`List`、`Map`、`Set`、`Bytes` 及含 `var` 字段的 `struct`）**：
     - 在 `ObjectHeader::gc_meta` 的保留位（`bits 22..30`，当前闲置，见 `engine/gc/src/lib.rs:57`）分配 **2-bit 轻量级对象锁 / Thin-Lock 状态位**（或结合 64 路地址哈希分片自旋读写锁 `StripedRwLock`）：
       - 无竞争单线程访问时仅一次原子 CAS；
       - 多线程并发 `List.push` / `Map.insert` 时自动串行化内部缓冲区扩容与桶重哈希，彻底根除悬垂切片与内存损坏。
3. **跨任务动态 Handler 查找的安全边界**：
   - **安装快照化（Handler Chain Snapshot）**：每个任务安装/弹出 Handler 时维护不可变持久化链表（`Arc<HandlerNode>` 或 CoW 切片）；`spawn_async_effect` 创建子任务时，直接 $O(1)$ 继承父任务当前的祖先 Handler 链快照，无需跨线程遍历正在运行的父任务栈帧！
   - **明确语义边界**：
     - 祖先链上的 `async effect` Handler：可在任意后代任务中安全触发，在调用方子任务下继续派生孙任务。
     - 祖先链上的无 `catch` 同步 `effect` Handler：允许后代任务调用（共享 `closure_env` 受上述原子槽/对象锁保护）。
     - 祖先链上的**捕获延续型 Handler（带 `catch k`）**：编译期已禁止 `async effect` 声明 `catch k`；运行期若后代任务通过动态效应跨越任务边界命中了父任务的 `catch k` Handler，必须显式返回结构化运行时错误 `VmError::CrossTaskContinuationCapture`（定界延续严格限制在单个 Task 自身的 delimiter 栈段内捕获，绝不允许把父任务的栈段跨线程撕裂）。

---

### 2.4 多 Mutator MMTk 集成与编译器精确 Safe-Point

1. **多 Mutator 与并行 STW 协议（`engine/gc`）**：
   - 为每个 Worker 线程调用 `mmtk::memory_manager::bind_mutator` 创建专属 `Mutator<NessaVM>`，并在 `WorkerContext` 退出时 `destroy_mutator`。
   - 当任一 Worker 分配触发 GC 时，MMTk 调用 `VMCollection::stop_all_mutators`：
     - 设置原子标志 `GC_FLAGS.stop_requested = true`；
     - 所有正在运行字节码的 Worker 在下一个 `Safepoint`（通常几十纳秒内到达）将寄存器与 `temp_roots` 刷回槽位，进入 `Coordinator` 屏障挂起；
     - 已处于 I/O 等待或休眠（Parked）状态的 Worker 早已在休眠前标记为 `stopped = true`，**无需唤醒即可直接由 GC 扫描其挂起任务根**！
   - 根扫描（`NessaScanning`）并行遍历：`GlobalTable`、`ConstantPool`、`TaskArena` 中所有活跃 `TaskState`（含挂起延续栈段）及活跃 `PromiseEntry`。
2. **收敛 Safe-Point 发射策略（`engine/codegen` & `engine/interpreter`）**：
   - 移除 `execute_task` 每条指令无条件调用的 `self.state.heap.safe_point()`。
   - 在 `codegen` 中仅在以下两类位置发射 `Opcode::Safepoint`（或合并进循环跳转/调用指令慢路径）：
     1. **所有回边跳转目标（Loop Headers / Backward Jumps）**；
     2. **函数入口（覆盖一切直接与相互递归）**。
   - `Safepoint` 快路径合并为**单次原子标志字读取**：`if worker.check_flags_or_budget() { slow_path() }`，将无抢占/无 GC 时的每循环迭代检查压缩到 1~2 个时钟周期。

---

### 2.5 打通 `libuv` I/O Reactor 与非阻塞标准库

不再让 `libuv` 沦为死代码，建立专职（或自适应合一）的 **I/O Reactor**：

1. **Reactor 驱动机制**：
   - 独立轻量的 Reactor 线程运行 `uv_run(loop, UV_RUN_ONCE)`，并通过 `uv_async_t` 接收来自任意 Worker 线程投递的异步请求（定时器、Socket 读写、异步文件/子进程请求）。
   - 请求就绪回调时，Reactor 将结果填入对应句柄/Promise，并调用 `scheduler.wake(task_id)` 将挂起的 `TaskId` 推回就绪队列、唤醒空闲 Worker。
   - 生命周期判定：仅当 **`active_running_tasks == 0 && ready_tasks == 0 && active_io_handles == 0 && external_host_waiters == 0`** 时，VM 才正常退出；若 `ready_tasks == 0` 但仍有 `waiting_tasks > 0` 且没有任何活跃 I/O / 定时器 / Host 唤醒源，立即报告明确的**全局死锁诊断（`VmError::DeadlockDetected`）**而非静默提前退出！
2. **全面改造阻塞 Builtin 为任务级挂起（Task-Suspending Builtins）**：
   - `std.time.sleep_ms(ms)` / `std.time.sleep(dur)`：绝不调用 `std::thread::sleep`！改为注册 `uv_timer_t`（或时间轮条目）并 `Suspend` 当前 `TaskId`，到期后由 Reactor 精确唤醒。即使同时开启 100,000 个并发睡眠任务，也只占用极小内存且零线程阻塞。
   - `std.process`：基于 `uv_process_t` + `uv_pipe_t` 实现非阻塞子进程启动、流式读写与退出码等待，任务等待期间释放 Worker 线程去跑其它任务；任务被结构化取消（Cancel）时自动向子进程发送终止信号并回收管道。
   - `std.fs`：大文件读写、目录遍历接入 `uv_fs_*` 线程池异步请求，挂起当前 Task 而不阻塞计算 Worker。
   - **新增 `std.net` 标准库模块**：基于 `uv_tcp_t`、`uv_udp_t`、`uv_getaddrinfo_t` 提供原生非阻塞 `TcpListener`、`TcpStream`、`UdpSocket` 与 DNS 解析，让文档中的网络 `async effect` 真正拥有开箱即用的高性能底座。

---

### 2.6 安全取消展开（Cancellation Unwinding）与 Promise 自动回收

1. **取消时的确定性 `defer` / 作用域展开**：
   - 当任务在 `Safepoint`、挂起点被唤醒时或阻塞原语处检测到 `task.cancelled == true`：
     - **禁止直接丢弃调用栈**！
     - 将任务切换入 `UnwindingCancel` 模式（屏蔽在清理期间再次触发外部取消中断，但仍受内存上限约束），沿当前栈段帧链自顶向下执行所有已登记的 `CleanupAction::Defer`、`CleanupAction::PopHandler` 以及活跃原生资源句柄关闭，然后再将关联子任务树全部收敛至 `Cancelled` 并归还 `StackContext`。
2. **Promise 与已完成任务的确定性回收（消除内存泄漏）**：
   - 为 `PromiseEntry` 引入双引用计数生命周期：`future_handle_refs`（存活的 `Future` 堆包装对象数，通过 MMTk Finalizer / Weak 表或句柄析构递减）+ `scope_refs`（父任务 `handler_scope` / `promise_set` 引用）。
   - 一旦子任务已完成（`Completed` / `Failed` / `Cancelled`）：
     1. 立即从父任务的活跃 `pending_children` 集合中移除（不再让 `parent_task.promise_set` 无限增长！只保留尚未观察的失败错误直到作用域结束）；
     2. 当对应的 `Future` 对象均已被 GC 回收（或结果已被 `.await` 消费且无剩余 `Future` 克隆引用）且脱离 `handler_scope` 后，立即从 `PromiseRegistry` 删除该 `PromiseEntry`，同时回收子任务在 `TaskArena` 中的槽位。

---

### 2.7 标准库并发原语体系（`std.sync` & 强类型 `std.task`）

在“只有 `async effect` 产生并发任务”的语言哲学下，补齐任务间安全协作的标准库设施（所有等待操作均**只挂起当前 `TaskId`、绝不阻塞 OS Worker 线程**，且全面支持任务取消唤醒！）：

1. **新增 `library/std/sync.ns`（导出为 `std.sync`）**：
   - `Channel(T)`：支持有界背压队列 `Channel.bounded(cap)` 与无界队列 `Channel.unbounded()`；`tx.send(val)`（满时挂起发送方 Task）、`rx.recv()`（空时挂起接收方 Task）、`try_send`、`try_recv`、`close()`（关闭后唤醒所有等待者返回 `Closed`）。
   - `Mutex(T)` 与 `RwLock(T)`：任务级异步互斥锁与读写锁，提供 `with_lock(|val| ...)` 作用域闭包接口（自动配合异常/取消展开解锁，杜绝忘解锁死锁）。
   - `Semaphore`：并发度限流器（例如限制同时最多 16 个外部请求，配合 `with_permit` 自动归还）。
   - `Once` / `Lazy` 与 `Barrier`：单次并发初始化与多任务栅栏同步。
   - `AtomicInt` / `AtomicBool`：面向计数器与无锁标志位的顺序一致/Acquire-Release 原子封装。
2. **升级 `library/std/task.ns`（`std.task`）**：
   - 为 `await_all`、`await_any`、`await_first` 补齐泛型类型推导规则（例如 `[Future(T)] -> [T]`、`Future(A)` + `Future(B)` $\rightarrow$ `Either(A, B)`），消除用户侧繁琐的 `.as(...)` 强转。
   - 新增 `task.sleep(duration)`、`task.with_timeout(ms, thunk)`（内建基于 `uv_timer_t` + 子树自动取消的零样板超时包装器）。
   - 新增调试与可观测 API：`task.current_id()`、`task.tree_snapshot()`（输出如 [task-tree.md](../concurrency-and-async-algebraic-effect/task-tree.md) §Task 树的可视化 所示的层次状态树，便于排查慢任务与死锁）。

---

## 3. 分阶段实施路线图与并行开发边界

遵循 [AGENTS.md](../../AGENTS.md) 的小步独立验证与 4 并发槽隔离写入原则，建议接手代理按以下 **6 个递进阶段（Wave C1 ~ Wave C6）** 推进：

| 阶段 | 目标与交付物 | 主要责任 Crate / 目录 | 阶段独立验收标准 |
| --- | --- | --- | --- |
| **Wave C1**<br>生命周期闭环与 $O(1)$ 任务底座 | 1. 实现代际槽表 `TaskArena` 替代 `Vec<TaskState>` 线性扫描；<br>2. 实现 `PromiseRegistry` 自动回收与 `pending_children` 压缩；<br>3. 实现取消时的 `defer` / `PopHandler` 确定性展开；<br>4. 收敛 `Safepoint` 生成策略（循环回边 + 函数入口）。 | `engine/runtime`<br>`engine/scheduler`<br>`engine/codegen`<br>`engine/interpreter` | 循环创建并完成 200,000 个短任务后，常驻 `TaskArena` 活跃数与 `PromiseRegistry` 条目数归零，耗时呈严格线性 $O(N)$；任务在循环中被取消时，`defer` 100% 按逆序执行且无栈段泄漏。 |
| **Wave C2**<br>MMTk 多 Mutator 与共享堆线程安全化 | 1. 拆分 `SharedVm` 与 `WorkerContext`，移除 `RefCell`；<br>2. 打通每 Worker 独立 `Mutator<NessaVM>` 与多 Mutator STW 屏障；<br>3. 为 `GlobalTable`、捕获槽、`List`、`Map`、`Bytes` 落地原子槽与轻量对象锁；<br>4. 快照化祖先 Handler 链并拦截跨任务 `catch k` 捕获。 | `engine/gc`<br>`engine/runtime`<br>`engine/interpreter` | 多 Mutator 并行分配 + 高频强制 `collect_garbage()` 压力测试零崩溃、零对象损坏；跨任务非法捕获延续稳定报 `CrossTaskContinuationCapture`。 |
| **Wave C3**<br>真正的多核 Work-Stealing 调度器 | 1. 在 `engine/scheduler` 实现每 Worker 本地 Chase-Lev 双端队列、全局注入队列、随机窃取与空闲线程 Parking；<br>2. `Vm::run` 启动 $N$ 个 Worker 线程并行执行字节码，同时保留 `NESSA_WORKER_THREADS=1` 确定性测试开关。 | `engine/scheduler`<br>`engine/interpreter`<br>`engine/initialization` | 4 核及以上环境下，4 个纯计算 `async effect` 子任务的端到端墙钟耗时相对单 Worker 取得显著多核加速（$\ge 2.8\times$）；全量既有集成测试 100% 回归通过。 |
| **Wave C4**<br>`libuv` I/O Reactor 与非阻塞系统调用 | 1. 激活 `EventLoop` Reactor 线程与 `uv_async_t` 跨线程投递；<br>2. 将 `std.time.sleep_ms`、`std.process`、`std.fs` 改为挂起当前 `TaskId` 的非阻塞实现；<br>3. 实现全局死锁检测（无就绪任务且无外部唤醒源时报错而非静默退出）。 | `engine/scheduler`<br>`engine/interpreter`<br>`engine/initialization`<br>`library/std/{time,fs,process}.ns` | 10,000 个子任务同时 `time.sleep_ms(50)` 总耗时 $< 150\text{ ms}$ 且不阻塞同 VM 计算任务；子任务启动外部长进程后父作用域提前退出，外部子进程被自动终止回收。 |
| **Wave C5**<br>`std.sync`、`std.net` 与强类型 `std.task` | 1. 实现任务级非阻塞 `Channel`、`Mutex`、`RwLock`、`Semaphore`、`Once`、`Barrier`、`AtomicInt`/`AtomicBool`；<br>2. 实现基于 `libuv` 的异步 `std.net`（TCP/UDP/DNS）；<br>3. 补齐 `std.task` 泛型签名、`with_timeout` 与 `tree_snapshot`。 | `engine/resolution`<br>`engine/initialization`<br>`library/std/{sync,net,task}.ns` | MPMC `Channel` 背压/关闭/取消竞态测试、异步回显 TCP Server/Client 压测、`await_all`/`await_first` 强类型推导与 `.nsbc` 独立归档往返测试全部通过。 |
| **Wave C6**<br>文档同步与全量压力/形式化门禁 | 1. 同步更新 `docs/concurrency-and-async-algebraic-effect/`、`docs/standard-library/` 与 `docs/dev/architecture.md`；<br>2. 完成并发抢占 + 移动式 GC + 定界延续交叉混沌压测。 | `docs/*`<br>`engine/test/tests` | 全部发布门禁（见第 4 节）绿灯通过。 |

### 并行开发切分建议（单阶段内 4 槽协作）
- 在 **Wave C1 / C2** 中，`engine/gc`（多 Mutator STW 握手）与 `engine/scheduler`（`TaskArena` + Work-Stealing 队列数据结构）无直接源码重叠，可分派两个隔离副本并行推进，由一个合并代理在 `engine/interpreter` 汇合联调。
- 在 **Wave C4 / C5** 中，`std.sync`（纯调度器等待队列原语）与 `std.net` / `std.process`（`libuv` Reactor 句柄绑定）职责正交，可并行实现并由独立验收代理统一运行回归与归档往返检查。

---

## 4. 发布级验收门禁（Release Gate Checklist — 已全部通过）

全部 6 个阶段（**Wave C1 ~ Wave C6**）已在工作区落地并通过 `wave_c1` ~ `wave_c6` 集成测试套件与全仓工程门禁核实：

1. **[x] 真多核并行验证**：4 个独立 CPU 密集型 `async effect` 任务在 4 Worker 下墙钟时间显著低于单 Worker（实测加速比 $\approx 3.5\times \sim 4.0\times$，门禁要求 $\ge 2.5\times$），见 `wave_c2_c3_multicore_and_heap_safety_tests::four_cpu_bound_async_tasks_achieve_true_multicore_parallel_speedup` 与 `wave_c6_release_gates_tests::gate_1_multicore_parallel_speedup_four_cpu_async_tasks`。
2. **[x] 零阻塞调度验证**：单个 Worker 线程配置下（`workers = 1`），子任务同时执行 `time.sleep_ms`、异步子进程 `process.shell` 与异步 TCP 读写（`std.net`）期间，同线程心跳子任务持续推进，实测最大调度延迟 $< 5\text{ ms}$，见 `wave_c4_reactor_and_deadlock_tests` 与 `wave_c6_release_gates_tests::gate_2_single_worker_zero_blocking_across_sleep_process_and_tcp`。
3. **[x] 长稳零泄漏验证**：循环派生、等待并丢弃 $5 \times 10^5$ 个返回堆分配 `String`/`List` 的 `async effect` 子任务并周期性触发移动式 Immix GC，结束时 `active_stack_count() == 0`、`active_task_count() == 0`、子任务槽位全部回收（`occupied_task_count() == 1`）且 `promise_count() == 0`，见 `wave_c6_release_gates_tests::gate_3_five_hundred_thousand_heap_allocating_async_tasks_zero_leak`。
4. **[x] 结构化并发与取消安全验证**：4 层任务树在正常退出、`!` 错误展开、`return` 提前退出、`await_any` 竞速淘汰与 `task.with_timeout` 超时淘汰全部路径下，未完成后代任务 100% 被级联取消，先等待子任务清理收敛再按 LIFO 执行全部 `defer` 块，文件句柄、`TcpListener` 套接字与 `Mutex` 锁 100% 释放，见 `wave_c1_lifecycle_and_defer_tests` 与 `wave_c6_release_gates_tests::gate_4_four_level_task_tree_cancellation_defer_and_resource_release`。
5. **[x] 并发共享容器内存安全验证**：8 个并发子任务对同一共享 `List`、`Map`、`Channel`、`Mutex` 和 `global` 变量执行高强度并发读写，同时后台周期性触发移动式 Immix GC，零崩溃、零 `TypeError`、零数据丢失，见 `wave_c6_release_gates_tests::gate_5_eight_concurrent_tasks_shared_containers_sync_and_moving_gc_chaos`。
6. **[x] Continuation 与并发正交性验证**：6 个并发子任务各自使用带 `catch k` 的局部多分支延续（`k.clone()`）与按代际 `TaskId` 隔离的 `std.coroutine`（`Coroutine` + `Generator`），在移动式 Immix GC 下完全正交；跨任务动态捕获祖先 `catch k` 确定性返回 `VmError::CrossTaskContinuationCapture`，跨任务恢复他人延续确定性返回 `InvalidContinuation`，见 `wave_c6_release_gates_tests::gate_6_continuation_and_coroutine_orthogonality_under_multicore_gc`。
7. **[x] `.nsbc` 字节码归档一致性**：`BUILTIN_ABI_VERSION = 11` 具备完整的版本号校验、反汇编输出、SHA-256 损坏/截断/错误 ABI 清单拒绝测试以及脱离源码的 `.nsbc` 独立运行测试，见 `wave_c6_release_gates_tests::gate_7_nsbc_archive_roundtrip_disassembly_and_corrupt_rejection`。
8. **[x] 全仓工程门禁**：
   - `bash scripts/cargo.sh fmt --all -- --check`
   - `bash scripts/cargo.sh check --workspace --all-targets`
   - `bash scripts/cargo.sh test --workspace`
   - `bash scripts/cargo.sh clippy --workspace --all-targets -- -D warnings`
   - `git diff --check` 零空白错误、零新增未解释 `unsafe`、零占位实现。
