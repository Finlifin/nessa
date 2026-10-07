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

字符字面量模式以Char标量值比较，支持Unicode及字符字面量的转义形式。
它可用于`match`、`matches`与Enum/Tuple载荷子模式；静态类型不兼容时编译报错，
动态Any值中的字符串和整数不会通过字符模式。

```ebnf
pattern_or -> pattern or pattern
pattern_not -> not pattern
pattern_as_bind -> pattern as id
pattern_rest_bind -> ...id
```

`or` 与 `as` 当前可用于 `match`、`matches` 和 `for`。`or` 按从左到右
顺序尝试，成功后不再求右分支；两边必须绑定相同名称，且每个名称的准确类型
一致（透明别名按 canonical type 比较），不自动提升整数宽度或擦除为 Any。
同一条成功路径不能重复绑定同一个名称。

`pattern as name` 在子模式成功后将整个输入值绑定到标识符 `name`，不重新
求输入表达式。别名保留该位置的输入类型；Any 输入的别名仍是 Any。
`as` 的优先级高于 `or`，所以为整个备选模式命名应写
`(left or right) as whole`。无逗号的括号只分组，带逗号才表示 Tuple 模式。

guard 在它所属的子模式成功后求值。`(left or right) if guard` 的 guard
失败会继续下一 arm；`(left if guard) or right` 的 guard 失败会尝试 right。
guard 只能访问该分支已遇到的绑定，不能提前读取后面的字段或另一分支的绑定。
`matches` 的绑定仅供其模式 guard 使用；`for` 中不匹配的元素跳过，继续迭代。
`not pattern` 当前也可用于 `match`、`matches` 和 `for`：它反转整个子模式
的成功与失败，包括字段测试和内部 guard，不重新求输入表达式。`not` 的优先级
高于 `as`、`or` 和 guard，低于构造调用和投影，因此 `not E.none as whole`
等价于 `(not E.none) as whole`；否定整个 guard 应写 `not (pattern if guard)`。

否定子模式的绑定只在它内部可见，供内部 guard 或其闭包使用；它们不导出给
arm body、外部 guard 或 `for` body，也不覆盖外部同名变量。内部 guard 所创建
并保存的闭包可正常保留这些捕获。`not _` 和 `not name` 永不匹配；`not not p`
恢复 p 的判断结果，但不会导出 p 的绑定。为否定结果命名需在外面使用 `as`。
内部仍检查准确的字段类型、备选分支绑定契约和 guard 的 bool 类型。

前缀 `!pattern` 是尚未实现的旧错误模式拼写，不表示逻辑否定；后缀 `pattern!`
仍是独立的 Error 解包模式。声明与函数参数中的组合模式，以及其他尚未接通的
模式仍需继续实现。Any 输入的 Tuple 解构仍要求先显式转换为静态 Tuple 类型。

## 守卫与约束

```ebnf
pattern_if_guard -> pattern if expr
pattern_and_is -> pattern and expr is pattern
```

`left and expr is right` 当前支持 `match`、`matches` 和 `for`。先匹配 left；
它成功后才求 expr，且每次尝试只求一次，再以该表达式的准确结果类型匹配 right。
任一侧失败都使整个约束失败；`for` 跳过该元素。右侧结果先保留快照，内部
guard 对原变量的写入不会改变当前匹配输入。快照保留引用共享及 trait 证明。

左右侧成功路径的绑定都供后续约束、guard 和 arm/loop body 使用；expr 只能
访问此前已遇到的绑定，不能提前读取 right 将引入的名称。两侧同一路径不能
重复绑定名称，`or` 两边仍要求全部成功绑定的名称与准确类型一致。`matches`
的所有绑定仍仅供其模式内部使用，`not` 内的约束绑定仍然保持私有。

`and … is …` 按左结合，优先级高于 `or` 和 guard，低于 `as` 与 `not`。
例如 `x and x+1 is y and y+1 is z` 依次求值；`p and e is q as computed`
的 computed 绑定右侧表达式结果，`(p and e is q) as whole` 的 whole 则绑定
原始输入。右侧多个备选或独立 guard 可用括号分组，如 `p and e is (q or r)`。
声明与函数参数中的此可失败模式仍报告不支持，不生成执行代码或归档。

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

List 模式当前可用于 `match`、`matches` 和 `for`。输入的静态类型须为 List
或 Any；实际值先通过 List 身份检查。没有 rest 时长度必须相等；有 rest 时
至少包含所有固定元素。固定元素的绑定类型是 Any，rest 的绑定类型是 List。

每个 List 模式允许一个具名 `...id`，可位于开头、中间或结尾，例如
`[first, ...middle, last]`；middle 可以是空 List。必须使用三个点和标识符，
`..id`、`...`、`..._`、多个 rest 和 List 以外位置的 rest 均拒绝。

每次尝试通过类型/长度检查后，先浅复制该 List 的全部元素槽位，再按从左
到右顺序匹配元素。guard 对原 List 的缩短、扩展或槽位写入不改变本次快照；
元素指向的堆对象仍共享。嵌套 List 在轮到其子模式时建立自己的快照。rest
创建独立 List，其槽位来自本次快照；修改 rest 槽位不修改原 List。外部
`as whole` 仍绑定原输入，失败后下一 arm 或备选会读取原 List 的当前状态。

绑定与 guard 的可见顺序、or 的名称/准确类型契约、not 的私有作用域以及
and/is 的计算顺序继续适用。guard 不能提前读取后面的 rest 名称；for 跳过
类型、长度或元素不匹配的输入。声明、函数参数、Tuple 中的 rest 和匿名
rest 仍需后续设计实现，不把它们当成当前已支持的解构形式。

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
