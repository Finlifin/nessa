# 语句 (Statements)

## 通用语句

```ebnf
statement ->
	expr_statement |
	assign_statement |
	decl_statement |
	control_statement |
	block

expr_statement -> expr
block -> { statement* }
```

## 词法代数效应处理器
```ebnf
handles_statement -> handles(expr)(param*) (-> expr)? ((= expr) | block)
```

## 赋值语句

```ebnf
assign_statement ->
	expr = expr |
	expr += expr |
	expr -= expr |
	expr *= expr |
	expr /= expr
```

## 声明语句

```ebnf
visibility_modifier -> pub | private

const_decl -> visibility_modifier? const pattern (: expr)? = expr (else expr)?
let_decl -> let pattern (: expr)? = expr (else expr)?
global_decl -> visibility_modifier? global id : expr = expr
assoc_decl -> assoc id : expr (= expr)?
let_handler -> handles(expr) let id (: expr)? = expr
```

说明：

- `global_decl` 用于声明全局变量。
- `assoc_decl` 用于在 `trait` 或 `impl` / `extend` 中声明关联内容。
- `pub` 与 `private` 可用于所有定义前缀，包含 `const_decl` 与 `global_decl`。

## 控制流语句

```ebnf
return_statement -> return expr? (if expr)?
resume_statement -> resume expr? (if expr)?
break_statement -> break id? (if expr)?
continue_statement -> continue id? (if expr)?

if_statement -> if expr block (else (if_statement | block))?
when_statement -> when { (else_condition_arm | condition_arm)* }

while_loop -> while (: id)? expr block
for_loop -> for (: id)? pattern in expr block
```

## 条件分支臂

```ebnf
condition_arm -> condition_expr => (block | statement)
else_condition_arm -> else => (block | statement)
catch_arm -> catch id => (block | statement)
```
