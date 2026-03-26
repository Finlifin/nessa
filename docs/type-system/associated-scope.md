# 关联作用域 (Associated Scope)

## 核心概念

在 nessa 中，每个有名字的类型都拥有一个**关联作用域 (associated scope)**，它具备完备的模块功能。关联作用域可以包含函数、常量、子类型、子模块等任何 item。

这意味着 `struct`、`enum`、`mod` 在作用域能力上是完全等价的——`mod` 只是一个没有实例化能力的命名类型。如果你愿意，完全可以把 `mod` 关键字换成 `struct` 或 `enum`，它们的关联作用域行为一致。

```nessa
struct Vec {
    -- 这些都定义在 Vec 的关联作用域中
    fn new() -> Vec = Vec.from_raw(0, 0, null)
    fn with_capacity(cap: usize) -> Vec { ... }

    const DEFAULT_CAPACITY: usize = 16

    -- 甚至可以嵌套定义子类型
    struct Iter {
        pos: usize,
        vec: Vec,
    }
}
```

## 定义符号的位置

关联作用域中的符号可以在两个位置定义：

### 1. 类型定义体内

直接在 `struct`、`enum`、`mod` 的花括号内定义：

```nessa
struct Point {
    x: f64,
    y: f64,

    fn origin() -> Point = Point { x: 0.0, y: 0.0 }
    fn distance(self, other: Point) -> f64 { ... }
}

enum Color {
    red,
    green,
    blue,
    rgb(r: u8, g: u8, b: u8),

    fn is_primary(self) -> bool {
        match self {
            red | green | blue => true,
            _ => false,
        }
    }
}
```

### 2. impl 块

通过 `impl` 块向类型的关联作用域追加符号：

```nessa
impl Point {
    fn translate(self, dx: f64, dy: f64) -> Point {
        Point { x: self.x + dx, y: self.y + dy }
    }

    fn scale(self, factor: f64) -> Point {
        Point { x: self.x * factor, y: self.y * factor }
    }
}
```

`impl` 块中定义的符号与类型定义体内的符号地位完全相同，都属于该类型的关联作用域。`impl` 块的存在是为了允许在类型定义之外的位置（如其他文件中）补充定义。

## extend 块

`extend` 块也可以向类型添加符号，但与 `impl` 有关键区别：

```nessa
extend SomeExternalType {
    fn my_helper(self) -> String { ... }
}
```

- `extend` 中定义的符号**仅在当前作用域内有效**，不会全局地修改目标类型的关联作用域
- `extend` 常用于**规避孤儿规则 (orphan rule)**——当你既不拥有类型也不拥有 trait 时，可以用 `extend` 在本地作用域内为外部类型添加方法
- 这保证了类型的关联作用域不会被远处的代码意外污染

```nessa
-- 在你自己的模块中，为标准库类型添加便捷方法
extend String {
    fn is_blank(self) -> bool {
        self.trim().len() == 0
    }
}

-- is_blank 仅在当前作用域及其子作用域中可用
```

## 模块作为特殊类型

`mod` 是一种专门用于代码组织的类型，它与 `struct`/`enum` 的唯一区别是**没有实例化能力**：

```nessa
mod math {
    const PI: f64 = 3.14159265358979

    fn sin(x: f64) -> f64 { ... }
    fn cos(x: f64) -> f64 { ... }

    mod trig {
        fn tan(x: f64) -> f64 = sin(x) / cos(x)
    }
}

-- 使用方式与访问类型的关联符号完全一致
math.sin(math.PI / 2.0)
math.trig.tan(0.5)
```

## 访问关联作用域中的符号

统一使用 `.` 投影运算符：

```nessa
-- 访问类型的关联函数
Point.origin()
Vec.new()
Color.is_primary(my_color)

-- 访问模块中的符号
net.http.Client.new("https://example.com")
```

## 关联作用域与可见性

关联作用域中的符号遵循与顶层 item 相同的三级可见性规则：

```nessa
struct Database {
    pub fn connect(url: String) -> Database { ... }  -- 公开
    fn ping(self) -> bool { ... }                    -- 包内可见
    private fn raw_query(self, sql: String) { ... }  -- 仅 Database 作用域内
}
```

## 关联作用域与 \_\_init\_\_

每个类型都可以在其关联作用域中定义 `__init__` 函数，在作用域加载完成后自动执行：

```nessa
mod registry {
    global handlers: Map = Map.new()

    fn __init__() {
        handlers.insert("/health", health_check)
    }

    fn health_check(req: Request) -> Response { ... }
}
```
