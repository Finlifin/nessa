# FFI (Foreign Function Interface)

Nessa 提供了简洁的 FFI 机制来加载和调用动态库中的函数。

## 加载与绑定

使用 `ffi.load` 函数加载动态库，并定义所需的函数签名。

```nessa
use ffi.types.*
-- 加载 "my_math" 库
const c = ffi.load("my_math") {
    -- 显式指定调用约定
    add: ffi.Func(fn(i32, i32) -> i32, callconv: "c"),
    
    -- 使用默认约定 (通常是 C 约定)
    sub: fn(i32, i32) -> i32,
    mul: fn(i32, i32) -> i32,
    
    -- 浮点数绑定
    sqrt: fn(f32) -> f32,
    sin: fn(f32) -> f32,
}

-- 调用
println(c.add(1, 2))
println(c.sin(1.57))
```

## FFI 类型

为了与 C 等语言的 ABI 兼容，FFI 接口通常使用特定宽度的数值类型：

*   `i8`, `i16`, `i32`, `i64`
*   `u8`, `u16`, `u32`, `u64`
*   `f32`, `f64`
*   `Ptr` (不透明指针)
*   `CStr` (C String)

*(注：具体的类型映射和高级数据结构（如 Struct 内存布局）支持将随着实现逐步完善)*
