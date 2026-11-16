# Nessa Foreign Function Interface (FFI) Architecture

Nessa provides a capability-gated, memory-safe Foreign Function Interface (`engine/ffi`) that enables Nessa libraries (such as `nessa/regex`) to bind native C-ABI code without granting compiler privileges or exposing raw pointers to untrusted user code.

## Core Design Principles

1. **Manifest Capability Gating (`[ffi]`)**:
   - Ordinary scripts and packages cannot reference `'builtin` views or load native libraries.
   - Only packages whose `package.toml` explicitly declares `[ffi] allow = true` may bind the five FFI builtins (`__ffi_dlopen`, `__ffi_dlsym`, `__ffi_call`, `__ffi_call4`, `__ffi_call6`).
   - When `libraries = ["..."]` is specified under `[ffi]`, compile-time resolution restricts `__ffi_dlopen` calls in that package to literal library names on the allowlist.
   - Granting `[ffi]` never grants access to internal runtime `'builtin` views (`std`/`core`).

2. **Monotonic Typed Resource Handles (`VmResourceTable`)**:
   - All native library and symbol descriptors live in `VmResourceTable` (`engine/interpreter/src/resources.rs`), keyed by monotonically increasing `i64` IDs (`>= 1`) that are **never reused**.
   - Every lookup checks both liveness and a 64-bit `type_tag` (`TAG_FFI_LIBRARY`, `TAG_FFI_SYMBOL`), turning forged, wrong-type, or already-closed handles into recoverable runtime errors instead of undefined behavior.
   - Dropping a `Vm` deterministically runs cleanup hooks for all remaining open resources.

3. **GC No-Scan `Bytes` & Zero-Copy Pinned Slices**:
   - Raw byte buffers use `PayloadRole::ByteBuffer` (`engine/gc`), storing up to ~256 MiB via MMTk's Immix and Large Object Space (`AllocationSemantics::Los`). The garbage collector marks/moves `ByteBuffer` objects as opaque leaves and never scans their bytes as `TaggedValue` pointers.
   - `Bytes` wrappers support $O(1)$ zero-copy sub-slicing (`slice(start, end)`) with reference-counted Copy-on-Write (`share_count`) detachment prior to any in-place mutation or mutable FFI borrow (`ptr_mut` / `buf_mut`).

4. **Non-Allocating Critical Section & Continuation Barrier**:
   - Synchronous C calls execute inside `Vm::with_native_frame`, which increments `native_frame_depth` via an RAII guard and borrows `&Vm` immutably while extracting raw pointers, guaranteeing zero GC allocations or object relocations for the duration of the C call.
   - Any attempt to capture, clone, or resume a delimited continuation or dispatch an unhandled algebraic effect across an active native frame (`native_frame_depth > 0`) is immediately rejected with `VmError::EffectAcrossNativeFrame`.

5. **GC Weak Finalization (`on_gc:any,i64->void`)**:
   - Native libraries that manage opaque integer handles (such as `nessa_regex_ffi`) can attach an `extern "C" fn(i64)` destructor to a heap-allocated Nessa guard object. When the guard becomes unreachable during GC sweep (or when the `Vm` drops), the destructor is invoked idempotently.
