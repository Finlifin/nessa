# 特殊函数 (Special Functions)

nessa 中有三个特殊函数名：`new`、`apply` 和 `update`。当类型定义了这些函数时，对应的语法糖会被编译为这些函数的调用。

## new — 构造函数

类似 Julia 的 `new`。当对类型名进行调用时，编译器将其转换为 `.new()` 调用：

```nessa
struct Point {
    x: f64,
    y: f64,

    fn new(x: f64, y: f64) -> Point = Point { x, y }
}

-- 语法糖
let p = Point(1.0, 2.0)
-- 编译为：
-- let p = Point.new(1.0, 2.0)
```

注意区分：`Point { x: 1.0, y: 2.0 }` 是结构体字面量构造（extended application），而 `Point(1.0, 2.0)` 是 `new` 函数调用。

## apply — 调用运算符

类似 Scala 的 `apply`。当对一个实例进行函数调用语法时，编译器将其转换为 `.apply()` 调用：

```nessa
struct Matrix {
    data: List,
    rows: usize,
    cols: usize,

    fn apply(self, row: usize, col: usize) -> Any {
        self.data.apply(row * self.cols + col)
    }
}

let m = Matrix { ... }

-- 语法糖
let val = m(0, 1)
-- 编译为：
-- let val = m.apply(0, 1)
```

`List.apply(index: usize) -> Any` 就是通过这个机制实现下标访问的。

## update — 更新运算符

当对实例使用赋值调用语法时，编译器将其转换为 `.update()` 调用：

```nessa
struct Grid {
    data: List,
    width: usize,

    fn update(self, x: usize, y: usize, value: Any) {
        self.data.update(y * self.width + x, value)
    }
}

let g = Grid { ... }

-- 语法糖
g(2, 3) = "wall"
-- 编译为：
-- g.update(2, 3, "wall")
```

## 总结

| 语法 | 编译为 | 说明 |
|------|--------|------|
| `Type(args...)` | `Type.new(args...)` | 类型名调用 → 构造函数 |
| `obj(args...)` | `obj.apply(args...)` | 实例调用 → apply |
| `obj(args...) = val` | `obj.update(args..., val)` | 实例赋值调用 → update |

这三个特殊函数都是可选的——只有当类型定义了对应函数时，语法糖才生效。
