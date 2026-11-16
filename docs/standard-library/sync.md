# `std.sync` — 任务级非阻塞同步原语与无锁原子类型

`std.sync` 提供面向多核结构化并发的任务间通信与同步设施。所有可能等待的原语（`Channel`、`Mutex`、`RwLock`、`Semaphore`、`Once`、`Barrier`）均为**任务级挂起（Task-Suspending）**：

- 未就绪时仅将当前 `TaskId` 挂入分片等待队列并立即释放 OS Worker 线程去执行其它就绪任务；
- 所有存储在同步原语内部的托管堆对象均纳入 MMTk 移动式 Immix GC 根追踪；
- 挂起等待纳入运行时全局死锁检测图（无外部唤醒源的环形锁/空通道等待确定性报告 `VmError::DeadlockDetected`）；
- 当等待中的任务被结构化取消时，自动从等待队列摘除并进入 `defer` 栈展开，绝不留下幽灵等待者或死锁。

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

- `send(val)`：缓冲区已满（或零缓冲尚无接收方）时挂起当前任务；若通道已关闭，返回 `error ChannelError.Closed`。
- `recv()`：队列非空时直接取出元素并唤醒一个挂起的发送方；队列为空且未关闭时挂起当前任务；通道关闭且已排空后返回 `error ChannelError.Closed`。
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

- 优先使用 `with_lock(|val| ...)` 与 `update(|val| next_val)`：内部通过 `defer { self.unlock() }` 保证即使闭包提前返回、抛出受检错误或被结构化取消，互斥锁也必定被释放并唤醒下一个等待任务。
- `get()` / `set(val)` 校验当前任务确实持有该锁，未持锁访问会显式报运行时错误。

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
    pub fn release_many(self, count: i64) -> Unit
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

### `Barrier`

可重用多阶段多任务同步栅栏。当第 `parties` 个任务到达 `wait()` 时，一次性放行本轮全部任务并开启下一代（最后一个到达的 leader 任务返回 `true`，其余任务返回 `false`）：

```nessa
impl Barrier {
    pub fn new(parties: i64) -> Barrier
    pub fn wait(self) -> bool
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

    {
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
        total.update(|acc| acc.as(i64) + next.as(i64))
    }

    assert_eq(total.with_lock(|v| v).as(i64), 30)
}
```
