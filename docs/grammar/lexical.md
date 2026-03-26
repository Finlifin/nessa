# 词法 (Lexical)

## 基本规则

- 源码先被切分为 token 流，再交给语法分析器。
- Nessa 使用缩进敏感语法，词法层会产生 `indent` 与 `outdent`。
- 文件边界会产生 `sof` 与 `eof`。

## 注释

- 行注释：`-- comment`
- 块注释：`{- comment -}`

## 标识符

```ebnf
id -> [A-Za-z_][A-Za-z0-9_]*
arbitrary_id -> `...`
```

说明：

- `id` 是常规标识符。
- `arbitrary_id` 允许使用反引号包裹任意文本。
- `_` 是独立关键字（占位符），不是普通标识符。

## 分隔符与运算符

```ebnf
punctuation -> . | : | , | ; | ( | ) | [ | ] | { | }
operators ->
	= | == | => |
	+ | += | - | -= | * | *= | / | /= | % | %= |
	! | != | # | ? |
	< | <= | > | >= |
	-> | |> |
	' | @ | $ | & | | | ^ | ~ | \\
```

## 布局 token

```ebnf
layout -> newline | indent | outdent
```

## 关键字

```ebnf
keywords ->
	and | as | assoc | async | atomic | await |
	break | case | catch | const | continue |
	def | defer | derive | do |
	effect | else | enum | error | extend | extern |
	false | fn | for |
	global |
	handles |
	if | impl | in | is | itself |
	lambda | let |
	match | matches | mod |
	newtype | not | null |
	or |
	private | pub |
	reset | resume | return |
	self | Self | shift | static | struct |
	test | trait | true | typealias |
	use |
	when | where | while |
	_
```

其中：

- `global`、`assoc`、`pub` 为新增保留关键字。
- 关键字是否可在当前语义上下文使用，由语法层与后续阶段共同决定。
