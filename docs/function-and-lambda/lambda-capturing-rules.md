# Lambda 捕获规则 (Lambda Capturing Rules)

Lambda 可以捕获定义时所在作用域中的变量。nessa 的捕获规则决定了变量以何种方式被 lambda 引用。

## 基本捕获

Lambda 自动捕获其函数体中引用的外部变量：

```nessa
let offset = 10
let add_offset = |x: i32| x + offset   -- 捕获 offset
add_offset(5)   -- 15
```

## 捕获方式

nessa 的 lambda 按值捕获 (capture by value)。捕获发生在 lambda 创建时，之后外部变量的修改不影响 lambda 内的副本：

```nessa
let x = 1
let f = || x + 1    -- 捕获 x 的当前值 1
x = 100
f()                  -- 2，不是 101
```

## 闭包与函数的统一类型

捕获了环境的 lambda（即闭包）和普通函数共享同一个类型签名 `fn(...) -> T`。调用方无法区分一个函数值是否捕获了环境：

```nessa
fn make_adder(n: i32) -> fn(i32) -> i32 {
    |x| x + n    -- 捕获 n，返回一个闭包
}

let add5 = make_adder(5)
add5(10)   -- 15
```

## 嵌套捕获

Lambda 可以捕获外层 lambda 已经捕获的变量：

```nessa
fn make_counter(start: i32) -> fn() -> fn() -> i32 {
    let base = start
    || {
        let step = base
        || step + base
    }
}
```

## 注意事项

- Lambda 捕获的是值的副本，不是引用。这避免了悬垂引用和数据竞争问题
- 如果需要共享可变状态，应使用显式的共享机制（如 `global` 变量）
- 捕获大型数据结构时注意性能——整个值会被复制
