# 动态效应调用 (Dynamic Effect Call)

在 nessa 的渐进类型 (gradual typing) 体系中，当编译器无法静态确定一个表达式的效应签名时，代数效应的 handler 查找会从高效的 evidence-passing 退化为**运行时动态查找**。

## 触发条件

动态效应调用发生在以下场景：

### 1. 调用 `Any` 类型的闭包

当闭包被赋值给 `Any` 类型的变量后，编译器丢失了其效应签名信息：

```nessa
effect log(msg: String) -> Unit

fn emit_log() -> #log Unit {
    log("hello")#
}

fn main() {
    handles log(msg) => println(msg);

    -- 编译器知道 emit_log 的签名，可以传递 evidence
    emit_log()#

    -- 但将其赋值给 Any 后，签名信息丢失
    let f: Any = emit_log
    f()  -- 无法传递 evidence，退化为动态查找
}
```

### 2. 返回类型包含 `Any` 的函数

当函数的返回值类型为 `Any` 或签名中包含 `Any`，编译器无法推断其内部可能发出的效应：

```nessa
fn get_callback() -> Any {
    -- 返回一个可能发出效应的闭包
    |x| {
        log("processing: {x}")#
        x
    }
}

fn main() {
    handles log(msg) => println(msg);

    let callback = get_callback()
    callback(42)  -- callback 类型为 Any，走动态查找
}
```

### 3. 动态分派的 Trait 方法

当 trait 方法通过动态分派调用，且方法可能发出效应时：

```nessa
trait Processor {
    def fn process(self, data: Any) -> #log Any
}

-- 通过动态分派调用时，如果编译器无法静态解析具体类型，
-- evidence 可能无法传递
```

## 动态查找过程

当 evidence 为 `null` 时，运行时会按以下顺序查找 handler：

```
效应调用点
    ↓
当前 Task 的动态上下文（调用栈上的 handler 绑定）
    ↓
父 Task 的动态上下文
    ↓
祖先 Task ... 直到 Root Task
    ↓
找不到 → 抛出运行时异常 (UnhandledEffectError)
```

### 1. 当前 Task 的调用栈搜索

首先在当前 task 的调用栈中，从效应调用点向上搜索，查找最近的 handler 绑定：

```nessa
fn main() {
    handles log(msg) => println(msg);

    let f: Any = || log("dynamic!")#
    f()  -- 动态查找会找到上面的 handles 绑定
}
```

### 2. Task 树上溯

如果当前 task 的调用栈中没有找到 handler，查找会沿着 task 树向上，到父 task、祖父 task 直到 root task：

```nessa
-- 假设 handler 定义在父 task 中
-- 子 task 中的动态效应调用会沿 task 树上溯找到它
```

详见 [Task Tree](../concurrency-and-async-algebraic-effect/task-tree.md)。

### 3. 运行时异常

如果遍历整棵 task 树都找不到匹配的 handler，运行时会抛出 `UnhandledEffectError` 异常：

```
UnhandledEffectError: no handler found for effect 'log' 
    at dynamic_call (main.ns:12)
    at main (main.ns:8)
```

## 性能影响

动态查找的开销远大于静态 evidence-passing：

| 方面 | Evidence-Passing | 动态查找 |
|------|-----------------|---------|
| Handler 查找 | O(1) — 直接指针 | O(n) — 栈搜索 |
| 跨 Task 查找 | N/A | O(d) — task 树深度 |
| 可预测性 | 编译期确定 | 运行时确定 |

因此，应当尽量避免使用 `Any` 类型来传递可能发出效应的闭包。nessa 的渐进类型设计中，`Any` 是一种**逃生舱**，使用它意味着放弃部分编译期保证。

## 设计权衡

动态效应调用是 nessa 的渐进类型哲学的直接体现：

- **静态路径**（有完整类型信息）：evidence-passing，零开销
- **动态路径**（`Any` 类型）：运行时查找，有开销但保证正确性

这种设计使得 nessa 可以在需要灵活性的场景下（如动态加载插件、脚本化扩展）仍然支持代数效应，而不会因为类型系统的限制而完全丧失表达能力。

> **最佳实践**：尽量为闭包提供精确的类型标注，避免不必要的 `Any` 退化。当确实需要传递类型未知的可效应闭包时，确保在合适的位置设置了 `handles` 绑定。