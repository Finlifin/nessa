# Builtins

Builtins are native engine capabilities exposed to nessa source through the
`'builtin` view. The compiler and runtime share one catalog of names and
stable IDs; only privileged packages (`std`, `core`, `alloc`) may take the
view.

## Categories

| Kind | Binding example | Engine side |
|------|-----------------|-------------|
| Function | `const print_native: fn(Any) -> Unit = .print'builtin` | `BuiltinFnId` + `CallBuiltin` |
| Type | `pub typealias i64 = .i64'builtin` | `TypePool` / `Intrinsic` `TypeIndex` |
| Effect | *(reserved)* | future |

Registration does **not** attach static signatures. Builtin implementations
perform dynamic checks via [`BuiltinCtx`]. Typed APIs live in `std` wrappers.
For example `std.io.print`/`println` are source functions: a value whose
Display implementation is visible from `std.io` prints its `to_string` text,
other values use the native `print`/`println` representation.

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
| 127–138 | String text | `str_contains`, `str_starts_with`, `str_ends_with`, `str_trim`, `str_trim_start`, `str_trim_end`, `str_to_upper`, `str_to_lower`, `str_repeat`, `str_replace`, `str_split`, `str_chars`; builtin ABI9 |
| 139 | Iteration protocol | `__iteration_step_done`; builtin ABI9 |
| 140–142 | Dynamic equality | `__native_equal`, `__tuple_len`, `__tuple_get`; builtin ABI9 |
| 143–146 | String character index | `str_char_len`, `str_char_at`, `str_find`, `str_slice`; builtin ABI9 |
| 147–158 | Char classification | `char_is_whitespace` through `char_to_ascii_lowercase`; builtin ABI9 |
| 159–161 | Numeric bounds | `min`, `max`, `clamp`; builtin ABI9 |
| 162–163 | String numeric parse | `str_parse_i64`, `str_parse_f64`; builtin ABI9 |
| 164 | Numeric sign | `sign`; builtin ABI9 |
| 165–166 | String padding | `str_pad_start`, `str_pad_end`; builtin ABI9 |
| 167–168 | Char code point | `char_to_code`, `char_from_code`; builtin ABI9 |
| 169–172 | String search and encoding | `str_rfind`, `str_count`, `str_is_ascii`, `char_into_string`; builtin ABI9 |
| 173–184 | Floating point | `exp`, `log2`, `log10`, `trunc`, `fract`, `copysign`, `atan2`, `hypot`, `tan`, `asin`, `acos`, `atan`; builtin ABI9 |
| 185–195 | Float classification | `is_nan`, `is_infinite`, `is_finite`, `is_sign_negative`, `sinh`, `cosh`, `tanh`, `cbrt`, `exp2`, `expm1`, `log1p`; builtin ABI9 |
| 196–200 | ASCII case and char case | `str_to_ascii_uppercase`, `str_to_ascii_lowercase`, `str_split_whitespace`, `char_to_uppercase`, `char_to_lowercase`; builtin ABI9 |
| 201–212 | Integer bits | `count_ones`, `count_zeros`, `leading_zeros`, `trailing_zeros`, `leading_ones`, `trailing_ones`, `reverse_bits`, `swap_bytes`, `rotate_left`, `rotate_right`, `is_power_of_two`, `ilog2`; builtin ABI9 |
| 213–227 | Char classes and string edits | `char_is_ascii_control` through `char_is_control`, `str_trim_start_matches`, `str_trim_end_matches`, `str_trim_matches`, `str_replacen`, `str_splitn`, `str_rsplit`, `str_rsplitn`; builtin ABI9 |
| 228–242 | Float remainder and escapes | `signum`, `recip`, `mul_add`, `div_euclid`, `rem_euclid`, `char_utf8_len`, `char_utf16_len`, char and string escapes, `str_split_inclusive`, `str_find_all`; builtin ABI9 |
| 243–257 | Angles and ASCII text | `to_radians`, `to_degrees`, `next_up`, `next_down`, `midpoint`, `round_ties_even`, `char_is_digit`, `char_to_digit`, `str_split_ascii_whitespace`, `str_eq_ignore_ascii_case`, `str_trim_ascii`, `str_trim_ascii_start`, `str_trim_ascii_end`, `ilog10`, `abs_diff`; builtin ABI9 |
| 258–272 | Integer overflow | `wrapping_add`, `wrapping_sub`, `wrapping_mul`, `wrapping_neg`, `saturating_add`, `saturating_sub`, `saturating_mul`, `saturating_neg`, `checked_add`, `checked_sub`, `checked_mul`, `checked_div`, `checked_rem`, `checked_neg`, `char_from_digit`; builtin ABI9 |
| 273–287 | Integer division and hyperbolic inverses | `int_div_euclid`, `int_rem_euclid`, `unsigned_abs`, `int_midpoint`, `checked_shl`, `checked_shr`, `wrapping_shl`, `wrapping_shr`, `checked_pow`, `wrapping_pow`, `saturating_pow`, `asinh`, `acosh`, `atanh`, `is_normal`; builtin ABI9 |
| 288–302 | Bit casts and encodings | `wrapping_div`, `wrapping_rem`, `saturating_abs`, `checked_abs`, `wrapping_abs`, `int_signum`, `checked_ilog`, `cast_unsigned`, `cast_signed`, `is_subnormal`, `to_bits`, `from_bits`, `powi`, `char_encode_utf8`, `char_encode_utf16`; builtin ABI9 |
| 303–317 | Full products and UTF conversion | `checked_div_euclid`, `checked_rem_euclid`, `widening_mul`, `next_power_of_two`, `checked_next_power_of_two`, `checked_add_signed`, `checked_sub_signed`, `is_multiple_of`, `log_base`, `str_encode_utf8`, `str_encode_utf16`, `str_from_utf8`, `str_from_utf16`, `str_from_utf8_lossy`, `str_from_utf16_lossy`; builtin ABI9 |
| 318–332 | Character boundaries and f64 constants | `str_is_char_boundary`, `str_floor_char_boundary`, `str_ceil_char_boundary`, `pi`, `euler`, `tau`, `ln_2`, `ln_10`, `log2_e`, `log10_e`, `sqrt_2`, `infinity`, `neg_infinity`, `nan`, `epsilon`; builtin ABI9 |
| 333–347 | Strict arithmetic and more f64 constants | `strict_add`, `strict_sub`, `strict_mul`, `strict_div`, `strict_rem`, `strict_neg`, `strict_shl`, `strict_shr`, `strict_pow`, `min_positive`, `frac_pi_2`, `frac_pi_4`, `frac_1_sqrt_2`, `log2_10`, `log10_2`; builtin ABI9 |
| 348–362 | Saturating division and float truncation | `saturating_div`, `saturating_div_euclid`, `trunc_i64`, `trunc_u64`, `trunc_i128`, `trunc_u128`, `char_is_ascii_octdigit`, `frac_1_pi`, `frac_2_pi`, `frac_2_sqrt_pi`, `frac_pi_3`, `frac_pi_6`, `frac_pi_8`, `f64_min`, `f64_max`; builtin ABI9 |
| 363–377 | Mixed unsigned arithmetic and radix text | `checked_add_unsigned`, `checked_sub_unsigned`, `saturating_add_unsigned`, `saturating_sub_unsigned`, `wrapping_add_unsigned`, `wrapping_sub_unsigned`, `strict_add_unsigned`, `strict_sub_unsigned`, `int_to_str_radix`, `i64_from_str_radix`, `u64_from_str_radix`, `i128_from_str_radix`, `u128_from_str_radix`, `str_char_at_byte`, `str_split_at`; builtin ABI9 |
| 378–392 | Ceiling division, multiples, and endian bytes | `div_ceil`, `checked_div_ceil`, `next_multiple_of`, `checked_next_multiple_of`, `int_to_be_bytes`, `int_to_le_bytes`, `i64_from_be_bytes`, `i64_from_le_bytes`, `u64_from_be_bytes`, `u64_from_le_bytes`, `i128_from_be_bytes`, `i128_from_le_bytes`, `u128_from_be_bytes`, `u128_from_le_bytes`, `total_cmp`; builtin ABI9 |
| 393–407 | Floor division and float layout | `div_floor`, `checked_div_floor`, `is_sign_positive`, `f64_to_be_bytes`, `f64_to_le_bytes`, `f64_from_be_bytes`, `f64_from_le_bytes`, `f64_radix`, `f64_mantissa_digits`, `f64_digits`, `f64_min_exp`, `f64_max_exp`, `str_byte_at`, `str_get`, `str_split_at_checked`; builtin ABI9 |
| 408–422 | Wider integer parsing and byte search | `str_parse_u64`, `str_parse_i128`, `str_parse_u128`, `f64_min_10_exp`, `f64_max_10_exp`, `str_split_terminator`, `str_rsplit_terminator`, `int_gcd`, `int_lcm`, `checked_int_lcm`, `str_byte_find`, `str_byte_rfind`, `str_byte_count`, `char_replacement`, `char_max`; builtin ABI9 |
| 423–437 | Signed adjust, unbounded shifts, and integer bounds | `saturating_add_signed`, `saturating_sub_signed`, `wrapping_add_signed`, `wrapping_sub_signed`, `strict_add_signed`, `strict_sub_signed`, `unbounded_shl`, `unbounded_shr`, `i64_min`, `i64_max`, `u64_max`, `i128_min`, `i128_max`, `u128_max`, `char_eq_ignore_ascii_case`; builtin ABI9 |
| 438–452 | Bit positions and match indexes | `highest_one`, `lowest_one`, `isolate_highest_one`, `isolate_lowest_one`, `int_bit_width`, `i64_bits`, `i128_bits`, `isize_bits`, `u128_bits`, `usize_bits`, `char_min`, `str_find_indices`, `str_rfind_indices`, `str_byte_find_indices`, `str_byte_rfind_indices`; builtin ABI9 |
| 453–467 | Exact arithmetic and affixes | `isqrt`, `checked_isqrt`, `strict_abs`, `wrapping_div_euclid`, `wrapping_rem_euclid`, `checked_signed_diff`, `checked_div_exact`, `strict_div_exact`, `checked_shl_exact`, `strict_shl_exact`, `checked_shr_exact`, `strict_shr_exact`, `str_trim_prefix`, `str_trim_suffix`, `str_strip_circumfix`; builtin ABI9 |
| 468–482 | List windows and in-place edits | `list_windows`, `list_chunks_exact`, `list_rchunks`, `list_rchunks_exact`, `list_chunks_remainder`, `list_rchunks_remainder`, `list_split_at`, `list_rotate_left`, `list_rotate_right`, `list_repeat`, `list_truncate`, `list_fill`, `list_swap_remove`, `list_split_off`, `list_resize`; builtin ABI9 |
| 483–497 | Map edits, float class, and ASCII spans | `map_values`, `map_clear`, `map_merge`, `map_get_or_insert`, `map_insert_absent`, `golden_ratio`, `euler_gamma`, `f64_classify`, `str_is_ascii_digit`, `str_is_ascii_alphabetic`, `str_is_ascii_alphanumeric`, `str_is_ascii_lowercase`, `str_is_ascii_uppercase`, `str_is_ascii_whitespace`, `str_is_ascii_hexdigit`; builtin ABI9 |
| 498–512 | Unicode spans and list copies | `str_is_numeric`, `str_is_alphabetic`, `str_is_alphanumeric`, `str_is_lowercase`, `str_is_uppercase`, `str_is_whitespace`, `str_is_control`, `str_is_ascii_graphic`, `str_is_ascii_punctuation`, `str_is_ascii_control`, `list_copy`, `list_reverse`, `list_concat`, `list_swap`, `list_clear`; builtin ABI9 |
| 513–527 | List edits and range queries | `list_insert`, `list_remove`, `list_take`, `list_drop`, `list_first`, `list_last`, `range_len`, `range_inclusive_len`, `range_is_empty`, `range_inclusive_is_empty`, `range_contains`, `range_inclusive_contains`, `range_from_contains`, `range_to_contains`, `range_to_inclusive_contains`; builtin ABI9 |
| 528–542 | Float algebra, native bytes, and bool | `f64_to_ne_bytes`, `f64_from_ne_bytes`, `algebraic_add`, `algebraic_sub`, `algebraic_mul`, `algebraic_div`, `algebraic_rem`, `f64_nan_to`, `f64_abs_diff`, `f64_is_zero`, `bool_not`, `bool_and`, `bool_or`, `bool_xor`, `bool_then`; builtin ABI9 |
| 543–557 | String views and list join | `str_bytes`, `str_char_indices`, `str_is_empty`, `str_repeat_char`, `str_take`, `str_drop`, `str_first`, `str_last`, `str_starts_with_char`, `str_ends_with_char`, `str_contains_char`, `str_replace_char`, `list_append`, `list_join`, `list_is_empty`; builtin ABI9 |
| 558–572 | Lines, pairs, and list grouping | `str_lines`, `str_split_once`, `str_rsplit_once`, `str_strip_prefix`, `str_strip_suffix`, `map_entries`, `map_is_empty`, `list_chunks`, `list_enumerate`, `list_flatten`, `list_zip`, `str_find_char`, `str_rfind_char`, `str_count_char`, `map_copy`; builtin ABI9 |

## Registration timing

1. `TypePool::with_intrinsics()` creates primitive types.
2. `Engine::new` builds the VM, then `builtin_fns::register_all` registers fn
   pointers from `runtime::builtin::BUILTIN_FN_META` into `Vm.builtins`.
3. Resolution looks up builtin functions directly via
   `runtime::builtin::lookup_builtin_fn` and intrinsic types via `type_pool::Intrinsic`.

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

## Current source compilation & single ABI baseline

Driver parses the embedded std source files with distinct source identities,
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

Self-contained NSBC artifacts preserve type, function, trait, global and
native-import metadata and run without source files. Artifact installation enforces
exact `builtin_abi_version == runtime::BUILTIN_ABI_VERSION` (`9`), verifies every
imported `(id, name)` pair against `BUILTIN_FN_META`, and checks `CollectionRole`
layouts for all `List`/`Buffer` and `Map`/`MapBuffer` builtins. Historical
compatibility ladders for revisions `1..=8` and the legacy struct native formatter
at ID 120 (`__derived_display`) have been removed.

Collection wrappers live in std source: List stores Any elements and Map accepts
String keys with Any values. Native Map lookup/removal return null for a missing
key; contains distinguishes absence from an explicitly stored null. Generic
collections, language Hash/Eq key dispatch and general package discovery remain
outside the implemented paths.

String `.concat()` / `.len()` are ordinary trusted std methods over the existing
`str_concat` / `str_len` native IDs. `++` statically binds a checked source concat
method; List concat is a source implementation that returns a fresh wrapper and
shares element references.

ID 121 (`__scalar_eq`) backs explicit scalar `Eq`/`PartialEq` implementations in
`std.traits`, sharing VM scalar comparison rules and rejecting non-scalar values.
ID 122 (`__scalar_cmp`) returns an optional integer sign mapped by std source to
nominal `Ordering` values (`NaN` is unordered).

IDs 123 (`__display_quote`), 124 (`__display_enter_tuple`), and 125
(`__display_exit_tuple`) support compositional `@derive(Display)` across Struct,
Enum, and Tuple types. Generated Display functions carry authenticated
`display_owner` metadata; frame-owned state preserves depth, active-path roots,
and balanced traversal across effects and continuation cloning.

Builtin ABI revision 9 appends String text operations at IDs 127–138. std.string
binds them to private typed constants and exposes `String` methods: `is_empty`
(pure source), `contains`, `starts_with`, `ends_with`, `trim`, `trim_start`,
`trim_end`, `to_upper`, `to_lower`, `repeat`, `replace`, `split` and `chars`.
Trimming uses Unicode White_Space and case mapping is full Unicode mapping, so a
result may change byte length. `split` returns a fresh List of new Strings and
`chars` a List of char values. A negative repeat count and an empty split
separator are `VmError::InvalidArgument`; `repeat` and `replace` compute the result
size with checked arithmetic and reject results beyond the String object limit
(`ObjectTooLarge`) before the host builds them. Index-based operations (find,
slicing) are not provided yet because String indexing semantics are undecided.
Revision 8 artifacts are accepted only when all imports have IDs below 127, and
`str_split`/`str_chars` imports require the List and Buffer roles.

Revision 9 also appends ID 139 (`__iteration_step_done`). A `for` loop over an
`Any` input selects its iterator at runtime, so the step type is unknown when the
loop is lowered; the generated done test calls this helper. It returns whether a
concrete `IterationStep(T)` value of any item type holds the `done` tag. Nominal
enums, templates and non-enum values are `TypeError`. Revision 8 artifacts cannot
import it.

Revision 9 also appends IDs 140–142 for `==` on `Any` and tuple operands, which
resolution routes to the trusted `std.collections.dynamic_equal`. The VM cannot
call source `eq` implementations from a native operation, so these helpers only
expose native facts. `__native_equal` returns a bool when native kernels decide
(scalars across numeric kinds, Strings, payload-free enums, error envelopes, or a
scalar against a heap value) and `null` for two other heap values.
`__tuple_len` returns a tuple's element count or `null` for any other value.
`__tuple_get` reads a tuple element; out-of-range indexes are `InvalidIndex`, and
non-tuples are `TypeError`. Revision 8 artifacts cannot import them.

Revision 9 also appends IDs 143–146 for character-oriented string operations.
`str_len` stays a UTF-8 byte count. These helpers count and index Unicode scalar
values, the same unit as `str_chars`. `str_char_at` returns a char or `null`
when the index is negative or past the end. `str_find` returns the scalar index
of the first match (`0` for an empty pattern) or `null` when it is absent.
`str_slice` returns the half-open scalar range; a negative index, a reversed
range, or an index past the scalar length is `InvalidArgument`. Revision 8
artifacts cannot import them.

Revision 9 also appends IDs 147–158 for `char` classification. The general
predicates use Unicode properties: whitespace is White_Space (the same set
`trim` removes), and alphabetic, numeric, alphanumeric, uppercase and lowercase
follow the corresponding Unicode classes. The `ascii` predicates and ASCII case
mapping cover only U+0000–U+007F; `to_ascii_uppercase` and `to_ascii_lowercase`
leave every other character unchanged. A non-char argument is `TypeError`.
Revision 8 artifacts cannot import them.

Revision 9 also appends IDs 159–161, `min`, `max` and `clamp`. Arguments must be
numbers; other values are `TypeError`. Comparison uses the exact numeric kernel,
so mixed integer and float values are not rounded first. The result is one of
the operands, keeping that operand's numeric kind. Equal values and unordered
pairs (NaN) keep the left operand: `min(1, 1.0)` is an i64 and `min(1.0, 1)` is
an f64. `clamp` uses inclusive bounds and returns `InvalidArgument` when the
lower bound is greater than the upper bound. Revision 8 artifacts cannot import
them.

Revision 9 also appends IDs 162–163, `str_parse_i64` and `str_parse_f64`. The
whole string must match; surrounding whitespace is malformed and returns null.
`str_parse_i64` accepts an optional sign plus decimal digits, or a `0b`, `0o`,
or `0x` magnitude (prefix case is ignored). `str_parse_f64` accepts an optional
sign, a decimal integer, or a dotted form that has digits on both sides of `.`,
with an optional exponent. A leading or trailing dot, `inf`, and `NaN` are
null. A well-formed integer outside `i64`, or a float that is not finite, is
`NumericOverflow`. A non-string argument is `TypeError`. Revision 8 artifacts
cannot import them.

Revision 9 also appends ID 164, `sign`. The argument must be a number; other
values are `TypeError`. The result is `-1`, `0`, or `1` by the exact numeric
order against zero, including mixed integer and float values. NaN is unordered
and returns null. Negative zero compares equal to zero, so its sign is `0`.
Revision 8 artifacts cannot import it.

Revision 9 also appends IDs 165–166, `str_pad_start` and `str_pad_end`. `width`
counts Unicode scalars. A string that is already at least that long is returned
unchanged, not truncated. The fill is cycled and cut so the result has exactly
`width` scalars. A negative width is `InvalidArgument("pad width must not be
negative")`. An empty fill is the same error, with message `"pad fill must not
be empty"`, only when padding is actually required. The result size is checked
before allocation and a result past the string limit is `ObjectTooLarge`.
Revision 8 artifacts cannot import them.

Revision 9 also appends IDs 167–168, `char_to_code` and `char_from_code`.
`char_to_code` returns the Unicode scalar value as `i64`; every char has one,
including U+0000. `char_from_code` accepts `0..=0x10FFFF` except surrogates
(`0xD800..=0xDFFF`). A negative code, a surrogate, or a code above U+10FFFF
returns null. A non-char or non-i64 argument is `TypeError`. Revision 8
artifacts cannot import them.

Revision 9 also appends IDs 169–184. `str_rfind` returns the last Unicode
scalar index; an empty pattern matches at `char_len`, and an absent pattern is
null. `str_count` counts non-overlapping matches; an empty pattern counts
`char_len + 1`. `str_is_ascii` is true when every byte is in U+0000–U+007F.
`char_into_string` encodes one char as a String. `exp`, `log2`, `log10`,
`trunc`, `fract`, `copysign`, `atan2`, `hypot`, `tan`, `asin`, `acos`, and
`atan` take `f64` and return the IEEE result, so domain errors and overflow are
NaN or infinity rather than a runtime error. `atan2(y, x)` is the angle of
`(x, y)`. `copysign` copies the sign bit, including negative zero. A non-string,
non-char, or non-`f64` argument is `TypeError`. Revision 8 artifacts cannot
import them.

Revision 9 also appends IDs 185–200. `is_nan`, `is_infinite`, `is_finite`, and
`is_sign_negative` take `f64` and return `bool`. `is_sign_negative` reads the
IEEE sign bit, so negative zero and a negative NaN are true; `sign` still
treats negative zero as zero. `sinh`, `cosh`, `tanh`, `cbrt`, `exp2`, `expm1`,
and `log1p` return the IEEE result, including infinity and NaN. `cbrt` accepts
negative inputs. `log1p(-1)` is negative infinity. `str_to_ascii_uppercase` and
`str_to_ascii_lowercase` change only ASCII letters and keep the byte length.
`str_split_whitespace` splits on Unicode White_Space, the same set `trim`
removes, and omits empty pieces. `char_to_uppercase` and `char_to_lowercase`
use full Unicode mapping and return a String because one char may become
several scalars (`'ß'` uppercases to `"SS"`). A non-`f64`, non-string, or
non-char argument is `TypeError`. Revision 8 artifacts cannot import them.

Revision 9 also appends IDs 201–212 for integer bit operations. `i64`, `u64`,
`isize`, and `usize` use 64 bits; `i128` and `u128` use 128 bits. Narrower
integers are not accepted, because their payload is a 64-bit scalar.
`count_zeros`, `leading_zeros`, and `trailing_zeros` of zero are the full
width. `reverse_bits`, `swap_bytes`, `rotate_left`, and `rotate_right` keep
the original numeric kind. A negative rotate distance is
`InvalidArgument("rotate distance must not be negative")`; a distance at or
above the width wraps. `is_power_of_two` is false for zero and for every
negative value. `ilog2` returns null for zero and for every negative value.
A float or other non-integer is `TypeError`. Revision 8 artifacts cannot
import them.

Revision 9 also appends IDs 213–227. `char_is_ascii_control`,
`char_is_ascii_graphic`, `char_is_ascii_punctuation`, `char_is_ascii_whitespace`,
`char_is_ascii_alphanumeric`, `char_is_ascii_uppercase`, and
`char_is_ascii_lowercase` are the remaining ASCII classes. `char_is_control`
uses the Unicode Control category, so U+0080 is control but not ASCII control.
`str_trim_start_matches`, `str_trim_end_matches`, and `str_trim_matches` remove
a string pattern repeatedly. An empty pattern returns the original string.
`str_replacen` replaces the first `count` non-overlapping matches; a negative
count is `InvalidArgument("replace count must not be negative")`, and zero
returns the original string. `str_splitn` and `str_rsplitn` return at most
`count` pieces. A negative count is `InvalidArgument("split limit must not be
negative")`, and zero returns an empty list. `str_rsplit` orders pieces from
the end of the string. An empty separator is the same error as `str_split`.
A non-char or non-string argument is `TypeError`. Revision 8 artifacts cannot
import them.

Revision 9 also appends IDs 228–242. `signum` returns `1.0` or `-1.0`, and
negative zero is `-1.0`; NaN stays NaN. This is not `sign`, which treats
negative zero as zero and returns null for NaN. `recip(0)` is infinity.
`mul_add(a, b, c)` is `a * b + c` with one rounding. `div_euclid` rounds
toward negative infinity, and `rem_euclid` is non-negative for finite inputs.
`char_utf8_len` is 1 to 4 and `char_utf16_len` is 1 or 2. The escape builtins
follow Rust's `escape_debug`, `escape_unicode`, and `escape_default`.
`str_split_inclusive` keeps the separator on each piece except a tail that has
none. `str_find_all` lists non-overlapping matches; an empty pattern matches
at every scalar boundary. An empty `split_inclusive` separator is the same
error as `str_split`. Revision 8 artifacts cannot import them.

Revision 9 also appends IDs 243–257. `to_radians` and `to_degrees` use Rust's
conversion factor. `next_up` and `next_down` move to the neighboring
representable value. `midpoint` is the midpoint of two finite values.
`round_ties_even` rounds halves to the even integer. `char_is_digit` and
`char_to_digit` take a radix from 2 to 36; any other radix is
`InvalidArgument("digit radix must be from 2 to 36")`. A non-digit is null
from `char_to_digit`. `str_split_ascii_whitespace`, `str_trim_ascii`,
`str_trim_ascii_start`, and `str_trim_ascii_end` use ASCII whitespace, so a
non-breaking space stays. `str_eq_ignore_ascii_case` folds only ASCII letters.
`ilog10` returns null for zero and for every negative value. `abs_diff`
returns a `u64` for 64-bit inputs and a `u128` for 128-bit inputs. Mixed
widths are `TypeError`. Revision 8 artifacts cannot import them.

Revision 9 also appends IDs 258–272. `wrapping_add`, `wrapping_sub`,
`wrapping_mul`, and `wrapping_neg` keep the low bits of the same width.
`saturating_*` stops at the minimum or maximum of that width. `checked_*`
returns null on overflow. `checked_div` and `checked_rem` also return null when
the divisor is zero, including `i64::MIN / -1`. Both operands must share a
runtime width and sign; a float or a mixed width is `TypeError`.
`char_from_digit` uses a radix from 2 to 36 and returns null when the digit
does not fit. Any other radix is
`InvalidArgument("digit radix must be from 2 to 36")`. Revision 8 artifacts
cannot import them.

Revision 9 also appends IDs 273–287. `int_div_euclid` and `int_rem_euclid`
round toward negative infinity and keep the input width. A zero divisor is
`InvalidArgument("divisor must not be zero")`. Dividing or taking the
remainder of a signed minimum by `-1` is `NumericOverflow`. `unsigned_abs`
accepts only signed integers and returns the unsigned width, so `i64::MIN`
fits. `int_midpoint` rounds toward negative infinity. A negative shift distance
is `InvalidArgument("shift distance must not be negative")`. `checked_shl` and
`checked_shr` return null when the distance is at least the width; wrapping
shifts reduce the distance modulo the width. A negative exponent is
`InvalidArgument("exponent must not be negative")`. `checked_pow` returns null
on overflow. `asinh`, `acosh`, and `atanh` return the IEEE result, including
NaN. `is_normal` is false for zero, infinity, NaN, and subnormal values.
Revision 8 artifacts cannot import them.

Revision 9 also appends IDs 288–302. `wrapping_div` and `wrapping_rem` round
toward zero. A zero divisor is `InvalidArgument("divisor must not be zero")`.
The signed minimum divided by `-1` wraps to itself, and its remainder is zero.
`saturating_abs` turns that minimum into the maximum, `checked_abs` returns
null, and `wrapping_abs` leaves it unchanged. `int_signum` is `-1`, `0`, or
`1` and accepts only signed integers. `checked_ilog` returns null for zero and
for every negative value. A base below 2 is
`InvalidArgument("logarithm base must be at least 2")`. `cast_unsigned` and
`cast_signed` keep the bits and change only the sign. `to_bits` returns the
IEEE-754 bits as `u64`, and `from_bits` accepts only an unsigned 64-bit
integer. `powi` takes an `i64` exponent that must fit in `i32`; otherwise it
is `NumericOverflow`. `is_subnormal` is false for zero. `char_encode_utf8` and
`char_encode_utf16` return a List of `i64` units. Revision 8 artifacts cannot
import them.

Revision 9 also appends IDs 303–317. `checked_div_euclid` and
`checked_rem_euclid` return null for a zero divisor and for the signed minimum
divided by `-1`. `widening_mul` returns the full product of two 64-bit
integers as `i128` or `u128`. 128-bit inputs are `TypeError`.
`next_power_of_two` is unsigned. Zero becomes 1. A result that does not fit is
`NumericOverflow`, and `checked_next_power_of_two` returns null instead.
`checked_add_signed` and `checked_sub_signed` add or subtract a same-width
signed value from an unsigned value; overflow is null. `is_multiple_of` is
false when the divisor is zero, including `0.is_multiple_of(0)`. The signed
minimum is a multiple of `-1`. `log_base` is the IEEE logarithm in an arbitrary
base. `str_encode_utf8` and `str_encode_utf16` return Lists of `i64`.
`str_from_utf8` and `str_from_utf16` return null for invalid text. A byte
outside 0..=255 is `InvalidArgument("utf-8 byte is out of range")`, and a code
unit outside 0..=65535 is `InvalidArgument("utf-16 code unit is out of
range")`. The lossy forms replace invalid sequences with U+FFFD. Revision 8
artifacts cannot import them.

Revision 9 also appends IDs 318–332. `str_is_char_boundary`,
`str_floor_char_boundary`, and `str_ceil_char_boundary` use byte indexes in
`0..=len`. A negative index or one past `len` is
`InvalidArgument("string index is out of range")`. `len` is a boundary.
`floor` returns the greatest scalar boundary at or before the index, and
`ceil` returns the least boundary at or after it. `pi`, `euler`, `tau`, `ln_2`,
`ln_10`, `log2_e`, `log10_e`, and `sqrt_2` are the exact IEEE-754 constants.
`infinity`, `neg_infinity`, `nan`, and `epsilon` are the matching f64 values.
`tau` is exactly `2 * pi`. `nan` is not equal to itself. Revision 8 artifacts
cannot import them.

Revision 9 also appends IDs 333–347. `strict_add`, `strict_sub`,
`strict_mul`, `strict_neg`, and `strict_pow` return the ordinary result, and
overflow is `NumericOverflow` rather than null or a wrapped value.
`strict_div` and `strict_rem` use the same error for overflow, including the
signed minimum divided by `-1`. A zero divisor is
`InvalidArgument("divisor must not be zero")`. `strict_shl` and `strict_shr`
reject a negative distance, and a distance at or above the width is
`NumericOverflow`. `min_positive` is the smallest positive normal f64.
`frac_pi_2` and `frac_pi_4` are exactly `pi / 2` and `pi / 4`.
`frac_1_sqrt_2` is `1 / sqrt(2)`. `log2_10 * log10_2` is exactly 1. Revision
8 artifacts cannot import them.

Revision 9 also appends IDs 348–362. `saturating_div` rounds toward zero.
`saturating_div_euclid` rounds toward negative infinity. A zero divisor is
`InvalidArgument("divisor must not be zero")`. The signed minimum divided by
`-1` saturates to the signed maximum. `trunc_i64`, `trunc_u64`, `trunc_i128`,
and `trunc_u128` truncate toward zero and return null when the value is not
finite or does not fit. A negative fraction in `(-1, 0)` becomes zero.
`char_is_ascii_octdigit` is true for ASCII `'0'` through `'7'`. `frac_1_pi *
pi` is 1 and `frac_2_pi * pi` is 2. `frac_pi_8 * 8` is `pi`. `f64_min` and
`f64_max` are the finite f64 bounds. Revision 8 artifacts cannot import them.

Revision 9 also appends IDs 363–377. `checked_add_unsigned` and
`checked_sub_unsigned` take a signed receiver and a same-width unsigned
operand. Overflow is null. The saturating forms stop at the signed bound, and
the wrapping forms keep the low bits. `strict_add_unsigned` and
`strict_sub_unsigned` report overflow as `NumericOverflow`. A signed operand,
or a different width, is `TypeError`. `int_to_str_radix` prints lowercase
digits. A radix outside 2..=36 is
`InvalidArgument("digit radix must be from 2 to 36")` for both directions.
`i64_from_str_radix`, `u64_from_str_radix`, `i128_from_str_radix`, and
`u128_from_str_radix` return null for malformed text, a bad sign, or overflow.
`str_char_at_byte` returns null when the byte index is not a character
boundary, including the index equal to `len`. `str_split_at` requires a
boundary and returns a two-element list; a mid-character index is
`InvalidArgument("string index is not a character boundary")`. An index below
0 or past `len` is `InvalidArgument("string index is out of range")`. Revision
8 artifacts cannot import them.

Revision 9 also appends IDs 378–392. `div_ceil` rounds toward positive
infinity. A zero divisor is `InvalidArgument("divisor must not be zero")`.
Overflow, including the signed minimum divided by `-1`, is `NumericOverflow`.
`checked_div_ceil` returns null for a zero divisor and for overflow.
`next_multiple_of` moves toward the sign of the factor: a positive factor
selects the smallest multiple at or above the receiver, and a negative factor
selects the largest multiple at or below it. A factor of `-1` returns the
receiver. A zero factor is `InvalidArgument("multiple must not be zero")`, and
overflow is `NumericOverflow`. `checked_next_multiple_of` returns null for
both. `int_to_be_bytes` and `int_to_le_bytes` return the raw bytes. The
`from_*_bytes` functions require 8 bytes for a 64-bit result and 16 bytes for
a 128-bit result. The wrong length is
`InvalidArgument("integer byte length does not match")`. A value outside
0..=255 is `InvalidArgument("integer byte is out of range")`. `total_cmp`
returns -1, 0, or 1 in total order, so `-0.0` is below `0.0` and a positive
NaN is above infinity. Revision 8 artifacts cannot import them.

Revision 9 also appends IDs 393–407. `div_floor` rounds toward negative
infinity. A zero divisor is `InvalidArgument("divisor must not be zero")`.
Overflow, including the signed minimum divided by `-1`, is `NumericOverflow`.
`checked_div_floor` returns null for a zero divisor and for overflow.
`is_sign_positive` reads the sign bit, so `+0.0` is positive and `-0.0` is
not. `f64_to_be_bytes` and `f64_to_le_bytes` return the eight raw bytes.
`f64_from_be_bytes` and `f64_from_le_bytes` require exactly eight bytes. The
wrong length is `InvalidArgument("float byte length does not match")`. A value
outside 0..=255 is `InvalidArgument("float byte is out of range")`.
`f64_radix` is 2, `f64_mantissa_digits` is 53, and `f64_digits` is 15.
`f64_min_exp` is -1021 and `f64_max_exp` is 1024. `str_byte_at` returns the
byte at an index in `0..len`; anything else is
`InvalidArgument("string index is out of range")`. `str_get` returns null when
the half-open byte range is reversed, out of range, or not on character
boundaries. `str_split_at_checked` returns null for an out-of-range or
mid-character index, and a two-element list otherwise. Revision 8 artifacts
cannot import them.

Revision 9 also appends IDs 408–422. `str_parse_u64`, `str_parse_i128`, and
`str_parse_u128` use the same whole-string rules as `str_parse_i64`: an
optional sign and decimal, `0b`, `0o`, or `0x` digits. Malformed text is null.
A leading minus on an unsigned parse is null. A value that does not fit is
`NumericOverflow`. `f64_min_10_exp` is -307 and `f64_max_10_exp` is 308.
`str_split_terminator` drops a trailing empty piece when the text ends with
the separator. An empty separator is
`InvalidArgument("split separator must not be empty")`.
`str_rsplit_terminator` yields the same pieces from the end. `int_gcd` and
`int_lcm` return the unsigned width of their operands. `gcd(0, 0)` is 0, and
`lcm` of zero is 0. `int_lcm` reports overflow as `NumericOverflow`.
`checked_int_lcm` returns null. `str_byte_find` and `str_byte_rfind` return
byte indexes. An empty pattern is found at 0, and `str_byte_rfind` of an empty
pattern returns `len`. An absent pattern is null. `str_byte_count` of an empty
pattern is `len + 1`. `char_replacement` is U+FFFD and `char_max` is
U+10FFFF. Revision 8 artifacts cannot import them.

Revision 9 also appends IDs 423–437. `saturating_add_signed`,
`wrapping_add_signed`, and `strict_add_signed` add a same-width signed value
to an unsigned receiver. Saturating stops at 0 and the unsigned maximum.
Wrapping keeps the low bits. Strict overflow is `NumericOverflow`. The `sub`
forms use the same rules. `unbounded_shl` and `unbounded_shr` do not reduce
the distance modulo the width. A distance at least the width yields 0, except
a negative value shifted right yields -1. A negative distance is
`InvalidArgument("shift distance must not be negative")`. `i64_min`,
`i64_max`, `u64_max`, `i128_min`, `i128_max`, and `u128_max` are the integer
bounds. `char_eq_ignore_ascii_case` compares ASCII letters without case.
Non-ASCII letters stay distinct. Revision 8 artifacts cannot import them.

Revision 9 also appends IDs 438–452. `highest_one` and `lowest_one` return
the index of that set bit. Zero is null. `isolate_highest_one` and
`isolate_lowest_one` keep only that bit, and zero stays zero.
`int_bit_width` counts bits in the two's-complement representation, so every
negative value uses the full width and zero uses none. `i64_bits` is 64,
`i128_bits` and `u128_bits` are 128, and `isize_bits` and `usize_bits` are 64.
`char_min` is U+0000. `str_find_indices` lists scalar indexes of
non-overlapping matches. An empty pattern lists every scalar boundary.
`str_rfind_indices` lists the same kind of matches from the end, so an
overlapping pattern can start at a different index. `str_byte_find_indices`
and `str_byte_rfind_indices` use byte indexes. Revision 8 artifacts cannot
import them.

Revision 9 also appends IDs 453–467. `isqrt` is the non-negative square root
rounded down. A negative value is
`InvalidArgument("square root of a negative value")`. `checked_isqrt` returns
null. `strict_abs` is signed only. The signed minimum is `NumericOverflow`.
`wrapping_div_euclid` rounds toward negative infinity. A zero divisor is
`InvalidArgument("divisor must not be zero")`. The signed minimum divided by
`-1` wraps to the minimum, and `wrapping_rem_euclid` of that pair is 0.
`checked_signed_diff` subtracts two unsigned values of the same width and
returns the signed width, or null when the difference does not fit.
`checked_div_exact` returns null for a zero divisor, overflow, or a remainder.
`strict_div_exact` reports a remainder as
`InvalidArgument("division is not exact")`. Exact shifts must be reversible.
A distance at least the width is null, or `NumericOverflow` for the strict
form. Lost bits are null, or `InvalidArgument("shift is not exact")`.
`str_trim_prefix` and `str_trim_suffix` remove one affix and otherwise return
the original string. `str_strip_circumfix` removes both once. Overlap or a
missing affix is null. Revision 8 artifacts cannot import them.

Revision 9 also appends IDs 468–482. `list_windows` returns overlapping
shallow copies. A size past the length yields an empty list. A size of zero is
`InvalidArgument("window size must not be zero")`. `list_chunks_exact` keeps
only full groups. `list_rchunks` groups from the end and keeps a short first
group. `list_rchunks_exact` drops that short prefix. A chunk size of zero is
`InvalidArgument("chunk size must not be zero")`. `list_chunks_remainder` is
the omitted tail, and `list_rchunks_remainder` is the omitted prefix.
`list_split_at` returns two new lists at an index in `0..=len`. An index past
the length is `InvalidIndex`. `list_rotate_left` and `list_rotate_right` return
new lists. A distance past the length is
`InvalidArgument("rotate distance is out of range")`; the length itself leaves
the order unchanged. `list_repeat` repeats the list. A negative count is
`InvalidArgument("repeat count must not be negative")`, and a product past the
list capacity is `ObjectTooLarge`. `list_truncate` shortens in place and
ignores a longer length. `list_fill` writes one value into every slot.
`list_swap_remove` swaps the index with the last element and returns the
removed value. `list_split_off` keeps the prefix and returns the tail.
`list_resize` shrinks like truncate or grows by sharing the fill value. Copies
share element objects with the source. Revision 8 artifacts cannot import them.

Revision 9 also appends IDs 483–497. `map_values` lists values in the same
order as `map_keys`, including a stored null. `map_clear` removes every entry
and drops its key and value edges. `map_merge` copies entries. An existing key
is overwritten, including a stored null. Merging a map into itself keeps those
entries. `map_get_or_insert` returns the stored value when the key is present,
including a stored null; otherwise it inserts `value` and returns it.
`map_insert_absent` inserts only when the key is absent and returns whether an
entry was added. A stored null counts as present. `golden_ratio` is φ and
`euler_gamma` is the Euler-Mascheroni constant. `f64_classify` returns `nan`,
`infinite`, `zero`, `subnormal`, or `normal`. Both signed zeros are `zero`.
`str_is_ascii_digit`, `str_is_ascii_alphabetic`, `str_is_ascii_alphanumeric`,
`str_is_ascii_lowercase`, `str_is_ascii_uppercase`, `str_is_ascii_whitespace`,
and `str_is_ascii_hexdigit` are true when every byte is in that ASCII class.
An empty string is true. A non-ASCII byte is false. Revision 8 artifacts
cannot import them.

Revision 9 also appends IDs 498–512. `str_is_numeric`, `str_is_alphabetic`,
`str_is_alphanumeric`, `str_is_lowercase`, `str_is_uppercase`,
`str_is_whitespace`, and `str_is_control` are true when every Unicode scalar
is in that class. An empty string is true. `str_is_ascii_graphic`,
`str_is_ascii_punctuation`, and `str_is_ascii_control` use the same rule on
bytes. `list_copy`, `list_reverse`, and `list_concat` return new lists and
leave their sources unchanged. Copies share element objects. `list_swap`
exchanges two indexes in place. The same index is a no-op, and an index past
the end is `InvalidIndex`. `list_clear` removes every element in place.
Revision 8 artifacts cannot import them.

Revision 9 also appends IDs 513–527. `list_insert` inserts at `index`.
`index == len` appends, and a larger index is `InvalidIndex`. `list_remove`
shifts later elements down and clears the vacated slot. `list_take` and
`list_drop` copy a prefix or suffix. A count past the length copies all
remaining elements. `list_first` and `list_last` return null on an empty list.
`range_len` is `end - start` for `start..end`, and 0 when empty. An inclusive
span that does not fit in i64 is `NumericOverflow`. `range_is_empty` and
`range_contains` follow `start <= value < end`. Inclusive, from, to, and
to-inclusive membership use the matching closed or open bound. Revision 8
artifacts cannot import them.

Revision 9 also appends IDs 528–542. `f64_to_ne_bytes` and `f64_from_ne_bytes`
use the host endianness. The algebraic operators may be reassociated. They
return the same values as `+`, `-`, `*`, `/`, and `%` on finite operands.
`f64_nan_to` replaces a NaN and leaves infinities unchanged. `f64_abs_diff` is
`(left - right).abs()`. `f64_is_zero` is true for both signed zeros.
`bool_not`, `bool_and`, `bool_or`, and `bool_xor` evaluate both arguments.
The methods are `negate`, `bitand`, `bitor`, and `xor` because `not`, `and`,
and `or` are keywords. `bool_then` returns the second argument when true,
otherwise null. Revision 8
artifacts cannot import them.

Revision 9 also appends IDs 543–557. `str_bytes` is the UTF-8 payload as
`i64` values in `0..=255`. `str_char_indices` is the byte offset of each
Unicode scalar. `str_take` and `str_drop` count scalars; a larger count
clamps. `str_first` and `str_last` return null on the empty string.
`str_repeat_char` is `char.repeat`. `list_append` copies a snapshot of the
other list, so appending a list to itself terminates. `list_join` requires
String elements. Revision 8 artifacts cannot import them.

Revision 9 also appends IDs 558–572. `str_lines` splits on `\n`, `\r\n`, and
`\r`, and drops a trailing ending. `str_split_once` and `str_rsplit_once`
return a `(String, String)` or null; an empty separator is the same error as
`split`. `str_strip_prefix` and `str_strip_suffix` return null when the affix
is absent. `map_entries` uses the same order as `keys`. `list_chunks` keeps a
short final group. `list_enumerate`, `list_flatten`, and `list_zip` allocate
fresh lists. `str_find_char` / `str_rfind_char` / `str_count_char` count
Unicode scalars. `map_copy` is a shallow snapshot. Revision 8 artifacts
cannot import them.

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
