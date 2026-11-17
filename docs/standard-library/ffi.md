# `std.ffi` — 外部函数接口标准库参考

`std.ffi` 提供 Nessa 语言标准 C ABI 动态库加载、符号绑定、指针与切片包装以及受检错误处理能力。此模块不在 `std.prelude` 中预导入，按需通过 `use std.ffi` 与 `use std.ffi.types.*` 引入。

语言层权限模型（`package.toml` 中的 `[ffi]`）与内存安全保证详见 [`docs/ffi.md`](../ffi.md)。

## 导出概览

```nessa
use std.ffi
use std.ffi.types.{
    BufMut, CStr, CallConv, Func, Ptr, PtrMut, PtrRo, SliceRo, func,
}
```

## 错误类型 `FfiError`

```nessa
pub enum FfiError {
    pub LibraryNotFound(library: String),
    pub SymbolNotFound(library: String, symbol: String),
    pub InvalidSignature(symbol: String, message: String),
    pub NullPointer(message: String),
    pub InvalidUtf8(message: String),
    pub Closed,
}
-- 实现 Eq, PartialEq, Display
```

- `LibraryNotFound(library)`：宿主注册表与系统动态链接器均未找到指定库，或库名非法。
- `SymbolNotFound(library, symbol)`：动态库已打开，但导出表中不存在指定符号。
- `InvalidSignature(symbol, message)`：函数签名不是合法的 FFI `fn(...) -> R` 类型、包含不支持的参数/返回类型、超过 12 个 C ABI 参数字，或在 `ffi.load` 块中传入了位置参数。
- `NullPointer(message)`：尝试对空指针（`Ptr.NULL` / `Ptr.zero()`）执行 `read_cstr` 等解引用读取。
- `InvalidUtf8(message)`：`read_cstr` 在 `max_len` 字节内未找到结尾 `\0`，或字节序列不是合法 UTF-8。
- `Closed`：`Library` 句柄已关闭后继续调用 `bind` 或 `attach_finalizer`。

## 调用约定与描述符

### `CallConv`

```nessa
pub enum CallConv {
    pub c,
    pub system,
}
-- 实现 Eq, PartialEq, Display（分别格式化为 "c" 与 "system"）
```

### `Func` 与 `func`

```nessa
pub struct Func {
    pub sig: Type,
    pub symbol: ?String = null,
    pub callconv: CallConv = CallConv.c,

    pub fn new(
        sig: Type,
        .symbol: ?String = null,
        .callconv: CallConv = CallConv.c,
    ) -> Func
}

pub fn func(
    sig: Type,
    .symbol: ?String = null,
    .callconv: CallConv = CallConv.c,
) -> Func
```

用于在 `ffi.load` / `ffi.must_load` 扩展块中显式指定底层 C 导出名（`symbol`）或调用约定（`callconv`）。若使用默认 `CallConv.c` 且 Nessa 字段名与 C 导出名相同，可直接写函数类型表达式（如 `add: fn(i32, i32) -> i32`）。

## 指针、C 字符串与切片类型

### `Ptr` 与 `read_cstr`

```nessa
pub struct Ptr {
    pub addr: usize,
}
-- 实现 Eq, PartialEq, Ord, PartialOrd, Hash, Display（格式化为 Ptr(0x...)）

impl Ptr {
    pub const NULL: Ptr = Ptr { addr: 0 }
    pub fn zero() -> Ptr
    pub fn from_addr(addr: usize) -> Ptr
    pub fn is_null(self) -> bool
    pub fn offset(self, delta: isize) -> Ptr
    pub fn read_cstr(self, .max_len: usize = 65536) -> !FfiError String
}

pub fn read_cstr(ptr: Ptr, .max_len: usize = 65536) -> !FfiError String
```

- `Ptr.NULL` / `Ptr.zero()`：地址为 `0` 的空指针常量与构造器。
- `Ptr.offset(delta)`：按有符号字节偏移量计算新地址（等价于 C `((uintptr_t)p) + delta`）。
- `Ptr.read_cstr(max_len = 65536)`：从非空地址扫描最多 `max_len` 字节读取 `\0` 结尾的 UTF-8 字符串；空指针返回 `FfiError.NullPointer`，缺少 `\0` 或非 UTF-8 字节返回 `FfiError.InvalidUtf8`。

### 名义 ABI 标记类型（`newtype`）

```nessa
pub newtype CStr = String
-- 实现 Eq, PartialEq, Display
impl CStr {
    pub fn as_str(self) -> String
}

pub newtype PtrRo = Any
pub newtype PtrMut = Bytes
pub newtype SliceRo = Any
pub newtype BufMut = Bytes
```

- `CStr("text")`：传给 C `const char*` 参数，或接收 C 返回的 `const char*`（通过 `.as_str()` 或 `.as(String)` 取出 `String`）。
- `PtrRo(data)` / `SliceRo(data)`：包装 `String` 或 `Bytes`，分别向 C 传递 `const uint8_t*` 或 `(const uint8_t* ptr, size_t len)`。
- `PtrMut(buf)` / `BufMut(buf)`：包装 `Bytes`，在 COW 独占分离后向 C 传递 `uint8_t*` 或 `(uint8_t* ptr, size_t len)`。

## 动态库句柄与加载器

### `ffi.load` 与 `ffi.must_load`

```nessa
pub struct Loader { ... }
pub struct MustLoader { ... }

pub fn load(name: String) -> Loader
pub fn must_load(name: String) -> MustLoader

impl Loader {
    pub fn apply(self, ...items: List, ...bindings: Map) -> !FfiError Library
}

impl MustLoader {
    pub fn apply(self, ...items: List, ...bindings: Map) -> Library
}
```

- 通过扩展调用语法 `ffi.load("lib_name") { sym_a: fn(...) -> ..., sym_b: ffi.func(...) }!` 打开动态库并一次性完成全部符号绑定；若任一符号绑定失败，加载器自动关闭已打开的库句柄并返回对应 `FfiError`。
- `ffi.must_load` 在内部调用同一加载流程，遇到 `FfiError` 时立即 `panic(err.to_string())`。

### `Library`

```nessa
pub struct Library {
    pub name: String,
    ...
}
-- 实现 Display

impl Library {
    pub fn open(name: String) -> !FfiError Library
    pub fn is_open(self) -> bool
    pub fn close(self) -> bool
    pub fn bind(
        self,
        symbol: String,
        sig: Type,
        .callconv: CallConv = CallConv.c,
    ) -> !FfiError Any
    pub fn attach_finalizer(
        self,
        owner: Any,
        symbol: String,
        raw_handle: i64,
    ) -> !FfiError Unit
    pub fn has_symbol(self, name: String) -> bool
    pub fn get_symbol(self, name: String) -> Any
}
```

- `Library.open(name)`：按命令式流程打开宿主注册库或系统动态库。
- `lib.bind(symbol, sig, callconv = CallConv.c)`：按需动态绑定单个符号并返回可调用闭包（可通过 `.as(fn(...) -> ...)` 赋予静态函数类型）。
- `lib.attach_finalizer(owner, symbol, raw_handle)`：将库中签名为 `extern "C" fn(int64_t)` 的释放函数注册到堆对象 `owner` 的 GC 弱终结器上（内部对同一 `symbol` 的析构句柄自动缓存复用）。
- `lib.close()`：幂等关闭底层动态库资源句柄；首次关闭返回 `true`，重复关闭返回 `false`。

## 完整示例：零拷贝缓冲区交互与 GC 终结器

```nessa
use std.bytes.Bytes
use std.ffi
use std.ffi.types.{BufMut, SliceRo}

private struct HandleGuard {
    raw_id: i64,
}

pub struct Matcher {
    raw_id: i64,
    guard: HandleGuard,
}

private const native_re = ffi.must_load("nessa_regex_ffi") {
    compile: ffi.func(
        fn(SliceRo, u32, usize, usize, BufMut) -> i64,
        symbol = "nessa_regex_compile",
    ),
    is_match: ffi.func(
        fn(i64, SliceRo, usize, u32) -> i32,
        symbol = "nessa_regex_is_match",
    ),
}

pub fn compile_matcher(pattern: String) -> ?Matcher {
    let err_buf = Bytes.zeroed(512)
    let flags: u32 = 8
    let raw_id: i64 = native_re.compile(
        SliceRo(pattern),
        flags,
        1048576,
        1048576,
        BufMut(err_buf),
    )
    if raw_id <= 0 {
        return null
    }
    let guard = HandleGuard { raw_id: raw_id }
    native_re.attach_finalizer(guard, "nessa_regex_free", raw_id)! {
        err => panic(err.to_string())
    }
    Matcher {
        raw_id: raw_id,
        guard: guard,
    }
}

pub fn check_match(m: Matcher, text: String) -> bool {
    let utf8_mode: u32 = 0
    let rc: i32 = native_re.is_match(m.raw_id, SliceRo(text), 0, utf8_mode)
    rc == 1
}
```
