# 类型推导 (Type Inference)

nessa 尽可能通过类型推导来获取足够多的类型信息，减少显式类型标注的负担。

## 基本推导

```nessa
let x = 42                    -- 推导为整数字面量类型，根据上下文确定具体类型
let s = "hello"               -- String
let list = [1, 2, 3]          -- List
let pair = (true, 3.14)       -- (bool, f64)
```

## 函数返回值推导

单表达式函数体可以省略返回类型标注：

```nessa
fn double(x: i32) -> i32 = x * 2     -- 显式标注
fn double(x: i32) = x * 2            -- 推导返回类型为 i32
```

## 闭包参数推导

闭包的参数类型通常可以从上下文推导：

```nessa
let numbers = [1, 2, 3, 4, 5]
-- |x| 的类型从上下文推导为 i32
let doubled = numbers.map(|x| x * 2)
```

## 类型标注与 as cast

当推导信息不足时，可以通过类型标注或 `as` cast 提供提示：

```nessa
-- 类型标注
let x: f64 = 0

-- as cast 用于类型提升
let result = some_fn().as(!ParseErr !HttpErr String)
```

nessa 中有较多依赖类型推导的类型提升风格，熟练使用 `as` cast 很重要。

## 推导的边界

类型推导不是万能的。以下场景通常需要显式标注：

- 函数参数类型（始终需要标注）
- 存在歧义的字面量（如整数字面量可能是 `i32` 或 `u64`）
- 复杂的泛型实例化
- `Any` 类型的向下转换
