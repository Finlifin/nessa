# 控制流与模式匹配 (Control Flow & Pattern Matching)

## if 表达式

`if` 是表达式，有值：

```nessa
let abs_x = if x >= 0 { x } else { -x }

if condition {
    do_something()
} else if other_condition {
    do_other()
} else {
    fallback()
}
```

## when 表达式

`when` 是多条件分支，类似其他语言的 `cond`。每个分支是一个独立的布尔条件：

```nessa
let label = when {
    score >= 90 => "A",
    score >= 80 => "B",
    score >= 70 => "C",
    else => "F",
}
```

## match 表达式

`match` 是后缀表达式，对值进行模式匹配：

```nessa
direction match {
    Direction.north => go(0, 1),
    Direction.south => go(0, -1),
    Direction.east => go(1, 0),
    Direction.west => go(-1, 0),
}

ast match {
    Ast.`null` => "null",
    Ast.id(name) => name,
    Ast.add(lhs, rhs) => lhs.eval() + rhs.eval(),
}
```

## 循环

### while

```nessa
while count < 10 {
    count = count + 1
}
```

### for

```nessa
for item in list {
    println(item.as(String))
}

for i in 0..10 {
    println(i.as(String))
}
```

### 带标签的循环

循环可以带标签，用于在嵌套循环中指定 `break` / `continue` 的目标：

```nessa
while :outer x < 100 {
    for :inner item in items {
        break outer if item.as(i32) == target
        continue inner if item.as(i32) < 0
    }
    x = x + 1
}
```

## 条件后缀 (if guard)

`return`、`resume`、`break`、`continue` 都支持 `if` 条件后缀，避免单独写 if 块：

```nessa
return null if user_id.is_blank()
break if count > limit
continue if item.as(i32) < 0
```

## return / break / continue

```nessa
fn find(list: List, target: i32) -> ?i32 {
    for item in list {
        return item if item.as(i32) == target
    }
    null
}
```

`break` 和 `continue` 可以指定标签：

```nessa
break outer
continue inner if should_skip
```

## resume

`resume` 用于在代数效应处理器中恢复被暂停的计算，详见 [代数效应](../algebraic-effect-and-multi-prompt-delimited-continuation/algebraic-effect.md)。

```nessa
resume value
resume if some_condition
```

---

# 模式 (Patterns)

模式用于 `match`、`let`、`for`、函数参数等位置，进行值的解构和条件判断。

## 基础模式

```nessa
x match {
    42 => "the answer",       -- 字面量模式
    name => "got: " ++ name,  -- 绑定模式（变量捕获）
    _ => "whatever",          -- 通配符模式
}
```

## 解构模式

### 元组解构

```nessa
let (a, b) = (1, 2)

pair match {
    (0, _) => "first is zero",
    (x, y) => "({x}, {y})",
}
```

### 列表解构

```nessa
list match {
    [] => "empty",
    [x] => "one element",
    [first, ...rest] => "head: {first}",
}
```

### 记录解构

```nessa
user match {
    { name, age } => println("{name} is {age}"),
    { name: "admin", ..._ } => println("admin found"),
}
```

### 调用型模式

用于 enum variant 和类型构造器的匹配：

```nessa
color match {
    Color.rgb(r, g, b) => "rgb({r}, {g}, {b})",
    Color.red => "red",
    _ => "other",
}
```

扩展调用型模式用于匹配带命名字段的结构：

```nessa
node match {
    Node { tag: "div", children } => process(children),
    _ => skip(),
}
```

## 组合模式

### or 模式

```nessa
x match {
    1 or 2 or 3 => "small",
    _ => "big",
}
```

### not 模式

```nessa
x match {
    not 0 => "nonzero",
    _ => "zero",
}
```

否定完整子模式，包括它内部的 guard：`not (E.some(x) if x > 0)`。
这里的 x 仅供内部 guard 使用，不进入 arm body。需要整个输入时写
`not E.none as whole`，它等价于 `(not E.none) as whole`。
`not` 同样可用于 `matches` 和 `for`；`for` 跳过未匹配的元素。

### as 绑定

在匹配的同时绑定整个值到一个变量：

```nessa
point match {
    Point { x: 0, y: 0 } as origin => use_origin(origin),
    p => use_point(p),
}
```

### rest 绑定

```nessa
list match {
    [first, ...rest] => process(first, rest),
    [] => default(),
}
```

List 模式支持一个具名 rest，也可以写 `[...prefix,last]` 或
`[first,...middle,last]`。固定元素为 Any，rest 为独立 List。匹配开始时
浅复制槽位，guard 修改原列表不会改变当前匹配结果；元素对象仍共享。
下一分支会读取原列表当前状态。详细规则见[模式语法](../grammar/patterns.md)。

## 守卫与约束

### if 守卫

```nessa
x match {
    n if n > 0 => "positive",
    n if n < 0 => "negative",
    _ => "zero",
}
```

### and is 约束

在匹配一个模式的同时，要求某个子表达式也匹配另一个模式：

```nessa
pair match {
    (a, b) and a is 1 => "first is one",
    _ => "other",
}
```

约束按顺序执行，左侧失败时不会求右侧表达式。左右侧模式的绑定都可供后续
guard 和分支体读取，例如 `(a,b) and a+b is total => total`。
`(a,b) and a+1 is c and c+1 is d` 可继续约束新结果；同一路径不能重复绑定
同一个名称。`p and e is q as computed` 绑定计算结果，为原始输入命名则写
`(p and e is q) as whole`。这套规则也适用于 `matches` 和 `for`。

## Optional / Error 模式

```nessa
-- optional: ? 后缀表示 some
maybe_value match {
    val? => println("got: {val}"),
    null => println("nothing"),
}

-- error: ! 后缀表示 ok
result match {
    data! => process(data),
    error e => handle(e),
}
```

## let / const 解构与 else

`let`、`const` 支持模式解构，配合 `else` 处理匹配失败：

```nessa
let (x, y) = get_pair()

let Point { x, y } = origin

-- 带 else：模式匹配失败时执行 else 分支
let val! = try_get() else {
    return null
}

const MAX_RETRIES = config.get("retries") else {
    return error ConfigErr.Missing
}
```

## var 声明与 else

`var` 用于声明可变变量，同样支持模式解构和 `else`：

```nessa
var count = 0
count = count + 1

var (x, y) = get_position()
x = x + dx

var current! = get_initial() else {
    return null
}
current = update(current)
```

## 语法参考

```ebnf
-- 控制流
if_statement -> if expr block (else (if_statement | block))?
when_statement -> when { (else_condition_arm | condition_arm)* }
while_loop -> while (: id)? expr block
for_loop -> for (: id)? pattern in expr block
return_statement -> return expr? (if expr)?
resume_statement -> resume expr? (if expr)?
break_statement -> break id? (if expr)?
continue_statement -> continue id? (if expr)?

-- 模式匹配
post_match -> expr match { case_arm* }

-- 模式
pattern ->
    id | underscore | literal |
    pattern_tuple | pattern_list | pattern_record |
    pattern_call | pattern_extended_call |
    pattern_if_guard | pattern_and_is |
    pattern_or | pattern_not |
    pattern_as_bind | pattern_rest_bind |
    pattern_option_some | pattern_error_ok | pattern_error
```
