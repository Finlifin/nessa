# enum

`enum` 是 nessa 中的代数数据类型 (ADT)，用于定义一组互斥的变体 (variant)。每个 variant 可以携带命名参数，`enum` 同样拥有关联作用域。

## 定义

```nessa
enum Direction {
    north,
    south,
    east,
    west,
}

enum Ast {
    `null`,
    id(name: String),
    add(lhs: Ast, rhs: Ast),
}
```

当 variant 名与关键字冲突时，使用反引号转义（如 `` `null` ``）。

## Variant 命名规范

- Error 用途的 enum：variant 使用大驼峰命名法 (`PascalCase`)
- 非 Error 用途的 enum：variant 使用蛇形命名法 (`snake_case`)

```nessa
-- Error enum：PascalCase variant
enum HttpError {
    Timeout(time: Duration),
    ConnectionReseted,
    NotFound(url: String),
}

-- 普通 enum：snake_case variant
enum IpAddr {
    v4(a: u8, b: u8, c: u8, d: u8),
    v6(addr: String),
}
```

## 构造与模式匹配

```nessa
-- 构造
let addr = IpAddr.v4(127, 0, 0, 1)
let tree = Ast.add(Ast.id("x"), Ast.id("y"))

-- 模式匹配
addr match {
    IpAddr.v4(a, b, c, d) => println("{a}.{b}.{c}.{d}"),
    IpAddr.v6(addr) => println("{addr}"),
}

tree match {
    Ast.`null` => println("null"),
    Ast.id(name) => println("id: {name}"),
    Ast.add(lhs, rhs) => println("add"),
}
```

## 关联作用域

```nessa
enum Shape {
    circle(radius: f64),
    rect(width: f64, height: f64),

    fn area(self) -> f64 {
        self match {
            circle(r) => 3.14159 * r * r,
            rect(w, h) => w * h,
        }
    }

    fn is_circle(self) -> bool {
        self matches circle(_)
    }
}
```

## 递归类型

enum 天然支持递归定义（如上面的 `Ast` 示例），运行时通过引用实现。

## 语法参考

```ebnf
enum_def -> enum id { (enum_variant | statement)* }
enum_variant -> id ((param*))?
```
