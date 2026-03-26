# 可见性 (Visibility)

nessa 采用三级可见性模型，适用于所有 item（函数、类型、常量、模块等）以及 `struct` 字段和 `enum` variant。

## 三级可见性

| 修饰符 | 可见范围 | 说明 |
|--------|----------|------|
| `pub` | 完全公开 | 任何包、任何模块均可访问 |
| （默认） | 包内可见 | 同一个包内的所有模块均可访问，外部包不可见 |
| `private` | 仅当前作用域 | 仅在定义所在的作用域内可见 |

## 默认可见性

不加任何修饰符时，item 默认是**包内可见**的。这是一个务实的默认值——同一个包内的代码通常需要自由协作，而包边界才是真正的封装边界。

```nessa
-- 包内可见（默认）
fn internal_helper() -> i32 = 42

-- 完全公开
pub fn public_api() -> String = "hello"

-- 仅当前作用域可见
private fn secret() -> bool = true
```

## 适用范围

可见性修饰符适用于所有可定义的 item：

```nessa
pub struct User {
    pub name: String,          -- 公开字段
    email: String,             -- 包内可见字段
    private password_hash: String,  -- 仅当前作用域可见
}

pub enum Result {
    pub ok(value: T),          -- 公开 variant
    pub err(error: E),
}

pub mod api {
    pub fn handle_request() { ... }
    private fn validate() { ... }
}
```

## 可见性与重导出

`pub use` 可以改变符号的有效可见性——将包内可见的符号通过重导出提升为公开：

```nessa
-- internal.ns 中
fn helper() -> i32 = 42       -- 包内可见

-- lib.ns 中
pub use internal.helper        -- 现在 helper 对外部包也可见了
```

## 与关联作用域的交互

可见性规则同样适用于类型关联作用域中定义的符号：

```nessa
struct Connection {
    pub fn new(addr: String) -> Connection { ... }
    fn reset() { ... }                -- 包内可见
    private fn raw_send(data: Bytes) { ... }  -- 仅 Connection 作用域内可见
}
```
