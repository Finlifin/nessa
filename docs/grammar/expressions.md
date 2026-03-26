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