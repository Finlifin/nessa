# Nessa2 语法总览

本文档组用于描述 Nessa2 的语法层，按主题拆分为多个文件：

- `lexical.md`：词法与 token 约定
- `literals.md`：字面量与基础构造
- `expressions.md`：表达式语法
- `patterns.md`：模式语法
- `statements.md`：语句语法
- `definitions.md`：定义语法（函数、类型、模块等）

## 记号约定

本文档采用轻量 EBNF 风格：

- `A -> B` 表示产生式
- `x?` 表示可选
- `x*` 表示零个或多个
- `x+` 表示一个或多个
- `(a | b)` 表示二选一

## 统一术语

- `expr`：表达式节点
- `pattern`：模式节点
- `statement`：语句节点
- `block`：代码块，通常是 `{ statement* }`

## 格式敏感上下文

Nessa 支持格式敏感（缩进）语法糖。

- 当解析器遇到 `:`、`=>`、`do`，且其后紧随一个换行时，会开启一层格式敏感上下文。
- 该上下文内可以省略显式 `{ ... }`，改由缩进确定代码块边界。
- 这里的“开启一层”表示只对当前触发点对应的下一层块生效，不会无限连锁扩展。

示例（等价）：

```nessa
if cond:
	do_something()

if cond {
	do_something()
}
```

## arbitrary_id

- 反引号包裹的内容会被词法层视为一个 `arbitrary_id`（一个标识符 token）。
- 该形式适用于需要保留特殊字符或空格的标识符文本。

## 备注

- 语法文档以 Nessa 当前 AST 与 token 定义为准。
- 某些关键字在词法层已保留，即使语义层暂未完全开放，仍会在词法章节列出。
