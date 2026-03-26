# Multi-prompt Delimited Continuation

Nessa 提供了原生的界定延续 (Delimited Continuation) 支持，这是比完全延续 (Call/cc) 更结构化且安全的控制流原语。

## 原语

*   `reset prompt_expr block`: 设定一个界定符 (Prompt)。
*   `shift prompt_expr, k`: 捕获当前到指定 Prompt 的延续 `k` 并跳出。

## 示例

```nessa
let result = reset .outer:
    shift .outer, k:
        println("in outer shift")
        let inner_result = reset .inner:
            shift .inner, k2:
                println("in inner shift")
                k2(10) + 1
        k(inner_result * 2) + 3

println("result: {}", result) -- Output: 25
```

## 解释

1.  `reset` 定义了延续的边界。
2.  `shift` 捕获了从当前点到 `reset` 边界的计算过程，将其封装为函数 `k`。
3.  调用 `k` 相当于恢复这段被捕获的计算。

这一机制是实现复杂控制流（如 Effect System, Coroutines, backtracking 等）的基石。
