# FFI（外部函数接口 / Foreign Function Interface）

Nessa 提供受能力门控（Capability-Gated）、内存安全且符合语言习惯的外部函数接口 `std.ffi`，用于加载宿主注册库或操作系统动态库（`.so` / `.dylib` / `.dll`）并按 C / 系统调用约定调用外部函数。

API 逐项说明参见 [`std.ffi` 标准库参考](standard-library/ffi.md)；引擎内部安全机制参见 [`docs/ffi/intro.md`](ffi/intro.md)。

## 1. 声明式加载与绑定

通过 `std.ffi.load`（返回 `!FfiError Library`）或 `std.ffi.must_load`（加载或符号绑定失败时直接 `panic`，适合模块级 `const` 声明）配合扩展调用块声明所需符号与函数签名：

```nessa
use std.ffi
use std.ffi.types.*

-- 加载 "my_math" 动态库（失败时抛出 FfiError）
fn run_math() -> !ffi.FfiError Unit {
    let c = ffi.load("my_math") {
        -- 显式指定调用约定或底层导出符号名
        add: ffi.func(fn(i32, i32) -> i32, callconv = CallConv.c),
        native_abs: ffi.func(fn(i32) -> i32, symbol = "abs"),

        -- 直接使用函数类型（默认 CallConv.c，符号名与字段名一致）
        sub: fn(i32, i32) -> i32,
        mul: fn(i32, i32) -> i32,

        -- 浮点数绑定
        sqrt: fn(f64) -> f64,
        sin: fn(f64) -> f64,
    }!
    defer { let _ = c.close() }

    println(c.add(1, 2))
    println(c.native_abs(-42))
    println(c.sin(1.57))
}
```

在受 `[ffi]` 授权的库包（如 `nessa/regex`）中，可使用 `ffi.must_load` 在模块级 `const` 一次性完成绑定：

```nessa
use std.bytes.Bytes
use std.ffi
use std.ffi.types.{BufMut, SliceRo}

private const regex_lib = ffi.must_load("nessa_regex_ffi") {
    compile: ffi.func(fn(SliceRo, u32, usize, usize, BufMut) -> i64, symbol = "nessa_regex_compile"),
    is_match: ffi.func(fn(i64, SliceRo, usize, u32) -> i32, symbol = "nessa_regex_is_match"),
    free: ffi.func(fn(i64) -> Unit, symbol = "nessa_regex_free"),
}
```

## 2. 包清单能力门控（`package.toml` `[ffi]`）

为保证默认沙箱与字节码隔离安全，普通脚本与未声明权限的包不能直接打开动态库：

1. **编译期 `[ffi]` 门控**：只有 `package.toml` 中显式声明 `[ffi] allow = true` 的包才允许调用 `std.ffi` 的库加载入口（`ffi.load`、`ffi.must_load`、`ffi.Library.open`）及底层 FFI 内建视图。
2. **动态库白名单（`libraries`）**：当 `[ffi]` 下配置非空 `libraries = ["..."]` 时，编译器静态校验该包只能加载白名单内列出的字面量库名；尝试加载未列出的库名或传入非常量字符串会在编译期直接拒绝。
3. **依赖边界隔离**：消费方包依赖已封装好的 FFI 库包（例如在 `[dependencies]` 中依赖 `"nessa/regex" = "0.1.0"`）时，消费方自身**无需**开启 `[ffi]`，也不会获得加载任意原生库的权限。
4. **VM 运行时二次校验**：嵌入宿主可通过 VM 配置进一步限制允许打开的原生库集合，运行时在 `dlopen` 前再次核验。

```toml
[package]
domain = "nessa"
name = "regex"
version = "0.1.0"
type = "lib"

[ffi]
allow = true
libraries = ["nessa_regex_ffi"]
```

## 3. FFI 类型与 C ABI 映射

`std.ffi.types` 导出了与 C ABI 精确对应的宽度类型和名义包装类型（`newtype` / `struct`）。除 `SliceRo` 与 `BufMut` 自动展开为 `(ptr, len)` 两个机器字外，其余参数均占 1 个 C ABI 参数槽；单次调用最多支持 12 个 C ABI 参数字。

### 3.1 标量与空返回类型

| Nessa 类型 | C ABI 类型 | 作为参数 | 作为返回值 |
| --- | --- | --- | --- |
| `Unit` | `void` | 否（无参写 `fn() -> R`） | 是 |
| `bool` | `_Bool` / `bool` | 是 | 是 |
| `i8`, `i16`, `i32`, `i64`, `isize` | `int8_t` .. `int64_t`, `ssize_t` / `intptr_t` | 是 | 是 |
| `u8`, `u16`, `u32`, `u64`, `usize` | `uint8_t` .. `uint64_t`, `size_t` / `uintptr_t` | 是 | 是 |
| `f32`, `f64` | `float`, `double` | 是 | 是 |

### 3.2 指针、C 字符串与零拷贝切片类型

| Nessa 类型 | 底层定义 | C ABI 映射 | 参数/返回支持与语义 |
| --- | --- | --- | --- |
| `Ptr` | `pub struct Ptr { pub addr: usize }` | `void*` / `const void*` | **参数 & 返回值**。不透明外部地址包装；支持 `Ptr.NULL` / `Ptr.zero()`、`is_null()`、`offset(delta)` 与受检 `read_cstr(max_len = ...)`。 |
| `CStr` | `pub newtype CStr = String` | `const char*` | **参数 & 返回值**。作为参数传入时自动附加结尾 `\0`（若内部含嵌有 `\0` 字节则运行时报错拒绝）；作为返回值时按 UTF-8 读取 NUL 结尾字符串并包装为 `CStr`。 |
| `PtrRo` | `pub newtype PtrRo = Any` | `const uint8_t*` | **仅参数**（1 个 C 字）。接受 `String` 或 `Bytes`，在无 GC 临界区内借用只读首字节指针。 |
| `SliceRo` | `pub newtype SliceRo = Any` | `(const uint8_t* ptr, size_t len)` | **仅参数**（2 个 C 字）。接受 `String` 或 `Bytes`，自动展开为只读首地址与字节长度。 |
| `PtrMut` | `pub newtype PtrMut = Bytes` | `uint8_t*` | **仅参数**（1 个 C 字）。仅接受 `Bytes`，进入原生帧前自动完成写时复制（COW）独占分离。 |
| `BufMut` | `pub newtype BufMut = Bytes` | `(uint8_t* ptr, size_t len)` | **仅参数**（2 个 C 字）。仅接受 `Bytes`，自动完成 COW 独占分离并展开为可写首地址与字节容量。 |

## 4. 调用约定（`CallConv`）

`CallConv` 枚举（非 Error 用途枚举，变体采用 `snake_case`）用于指定外部函数的平台调用约定：

- `CallConv.c`：平台默认 C 调用约定（System V AMD64 / AAPCS64 / Windows x64 C ABI，x86 下为 `cdecl`）。
- `CallConv.system`：操作系统 API 调用约定（32 位 Windows 下为 `stdcall`，其余主流 64 位平台与 `CallConv.c` 一致）。

## 5. 内存安全、别名检查与延续屏障

1. **零 GC 分配临界区（Pinned Borrow Window）**：
   `PtrRo`、`SliceRo`、`PtrMut`、`BufMut` 借用的裸指针仅在当前同步 C 调用栈帧活跃期间有效，期间运行时锁定原生帧计数且绝不触发对象搬移。C 代码严禁将借用指针缓存到调用返回之后。
2. **写时复制分离与可变别名拒绝**：
   所有 `PtrMut` / `BufMut` 参数在进入原生调用前自动解除 `Bytes` 切片共享；若同一次调用传入两个重叠的可写 `Bytes` 区间，或同一段内存同时作为只读与可写切片传入，运行时立即以类型/参数错误拒绝调用，杜绝可变别名数据竞争。
3. **跨原生帧效应与延续屏障（`EffectAcrossNativeFrame`）**：
   当调用栈中存在活跃的原生 C 栈帧时，捕获、克隆或恢复定界延续（`Continuation`）以及派发未处理的代数效应均被明确拒绝，防止跨越非托管 C 栈帧挂起或重入。
4. **GC 弱终结器（`Library.attach_finalizer`）**：
   管理外部整数句柄（如自动机或编解码上下文）的库可将 `extern "C" fn(int64_t)` 析构符号绑定到一个 Nessa 私有守卫结构体对象（例如 `private struct RegexGuard { raw_id: i64 }`）。当守卫对象不可达并被 GC 清扫或 VM 销毁时，运行时自动回调清理函数，同时保留显式 `close()` 提前确定性释放的能力。
