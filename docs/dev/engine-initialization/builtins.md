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
| 100–105 | List | `__list_init`, `__list_len`, `__list_get`, `__list_set`, `__list_push`, `__list_pop` |
| 110–115 | Map | `__map_init`, `__map_len`, `__map_get`, `__map_set`, `__map_remove`, `__map_contains` |
| 126 | Map snapshot | `__map_keys`: one Map argument, returns a rooted List of String key references; builtin ABI8 |

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
allowed when the declaration is a trusted source node, or when a standalone
resolver explicitly enables `ResolveOptions::builtin_access`. Driver uses the
trusted-node boundary for std and leaves user nodes unprivileged.

## Current source compilation (2026-10-07)

Driver parses the eight embedded std source files with distinct source identities,
merges their AST arenas, and grants builtin access only to those trusted nodes.
Root type and function name injection is disabled in this path; real std modules
and the implicit `use std.prelude.*` provide the names. Explicit imports and local
bindings can shadow the implicit prelude.

Typed native constants get their signatures from source annotations. They lower
to adapter closures whose entry checks arguments and whose return checks the
declared type. Their Function metadata is available to runtime reflection and
exact signature checks. Raw native values without a signature are rejected;
standalone resolver direct native calls retain the dynamic registration boundary.

Top-level values now use shared global slots with type and mutability schemas.
File, module, struct and enum initializers execute declarations and statements in
source order, then their parameterless Unit-returning `__init__` hook. A generated
bootstrap orders loaded scopes before the root main; native registration remains
separate from this source initialization. Closures read globals through the same
slots rather than capturing their previous values. Failed initialization prevents
main from running.

Self-contained NSBC artifacts now preserve type, function, trait, global and
native-import metadata and run without source files. Builtin ABI revision 5 adds
ID 121 (`__scalar_eq`) for the explicit scalar Eq/PartialEq implementations in
`std.traits`. It shares VM scalar comparison rules and rejects non-scalar values.
Revision 4 artifacts are accepted only when all imports have IDs below 121.
Revision 4 adds
the compiler-generated Display adapter native at ID 120 (`__derived_display`).
It preserves the legacy struct field formatter, authenticates its ordinary typed
wrapper and trusted Display contract, and checks receiver layout before reading.
Revision 3 keeps String-keyed Map capabilities at IDs 110–115 and is accepted
when all imports have IDs below 120. Revision 2 artifacts retain their
List contracts when all imports have IDs below 110; revision 1 is supported only
without the former placeholder List IDs (all imports below 100). Every imported
ID/name pair is checked, and List/Map imports require their exact paired layouts.

Collection wrappers live in std source: List stores Any elements and Map accepts
String keys with Any values. Native Map lookup/removal return null for a missing
key; contains distinguishes absence from an explicitly stored null. Generic
collections, language Hash/Eq key dispatch and Iterator remain incomplete.
General package discovery, core/alloc, Newtype/Extend initialization and full
function variance also remain outside the implemented paths.

String `.concat()` / `.len()` are ordinary trusted std methods over the existing
`str_concat` / `str_len` native IDs. `++` statically binds a checked source concat
method; List concat is a source implementation that returns a fresh wrapper and
shares element references. Neither operation adds native IDs or changes ABI3.


Builtin ABI revision 6 adds ID 122 (`__scalar_cmp`). Its rooted, arity-checked
scalar comparison returns an optional integer sign. Ordinary std source maps
that sign to nominal Ordering values; no new intrinsic prefix or archived type
role is introduced. NaN is unordered. Revision 5 artifacts remain accepted only
when all imports have IDs below 122; earlier compatibility limits remain intact.
Old integer comparison artifacts retain their original signatures and code.


Builtin ABI revision 7 adds IDs 123 (`__display_quote`), 124
(`__display_enter_tuple`) and 125 (`__display_exit_tuple`) for derived Enum/Tuple
Display. Tuple traversal helpers require the authenticated generated function;
frame-owned state preserves depth, active-path roots and balanced traversal across
effects and continuation cloning. Revision 6 artifacts are accepted only when all
imports have IDs below 123. Earlier limits and legacy struct ID 120 remain unchanged.

## Native roots during the Error and moving-GC implementation

The current native API promises that a copied value returned by `arg()` remains
valid until the builtin exits, including after its argument register is replaced.
The moving-GC implementation must preserve this contract with precise native
pinning for copies the host cannot update. Managed VM roots use updateable slots.
Pinning every VM root would prevent the required moving-GC behavior.

The merged implementation provides `BuiltinRoot`, `arg_rooted()` and
`load_rooted()` for native code that can reload a movable value. Handles belong
to one builtin invocation; stale or foreign handles must fail. A loaded snapshot
must be reloaded after allocation or collection. Native collection accessors also
retain their copied results until builtin exit. These are host API changes, with
no new source builtin ID or bytecode instruction.

Independent validation passed with actual movement, preserved arguments after
register replacement, rejected expired and foreign handles, and cleanup on
builtin error and unwinding. This validates the current interpreter API; a general
external HostRoot or FFI root interface remains future work.
