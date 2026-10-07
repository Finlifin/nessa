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

## 当前实现的参数与求值规则

静态已知实例的 `apply/update` 使用普通源函数的参数绑定：检查 `self`、访问权限、
参数类型和数量，支持具名实参、默认值以及单个 List 变参。`apply` 的表达式类型
来自方法返回类型；`update` 可以返回任意类型，但赋值表达式丢弃该返回值，类型为 Unit。
调用赋值只查找并执行 `update`，不要求存在 `apply`，也不会先调用 getter。

接收者先求值并保存，显式实参按源码顺序求值并保存；更新的右值随后求值并保存。
参数重排不改变这些副作用的顺序。默认值在显式实参和右值之后按参数声明顺序
求值，可以引用 `self` 和已绑定参数。更新的右值绑定到最后一个固定位置参数；
若位置参数尾部是变参，则右值作为最后一个变参元素。可选具名参数仍独立绑定。

`Any` 接收者使用运行时实际类型分派，包含无载荷 Enum 的立即值表示；当前动态
调用要求完整的位置参数，不进行声明侧的具名、默认值或变参展开。动态方法访问
按持久化的调用点词法 scope、方法访问等级及 extend 范围筛选候选，歧义和
不可访问均明确报错。旧归档缺权限信息时不能默认授权，详见
[实施要求与剩余范围](../dev/dynamic-method-access-plan.md)。

已绑定的源方法沿用普通函数调用、GC 根、效应暂停恢复与 NSBC 函数身份，无需
新增指令或改变当前归档版本；删除源码后的跨进程回归验证了上述调用和更新路径。
