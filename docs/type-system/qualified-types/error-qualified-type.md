# Error 限定类型 (Error Qualified Type)

Error qualified type 是 nessa 中处理可恢复错误的核心机制。它是一种 tagged union，其 tag 位存储 128 位的类型 ID，payload 是具体的类型实例。tag 为 0 表示 ok 的情况。

## 语法

```nessa
!ErrorType MainType
![ErrorType1, ErrorType2] MainType
```

`!` 前缀表示 error qualification，第一个表达式是 error 类型（或 error 类型集合），第二个表达式是主类型。

## 等价关系

Error qualified type 满足以下代数等价关系：

```
forall t.           t == ![] t                          -- 空 error set 等价于无 error
forall e, t.        !e t == ![e] t                      -- 单个 error 等价于单元素集合
forall e1, e2, t.   !e1 !e2 t == !e2 !e1 t             -- error set 交换律
                    == ![e1, e2] t == ![e2, e1] t
forall es1, es2, t. !es1 !es2 t == !es1 ++ es2 t       -- error set 合并
```

这意味着 error 类型集合是无序的，多层嵌套会自动展平。

## Normalized Representation

Error qualified type 的规范化表示由主类型和 error 类型集合组成，其中 error 类型集合按类型 ID 排序以保证一致性。

## Tag 位设计

tag 位占用 128 位（存储类型 ID）。这是合理的，因为 error qualified type 通常是短生命周期的数据（如函数返回值），不会大量存储。

## Error 消除 (Elimination)

通过 `!` 后缀加消除块来逐个处理 error 类型。每穷尽处理 error type set 中的一个类型，表达式的 error type set 就削去该类型：

```nessa
fn get_user(user_id: String) -> !ParseErr User {
    -- get: fn(String) -> !HttpErr Response
    https.get(...)! {
        -- 消除块可以一次处理多个 error type
        -- 隐式的 ok 分支：将 parse 的可能 error 传播出去
        ok! => response.parse(.json, User)!.as(User),
        HttpErr.Timeout(...) => ...,
        HttpErr.* as e => ...,
    }

    error ParseErr.Unknown
}
```

函数返回值的 error type set 必须是内部传播出去的错误的父集，nessa 将自动生成类型转换。

## Error 传播 (Propagation)

类似 Zig 的 `try` 或 Rust 的 `?`，使用 `!` 后缀运算符传播错误：

```nessa
fn load_config(path: String) -> !IoErr !ParseErr Config {
    let content = read_file(path)!       -- 传播 IoErr
    let config = parse_toml(content)!    -- 传播 ParseErr
    config
}
```

## Error 构造

```nessa
-- 直接构造 error 值
error ParseErr.Unknown

-- 需要指定具体类型时使用 as cast
(error ParseErr.Unknown).as(!ParseErr !HttpErr String)
```

## 模式匹配

也可以直接对 error qualified type 的值进行模式匹配：

```nessa
get() match {
    ok! => ...,
    error NetworkErr.Timeout(...) => ...,
    error e => ...,    -- e: Any
}
```

## Error Type Set 表达式

`!expr expr` 中第一个 `expr`（error type set）允许的语法受到限制（因为 nessa 没有编译时执行）：

- 可解析为具体类型的语法（如 projection 连成的 path）
- list construction 表达式，用于表达集合，每个元素需解析到具体类型
- 单个 id，可能直接指向类型，或指向一个 `const` 声明（其 init expr 同样受限）
- `++` concat 表达式，lhs 和 rhs 都需解析到 error type set

## 语法参考

```ebnf
error_qualified_type -> !error_set_expr type_expr
error_construction -> error expr
error_propagation -> expr !
error_elimination -> expr ! { (catch_arm | case_arm)* }
pattern_error_ok -> pattern !
pattern_error -> error pattern
```
