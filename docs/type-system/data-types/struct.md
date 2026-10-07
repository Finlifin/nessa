# struct

`struct` 是 nessa 中最基本的复合数据类型，用于将多个命名字段组合为一个类型。每个 `struct` 同时拥有关联作用域，可以在定义体内直接定义函数、常量等符号。

## 定义

```nessa
struct Point {
    x: f64,
    y: f64,
}

struct User {
    id: u64,
    name: String,
    email: String,
    age: u32 = 0,       -- 带默认值的字段
}
```

## 字段默认值

字段可以指定默认值，构造时可省略这些字段：

```nessa
struct Config {
    host: String = "localhost",
    port: u16 = 8080,
    max_connections: u32 = 100,
}

let cfg = Config { host: "0.0.0.0" }
-- port 和 max_connections 使用默认值
```

构造中的显式字段按源码顺序求值并保存，然后按字段声明顺序装载。
省略的默认字段在显式字段之后按声明顺序求值；已显式提供的字段不执行默认表达式。
默认名称在结构体声明作用域解析，可访问关联常量、全局值和 helper。
当前不支持默认值引用未构造实例的其他字段，或跨默认表达式边界的 return/resume；
这些情况产生编译诊断。默认表达式内部自己的局部变量、循环和嵌套函数正常使用。

## 构造与解构

```nessa
-- 构造
let p = Point { x: 1.0, y: 2.0 }

-- 字段访问
let dx = p.x

-- 模式匹配解构
p match {
    Point { x, y } => println("({x}, {y})"),
}

-- 简写：变量名与字段名相同时
let x = 1.0
let y = 2.0
let p = Point { x, y }
```

## 关联作用域中的定义

```nessa
struct Vec2 {
    x: f64,
    y: f64,

    fn new(x: f64, y: f64) -> Vec2 = Vec2 { x, y }
    fn zero() -> Vec2 = Vec2 { x: 0.0, y: 0.0 }

    fn length(self) -> f64 = sqrt(self.x * self.x + self.y * self.y)

    fn add(self, other: Vec2) -> Vec2 {
        Vec2 { x: self.x + other.x, y: self.y + other.y }
    }
}
```

## 可见性

字段和关联符号都遵循三级可见性规则：

```nessa
pub struct Connection {
    pub host: String,
    pub port: u16,
    private socket: RawSocket,

    pub fn connect(host: String, port: u16) -> !IoErr Connection { ... }
    private fn raw_write(self, data: Bytes) { ... }
}
```

## 语法参考

```ebnf
struct_def -> struct id { (struct_field | statement)* }
struct_field -> (pub | private)? id : expr (= expr)?
```
