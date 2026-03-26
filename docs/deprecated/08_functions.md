# 函数 (Functions)

Nessa 中的函数是第一类公民，支持多种定义方式和强大的参数处理机制。

## 定义 (Definition)

函数定义使用 `fn` 关键字。

### 块定义 (Block Style)

```nessa
fn add(a: i32, b: i32) -> i32:
    let result = a + b
    result
```

### 表达式定义 (Expression Style)

对于单行函数，可以直接赋值给表达式。

```nessa
fn square(x: i32) = x * x
```

## 参数与返回值

*   **返回类型**: 返回值类型注解可省略，省略时按渐进类型原则视为 `Any`。不支持返回类型自动推导。
*   **变参 (Varargs)**: 使用 `...` 前缀定义变参。
*   **具名可选参数 (Named Arguments)**: 使用 `.id` 语法定义，必须提供默认值。

```nessa
fn log(level: String, ...msgs: List):
    print("[{}] ", level)
    for msg in msgs:
        print("{} ", msg)

-- 具名参数定义
fn window(width: i32, height: i32, .title: String = "Untitled", .visible: Bool = true):
    ...

-- 具名参数调用
window(800, 600, .title = "App")
window(1024, 768, .visible = false)
```

## 函数类型 (Function Types)

函数类型表示为 `fn(Args) -> Ret`。

```nessa
let f: fn(i32) -> i32 = square
```

## 方法 (Methods)

方法是第一个参数名为 `self` 的函数。定义在 Struct 或 Trait 块内部。

```nessa
struct Counter:
    count: i32 = 0

    fn tick(self):
        self.count += 1
```

## 特殊方法 (Magic Methods)

Nessa 提供了一些特殊的命名约定来实现语法糖：

1.  **构造函数 (`new`)**: 类型名调用 `Type(...)` 会被转换为 `Type.new(...)`.
2.  **Apply (`apply`)**: 对象调用 `obj(...)` 会被转换为 `obj.apply(...)`.
3.  **Update (`update`)**: `obj(k) = v` 会被转换为 `obj.update(k, v)`.

```nessa
struct Map:
    fn apply(self, key: String) -> Any: ...
    fn update(self, key: String, value: Any): ...

let m = Map()
m("name") = "Nessa" -- calls m.update("name", "Nessa")
println(m("name"))  -- calls m.apply("name")
```

## 扩展调用 (Extended Call)

nessa函数可以定义两个variadic参数，此时就要用扩展调用语法来调用，这适用于构建DSL或UI树。

```nessa
-- 假设定义: fn Div.new(...children: List, ...props: Map)
let widget = Div {
    class: "container", -- Map entry (prop)
    "Hello",            -- List item (child)
    Span { "World" }    -- List item (child)
}
```
