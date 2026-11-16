# `std.task` — 结构化并发组合子、超时控制与任务内省

`std.task` 提供面向 `async effect` 与 `Future(T)` 的任务调度协作、批量等待/竞速组合子、基于 `libuv` 定时器的结构化超时包装器以及任务树调试内省 API。

> 在 Nessa 中，新任务的创建始终通过 `async effect` handler 完成，`std.task` 不提供脱离效应作用域的非结构化裸 `spawn`。

## 类型定义

### `TimeoutError`

```nessa
pub struct TimeoutError {
    pub millis: i64,
    pub fn new(millis: i64) -> TimeoutError
}
-- 实现 Eq, PartialEq, Display
```

表示 `task.with_timeout` 超过指定毫秒数后触发的结构化超时错误。

### `Either`

```nessa
pub enum Either {
    pub Left(value: Any),
    pub Right(value: Any),

    pub fn is_left(self) -> bool
    pub fn is_right(self) -> bool
    pub fn unwrap_left(self) -> Any
    pub fn unwrap_right(self) -> Any
}
pub use Either.{Left, Right}
-- 实现 PartialEq, Display
```

由 `task.await_first(first, second)` 返回，区分哪一路 `Future` 先完成。编译器会为 `await_first` 返回值记录两侧载荷静态类型，解构 `Left(a)` / `Right(b)` 或调用 `unwrap_left()` / `unwrap_right()` 时自动恢复精确静态类型。

## 调度、睡眠与内省函数

| 函数签名 | 说明 |
| --- | --- |
| `yield_now() -> Unit` | 主动让出当前任务时间片，重新排入调度器就绪队列 |
| `sleep_ms(ms: i64) -> Unit` | 在 `libuv` Reactor 注册非阻塞定时器并挂起当前任务 `ms` 毫秒（`ms <= 0` 立即返回），绝不阻塞 OS Worker 线程；被取消时立即解除定时器并展开 `defer` |
| `sleep(dur: Duration) -> Unit` | 等价于 `sleep_ms(dur.as_millis())` |
| `is_ready(f: Future) -> bool` | 非阻塞查询 `f` 是否已完成 |
| `current_id() -> i64` | 返回当前正在执行的代际 `TaskId` |
| `tree_snapshot() -> String` | 生成当前活跃 Task 树的层次状态字符串快照（含父子关系、状态与挂起原因） |

## 组合子与编译期静态类型精化

### `await_all`

```nessa
pub fn await_all(futures: List) -> List
-- 同时作为 List 方法提供：futures.await_all() -> List
```

并发等待列表中的所有 `Future` 完成，按原顺序返回结果列表：

- **静态类型精化**：若 `futures` 来自字面量 `[f1, f2]` 或通过 `List()` + `.push(fut)` 构造，编译器自动检查所有元素均为 `Future(T)`（否则编译期直接报错），并将结果列表元素类型追踪为 `T`（`done(i)`、列表解构 `[a, b]` 直接获得类型 `T`）。

### `await_any`

```nessa
pub fn await_any(futures: List) -> Any
-- 同时作为 List 方法提供：futures.await_any() -> Any
```

竞速等待非空 `Future` 列表，返回第一个完成的 `Future` 的结果，并**自动级联取消其余未完成的兄弟子任务**（等待其 `defer` 清理收敛后再返回）：

- **静态类型精化**：当输入列表元素静态已知为 `Future(T)` 时，表达式返回类型自动精化为 `T`。

### `await_first`

```nessa
pub fn await_first(first: Future, second: Future) -> Either
```

二元竞速组合子：`first` 先完成时取消 `second` 并返回 `Either.Left(v1)`；`second` 先完成时取消 `first` 并返回 `Either.Right(v2)`。对 `Future(A)` 与 `Future(B)`，解构 `Left(x)` 得到 `A`，解构 `Right(y)` 得到 `B`。

### `with_timeout`

```nessa
pub fn with_timeout(ms: i64, thunk: fn() -> Any) -> Any
```

在独立子任务中执行 `thunk()`，与 `ms` 毫秒 `libuv` 定时器竞速：

- 若 `thunk()` 在超时前返回，取消定时器并原样透传其返回值或业务错误；
- 若定时器先到期（或 `ms < 0`），自动级联取消 `thunk` 子任务树（完整执行内部全部 `defer` 清理），并返回 `error TimeoutError.new(ms)`；
- **静态类型精化**：若 `thunk` 的返回类型为 `T`（无受检错误），调用点类型为 `!TimeoutError T`；若 `thunk` 返回 `![E1, E2] T`，调用点类型自动合并为 `![TimeoutError, E1, E2] T`。

## 示例

```nessa
use std.task
use std.task.{Left, Right, TimeoutError}

async effect compute(x: i64) -> i64

fn main() {
    handles async compute(x: i64) => {
        task.sleep_ms(5)
        x * 10
    }

    -- 1. 强类型 await_all：done(0) 静态类型为 i64
    let done = [compute(1)#, compute(2)#, compute(3)#].await_all()
    let total: i64 = done(0) + done(1) + done(2)

    -- 2. 结构化超时：超时后自动取消慢任务并执行其内部 defer
    let res = task.with_timeout(20, || -> i64 {
        task.sleep_ms(200)
        999
    }) ! {
        TimeoutError { millis } => millis,
    }

    assert_eq(total, 60)
    assert_eq(res, 20)
}
```
