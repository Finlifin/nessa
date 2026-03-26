# 类型系统 (Type System)

Nessa 采用简单的渐进类型系统，不支持泛型，但提供了丰富的类型组合和多态机制。

## 基础类型与组合

Nessa 支持三种核心的限定类型 (Qualified Types)：Option, Error, 和 Effect。

### Option Type: `?T`

表示一个可能为空的值。类似于 Rust 的 `Option<T>` 或 Swift 的 `Optional`.

*   **自动提升**: 类型 `T` 的值可以自动转换为 `?T`。
*   **空值**: 使用 `null` 表示空。
*   **判断与解包**:
    *   `val match { some? => ..., null => ... }`
    *   `if val == null`
*   **传播规则**: 支持 `?.` 操作符进行链式调用和传播。
    *   规则: `e: A, e.f : B, o: ?A => o?.f : ?B`

```nessa
let alright: i32 = 23
let ok: ?i32 = alright -- 自动提升

if ok == null:
    println("ok is null")
else:
    println("ok is {}", ok) -- 自动 unwrapping 或需要在打印时处理

-- 传播
struct Student(name: String, age: i32)
let student: ?Student = Student("julia", 18)
-- student?.age 类型为 ?i32
```

### Error Qualified Type: `!Es T`

表示计算可能返回 `T` 类型的结果，或者抛出错误集 `Es` 中的某种错误。

*   **语法**: `!Es T` 或 `![E1, E2] T`。单错误语法糖: `!E T` 等价于 `![E] T`。
*   **错误集 (Error Set)**: `Es` 是一个类型集合 (Set of Types)。
*   **自动扁平化**: `!A !B T` 等价于 `!Merge(A, B) T`。
*   **子类型关系**:
    *   如果 `Subset(A, B)`，则 `!A T <: !B T`。（例如：抛出 `A` 错误的函数可以被视为抛出 `A` 或 `B` 错误的函数）。
    *   `![] T` 等价于 `T`。
*   **构造与传播**:
    *   `error expr`: 构造一个错误值。
    *   `expr!`: 将表达式的错误向上传播（Control Flow 中详述）。

```nessa
-- 自动转换与子类型
let result: ![String] i32 = 23
let result2: ![String] i32 = error "an error occurred"
let result3: ![String, NetworkErr] i32 = result2 -- 子类型赋值 ok

-- 扁平化
-- !NetworkErr !ParseErr i32 == ![NetworkErr, ParseErr] i32

fn fetch_user(id: i32) -> !NetworkErr !ParseErr User:
    if id < 0: return error "Invalid ID"         --String error inferred if signature allows or Any
    -- ...
```

### Effect Qualified Type: `#Es T`

类似于 Error Qualified Type，但用于代数效应 (Algebraic Effects)。

*   **语法**: `#Es T` 或 `#[E1, E2] T`。单效用语法糖: `#E T`。
*   **效应集 (Effect Set)**: `Es` 是效应的集合。
*   **规则**: 与 Error Qualified Type 完全一致（扁平化、子类型、空集等价于 T）。
*   **传播**: 同样使用 Propagation/Elimination 机制（详见 Control Flow）。

### Tuple

使用 `(T1, T2, ...)` 表示元组。

```nessa
let t: (i32, Float64) = (1, 2.0)
```

## 结构体 (Struct) & 枚举 (Enum)

*   **Struct**:
    ```nessa
    struct Post:
        title: String
        content: String
    
    -- Tuple Struct / Case Class
    struct Point(x: Float64, y: Float64)
    ```
*   **Enum (ADT)**:
    ```nessa
    enum Ast:
        int(value: i32)
        float(value: Float64)
        string(value: String)
    ```

## Typeclass / Trait

Traits 是 Nessa 多态的核心机制，定义了一组行为约束。

### 定义与实现

*   `def`: 定义必须由实现者提供的接口。
*   `derive`: (可选) 定义可以自动派生的接口。
*   默认实现: 可以直接在 Trait 中提供默认方法体。

```nessa
trait Animal:
    -- 必须实现的方法
    def fn speak(self) -> String
    
    -- 带有默认实现的方法
    fn introduce(self):
        println("I am an animal: {}", self.speak())
```

### 线性可见性 (Linear Visibility)

由于 Nessa 是按行执行的，Trait 的实现 (`impl`) 只有在执行到该行之后才对后续代码可见。

```nessa
struct Dog:
    data: String = "Woof!"

let dog = Dog()
-- dog.speak() -- Error: unresolved symbol 'speak' (Trait explicit implementation not yet visible)

impl Animal for Dog:
    fn speak(self) -> String:
        self.data

dog.speak() -- "Woof!"
```

### 孤儿原则 (Orphan Rules)

`impl Trait for Type` 必须遵守孤儿原则：`Trait` 或 `Type` 必须至少有一个是在当前模块中定义的。这防止了不同模块对同一类型实现同一 Trait 的冲突。

## 扩展 (Extensions)

如果需要绕过孤儿原则，或者临时为一个类型增加方法，可以使用 `extend` 块。

*   `extend Type`: 为类型临时增加方法。
*   `extend Trait for Type`: 临时实现 Trait。
*   **作用域受限**: 扩展只在当前的词法作用域内有效。

```nessa
do {
    extend Dog:
        fn fetch(self, item: String) -> String:
            "Fetching " + item

    println(dog.fetch("ball")) -- "Fetching ball"
}
-- dog.fetch("stick") -- Error: unresolved symbol 'fetch'
```

## First-class Types

类型本身是一等公民 (`Type` 类型)。

```nessa
fn parse(ty: Type, s: String) -> Any:
    match ty:
        i32 => s.to_int()
        Float64 => s.to_float()
```

## `Any` 与容器

由于没有泛型，容器（如 `List`, `Map`）通常存储 `Any` 类型。

```nessa
let list = [1, 2, 3] -- List holding Any
assert(typeof(list(1)) == i32)
```
