# `std.task` — 结构化并发组合子、超时控制与任务内省

`std.task` 提供面向 `async effect` 与 `Future(T)` 的任务调度协作、批量等待/竞速组合子、结构化数据并行（`par_map` / `par_for_each` / `race`）、基于跨平台 I/O Reactor 非阻塞定时器的结构化超时包装器以及任务树调试内省 API。

> 在 Nessa 中，所有并发子任务均受结构化词法作用域管理，`std.task` 不提供脱离父任务生命周期的裸 `spawn`。

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
    pub left(value: Any),
    pub right(value: Any),

    pub fn is_left(self) -> bool
    pub fn is_right(self) -> bool
    pub fn unwrap_left(self) -> Any
    pub fn unwrap_right(self) -> Any
}
pub use Either.{left, right}
-- 实现 PartialEq, Display
```

由 `task.await_first(first, second)` 返回，区分哪一路 `Future` 先完成。编译器会为 `await_first` 返回值记录两侧载荷静态类型，解构 `left(a)` / `right(b)` 或调用 `unwrap_left()` / `unwrap_right()` 时自动恢复精确静态类型。

## 调度、睡眠与内省函数

| 函数签名 | 说明 |
| --- | --- |
| `yield_now() -> Unit` | 主动让出当前任务时间片，重新排入调度器就绪队列 |
| `sleep_ms(ms: i64) -> Unit` | 在 Reactor 注册非阻塞定时器并挂起当前任务 `ms` 毫秒（`ms <= 0` 立即返回），绝不阻塞 OS Worker 线程；被取消时立即解除定时器并展开 `defer` |
| `sleep(dur: Duration) -> Unit` | 等价于 `sleep_ms(dur.as_millis())` |
| `is_ready(f: Future) -> bool` | 非阻塞查询 `f` 是否已完成 |
| `current_id() -> i64` | 返回当前正在执行的代际 `TaskId` |
| `tree_snapshot() -> String` | 生成当前活跃 Task 树的层次状态字符串快照（含父子关系、状态与挂起原因） |

## 组合子与编译期静态类型精化

### `par_map` 与 `par_for_each`

```nessa
pub fn par_map(items: List, mapper: fn(Any) -> Any) -> List
pub fn par_for_each(items: List, action: fn(Any) -> Unit) -> Unit
```

在内部词法作用域的结构化 `handles async` 下为 `items` 中每个元素派生并发子任务并等待全部收敛：

- **静态元素推导与错误传播**：若 `items` 元素静态已知为 `T`，内联 `mapper` / `action` 参数自动推导为 `T`；若 `mapper` 返回 `!E Out`，`par_map` 返回类型自动精化为 `!E List`（且输出 `List` 的同构元素类型追踪为 `Out`），首个子任务错误按序透传。

### `race`

```nessa
pub fn race(branches: List) -> Any
```

接收非空无参闭包列表 `[|| branch_a(), || branch_b(), ...]`，并发启动全部支路，返回首个完成支路的结果并结构化取消其余未完成支路（等待所有败者 `defer` 清理完成后再返回）。对列表字面量传入的支路，编译期自动求并各支路返回类型与受检错误集。

### `await_all` / `await_any` / `await_first`

```nessa
pub fn await_all(futures: List) -> List
pub fn await_any(futures: List) -> Any
pub fn await_first(first: Future, second: Future) -> Either
```

- `await_all`：并发等待列表中所有 `Future(T)` 完成，返回有序结果 `List`（并保留元素类型 `T` 供索引与循环推导）。
- `await_any`：返回首个完成的 `Future(T)` 结果 `T`，自动取消其余未完成兄弟子任务并等待其 `defer` 收敛。
- `await_first`：二元异构竞速，按完成先后返回 `Either.left(v1)` 或 `Either.right(v2)`。

### `with_timeout`

```nessa
pub fn with_timeout(ms: i64, thunk: fn() -> Any) -> Any
```

在独立子任务中执行 `thunk()`，与 `ms` 毫秒非阻塞定时器竞速：

- 若 `thunk()` 在超时前返回，取消定时器并原样透传其返回值或业务错误；
- 若定时器先到期（或 `ms < 0`），自动级联取消 `thunk` 子任务树（完整执行内部全部 `defer` 清理），并返回 `error TimeoutError.new(ms)`；
- **静态类型精化**：若 `thunk` 返回 `T`，调用点精化为 `!TimeoutError T`；若 `thunk` 返回 `![E1, E2] T`，调用点合并为 `![TimeoutError, E1, E2] T`。

## 示例

```nessa
use std.task
use std.task.TimeoutError

fn main() {
    -- 1. 结构化数据并行：x 自动推导为 i64，squares 元素静态追踪为 i64
    let squares = task.par_map([1, 2, 3, 4]) do |x| {
        x * x
    }
    assert_eq(squares, [1, 4, 9, 16])

    -- 2. 结构化多路竞速：自动取消慢支路
    let fastest: String = task.race([
        || -> String {
            task.sleep_ms(80)
            "slow"
        },
        || -> String {
            task.sleep_ms(5)
            "fast"
        },
    ])
    assert_eq(fastest, "fast")

    -- 3. 结构化超时：超时后自动取消慢任务并执行其内部 defer
    let res = task.with_timeout(20, || -> i64 {
        task.sleep_ms(200)
        999
    }) ! {
        TimeoutError { millis } => millis,
    }
    assert_eq(res, 20)
}
```
