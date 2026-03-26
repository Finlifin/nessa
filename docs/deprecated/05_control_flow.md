# 控制流 (Control Flow)

Nessa 提供了一系列标准的控制流语句以及基于模式匹配的高级控制流。

## 基础流程控制

*   `if condition block (else ...)?`
*   `while condition block`
*   `for pattern in iterable block`
*   `loop block` (implied by context usually)

## 跳转语句

支持带标签的跳转：

*   `break label? while_guard?`
*   `continue label? while_guard?`
*   `return value? while_guard?`
*   `resume value? while_guard?`

卫语句 (Guard Condition) 可用于跳转语句：

```nessa
break while x > 10
```

## 延迟执行

*   `defer`: 作用域结束时执行。
*   `errdefer`: 返回错误分支时执行。

## 错误与效应传播/消除 (Propagation and Elimination)

Nessa 继承了 Flurry 语言中关于 Error 和 Effect 优雅、统一的处理设计。

### 基本概念

*   **Propagation (传播)**: 将潜在的 Error 或 Effect 向上传递给调用者。
*   **Elimination (消除)**: 在当前层级处理 Error 或 Effect，使其不再向上传播。

### 规则与类型签名

对于一个可能产生 Error 集合 `!Es` 或 Effect 集合 `#Effs` 的表达式：

*   **Result**: 类型为 `!Es T`
*   **Effectful**: 类型为 `#Effs T`

### 消除 (Elimination)

使用花括号 `{ ... }` 接在传播算符后进行模式匹配处理：

```nessa
-- 处理单个表达式
let result = getUser("fin")! {
    .NotFound => createUser("fin"),
    error => panic("Unexpected error: {}", error)
}
```

```nessa
-- 批量处理 (Scope Propagation)
do {
    let userA = getUser("luna")!
    let userB = getUser("celestia")!
    userA.transfer(userB, 100)!
}! {
    .NotFound => println("User not found"),
    .InsufficientFunds => println("Not enough money"),
    _ => println("Other error")
}
```

在上述 `do` 块中：
1.  内部的 `!` 将具体函数的 `!Error` 传播到 `do` 块的层级。
2.  `do` 块整体不仅产生正常计算的值，也汇聚了所有传播出来的 Errors。
3.  块后的 `! { ... }` 统一捕获并消除了这些 Errors。
4.  如果有未匹配的 Error，则剩余的 Error 类型继续向外传播（Partial Elimination）。

### 传播 (Propagation)

如果不提供处理块，使用 `!` (针对 Error) 或 `#` (针对 Effect) 只会解包值并将 Effect/Error 向上层函数签名添加。

*   `expr!` : 解包值，如果出错则 return error。
*   `expr#` : 发出 Effect，暂停并等待 Handler。

### 类型收缩

处理块可以完全消除类型签名中的 Error/Effect，也可以只消除一部分。

```nessa
-- 假设 op() -> ![A, B] T

-- 完全消除
op()! { .A => ..., .B => ... } -- 类型变为 T

-- 部分消除
op()! { .A => ... } -- 类型变为 !B T
```
