# C ABI Specification & Binding Guide (`engine/ffi`)

This document specifies how `[ffi]`-enabled Nessa packages declare permissions, load host or shared libraries, describe C signatures, and exchange zero-copy `String` and `Bytes` buffers safely.

## 1. `package.toml` Configuration

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

- `allow` (`bool`, required to enable FFI): Permits `.__ffi_dlopen'builtin`, `.__ffi_dlsym'builtin`, `.__ffi_call'builtin`, `.__ffi_call4'builtin`, and `.__ffi_call6'builtin` within this package's AST nodes.
- `libraries` (`Array<String>`, optional): When non-empty, restricts `__ffi_dlopen` in this package to string literals matching the listed library names.

## 2. Builtin Entry Points

| Builtin | Signature | Description |
| --- | --- | --- |
| `__ffi_dlopen` | `fn(String) -> i64` | Resolves a host-registered C-ABI library first (`HostLibraryRegistry`), falling back to `libloading::Library::new` for OS shared libraries (`.so` / `.dylib` / `.dll`). Returns a `TAG_FFI_LIBRARY` handle. |
| `__ffi_dlsym` | `fn(i64, String, String) -> i64` | Binds `symbol` from `lib_handle` with a validated C signature descriptor, returning a `TAG_FFI_SYMBOL` handle. Passing `(0, "close", "i64->bool")` binds the idempotent resource closer. |
| `__ffi_call` | `fn(i64, List) -> Any` | Invokes a bound C symbol with an arbitrary argument `List` (up to 12 C ABI words). |
| `__ffi_call4` | `fn(i64, ...1..4 args) -> Any` | Zero-allocation fast path for 1 to 4 Nessa arguments. |
| `__ffi_call6` | `fn(i64, ...1..6 args) -> Any` | Zero-allocation fast path for 1 to 6 Nessa arguments. |

## 3. Signature Descriptor Grammar

Descriptors have the form `<params>-><return>` (whitespace is ignored):

- **Scalar parameters**: `i64`, `u64`, `usize`, `isize`, `i32`, `u32`, `bool` (each 1 C word, range-checked before entering the native frame).
- **Read-only pointer/slice parameters** (accept Nessa `String` or `Bytes`):
  - `ptr_ro` $\rightarrow$ `const uint8_t*` (1 C word)
  - `slice_ro` $\rightarrow$ `(const uint8_t* ptr, size_t len)` (2 C words, auto-expanded from 1 Nessa argument)
- **Mutable `Bytes` buffer parameters** (accept Nessa `Bytes`, automatically detaching COW aliases before borrowing):
  - `ptr_mut` $\rightarrow$ `uint8_t*` (1 C word)
  - `buf_mut` $\rightarrow$ `(uint8_t* ptr, size_t len)` (2 C words, auto-expanded from 1 Nessa `Bytes` argument)
- **Return types**: `void` (returns `Unit`), `i64`, `u64`, `usize`, `isize`, `i32`, `u32`, `bool`.
- **GC weak cleanup hook**: `on_gc:any,i64->void` attaches an `extern "C" fn(int64_t)` finalizer to `(heap_guard_obj, raw_handle_id)`.

## 4. Memory & Aliasing Guarantees

1. **No-Safepoint Borrow Window**: Raw pointers extracted via `ptr_ro`/`slice_ro`/`ptr_mut`/`buf_mut` are valid strictly until the synchronous C function returns; C code must never stash borrowed pointers past return.
2. **COW Detachment & Aliasing Rejection**: All `ptr_mut`/`buf_mut` `Bytes` arguments run `ensure_unique` before entering the native frame. Passing overlapping mutable `Bytes` buffers (or the same `Bytes` slice as both read-only and mutable) in a single call fails cleanly with `VmError::InvalidArgument`.
3. **Continuation Safety**: While any native frame is active (`native_frame_depth > 0`), `EffectCallDyn`, `ResetClosure`, `ResumeContinuation`, and `Continuation.clone` return `VmError::EffectAcrossNativeFrame`.
