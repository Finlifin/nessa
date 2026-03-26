# 字面量 (Literals)

## 原始字面量

```ebnf
literal ->
	integer | int_bin | int_oct | int_hex |
	real | real_sci |
	string | character |
	true | false |
	null | unit
```

## Symbol

```ebnf
symbol -> . id
```

示例：`.ok`、`.err`、`.status`

## 容器与复合构造

```ebnf
tuple_construction -> (expr, expr*) | ()
list_construction -> [expr*]
object_construction -> { (property | expr)* }
property -> id : expr
```

说明：

- `()` 在值位置可表示空元组（与 unit 在语义层可等价处理）。
- `object_construction` 允许属性与普通表达式混排。

## 特殊值

```ebnf
self_lower -> self
self_upper -> Self
```

- `self`：实例上下文值。
- `Self`：类型层或当前类型占位。
