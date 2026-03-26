# 基本文法 (Basic Grammar)

Nessa 的文法设计受到 Scala 等语言的影响，强调简洁与表达力。

## 注释

*   行注释：`--`
*   块注释：`{- -}`

```nessa
-- 这是一个行注释
{-
    这是一个
    块注释
-}
```

## 字面量 (Literals)

Nessa 支持常见的数据类型字面量。

### 数值 (Numeric)

*   **整数**: 如 `123`, `-45`, `0xFF` (十六进制)。
*   **浮点数**: 默认为 64 位 (`Float64`)。
    *   `123.45`
    *   `1.2e-3` (科学计数法)

### 文本与符号 (Text & Symbol)

*   **字符串**: 双引号包裹的字符串 `"Hello World"`，支持转义字符。
*   **字符**: 单引号包裹的字符 `'a'`, `'\n'`。
*   **Symbol**: 以 `.` 开头的标识符，如 `.id`, `.status_ok`。
    *   Symbol 是轻量的值 (Imm Value)。
    *   与 String 不同，Symbol 通常用于标识状态、键名等，在运行时高效比较（Interned）。

### 布尔 (Bool)

*   `true`
*   `false`

### 特殊值 (Special Values)

*   **Null**: `null`。
    *   `null` 可以是任意可空类型 `?T` 的值。
*   **Unit**: `unit`。
    *   `Unit` 类型的唯一值，表示“无返回值”或“副作用结果”。
*   **Nothing**: `nothing`。
    *   `Nothing` 类型的唯一值，通常表示“不存在”或底类型概念。

### 复合字面量 (Compound Literals)

### 复合字面量 (Compound Literals)

Nessa 中的 Object 和 List 字面量是统一的。容器中的每一项：
*   如果以 `id: value` 或 `"key": value` 形式出现，则视为 Map 中的键值对。
*   否则视为 List 中的元素。

这意味着一个对象字面量可以同时包含列表元素和键值对（类似于 XML 属性与子节点的设计思想，或 Lua 的 Table）。

```nessa
-- 纯列表
[1, 2, 3]

-- 纯 Map
{
    name: "Nessa",
    version: 1.0
}

-- 混合结构
{
    1, 2,           -- List data
    name: "Mixed",  -- Map data
    3,
    active: true
}
```

*   **Tuple**: 类似于 Rust 元组。
    ```nessa
    (1, "hello")
    ```

*   **Tuple**: 类似于 Rust 元组。
    ```nessa
    (1, "hello")
    () -- Unit literal equivalent in value context? No, `unit` is preferred, but specific tuple syntax remains `(a, b)`
    ```

## 缩进与代码块 (Indentation and Blocks)

Nessa 支持显式的 `{ ... }` 代码块，同时也支持类似 Scala/Python 的基于缩进的语法糖。

### 缩进语法糖 (`:`)

冒号 `:` 开启一个格式敏感区域，后续缩进的内容等价于被包裹在 `{ ... }` 中。

```nessa
-- 使用大括号
if condition {
    println("true")
}

-- 使用缩进语法糖 (等价)
if condition:
    println("true")
```

### 闭包调用 (`do`)

`do` 关键字用于传递闭包（Trailing Closure），类似于 Kotlin 或 Scala。

```nessa
-- 传递闭包
list.map do |x|:
    x * 2

-- 等价于
list.map do |x| { x * 2 }
```

注意：直接跟在表达式后的 `{ ... }` (如 `Div { ... }`) 是**拓展变参调用** (Extended Variadic Call)，用于构建结构数据，而非简单的闭包传递。区分 `expr do { ... }` (闭包) 与 `expr { ... }` (拓展调用) 非常重要。

## 定义语句

*   `let pattern = expr (else expr)?`
*   `var pattern = expr (else expr)?`
*   `global pattern = expr (else expr)?`
*   `const pattern = expr (else expr)?`
*   `newtype id = type_expr`
*   `typealias id = type_expr`

## 常用语句概览

*   `while(: label)? condition block`
*   `for(: label)? pattern in iterable block`
*   `break label? (if guard_condition)?`
*   `continue label? (if guard_condition)?`
*   `return value? (if guard_condition)?`
*   `resume value? (if guard_condition)?`
*   `if condition block (else else_block | if)?`
