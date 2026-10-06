# Builtins

Builtins are native engine capabilities exposed to nessa source through the
`'builtin` view. The compiler and runtime share one catalog of names and
stable IDs; only privileged packages (`std`, `core`, `alloc`) may take the
view.

## Categories

| Kind | Binding example | Engine side |
|------|-----------------|-------------|
| Function | `pub const print: fn(Any) -> Unit = .print'builtin` | `BuiltinFnId` + `CallBuiltin` |
| Type | `pub typealias i64 = .i64'builtin` | `TypePool` / `Intrinsic` `TypeIndex` |
| Effect | *(reserved)* | future |

Registration does **not** attach static signatures. Builtin implementations
perform dynamic checks via [`BuiltinCtx`]. Typed APIs live in `std` wrappers.

## Stable function IDs

IDs are sparse by domain (see `runtime::builtin::ids`):

| Range | Domain | Examples |
|-------|--------|----------|
| 0– | I/O | `print` (0), `println` (1) |
| 2– | Convert | `type_of`, `to_i64`, `to_f64`, `to_string` |
| 10– | Math | `abs` … `log` |
| 30– | String | `str_len`, `str_concat` |
| 50– | Process | `exit`, `panic` |
| 100– | List | `__list_init` |

## Registration timing

1. `TypePool::with_intrinsics()` creates primitive types.
2. `Engine::new` builds the VM, then `builtin_fns::register_all`:
   - registers fn pointers into `Vm.builtins`
   - fills the global catalog with intrinsic type names
3. Resolution looks up names via `runtime::catalog_lookup` (functions are
   available even before the engine runs).

## Calling convention

Same as ordinary `Call`: arguments in `r0..rN`, return in `r0`, no new frame.
Opcode `CallBuiltin` (`0x8D`) encodes `(arg_count << 14) | (id & 0x3FFF)`.

## `BuiltinCtx`

Implementations receive `&mut BuiltinCtx` with:

- `arg` / `arg_i64` / `arg_f64` / … (typed getters → `VmError::TypeError`)
- `require_arity`
- `set_return` / `return_unit` / …
- `alloc_string`, `type_pool`
- `exit` / `panic` outcomes

## Impl restriction

`impl` / `extend` on a type that resolves to `TypeKind::Intrinsic` is only
allowed when `ResolveOptions::builtin_access` is true (privileged packages).
