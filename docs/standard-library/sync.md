# `std.sync` — 任务级非阻塞同步原语与无锁原子类型

`std.sync` 提供面向多核结构化并发的任务间通信与同步设施。

- **任务级非阻塞挂起（分片锁等待队列）**：`Channel`、`Mutex`、`RwLock`、`Semaphore`、`Once` 与 `Barrier` 内部采用 64 路分片互斥锁保护等待队列；未就绪时仅将当前 `TaskId` 挂入队列并立即释放 OS Worker 线程去执行其它就绪任务，绝不自旋阻塞 Worker 线程。
- **高阶闭包静态精化与错误/效应传播**：`Mutex.{with_lock, update}`、`RwLock.{with_read, with_write}`、`Semaphore.with_permit` 与 `Once.call_once` 在编译期自动推导内联闭包的返回类型、受检错误集（`!Error`）与代数效应（`#Effect`）；当 `Mutex.update`、`RwLock.with_write` 或 `Once.call_once` 的闭包返回受检错误时，锁或初始化状态自动回滚，绝不写入错误信封值。
- **无锁原子单元**：`AtomicInt` 与 `AtomicBool` 的加载、存储、加减与 CAS 均为 `SeqCst` 无锁原子指令（仅在每 1024 个原子单元扩容新块时获取一次块分配锁）。
- **精确 GC 与全局死锁检测**：所有存储在同步原语内部的托管堆对象均纳入精确 GC 根追踪（支持移动式回收槽位更新）；挂起等待纳入全局死锁检测图（无外部唤醒源的环形锁/空通道等待确定性报告 `VmError::DeadlockDetected`）。
- **取消安全与异常退出兜底**：等待中的任务被结构化取消（且未处于 `defer` 展开期）时自动从等待队列摘除，并将已交接的锁/信号量令牌级联传递给下一名合法等待者；若持锁任务因未捕获运行时错误异常终止，调度器任务退出钩子会自动释放其持有的 `Mutex` / `RwLock` 并回滚运行中的 `Once`，防止永久死锁。

## `Channel` 与 `ChannelError`

支持零容量同步交接（Rendezvous，`capacity == 0`）与有界背压缓冲队列（`capacity > 0`）的多生产者多消费者（MPMC）通道。

```nessa
pub enum ChannelError {
    pub Closed,
    pub Full,
    pub Empty,
}

impl Channel {
    pub fn new(.capacity: i64 = 0) -> Channel
    pub fn unbuffered() -> Channel
    pub fn buffered(capacity: i64) -> Channel

    pub fn send(self, val: Any) -> !ChannelError Unit
    pub fn try_send(self, val: Any) -> !ChannelError Unit
    pub fn recv(self) -> !ChannelError Any
    pub fn try_recv(self) -> !ChannelError Any

    pub fn close(self) -> Unit
    pub fn is_closed(self) -> bool
    pub fn len(self) -> i64
    pub fn is_empty(self) -> bool
}
```

- `send(val)`：缓冲区已满（或零缓冲尚无活跃接收方）时挂起当前任务；若挂起期间发送方被取消，保证元素**绝不会**残留在通道中；若通道已关闭，返回 `error ChannelError.Closed`。
- `recv()`：有缓冲队列非空或零缓冲通道存在就绪发送方时直接完成交接；否则挂起当前任务（零缓冲通道 `len()` 恒为 `0`，接收方取消绝不缓冲或丢弃发送方消息）；通道关闭且已排空后返回 `error ChannelError.Closed`。
- `close()`：幂等关闭通道，唤醒所有正在等待的发送方（返回 `Closed`）与接收方（排空剩余元素后返回 `Closed`）。

## `Mutex`（任务级互斥锁）

保护共享状态并提供基于 `defer` 的异常/取消安全闭包接口：

```nessa
impl Mutex {
    pub fn new(initial: Any) -> Mutex
    pub fn with_lock(self, f: fn(Any) -> Any) -> Any
    pub fn update(self, f: fn(Any) -> Any) -> Any

    pub fn lock(self) -> Any
    pub fn try_lock(self) -> bool
    pub fn unlock(self) -> Unit
    pub fn get(self) -> Any
    pub fn set(self, val: Any) -> Unit
    pub fn is_locked(self) -> bool
}
```

- 优先使用 `with_lock(|val| ...)` 与 `update(|val| next_val)`：内部通过 `defer` 保证即使闭包提前返回、抛出受检错误（`!E`）或被结构化取消，互斥锁也必定被释放并唤醒下一个等待任务，且调用点静态恢复闭包的返回类型与 `!Error` / `#Effect`。
- 若 `update` 闭包返回 `!E` 错误，原有锁内值保持不变，错误原样向上传播。

## `RwLock`（写优先公平读写锁）

允许多个读任务并发共享访问，或单个写任务独占访问（当存在等待中的写任务时阻止新读者插队，防止写饥饿）：

```nessa
impl RwLock {
    pub fn new(initial: Any) -> RwLock
    pub fn with_read(self, f: fn(Any) -> Any) -> Any
    pub fn with_write(self, f: fn(Any) -> Any) -> Any

    pub fn read_lock(self) -> Any
    pub fn try_read_lock(self) -> bool
    pub fn read_unlock(self) -> Unit
    pub fn write_lock(self) -> Any
    pub fn try_write_lock(self) -> bool
    pub fn write_unlock(self) -> Unit
    pub fn get(self) -> Any
    pub fn set(self, val: Any) -> Unit
}
```

## `Semaphore`（并发度计数信号量）

用于限制并发访问外部资源的最大任务数：

```nessa
impl Semaphore {
    pub fn new(permits: i64) -> Semaphore
    pub fn with_permit(self, f: fn() -> Any) -> Any
    pub fn acquire(self) -> Unit
    pub fn try_acquire(self) -> bool
    pub fn release(self, .count: i64 = 1) -> Unit
    pub fn available(self) -> i64
}
```

## `Once`、`Lazy` 与 `Barrier`

### `Once` / `Lazy`

单次并发初始化原语。首个进入 `call_once` / `force` 的任务执行初始化函数，其余并发到达的任务挂起等待结果；若初始化函数因取消或返回受检错误（`!E`）而未成功完成，`Once` 自动回滚至未初始化状态并唤醒一名等待者重试，不会缓存半成品错误值。

```nessa
impl Once {
    pub fn new() -> Once
    pub fn call_once(self, init: fn() -> Any) -> Any
    pub fn get(self) -> ?Any
    pub fn is_completed(self) -> bool
}

impl Lazy {
    pub fn new(init: fn() -> Any) -> Lazy
    pub fn force(self) -> Any
    pub fn is_initialized(self) -> bool
}
```

### `Barrier` 与 `SyncError`

可重用多阶段多任务同步栅栏。当第 `parties` 个任务到达 `wait()` 时，一次性放行本轮全部任务并开启下一代（最后一个到达的 leader 任务返回 `true`，其余任务返回 `false`）。

若某个已到达 `wait()` 的参与者任务在本轮凑齐 `parties` 之前被结构化取消，栅栏立即进入**破损（Broken）**状态并唤醒所有已到达的等待任务，以受检错误 `SyncError.BrokenBarrier` 返回而非让其余 $N-1$ 个任务永久挂死：

```nessa
pub enum SyncError {
    pub BrokenBarrier,
}

impl Barrier {
    pub fn new(parties: i64) -> Barrier
    pub fn wait(self) -> !SyncError bool
}
```

## `AtomicInt` 与 `AtomicBool`

面向高频无锁计数器与状态标志位的顺序一致原子封装：

```nessa
impl AtomicInt {
    pub fn new(initial: i64) -> AtomicInt
    pub fn zero() -> AtomicInt
    pub fn load(self) -> i64
    pub fn store(self, val: i64) -> Unit
    pub fn swap(self, val: i64) -> i64
    pub fn fetch_add(self, delta: i64) -> i64
    pub fn fetch_sub(self, delta: i64) -> i64
    pub fn compare_exchange(self, expected: i64, desired: i64) -> (bool, i64)
}

impl AtomicBool {
    pub fn new(initial: bool) -> AtomicBool
    pub fn load(self) -> bool
    pub fn store(self, val: bool) -> Unit
    pub fn swap(self, val: bool) -> bool
    pub fn compare_exchange(self, expected: bool, desired: bool) -> (bool, bool)
}
```

## 示例

```nessa
use std.sync.{Channel, ChannelError, Mutex}

async effect produce(id: i64) -> Unit

fn main() {
    let ch = Channel.buffered(4)
    let total = Mutex.new(0)

    if true {
        handles async produce(id: i64) => {
            ch.send(id * 10)! {}
        }
        let p1 = produce(1)#
        let p2 = produce(2)#
        p1.await
        p2.await
        ch.close()
    }

    while true {
        let next = ch.recv() ! {
            ChannelError.Closed => break,
            _ => break,
        }
        let _: i64 = total.update(|acc| acc.as(i64) + next.as(i64))
    }

    let final_sum: i64 = total.with_lock(|v| v.as(i64))
    assert_eq(final_sum, 30)
}
```
