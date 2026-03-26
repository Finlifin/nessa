# 模式匹配 (Pattern Matching)

模式匹配是 Nessa 的核心特性之一，广泛应用于 `match` 表达式、变量定义和函数参数中。

## Match 表达式

```nessa
a match
    value? => println("It is an optional with value: {}", value)
    null => println("It is null")
```

## 枚举匹配

```nessa
ast_node match:
    Ast.int(v) => v
    Ast.float(v) => v
    _ => 0
```

## 偏函数 (Partial Functions)

`case` 关键字用于创建基于模式匹配的偏函数（Closure）。

```nessa
let get_int_value = case Ast.int(v) => v
-- 默认包含一个隐式参数，如果匹配失败返回 null (Context dependent behavior)

let values = asts.collect_map(
    case Ast.int(v) => v.to_string() 
    | Ast.float(v) => v.to_string()
)
```

## 守卫 (Guards)

```nessa
let alright = case (x, y) if x > 100 => x + y
```
