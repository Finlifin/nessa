# tuple

元组 (tuple) 是 nessa 中的匿名积类型，用于将多个值组合在一起而无需定义命名类型。

## 构造

```nessa
let pair = (1, "hello")
let triple = (true, 42, 3.14)
let unit = ()                    -- 空元组，即 Unit
```

## 元素访问

通过 `.整数索引` 语法访问元组元素（从 0 开始）：

```nessa
let t = ("alice", 30, true)
let name = t.0               -- "alice"
let age = t.1                -- 30
let active = t.2             -- true
```

## 模式匹配解构

```nessa
let (name, age) = ("bob", 25)

(1, "hello") match {
    (0, _) => println("zero"),
    (n, s) => println("{n}: {s}"),
}
```

## 作为函数返回值

元组常用于从函数返回多个值：

```nessa
fn min_max(list: List) -> (i32, i32) {
    let mn = list.fold(i32.MAX, |a, b| if a < b { a } else { b })
    let mx = list.fold(i32.MIN, |a, b| if a > b { a } else { b })
    (mn, mx)
}

let (lo, hi) = min_max([3, 1, 4, 1, 5])
```

## 类型语法

```nessa
-- 元组类型
(i32, String)
(bool, f64, char)
()                  -- Unit
```

## 语法参考

```ebnf
tuple_construction -> (expr, expr*) | ()
tuple_type -> (expr, expr*)
```
