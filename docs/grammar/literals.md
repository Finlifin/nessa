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

## 字符串与插值

```ebnf
string -> " (string_char | escape | {{ | }} | interpolation)* "
interpolation -> { expr }
```

所有字符串字面量都可以插值，没有前缀：`"hi {name}, {x + 1}"`。字面花括号
写作 `{{` 和 `}}`，单独的 `}` 是词法错误；`{}` 缺少表达式时报告语法错误。
转义序列（`\n`、`\t`、`\\`、`\"`、`\xHH`、`\u{...}`/`\uHHHH` 等）在每个文本
片段中同样生效，`\u` 必须表示 Unicode 标量值，非法转义报告错误。

插值表达式内部可以包含任意嵌套字符串，例如 `"{f("x")}"` 或
`"{"in{x}"}"`；嵌套字符串遵守同样的规则。插值不能跨行，在换行或文件结束
前未闭合时报告未终止的插值。

每个插值值通过 `Display.to_string` 转为 String，再按从左到右顺序拼接。
静态类型必须实现 `Display`，否则编译期报错；静态类型为 Any 的值在运行时
检查 `Display` 证明，缺少时产生 TypeError。String 值直接拼接。

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
