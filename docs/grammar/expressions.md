# 表达式 (Expressions)

## 前缀表达式

```ebnf
prefix_expr ->
	self_lower |
	self_upper |
	null |
	unit |
	neg |
	not |
	lambda |
	case_map |
	case_alternative |
	error_construction |
	tuple_construction |
	list_construction |
	object_construction |
	if |
	when |
	return |
	resume |
	break |
	continue

self_lower -> self
self_upper -> Self
null -> null
unit -> ()

neg -> -expr
not -> not expr

lambda -> |param*| (-> type) (block | statement)
case_map -> case pattern => (block | statement)
case_alternative -> case_map | case_map

error_construction -> error expr
tuple_construction -> (expr, expr*)
list_construction -> [expr*]
object_construction -> { (property | expr)* }

if -> if_statement
when -> when_statement
return -> return_statement
resume -> resume_statement
break -> break_statement
continue -> continue_statement
```

## 中缀表达式

```ebnf
infix_expr ->
	add |
	sub |
	mul |
	div |
	mod |
	and |
	or |
	not_eq |
	eq |
	lt |
	gt |
	lte |
	gte |
	concat |
	projection |
	view |
	type_cast |
	handler_application |
	bool_matches |
	pipeline |
	infix_fn_call

add -> expr + expr
sub -> expr - expr
mul -> expr * expr
div -> expr / expr
mod -> expr % expr
and -> expr and expr
or -> expr or expr
not_eq -> expr != expr
eq -> expr == expr
lt -> expr < expr
gt -> expr > expr
lte -> expr <= expr
gte -> expr >= expr
concat -> expr ++ expr
projection -> expr . id

{-
	取view操作是nessa中一类特殊的语法映射，可以通过这一种语法覆盖不常用的语法，常见的操作比如expr'type，取类型
-}
view -> expr ' id

type_cast -> expr.as(expr)
handler_application -> expr.use(expr)
bool_matches -> expr matches pattern
pipeline -> expr |> expr
infix_fn_call -> expr id expr
```

## 后缀表达式

```ebnf
postfix_expr ->
	application |
	extended_application |
	option_propagation |
	effect_propagation |
	error_propagation |
	post_match |
	post_do

arg -> ...expr | id = expr | expr
property -> id : expr
application -> expr (arg*)
extended_application -> expr { (property | expr)* }

option_propagation -> expr ?
effect_propagation -> expr #
error_propagation -> expr !

post_match -> expr match { case_arm* }
post_do -> expr do (lambda | block)
```

尾随 `do` 在名称解析前展开为普通调用。`f do lambda` 等价于 `f(lambda)`；
`f(args) do lambda` 等价于 `f(args, lambda)`，尾随闭包按普通位置实参绑定。
`f do { statements }` 等价于 `f(|| { statements })`，没有隐式参数；需要参数
时使用显式 `|params|`。源码中的 callee、已有实参和闭包按该普通调用的顺序求值。
闭包体在 callee 调用回调时执行，return/break/continue 使用闭包自身的控制边界。
可选参数仍按名字供给，单 List 变参仍收集追加的位置实参。解析器的原始 AST
保留 PostDo；编译后的 AST 与后续阶段使用普通 Call/Lambda。


## 消除表达式

```ebnf
effect_elimination -> expr # { case_arm* }
error_elimination -> expr ! { (catch_arm | case_arm)* }
```

## 区间表达式

```ebnf
range_expr ->
	range_from |
	range_to |
	range_to_inclusive |
	range_from_to |
	range_from_to_inclusive |
	range_full

range_from -> expr ..
range_to -> .. expr
range_to_inclusive -> ..= expr
range_from_to -> expr .. expr
range_from_to_inclusive -> expr ..= expr
range_full -> ..
```

## 类型相关表达式

```ebnf
param_type -> .id : expr | ...id : expr | expr
arrow -> expr -> expr
fn_type -> fn (param_type*)
effect_type -> async? effect (param_type*)
tuple_type -> (expr, expr*)

effect_qualified_type -> #effect_set_expr type_expr
error_qualified_type -> !error_set_expr type_expr
optional_type -> ?type_expr
```
## 拼接的当前编译路径

`left ++ right` 调用左值类型的 `concat(self, other)` 实例方法。静态类型检查
固定单操作数、可见性、参数及结果类型；结果不要求与左值同类型。先求值并保存
left，再求值right。Any左值动态分派，非法方法或参数在运行时报错。

String在std.string提供concat和len，len计UTF-8字节；`++`不自动把非字符串
转成字符串。List在std.collections提供concat，创建新的容器，按顺序合并元素
引用；输入容器保持不变，嵌套可变元素仍共享。Map没有定义隐含合并语义。
