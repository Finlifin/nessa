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
- 当前实现要求`assoc`给出初始化表达式，仅支持Type关联绑定；上式无初值的
  声明仍为设计目标，不能将其自动解释为Any默认值。
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

`for` 每轮调用一次 `next`，仅带标签结果的 `done` 结束循环。Item 在 body
类型检查前绑定；支持的可失败模式不匹配时跳过该元素。输入与 `into_iter`
只求值一次，continue 回到下一次 next，break 不额外消费元素。

## 条件分支臂

```ebnf
condition_arm -> condition_expr => (block | statement)
else_condition_arm -> else => (block | statement)
catch_arm -> catch id => (block | statement)
```
