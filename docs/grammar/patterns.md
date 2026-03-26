# 模式 (Patterns)


## 基础模式

```ebnf
pattern ->
	id |
	underscore |
	literal |
	pattern_tuple |
	pattern_list |
	pattern_record |
	pattern_call |
	pattern_extended_call |
	pattern_if_guard |
	pattern_and_is |
	pattern_or |
	pattern_not |
	pattern_as_bind |
	pattern_rest_bind |
	pattern_option_some |
	pattern_error_ok |
	pattern_async |
	pattern_error
```

## 组合模式

```ebnf
pattern_or -> pattern or pattern
pattern_not -> not pattern
pattern_as_bind -> pattern as id
pattern_rest_bind -> ...id
```

## 守卫与约束

```ebnf
pattern_if_guard -> pattern if expr
pattern_and_is -> pattern and expr is pattern
```

## Option / Error 模式

```ebnf
pattern_option_some -> pattern ?
pattern_error_ok -> pattern !
pattern_error -> error pattern
```

## 解构模式

```ebnf
pattern_tuple -> (pattern*)
pattern_list -> [pattern*]
pattern_record -> { (property_pattern | id)* }
property_pattern -> id : pattern
```

## 调用型模式

```ebnf
pattern_call -> expr (pattern*)
pattern_extended_call -> expr { (property_pattern | id)* }
```

## async pattern
这个pattern仅用于标记一个effect call为异步调用，表面该调用将在新的task中执行
```ebnf
pattern_async -> async pattern
```

note: pattern语法同样用于错误限定类型与代数效应的匹配