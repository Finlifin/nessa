# 执行模型 (Execution Model)

## 按行执行 (Linear Execution)

To keep the language lightweight and script-like, Nessa follows a strict linear execution model.

*   代码从上到下按行执行。
*   符号必须先定义后使用（Line-by-line）。
*   Trait 的实现 (`impl`) 在定义后立即生效。
*   不存在复杂的预编译或两遍扫描阶段。

## 模块初始化

*   模块内的语句自动被包裹在一个隐式的 `fn __module__init__()` 中。
*   该初始化函数只在第一次加载（分析）模块时调用。
*   循环依赖是不允许的，因为在分析模块 A 时，如果它依赖 B，而 B 又依赖 A，解释器此时还未完全知晓 A 的符号。

```nessa
-- 循环依赖示例（不允许）
mod a:
    use b -- Error: b is not yet defined or fully analyzed

mod b:
    use a
```
