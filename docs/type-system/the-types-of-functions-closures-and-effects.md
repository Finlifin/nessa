# 函数、闭包与效应的类型

## 函数类型

函数类型使用 `fn` 关键字描述：

```nessa
fn(i32) -> i32                     -- 接受 i32，返回 i32
fn(String, u32) -> bool            -- 接受两个参数
fn() -> Unit                       -- 无参数，返回 Unit
fn(i32, i32) -> i32                -- 二元函数
```

函数是一等公民，可以赋值给变量、作为参数传递、作为返回值：

```nessa
let add: fn(i32, i32) -> i32 = |a, b| a + b

fn apply(f: fn(i32) -> i32, x: i32) -> i32 = f(x)

fn make_adder(n: i32) -> fn(i32) -> i32 {
    |x| x + n
}
```

## 闭包类型

闭包（lambda）使用与函数相同的类型签名。闭包可以捕获外部环境中的变量：

```nessa
let offset = 10
let add_offset: fn(i32) -> i32 = |x| x + offset
```

闭包的类型签名不区分是否捕获了环境——`fn(i32) -> i32` 既可以是普通函数也可以是闭包。捕获规则的细节参见 [捕获规则](../function-and-closure/capturing-rules.md)。

## 代数效应类型

代数效应的类型签名与函数类似，使用 `effect` 关键字：

```nessa
effect(Any) -> Unit                -- 同步效应
async effect(Any) -> Unit          -- 异步效应
```

### 效应定义示例

```nessa
-- 同步效应
effect read_line() -> String
effect log(msg: String) -> Unit

-- 异步效应
async effect yield(value: Any) -> Unit
async effect await_io(task: IoTask) -> Bytes
```

### 效应类型签名

效应在类型层面的表示：

```nessa
-- yield 的类型
yield: effect(Any) -> Unit

-- async yield 的类型
async_yield: async effect(Any) -> Unit
```

## handles 子句

函数可以通过 `handles` 子句声明它处理的效应：

```nessa
fn run_with_logging(f: fn() -> #[log] Unit) -> Unit handles log {
    f().use(|msg| println("[LOG] {msg}"))
}
```

## 类型语法参考

```ebnf
fn_type -> fn (param_type*)
effect_type -> async? effect (param_type*)
```
