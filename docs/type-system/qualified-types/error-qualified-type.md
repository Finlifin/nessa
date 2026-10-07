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
        response! => response.parse(.json, User)!.as(User),
        HttpErr.Timeout(...) => ...,
        HttpErr.* as e => ...,
    }

    error ParseErr.Unknown
}
```

函数返回值的 error type set 必须是内部传播出去的错误的父集，nessa 将自动生成类型转换。

这里的父集是具体错误类型集合的包含关系。成员按别名规范化后的完整类型
身份比较；数值类型的成功值转换不会改变错误标签。例如 `!i8 T` 的错误
不能直接进入 `!i64 T`，但 `!E i8` 的成功载荷可以转换成 `!E i64`。

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

## 消除分支的值与绑定

未写成功分支时，消除块保留原成功值；未匹配的错误仍作为剩余Error限定值
传递，只有显式后缀传播 `!` 才提前退出当前函数。不能仅因某个错误未匹配
就在消除块中自动return。guard或可失败子模式不能证明该错误类型已完全消除。

成功模式使用 `pattern!`：`value!`绑定成功载荷，`_!`忽略载荷，`ok!`中的ok
也是普通绑定名，不是保留字或隐式通配符。这些规则已由用户明确选择。
完整构造、传播、消除、模式与GC安全的128位标签表示已贯通并通过独立
验收；具体布局与归档能力见开发文档，不能仅凭语法接受推断其他功能完成。


## 动态载荷与错误集合

`error value` 的 value 为 `Any` 时，标签取实际载荷具体类型的完整 TypeId，
不能使用 Any 的 TypeId 代替。进入显式限定的错误集合时检查实际具体类型；
不属于集合的值产生受检失败。这也适用于 `error e` 模式绑定后的重抛。
普通未包装值进入 Error 限定类型时是成功分支，即使其具体类型也在错误
集合中，仍不能凭载荷类型把它猜成错误分支。

错误集合源码只接受可实例化的具体类型表达式；无上下文的动态错误在编译器
内部保留开放错误集合，有限的类型分支不能证明消除了其中所有可能错误。
开放集合的内部元数据表达见实施协议，不把它当作一个名为 Any 的具体错误。

`catch e => body` 消除分支捕获任意错误的载荷，e 为普通 Any 绑定；这里不
捕获 continuation，也不为 body 创建新的函数返回边界。`E.*` 选择一个
具体枚举类型的所有分支，guard 或可失败子模式仍要求保留剩余错误。

本节是完整 Error 实施采用并已验证的语义。

## 普通 match 的穷尽性

直接对 Error 限定值使用 `match` 时，分支必须覆盖所有可达的成功值和错误。
成功类型为 NoReturn 时不需要成功分支；封闭错误集合中的每个类型必须被
完整覆盖。开放错误集合需要无条件的 `error e`、`catch e` 或整体通配模式。
仅匹配有限的具体错误类型不能穷尽开放集合，guard 也不能单独证明覆盖。
整体通配模式绑定原限定值，`value!` 和 `error value` 才提取对应载荷。

此规则适用于普通 `match`。Error 消除块 `value! { ... }` 仍按前述规则
保留隐式成功值和未处理的剩余错误。

错误集合允许可实例化的具体 Type 和 Continuation 类型，以及可作为载荷的
具体限定类型别名。集合成员本身不会被展平；只有成功类型上连续的 Error
限定会合并。Any、NoReturn、裸 trait、effect 和 module 不能作为显式集合成员。
