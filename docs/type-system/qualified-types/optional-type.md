# Optional 类型

Optional 类型表示一个值可能存在也可能为 `null`。语法上使用 `?` 前缀。

## 语法

```nessa
?String          -- 可能为 null 的 String
?i32             -- 可能为 null 的 i32
```

## 构造

```nessa
let name: ?String = "alice"     -- 有值
let missing: ?String = null     -- 无值
```

## Optional 消除 (Elimination)

通过 `?` 后缀加消除块来处理 null 情况：

```nessa
let a = map.get("key")? {
    -- 当值为 null 时执行此块
    return null
}
-- 此处 a 的类型已经是非 optional 的
```

## Optional 传播 (Propagation)

使用 `?` 后缀运算符传播 null——如果值为 null，立即返回 null：

```nessa
struct User {
    id: String,
    age: u32,
    name: String,
}

-- get_user_by_id: fn(String) -> ?User
-- user_name: ?String
let user_name = get_user_by_id(id)?.name
```

链式传播非常自然：

```nessa
-- 如果任何一步返回 null，整个表达式为 null
let city = get_user(id)?.address?.city
```

## 模式匹配

```nessa
get_user(id) match {
    user? => println("found: {user.name}"),
    null => println("not found"),
}
```

## unwrap

当你确信值不为 null 时，可以使用 `unwrap()`（值为 null 时会 panic）：

```nessa
let name = get_user(id).unwrap().name
```

Optional 和 error qualified type 都支持 `value.unwrap()`。

## 语法参考

```ebnf
optional_type -> ?type_expr
option_propagation -> expr ?
pattern_option_some -> pattern ?
```
