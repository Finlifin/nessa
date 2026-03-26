# Effect 限定类型 (Effect Qualified Type)

Effect qualified type 描述一个表达式在求值过程中可能发出的代数效应集合。其结构与 error qualified type 类似。

## 语法

```nessa
#effect_name Type              -- 单个效应
#[eff1, eff2] Type             -- 效应集合
```

## 等价关系

与 error qualified type 完全对称：

```
forall t.           t == #[] t                          -- 空 effect set 等价于无 effect
forall e, t.        #e t == #[e] t                      -- 单个 effect 等价于单元素集合
forall e1, e2, t.   #e1 #e2 t == #e2 #e1 t             -- effect set 交换律
                    == #[e1, e2] t == #[e2, e1] t
forall es1, es2, t. #es1 #es2 t == #es1 ++ es2 t       -- effect set 合并
```

效应集合同样是无序的，多层嵌套自动展平。

## 示例

```nessa
effect log(msg: String) -> Unit
effect read_line() -> String

-- 函数返回类型标注了可能发出的效应
fn interactive_greeting() -> #[log, read_line] String {
    let name = read_line()
    log("greeted: {name}")#
    "hello, {name}"
}
```

## Effect 传播

使用 `#` 后缀运算符传播效应：

```nessa
fn wrapper() -> #[log] Unit {
    some_effectful_fn()#
}
```

## Effect 消除

通过 `#` 后缀加消除块来处理效应，或使用 `.use()` handler application：

```nessa
-- 使用 handler application
interactive_greeting().use(handler)

-- 使用消除块
some_fn()# {
    ...
}
```

详细的效应处理机制参见 [代数效应](../../algebraic-effect-and-multi-prompt-delimited-continuation/algebraic-effect.md)。

## 语法参考

```ebnf
effect_qualified_type -> #effect_set_expr type_expr
effect_propagation -> expr #
effect_elimination -> expr # { case_arm* }
handler_application -> expr.use(expr)
```
