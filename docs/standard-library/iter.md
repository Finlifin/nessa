# `std.iter` 与 `std.stream` — 统一惰性迭代器与代数效应流

Nessa 通过 `std.iter`（拉式惰性 `Iterator` / `IntoIterator`）与 `std.stream`（推式同步代数效应 `#emit`）提供零样板、强类型推导、天然支持受检错误（`!Error`）与代数效应（`#Effect`）传播的序列处理体系。

## 核心协议与按需特化

任何实现了语言内建 `Iterator`（仅需定义 `assoc Item: Type = ...` 与 `pub fn next(self) -> IterationStep(Item)`）或 `IntoIterator`（定义 `assoc Iter: Type = ...` 与 `pub fn into_iter(self) -> Iter`）的类型——无论是标准库容器还是用户自定义 `struct` / `extend Iterator for T`——都会在调用点**按需自动获得全部 35 个默认方法**，未调用的适配器不会生成多余字节码：

- **`IntoIterator` 默认方法**：
  - `xs.iter() -> Iter`：等价于 `xs.into_iter()`，无需先分配中间 `List` 即可开启惰性流水线。
- **原生实现 `IntoIterator` / `Iterator` 的标准库类型**：
  - 集合与区间：`List`、`Map`（产出 `(String, V)`）、`HashMap`（产出 `(K, V)`）、`Set`、`Deque`、`Range`（`a..b`）、`RangeInclusive`（`a..=b`）、`RangeFrom`（`a..`）、`RevRangeIterator`（`(a..b).rev()`）。
  - 文本与字节：`String.chars_iter()`（`char`）、`String.char_indices_iter()`（`(usize, char)`）、`String.bytes_iter()`（`u8`）、`String.lines_iter()`（`String`）、`Bytes.iter()`（`u8`）。
  - 正则与系统 I/O：`Regex.find_all`（`SpanMatch`）、`Regex.find_all_bytes`（`(usize, usize)`）、`Regex.split_iter`（`String`）、`File.lines()`（`!IoError String`）、`fs.iter_dir(path)`（`DirEntry`）。

## `Iterator` 惰性适配器

所有适配器均为 $O(1)$ 构造，仅在下游 `for` 循环或终端消费器拉取时按需推进：

| 方法 | 说明 |
| --- | --- |
| `map(f)` | 对每个元素应用转换闭包 `f` |
| `filter(pred)` | 仅保留满足谓词 `pred(item) == true` 的元素 |
| `filter_map(f)` | 同时过滤与映射：保留返回非 `null`（`?Out`）的结果并解包 |
| `flat_map(f)` | 将每个元素映射为可迭代对象并惰性展平 |
| `flatten()` | 将嵌套可迭代元素逐层惰性展平 |
| `take(n)` / `skip(n)` | 截取前 `n` 项 / 跳过前 `n` 项 |
| `take_while(pred)` / `skip_while(pred)` | 按谓词连续截取前缀 / 跳过前缀 |
| `step_by(step)` | 每隔 `step`（`step >= 1`）步采样一项 |
| `enumerate()` | 产出 `(index: usize, item)` 二元组 |
| `zip(other)` | 与另一可迭代源并行配对为 `(a, b)`，任一耗尽即停止 |
| `chain(other)` | 当前迭代器耗尽后无缝衔接 `other` |
| `chunks(size)` | 按固定块大小 `size`（`size >= 1`）分批产出 `List` |
| `inspect(f)` | 在不改变元素的前提下对每个流经元素执行观察闭包 `f` |
| `peekable()` | 包装为支持 `peek() -> ?Any` 预读下一项而不消费的 `PeekableIter` |

## `Iterator` 终端消费器

| 方法 | 返回类型 | 说明 |
| --- | --- | --- |
| `fold(init, f)` | `Acc`（静态精化） | 左折叠累积；若 `f` 返回 `!E Acc`，自动精化为 `!E Acc` |
| `reduce(f)` | `?Item` | 无初值归约；空序列返回 `null` |
| `for_each(f)` | `Unit` | 逐项消费；支持 `!Error` 短路返回 |
| `count()` | `usize` | 统计剩余元素总数 |
| `any(pred)` / `all(pred)` | `bool` | 存在量词 / 全称量词（短路求值） |
| `find(pred)` / `find_map(f)` | `?Item` / `?Out` | 查找首个匹配项 / 首个非 `null` 映射结果 |
| `position(pred)` | `?usize` | 查找首个匹配项的 0-based 下标 |
| `nth(n)` / `first()` / `last()` | `?Item` | 取第 `n` 项 / 首项 / 末项 |
| `is_empty()` | `bool` | 判断是否无剩余元素 |
| `collect()` / `to_list()` | `List` | 收集为 `List`（保留静态元素类型追踪） |
| `to_set()` | `Set` | 去重收集为 `Set` |
| `to_map()` | `Map` | 将 `(String, V)` 序列收集为 `Map` |
| `to_hash_map()` | `HashMap` | 将 `(K, V)` 序列收集为 `HashMap` |
| `partition(pred)` | `(List, List)` | 按谓词拆分为 `(matched, unmatched)` |
| `min()` / `max()` | `?Any` | 按 `Ord` 求最小 / 最大元素 |
| `sum()` / `product()` | `i64` | 整数求和（空为 `0`）/ 求积（空为 `1`） |
| `join(.sep = "")` | `String` | 将各元素按 `Display` 拼接为字符串 |

## `std.iter` 构造器函数

```nessa
use std.iter

let a = iter.empty().collect()                              -- []
let b = iter.once(42).collect()                             -- [42]
let c = iter.repeat("ha").take(3).join(sep = "-")           -- "ha-ha-ha"
let p2 = iter.successors(1, |n| if n < 16 { n * 2 } else { null }).collect()
-- [1, 2, 4, 8, 16]
let fib = iter.unfold((0, 1), |st| {
    let (x, y) = st.as((i64, i64))
    if x > 10 { null } else { (x, (y, x + y)) }
}).collect()
-- [0, 1, 1, 2, 3, 5, 8]
```

## `std.stream` — 基于同步代数效应的生成器流

对于树遍历、状态机拆分等“推式（Push）”更自然的生产逻辑，`std.stream` 定义了同步效应：

```nessa
pub effect emit(value: Any) -> Unit
```

生产者只需像写普通递归/循环代码一样调用 `stream.emit(val)#`，消费端即可选择直接折叠收集，或通过 `stream.to_iter` 利用一等定界延续（`Continuation`）零拷贝挂起转换为标准惰性 `StreamIter`：

```nessa
use std.stream

fn naturals_from(start: i64) -> #stream.emit Unit {
    var n = start
    while true {
        stream.emit(n)#
        n += 1
    }
}

fn main() {
    -- 转换为惰性 Iterator：仅恢复定界延续 4 次即提前终止无限流！
    let picked = stream.to_iter(|| naturals_from(10)#)
        .filter(|n| n % 2 == 0)
        .take(4)
        .collect()
    assert_eq(picked, [10, 12, 14, 16])
}
```
