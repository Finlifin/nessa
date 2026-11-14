//! Builtin function metadata — shared name→id table for compiler and runtime.
//!
//! Function pointers live on the VM (`interpreter`); this module owns stable
//! IDs, names, and the compile-time lookup table used by `'builtin` views.

/// Stable identifier for a builtin function (encoded in `CallBuiltin`).
pub type BuiltinFnId = u32;

/// Version of the stable builtin IDs and calling semantics stored in artifacts.
/// Increment when an ID changes meaning or compilation requires new native capabilities.
pub const BUILTIN_ABI_VERSION: u32 = 9;

// ---------------------------------------------------------------------------
// Stable builtin function IDs (sparse ranges by domain)
// ---------------------------------------------------------------------------

pub mod ids {
    use super::BuiltinFnId;

    // ── I/O ────────────────────────────────────────────────────────
    pub const PRINT: BuiltinFnId = 0;
    pub const PRINTLN: BuiltinFnId = 1;

    // ── Convert / introspection ────────────────────────────────────
    pub const TYPE_OF: BuiltinFnId = 2;
    pub const TO_I64: BuiltinFnId = 3;
    pub const TO_F64: BuiltinFnId = 4;
    pub const TO_STRING: BuiltinFnId = 5;

    // ── Math ───────────────────────────────────────────────────────
    pub const ABS: BuiltinFnId = 10;
    pub const SIN: BuiltinFnId = 11;
    pub const COS: BuiltinFnId = 12;
    pub const SQRT: BuiltinFnId = 13;
    pub const FLOOR: BuiltinFnId = 14;
    pub const CEIL: BuiltinFnId = 15;
    pub const ROUND: BuiltinFnId = 16;
    pub const POW: BuiltinFnId = 17;
    pub const LOG: BuiltinFnId = 18;

    // ── String ─────────────────────────────────────────────────────
    pub const STR_LEN: BuiltinFnId = 30;
    pub const STR_CONCAT: BuiltinFnId = 31;

    // ── Process ────────────────────────────────────────────────────
    pub const EXIT: BuiltinFnId = 50;
    pub const PANIC: BuiltinFnId = 51;

    // ── List ───────────────────────────────────────────────────────
    pub const LIST_INIT: BuiltinFnId = 100;
    pub const LIST_LEN: BuiltinFnId = 101;
    pub const LIST_GET: BuiltinFnId = 102;
    pub const LIST_SET: BuiltinFnId = 103;
    pub const LIST_PUSH: BuiltinFnId = 104;
    pub const LIST_POP: BuiltinFnId = 105;

    // ── String-keyed Map ────────────────────────────────────────────
    pub const MAP_INIT: BuiltinFnId = 110;
    pub const MAP_LEN: BuiltinFnId = 111;
    pub const MAP_GET: BuiltinFnId = 112;
    pub const MAP_SET: BuiltinFnId = 113;
    pub const MAP_REMOVE: BuiltinFnId = 114;
    pub const MAP_CONTAINS: BuiltinFnId = 115;
    /// Appended after ABI7 capabilities; earlier IDs retain their meanings.
    pub const MAP_KEYS: BuiltinFnId = 126;

    /// Scalar equality used by the standard library trait implementations.
    pub const SCALAR_EQ: BuiltinFnId = 121;
    /// Partial scalar comparison; std maps its optional sign to Ordering.
    pub const SCALAR_CMP: BuiltinFnId = 122;
    pub const DISPLAY_QUOTE: BuiltinFnId = 123;
    pub const DISPLAY_ENTER_TUPLE: BuiltinFnId = 124;
    pub const DISPLAY_EXIT_TUPLE: BuiltinFnId = 125;

    // ── String text operations (appended in ABI9) ──────────────────
    pub const STR_CONTAINS: BuiltinFnId = 127;
    pub const STR_STARTS_WITH: BuiltinFnId = 128;
    pub const STR_ENDS_WITH: BuiltinFnId = 129;
    pub const STR_TRIM: BuiltinFnId = 130;
    pub const STR_TRIM_START: BuiltinFnId = 131;
    pub const STR_TRIM_END: BuiltinFnId = 132;
    pub const STR_TO_UPPER: BuiltinFnId = 133;
    pub const STR_TO_LOWER: BuiltinFnId = 134;
    pub const STR_REPEAT: BuiltinFnId = 135;
    pub const STR_REPLACE: BuiltinFnId = 136;
    pub const STR_SPLIT: BuiltinFnId = 137;
    pub const STR_CHARS: BuiltinFnId = 138;

    // ── Iteration protocol (appended in ABI9) ──────────────────────
    /// Compiler-generated test of a dynamically selected iterator result:
    /// any concrete `IterationStep(T)` is accepted; other values are rejected.
    pub const ITERATION_STEP_DONE: BuiltinFnId = 139;

    // ── Dynamic equality (appended in ABI9) ────────────────────────
    /// `?bool`: equality decided by native kernels, `null` for two values
    /// that the standard library compares through `PartialEq`.
    pub const NATIVE_EQUAL: BuiltinFnId = 140;
    /// `?i64`: element count of a tuple object, `null` for other values.
    pub const TUPLE_LEN: BuiltinFnId = 141;
    /// Element of a tuple object by zero-based index.
    pub const TUPLE_GET: BuiltinFnId = 142;

    // ── String character indexing (appended in ABI9) ───────────────
    /// Count of Unicode scalar values. Distinct from `str_len`, which counts bytes.
    pub const STR_CHAR_LEN: BuiltinFnId = 143;
    /// `?char` at a scalar index; `null` when the index is out of range.
    pub const STR_CHAR_AT: BuiltinFnId = 144;
    /// `?i64` scalar index of the first match; `null` when absent.
    pub const STR_FIND: BuiltinFnId = 145;
    /// Half-open scalar-index substring. Out of range is `InvalidArgument`.
    pub const STR_SLICE: BuiltinFnId = 146;

    // ── Char classification (appended in ABI9) ─────────────────────
    pub const CHAR_IS_WHITESPACE: BuiltinFnId = 147;
    pub const CHAR_IS_ALPHABETIC: BuiltinFnId = 148;
    pub const CHAR_IS_NUMERIC: BuiltinFnId = 149;
    pub const CHAR_IS_ALPHANUMERIC: BuiltinFnId = 150;
    pub const CHAR_IS_UPPERCASE: BuiltinFnId = 151;
    pub const CHAR_IS_LOWERCASE: BuiltinFnId = 152;
    pub const CHAR_IS_ASCII: BuiltinFnId = 153;
    pub const CHAR_IS_ASCII_DIGIT: BuiltinFnId = 154;
    pub const CHAR_IS_ASCII_ALPHABETIC: BuiltinFnId = 155;
    pub const CHAR_IS_ASCII_HEXDIGIT: BuiltinFnId = 156;
    pub const CHAR_TO_ASCII_UPPERCASE: BuiltinFnId = 157;
    pub const CHAR_TO_ASCII_LOWERCASE: BuiltinFnId = 158;

    // ── Numeric bounds (appended in ABI9) ──────────────────────────
    /// Lesser number. Equal and unordered pairs keep the left operand.
    pub const MIN: BuiltinFnId = 159;
    /// Greater number. Equal and unordered pairs keep the left operand.
    pub const MAX: BuiltinFnId = 160;
    /// Restrict a number to inclusive bounds. Reversed bounds are `InvalidArgument`.
    pub const CLAMP: BuiltinFnId = 161;

    // ── String numeric parsing (appended in ABI9) ──────────────────
    /// Parse a whole string as `i64`. Malformed text is null; overflow is `NumericOverflow`.
    pub const STR_PARSE_I64: BuiltinFnId = 162;
    /// Parse a whole string as `f64`. Malformed text is null; non-finite results are `NumericOverflow`.
    pub const STR_PARSE_F64: BuiltinFnId = 163;

    // ── Numeric sign (appended in ABI9) ────────────────────────────
    /// `-1`, `0`, or `1`. NaN is null. Negative zero compares equal to zero.
    pub const SIGN: BuiltinFnId = 164;

    // ── String padding (appended in ABI9) ──────────────────────────
    /// Pad on the left to a scalar width. A short result is not truncated.
    pub const STR_PAD_START: BuiltinFnId = 165;
    /// Pad on the right to a scalar width. A short result is not truncated.
    pub const STR_PAD_END: BuiltinFnId = 166;

    // ── Char code points (appended in ABI9) ────────────────────────
    /// Unicode scalar value of a char, as `i64`.
    pub const CHAR_TO_CODE: BuiltinFnId = 167;
    /// Char from a Unicode scalar value. Surrogates and out-of-range values are null.
    pub const CHAR_FROM_CODE: BuiltinFnId = 168;

    // ── String search and char encoding (appended in ABI9) ─────────
    /// Last scalar index of a pattern. An empty pattern matches at `char_len`.
    pub const STR_RFIND: BuiltinFnId = 169;
    /// Non-overlapping match count. An empty pattern counts `char_len + 1`.
    pub const STR_COUNT: BuiltinFnId = 170;
    /// True when every byte is ASCII.
    pub const STR_IS_ASCII: BuiltinFnId = 171;
    /// One char encoded as a String.
    pub const CHAR_INTO_STRING: BuiltinFnId = 172;

    // ── Extra floating-point functions (appended in ABI9) ──────────
    /// `e^x`. The result is the IEEE value, including infinity and NaN.
    pub const EXP: BuiltinFnId = 173;
    /// Base-2 logarithm. Domain errors are NaN.
    pub const LOG2: BuiltinFnId = 174;
    /// Base-10 logarithm. Domain errors are NaN.
    pub const LOG10: BuiltinFnId = 175;
    /// Toward zero.
    pub const TRUNC: BuiltinFnId = 176;
    /// `value - trunc(value)`.
    pub const FRACT: BuiltinFnId = 177;
    /// Magnitude of the first argument and sign bit of the second.
    pub const COPY_SIGN: BuiltinFnId = 178;
    /// `atan2(y, x)`.
    pub const ATAN2: BuiltinFnId = 179;
    /// Euclidean length of `(x, y)`.
    pub const HYPOT: BuiltinFnId = 180;
    /// Tangent.
    pub const TAN: BuiltinFnId = 181;
    /// Arcsine. Inputs outside `-1..=1` are NaN.
    pub const ASIN: BuiltinFnId = 182;
    /// Arccosine. Inputs outside `-1..=1` are NaN.
    pub const ACOS: BuiltinFnId = 183;
    /// Arctangent.
    pub const ATAN: BuiltinFnId = 184;

    // ── Float classification and further IEEE functions (ABI9) ─────
    /// True for every NaN, including domain-error results.
    pub const IS_NAN: BuiltinFnId = 185;
    /// True for positive and negative infinity.
    pub const IS_INFINITE: BuiltinFnId = 186;
    /// False for NaN and infinity.
    pub const IS_FINITE: BuiltinFnId = 187;
    /// The IEEE sign bit. Negative zero and a negative NaN are true.
    pub const IS_SIGN_NEGATIVE: BuiltinFnId = 188;
    /// Hyperbolic sine.
    pub const SINH: BuiltinFnId = 189;
    /// Hyperbolic cosine.
    pub const COSH: BuiltinFnId = 190;
    /// Hyperbolic tangent.
    pub const TANH: BuiltinFnId = 191;
    /// Cube root, including negative inputs.
    pub const CBRT: BuiltinFnId = 192;
    /// `2^x`.
    pub const EXP2: BuiltinFnId = 193;
    /// `e^x - 1`, accurate near zero.
    pub const EXPM1: BuiltinFnId = 194;
    /// `ln(1 + x)`. Domain errors are NaN; `log1p(-1)` is negative infinity.
    pub const LOG1P: BuiltinFnId = 195;

    // ── ASCII case, whitespace split, Unicode char case (ABI9) ─────
    /// ASCII letters only. The byte length does not change.
    pub const STR_TO_ASCII_UPPERCASE: BuiltinFnId = 196;
    /// ASCII letters only. The byte length does not change.
    pub const STR_TO_ASCII_LOWERCASE: BuiltinFnId = 197;
    /// Split on Unicode White_Space. Empty pieces are omitted.
    pub const STR_SPLIT_WHITESPACE: BuiltinFnId = 198;
    /// Full Unicode uppercase mapping. The result may contain more than one scalar.
    pub const CHAR_TO_UPPERCASE: BuiltinFnId = 199;
    /// Full Unicode lowercase mapping. The result may contain more than one scalar.
    pub const CHAR_TO_LOWERCASE: BuiltinFnId = 200;

    // ── Integer bit operations (appended in ABI9) ──────────────────
    /// Population count of the runtime width: 64 bits for `i64`/`u64`, 128 for `i128`/`u128`.
    pub const COUNT_ONES: BuiltinFnId = 201;
    /// Width minus the population count. Zero uses the full width.
    pub const COUNT_ZEROS: BuiltinFnId = 202;
    /// Zeros above the highest set bit. Zero uses the full width.
    pub const LEADING_ZEROS: BuiltinFnId = 203;
    /// Zeros below the lowest set bit. Zero uses the full width.
    pub const TRAILING_ZEROS: BuiltinFnId = 204;
    /// Ones above the highest zero bit.
    pub const LEADING_ONES: BuiltinFnId = 205;
    /// Ones below the lowest zero bit.
    pub const TRAILING_ONES: BuiltinFnId = 206;
    /// Reverse the bits of the runtime width. The numeric kind is preserved.
    pub const REVERSE_BITS: BuiltinFnId = 207;
    /// Reverse the bytes of the runtime width. The numeric kind is preserved.
    pub const SWAP_BYTES: BuiltinFnId = 208;
    /// Rotate left. The distance is taken modulo the width.
    pub const ROTATE_LEFT: BuiltinFnId = 209;
    /// Rotate right. The distance is taken modulo the width.
    pub const ROTATE_RIGHT: BuiltinFnId = 210;
    /// True when the value is positive and has exactly one bit set.
    pub const IS_POWER_OF_TWO: BuiltinFnId = 211;
    /// Floor log base 2. Zero and negative values are null.
    pub const ILOG2: BuiltinFnId = 212;

    // ── More char classes and string edits (appended in ABI9) ──────
    /// ASCII control characters, U+0000–U+001F and U+007F.
    pub const CHAR_IS_ASCII_CONTROL: BuiltinFnId = 213;
    /// ASCII graphic characters, U+0021–U+007E.
    pub const CHAR_IS_ASCII_GRAPHIC: BuiltinFnId = 214;
    /// ASCII punctuation.
    pub const CHAR_IS_ASCII_PUNCTUATION: BuiltinFnId = 215;
    /// ASCII whitespace: space, tab, newline, form feed, carriage return, and vertical tab.
    pub const CHAR_IS_ASCII_WHITESPACE: BuiltinFnId = 216;
    /// ASCII letters and digits.
    pub const CHAR_IS_ASCII_ALPHANUMERIC: BuiltinFnId = 217;
    /// ASCII `A`–`Z`.
    pub const CHAR_IS_ASCII_UPPERCASE: BuiltinFnId = 218;
    /// ASCII `a`–`z`.
    pub const CHAR_IS_ASCII_LOWERCASE: BuiltinFnId = 219;
    /// Unicode general category Control.
    pub const CHAR_IS_CONTROL: BuiltinFnId = 220;
    /// Remove a prefix repeatedly. An empty pattern returns the original string.
    pub const STR_TRIM_START_MATCHES: BuiltinFnId = 221;
    /// Remove a suffix repeatedly. An empty pattern returns the original string.
    pub const STR_TRIM_END_MATCHES: BuiltinFnId = 222;
    /// Remove a prefix and a suffix repeatedly. An empty pattern returns the original string.
    pub const STR_TRIM_MATCHES: BuiltinFnId = 223;
    /// Replace the first `count` non-overlapping matches.
    pub const STR_REPLACEN: BuiltinFnId = 224;
    /// Split into at most `count` pieces, from the left.
    pub const STR_SPLITN: BuiltinFnId = 225;
    /// Split from the right. Pieces are ordered from the end of the string.
    pub const STR_RSPLIT: BuiltinFnId = 226;
    /// Split into at most `count` pieces, from the right.
    pub const STR_RSPLITN: BuiltinFnId = 227;

    // ── Float remainder, char encoding, string escapes (ABI9) ──────
    /// IEEE sign: `1.0`, `-1.0`, or NaN. Negative zero is `-1.0`, unlike `sign`.
    pub const SIGNUM: BuiltinFnId = 228;
    /// `1/x`. Zero is an infinity, not a runtime error.
    pub const RECIP: BuiltinFnId = 229;
    /// `mul_add(a, b, c)` is `a * b + c`, with a single rounding.
    pub const MUL_ADD: BuiltinFnId = 230;
    /// Euclidean division. The quotient rounds toward negative infinity.
    pub const DIV_EUCLID: BuiltinFnId = 231;
    /// Euclidean remainder, non-negative for finite inputs.
    pub const REM_EUCLID: BuiltinFnId = 232;
    /// UTF-8 byte length of one char, from 1 to 4.
    pub const CHAR_UTF8_LEN: BuiltinFnId = 233;
    /// UTF-16 unit length of one char, 1 or 2.
    pub const CHAR_UTF16_LEN: BuiltinFnId = 234;
    /// Debug escape of one char. Graphic characters stay themselves.
    pub const CHAR_ESCAPE_DEBUG: BuiltinFnId = 235;
    /// `\u{...}` escape of one char.
    pub const CHAR_ESCAPE_UNICODE: BuiltinFnId = 236;
    /// Default escape of one char. Non-ASCII characters use `\u{...}`.
    pub const CHAR_ESCAPE_DEFAULT: BuiltinFnId = 237;
    /// Debug escape of a string.
    pub const STR_ESCAPE_DEBUG: BuiltinFnId = 238;
    /// `\u{...}` escape of every scalar in a string.
    pub const STR_ESCAPE_UNICODE: BuiltinFnId = 239;
    /// Default escape of a string.
    pub const STR_ESCAPE_DEFAULT: BuiltinFnId = 240;
    /// Split while keeping the separator at the end of each piece except the tail.
    pub const STR_SPLIT_INCLUSIVE: BuiltinFnId = 241;
    /// Non-overlapping matches. An empty pattern matches at every scalar boundary.
    pub const STR_FIND_ALL: BuiltinFnId = 242;

    // ── Angles, ASCII text, and integer logs (appended in ABI9) ────
    /// Degrees to radians, using the same factor as Rust.
    pub const TO_RADIANS: BuiltinFnId = 243;
    /// Radians to degrees, using the same factor as Rust.
    pub const TO_DEGREES: BuiltinFnId = 244;
    /// The next representable value toward positive infinity.
    pub const NEXT_UP: BuiltinFnId = 245;
    /// The next representable value toward negative infinity.
    pub const NEXT_DOWN: BuiltinFnId = 246;
    /// The midpoint of two finite values.
    pub const MIDPOINT: BuiltinFnId = 247;
    /// Round half to even.
    pub const ROUND_TIES_EVEN: BuiltinFnId = 248;
    /// Digit in a radix from 2 to 36.
    pub const CHAR_IS_DIGIT: BuiltinFnId = 249;
    /// Digit value in a radix from 2 to 36. A non-digit is null.
    pub const CHAR_TO_DIGIT: BuiltinFnId = 250;
    /// Split on ASCII whitespace. Empty pieces are omitted.
    pub const STR_SPLIT_ASCII_WHITESPACE: BuiltinFnId = 251;
    /// ASCII case-insensitive equality. Non-ASCII scalars must match exactly.
    pub const STR_EQ_IGNORE_ASCII_CASE: BuiltinFnId = 252;
    /// Trim ASCII whitespace from both ends.
    pub const STR_TRIM_ASCII: BuiltinFnId = 253;
    /// Trim ASCII whitespace from the start.
    pub const STR_TRIM_ASCII_START: BuiltinFnId = 254;
    /// Trim ASCII whitespace from the end.
    pub const STR_TRIM_ASCII_END: BuiltinFnId = 255;
    /// Floor log base 10. Zero and negative values are null.
    pub const ILOG10: BuiltinFnId = 256;
    /// Absolute difference. The result uses the unsigned width of the inputs.
    pub const ABS_DIFF: BuiltinFnId = 257;

    // ── Integer overflow and digit characters (appended in ABI9) ───
    /// Wrapping addition. The result keeps the input width and sign.
    pub const WRAPPING_ADD: BuiltinFnId = 258;
    /// Wrapping subtraction. The result keeps the input width and sign.
    pub const WRAPPING_SUB: BuiltinFnId = 259;
    /// Wrapping multiplication. The result keeps the input width and sign.
    pub const WRAPPING_MUL: BuiltinFnId = 260;
    /// Wrapping negation. The result keeps the input width and sign.
    pub const WRAPPING_NEG: BuiltinFnId = 261;
    /// Saturating addition. The result keeps the input width and sign.
    pub const SATURATING_ADD: BuiltinFnId = 262;
    /// Saturating subtraction. The result keeps the input width and sign.
    pub const SATURATING_SUB: BuiltinFnId = 263;
    /// Saturating multiplication. The result keeps the input width and sign.
    pub const SATURATING_MUL: BuiltinFnId = 264;
    /// Saturating negation. The result keeps the input width and sign.
    pub const SATURATING_NEG: BuiltinFnId = 265;
    /// Checked addition. Overflow is null.
    pub const CHECKED_ADD: BuiltinFnId = 266;
    /// Checked subtraction. Overflow is null.
    pub const CHECKED_SUB: BuiltinFnId = 267;
    /// Checked multiplication. Overflow is null.
    pub const CHECKED_MUL: BuiltinFnId = 268;
    /// Checked division. Overflow and division by zero are null.
    pub const CHECKED_DIV: BuiltinFnId = 269;
    /// Checked remainder. Overflow and division by zero are null.
    pub const CHECKED_REM: BuiltinFnId = 270;
    /// Checked negation. Overflow is null.
    pub const CHECKED_NEG: BuiltinFnId = 271;
    /// The character for a digit in a radix from 2 to 36. An out-of-range digit is null.
    pub const CHAR_FROM_DIGIT: BuiltinFnId = 272;

    // ── Integer division, shifts, powers, and hyperbolic inverses (ABI9) ──
    /// Integer Euclidean quotient. The result keeps the input width and sign.
    pub const INT_DIV_EUCLID: BuiltinFnId = 273;
    /// Integer Euclidean remainder. The result keeps the input width and sign.
    pub const INT_REM_EUCLID: BuiltinFnId = 274;
    /// Absolute value as the unsigned width. Signed inputs only.
    pub const UNSIGNED_ABS: BuiltinFnId = 275;
    /// Midpoint, rounded toward negative infinity for signed values.
    pub const INT_MIDPOINT: BuiltinFnId = 276;
    /// Checked left shift. A distance at or above the width is null.
    pub const CHECKED_SHL: BuiltinFnId = 277;
    /// Checked right shift. A distance at or above the width is null.
    pub const CHECKED_SHR: BuiltinFnId = 278;
    /// Wrapping left shift. The distance is reduced to the width.
    pub const WRAPPING_SHL: BuiltinFnId = 279;
    /// Wrapping right shift. The distance is reduced to the width.
    pub const WRAPPING_SHR: BuiltinFnId = 280;
    /// Checked power. Overflow is null.
    pub const CHECKED_POW: BuiltinFnId = 281;
    /// Wrapping power. The result keeps the input width and sign.
    pub const WRAPPING_POW: BuiltinFnId = 282;
    /// Saturating power. The result keeps the input width and sign.
    pub const SATURATING_POW: BuiltinFnId = 283;
    /// Inverse hyperbolic sine.
    pub const ASINH: BuiltinFnId = 284;
    /// Inverse hyperbolic cosine.
    pub const ACOSH: BuiltinFnId = 285;
    /// Inverse hyperbolic tangent.
    pub const ATANH: BuiltinFnId = 286;
    /// True for a finite, non-zero, non-subnormal value.
    pub const IS_NORMAL: BuiltinFnId = 287;

    // ── Wrapping division, bit casts, and encodings (appended in ABI9) ──
    /// Wrapping quotient. A zero divisor is an error; the signed minimum divided by -1 wraps.
    pub const WRAPPING_DIV: BuiltinFnId = 288;
    /// Wrapping remainder. A zero divisor is an error; the signed minimum remainder by -1 is zero.
    pub const WRAPPING_REM: BuiltinFnId = 289;
    /// Absolute value, saturating the signed minimum to the maximum.
    pub const SATURATING_ABS: BuiltinFnId = 290;
    /// Absolute value. The signed minimum is null.
    pub const CHECKED_ABS: BuiltinFnId = 291;
    /// Absolute value. The signed minimum wraps to itself.
    pub const WRAPPING_ABS: BuiltinFnId = 292;
    /// Integer sign: -1, 0, or 1. Signed inputs only.
    pub const INT_SIGNUM: BuiltinFnId = 293;
    /// Floor log in an explicit base. A non-positive value is null.
    pub const CHECKED_ILOG: BuiltinFnId = 294;
    /// Reinterpret a signed value as the same bits, unsigned.
    pub const CAST_UNSIGNED: BuiltinFnId = 295;
    /// Reinterpret an unsigned value as the same bits, signed.
    pub const CAST_SIGNED: BuiltinFnId = 296;
    /// True for a non-zero number too small to be normal.
    pub const IS_SUBNORMAL: BuiltinFnId = 297;
    /// IEEE-754 bits of an f64, as u64.
    pub const TO_BITS: BuiltinFnId = 298;
    /// f64 from IEEE-754 bits. The argument is an unsigned 64-bit integer.
    pub const FROM_BITS: BuiltinFnId = 299;
    /// f64 raised to an integer power.
    pub const POWI: BuiltinFnId = 300;
    /// UTF-8 bytes of one character, as a List of i64.
    pub const CHAR_ENCODE_UTF8: BuiltinFnId = 301;
    /// UTF-16 code units of one character, as a List of i64.
    pub const CHAR_ENCODE_UTF16: BuiltinFnId = 302;

    // ── Checked Euclid, full products, and UTF conversion (ABI9) ────
    /// Euclidean quotient. Zero and overflow are null.
    pub const CHECKED_DIV_EUCLID: BuiltinFnId = 303;
    /// Euclidean remainder. Zero and overflow are null.
    pub const CHECKED_REM_EUCLID: BuiltinFnId = 304;
    /// Full product of two 64-bit integers, returned at 128 bits.
    pub const WIDENING_MUL: BuiltinFnId = 305;
    /// Smallest power of two at least as large as the value. Unsigned only.
    pub const NEXT_POWER_OF_TWO: BuiltinFnId = 306;
    /// `next_power_of_two`, or null when the result does not fit.
    pub const CHECKED_NEXT_POWER_OF_TWO: BuiltinFnId = 307;
    /// Unsigned plus a same-width signed value. Overflow is null.
    pub const CHECKED_ADD_SIGNED: BuiltinFnId = 308;
    /// Unsigned minus a same-width signed value. Overflow is null.
    pub const CHECKED_SUB_SIGNED: BuiltinFnId = 309;
    /// True when the first value is an integer multiple of the second.
    pub const IS_MULTIPLE_OF: BuiltinFnId = 310;
    /// Logarithm of an f64 in an arbitrary base. Domain errors are NaN or infinity.
    pub const LOG_BASE: BuiltinFnId = 311;
    /// UTF-8 bytes of a string, as a List of i64.
    pub const STR_ENCODE_UTF8: BuiltinFnId = 312;
    /// UTF-16 code units of a string, as a List of i64.
    pub const STR_ENCODE_UTF16: BuiltinFnId = 313;
    /// String from UTF-8 bytes. Invalid UTF-8 is null.
    pub const STR_FROM_UTF8: BuiltinFnId = 314;
    /// String from UTF-16 code units. Invalid UTF-16 is null.
    pub const STR_FROM_UTF16: BuiltinFnId = 315;
    /// String from UTF-8 bytes. Invalid bytes become U+FFFD.
    pub const STR_FROM_UTF8_LOSSY: BuiltinFnId = 316;
    /// String from UTF-16 code units. Unpaired surrogates become U+FFFD.
    pub const STR_FROM_UTF16_LOSSY: BuiltinFnId = 317;

    // ── Character boundaries and f64 constants (appended in ABI9) ───
    /// True when a byte index is a Unicode scalar boundary. The index may be `len`.
    pub const STR_IS_CHAR_BOUNDARY: BuiltinFnId = 318;
    /// Greatest scalar boundary at or before a byte index.
    pub const STR_FLOOR_CHAR_BOUNDARY: BuiltinFnId = 319;
    /// Least scalar boundary at or after a byte index.
    pub const STR_CEIL_CHAR_BOUNDARY: BuiltinFnId = 320;
    /// Archimedes' constant.
    pub const PI: BuiltinFnId = 321;
    /// Euler's number. The name is `euler` so a pattern can still bind `e`.
    pub const E: BuiltinFnId = 322;
    /// `2 * pi`.
    pub const TAU: BuiltinFnId = 323;
    /// Natural logarithm of 2.
    pub const LN_2: BuiltinFnId = 324;
    /// Natural logarithm of 10.
    pub const LN_10: BuiltinFnId = 325;
    /// Base-2 logarithm of e.
    pub const LOG2_E: BuiltinFnId = 326;
    /// Base-10 logarithm of e.
    pub const LOG10_E: BuiltinFnId = 327;
    /// Square root of 2.
    pub const SQRT_2: BuiltinFnId = 328;
    /// Positive infinity.
    pub const INFINITY: BuiltinFnId = 329;
    /// Negative infinity.
    pub const NEG_INFINITY: BuiltinFnId = 330;
    /// A quiet NaN.
    pub const NAN: BuiltinFnId = 331;
    /// Difference between 1 and the next larger f64.
    pub const EPSILON: BuiltinFnId = 332;

    // ── Strict integer arithmetic and more f64 constants (ABI9) ────
    /// Addition. Overflow is `NumericOverflow`.
    pub const STRICT_ADD: BuiltinFnId = 333;
    /// Subtraction. Overflow is `NumericOverflow`.
    pub const STRICT_SUB: BuiltinFnId = 334;
    /// Multiplication. Overflow is `NumericOverflow`.
    pub const STRICT_MUL: BuiltinFnId = 335;
    /// Division toward zero. A zero divisor is an error; overflow is `NumericOverflow`.
    pub const STRICT_DIV: BuiltinFnId = 336;
    /// Remainder toward zero. A zero divisor is an error; overflow is `NumericOverflow`.
    pub const STRICT_REM: BuiltinFnId = 337;
    /// Negation. Overflow is `NumericOverflow`.
    pub const STRICT_NEG: BuiltinFnId = 338;
    /// Left shift. A distance at or above the width is `NumericOverflow`.
    pub const STRICT_SHL: BuiltinFnId = 339;
    /// Right shift. A distance at or above the width is `NumericOverflow`.
    pub const STRICT_SHR: BuiltinFnId = 340;
    /// Power. Overflow is `NumericOverflow`.
    pub const STRICT_POW: BuiltinFnId = 341;
    /// Smallest positive normal f64.
    pub const MIN_POSITIVE: BuiltinFnId = 342;
    /// `pi / 2`.
    pub const FRAC_PI_2: BuiltinFnId = 343;
    /// `pi / 4`.
    pub const FRAC_PI_4: BuiltinFnId = 344;
    /// `1 / sqrt(2)`.
    pub const FRAC_1_SQRT_2: BuiltinFnId = 345;
    /// Base-2 logarithm of 10.
    pub const LOG2_10: BuiltinFnId = 346;
    /// Base-10 logarithm of 2.
    pub const LOG10_2: BuiltinFnId = 347;

    // ── Saturating division, float truncation, and more constants (ABI9) ──
    /// Toward-zero quotient. A zero divisor is an error; the signed minimum divided by -1 saturates.
    pub const SATURATING_DIV: BuiltinFnId = 348;
    /// Euclidean quotient. A zero divisor is an error; the signed minimum divided by -1 saturates.
    pub const SATURATING_DIV_EUCLID: BuiltinFnId = 349;
    /// Truncate toward zero into i64. Out of range and non-finite values are null.
    pub const TRUNC_I64: BuiltinFnId = 350;
    /// Truncate toward zero into u64. Out of range and non-finite values are null.
    pub const TRUNC_U64: BuiltinFnId = 351;
    /// Truncate toward zero into i128. Out of range and non-finite values are null.
    pub const TRUNC_I128: BuiltinFnId = 352;
    /// Truncate toward zero into u128. Out of range and non-finite values are null.
    pub const TRUNC_U128: BuiltinFnId = 353;
    /// ASCII digit from `'0'` through `'7'`.
    pub const CHAR_IS_ASCII_OCTDIGIT: BuiltinFnId = 354;
    /// `1 / pi`.
    pub const FRAC_1_PI: BuiltinFnId = 355;
    /// `2 / pi`.
    pub const FRAC_2_PI: BuiltinFnId = 356;
    /// `2 / sqrt(pi)`.
    pub const FRAC_2_SQRT_PI: BuiltinFnId = 357;
    /// `pi / 3`.
    pub const FRAC_PI_3: BuiltinFnId = 358;
    /// `pi / 6`.
    pub const FRAC_PI_6: BuiltinFnId = 359;
    /// `pi / 8`.
    pub const FRAC_PI_8: BuiltinFnId = 360;
    /// Most negative finite f64.
    pub const F64_MIN: BuiltinFnId = 361;
    /// Most positive finite f64.
    pub const F64_MAX: BuiltinFnId = 362;
    /// Signed value plus a same-width unsigned value. Overflow is null.
    pub const CHECKED_ADD_UNSIGNED: BuiltinFnId = 363;
    /// Signed value minus a same-width unsigned value. Overflow is null.
    pub const CHECKED_SUB_UNSIGNED: BuiltinFnId = 364;
    /// Signed value plus a same-width unsigned value, stopping at the bound.
    pub const SATURATING_ADD_UNSIGNED: BuiltinFnId = 365;
    /// Signed value minus a same-width unsigned value, stopping at the bound.
    pub const SATURATING_SUB_UNSIGNED: BuiltinFnId = 366;
    /// Signed value plus a same-width unsigned value, keeping the low bits.
    pub const WRAPPING_ADD_UNSIGNED: BuiltinFnId = 367;
    /// Signed value minus a same-width unsigned value, keeping the low bits.
    pub const WRAPPING_SUB_UNSIGNED: BuiltinFnId = 368;
    /// Signed value plus a same-width unsigned value. Overflow is an error.
    pub const STRICT_ADD_UNSIGNED: BuiltinFnId = 369;
    /// Signed value minus a same-width unsigned value. Overflow is an error.
    pub const STRICT_SUB_UNSIGNED: BuiltinFnId = 370;
    /// Integer text in a radix from 2 to 36. Digits above 9 are lowercase.
    pub const INT_TO_STR_RADIX: BuiltinFnId = 371;
    /// Parse text as `i64` in a radix from 2 to 36. Malformed text is null.
    pub const I64_FROM_STR_RADIX: BuiltinFnId = 372;
    /// Parse text as `u64` in a radix from 2 to 36. Malformed text is null.
    pub const U64_FROM_STR_RADIX: BuiltinFnId = 373;
    /// Parse text as `i128` in a radix from 2 to 36. Malformed text is null.
    pub const I128_FROM_STR_RADIX: BuiltinFnId = 374;
    /// Parse text as `u128` in a radix from 2 to 36. Malformed text is null.
    pub const U128_FROM_STR_RADIX: BuiltinFnId = 375;
    /// Character starting at a byte index, or null when that index is not a boundary.
    pub const STR_CHAR_AT_BYTE: BuiltinFnId = 376;
    /// Split a string at a byte index that is a character boundary.
    pub const STR_SPLIT_AT: BuiltinFnId = 377;
    /// Quotient rounded toward positive infinity. Overflow is an error.
    pub const DIV_CEIL: BuiltinFnId = 378;
    /// Quotient rounded toward positive infinity. Zero and overflow are null.
    pub const CHECKED_DIV_CEIL: BuiltinFnId = 379;
    /// Next multiple in the direction of the factor. Overflow is an error.
    pub const NEXT_MULTIPLE_OF: BuiltinFnId = 380;
    /// Next multiple in the direction of the factor. Zero and overflow are null.
    pub const CHECKED_NEXT_MULTIPLE_OF: BuiltinFnId = 381;
    /// Big-endian bytes of an integer.
    pub const INT_TO_BE_BYTES: BuiltinFnId = 382;
    /// Little-endian bytes of an integer.
    pub const INT_TO_LE_BYTES: BuiltinFnId = 383;
    /// Decode 8 big-endian bytes as `i64`.
    pub const I64_FROM_BE_BYTES: BuiltinFnId = 384;
    /// Decode 8 little-endian bytes as `i64`.
    pub const I64_FROM_LE_BYTES: BuiltinFnId = 385;
    /// Decode 8 big-endian bytes as `u64`.
    pub const U64_FROM_BE_BYTES: BuiltinFnId = 386;
    /// Decode 8 little-endian bytes as `u64`.
    pub const U64_FROM_LE_BYTES: BuiltinFnId = 387;
    /// Decode 16 big-endian bytes as `i128`.
    pub const I128_FROM_BE_BYTES: BuiltinFnId = 388;
    /// Decode 16 little-endian bytes as `i128`.
    pub const I128_FROM_LE_BYTES: BuiltinFnId = 389;
    /// Decode 16 big-endian bytes as `u128`.
    pub const U128_FROM_BE_BYTES: BuiltinFnId = 390;
    /// Decode 16 little-endian bytes as `u128`.
    pub const U128_FROM_LE_BYTES: BuiltinFnId = 391;
    /// Total order of two f64 values: -1, 0, or 1.
    pub const TOTAL_CMP: BuiltinFnId = 392;
    /// Quotient rounded toward negative infinity. Overflow is an error.
    pub const DIV_FLOOR: BuiltinFnId = 393;
    /// Quotient rounded toward negative infinity. Zero and overflow are null.
    pub const CHECKED_DIV_FLOOR: BuiltinFnId = 394;
    /// True when the f64 sign bit is clear, including +0 and positive NaN.
    pub const IS_SIGN_POSITIVE: BuiltinFnId = 395;
    /// Big-endian bytes of an f64.
    pub const F64_TO_BE_BYTES: BuiltinFnId = 396;
    /// Little-endian bytes of an f64.
    pub const F64_TO_LE_BYTES: BuiltinFnId = 397;
    /// Decode 8 big-endian bytes as f64.
    pub const F64_FROM_BE_BYTES: BuiltinFnId = 398;
    /// Decode 8 little-endian bytes as f64.
    pub const F64_FROM_LE_BYTES: BuiltinFnId = 399;
    /// Radix of f64, which is 2.
    pub const F64_RADIX: BuiltinFnId = 400;
    /// Binary digits in an f64 significand, including the implicit bit.
    pub const F64_MANTISSA_DIGITS: BuiltinFnId = 401;
    /// Approximate decimal digits of an f64.
    pub const F64_DIGITS: BuiltinFnId = 402;
    /// One greater than the minimum normal binary exponent of f64.
    pub const F64_MIN_EXP: BuiltinFnId = 403;
    /// Maximum binary exponent of f64.
    pub const F64_MAX_EXP: BuiltinFnId = 404;
    /// Byte at a UTF-8 index. The index must be inside the string.
    pub const STR_BYTE_AT: BuiltinFnId = 405;
    /// Byte-range substring. A bad range or mid-character bound is null.
    pub const STR_GET: BuiltinFnId = 406;
    /// Split at a byte index. A bad index or mid-character index is null.
    pub const STR_SPLIT_AT_CHECKED: BuiltinFnId = 407;
    /// Parse text as `u64`. A leading minus is null. Overflow is an error.
    pub const STR_PARSE_U64: BuiltinFnId = 408;
    /// Parse text as `i128`. Overflow is an error.
    pub const STR_PARSE_I128: BuiltinFnId = 409;
    /// Parse text as `u128`. A leading minus is null. Overflow is an error.
    pub const STR_PARSE_U128: BuiltinFnId = 410;
    /// Minimum decimal exponent of a normal f64.
    pub const F64_MIN_10_EXP: BuiltinFnId = 411;
    /// Maximum decimal exponent of an f64.
    pub const F64_MAX_10_EXP: BuiltinFnId = 412;
    /// Split, dropping a trailing empty piece when the text ends with the separator.
    pub const STR_SPLIT_TERMINATOR: BuiltinFnId = 413;
    /// `split_terminator` ordered from the end.
    pub const STR_RSPLIT_TERMINATOR: BuiltinFnId = 414;
    /// Greatest common divisor, returned as the unsigned width.
    pub const INT_GCD: BuiltinFnId = 415;
    /// Least common multiple, returned as the unsigned width. Overflow is an error.
    pub const INT_LCM: BuiltinFnId = 416;
    /// Least common multiple. Overflow is null.
    pub const CHECKED_INT_LCM: BuiltinFnId = 417;
    /// First byte index of a pattern, or null when it is absent.
    pub const STR_BYTE_FIND: BuiltinFnId = 418;
    /// Last byte index of a pattern. An empty pattern returns the byte length.
    pub const STR_BYTE_RFIND: BuiltinFnId = 419;
    /// Non-overlapping matches. An empty pattern returns the byte length plus one.
    pub const STR_BYTE_COUNT: BuiltinFnId = 420;
    /// U+FFFD.
    pub const CHAR_REPLACEMENT: BuiltinFnId = 421;
    /// U+10FFFF.
    pub const CHAR_MAX: BuiltinFnId = 422;
    /// Unsigned value plus a same-width signed value, stopping at the bounds.
    pub const SATURATING_ADD_SIGNED: BuiltinFnId = 423;
    /// Unsigned value minus a same-width signed value, stopping at the bounds.
    pub const SATURATING_SUB_SIGNED: BuiltinFnId = 424;
    /// Unsigned value plus a same-width signed value, keeping the low bits.
    pub const WRAPPING_ADD_SIGNED: BuiltinFnId = 425;
    /// Unsigned value minus a same-width signed value, keeping the low bits.
    pub const WRAPPING_SUB_SIGNED: BuiltinFnId = 426;
    /// Unsigned value plus a same-width signed value. Overflow is an error.
    pub const STRICT_ADD_SIGNED: BuiltinFnId = 427;
    /// Unsigned value minus a same-width signed value. Overflow is an error.
    pub const STRICT_SUB_SIGNED: BuiltinFnId = 428;
    /// Shift left without wrapping the distance. A large distance yields zero.
    pub const UNBOUNDED_SHL: BuiltinFnId = 429;
    /// Shift right without wrapping the distance. A large distance fills with the sign.
    pub const UNBOUNDED_SHR: BuiltinFnId = 430;
    /// Smallest `i64`.
    pub const I64_MIN: BuiltinFnId = 431;
    /// Largest `i64`.
    pub const I64_MAX: BuiltinFnId = 432;
    /// Largest `u64`.
    pub const U64_MAX: BuiltinFnId = 433;
    /// Smallest `i128`.
    pub const I128_MIN: BuiltinFnId = 434;
    /// Largest `i128`.
    pub const I128_MAX: BuiltinFnId = 435;
    /// Largest `u128`.
    pub const U128_MAX: BuiltinFnId = 436;
    /// ASCII case-insensitive character equality. Non-ASCII letters stay distinct.
    pub const CHAR_EQ_IGNORE_ASCII_CASE: BuiltinFnId = 437;
    /// Index of the highest set bit, or null when the value is zero.
    pub const HIGHEST_ONE: BuiltinFnId = 438;
    /// Index of the lowest set bit, or null when the value is zero.
    pub const LOWEST_ONE: BuiltinFnId = 439;
    /// Value with only the highest set bit kept. Zero stays zero.
    pub const ISOLATE_HIGHEST_ONE: BuiltinFnId = 440;
    /// Value with only the lowest set bit kept. Zero stays zero.
    pub const ISOLATE_LOWEST_ONE: BuiltinFnId = 441;
    /// Bits needed for the two's-complement representation. Zero is zero.
    pub const INT_BIT_WIDTH: BuiltinFnId = 442;
    /// Bit width of `i64`, which is 64.
    pub const I64_BITS: BuiltinFnId = 443;
    /// Bit width of `i128`, which is 128.
    pub const I128_BITS: BuiltinFnId = 444;
    /// Bit width of `isize`, which is 64.
    pub const ISIZE_BITS: BuiltinFnId = 445;
    /// Bit width of `u128`, which is 128.
    pub const U128_BITS: BuiltinFnId = 446;
    /// Bit width of `usize`, which is 64.
    pub const USIZE_BITS: BuiltinFnId = 447;
    /// U+0000.
    pub const CHAR_MIN: BuiltinFnId = 448;
    /// Scalar indexes of non-overlapping matches. An empty pattern lists every boundary.
    pub const STR_FIND_INDICES: BuiltinFnId = 449;
    /// `find_indices` ordered from the end. Overlapping patterns can differ.
    pub const STR_RFIND_INDICES: BuiltinFnId = 450;
    /// Byte indexes of non-overlapping matches.
    pub const STR_BYTE_FIND_INDICES: BuiltinFnId = 451;
    /// `byte_find_indices` ordered from the end.
    pub const STR_BYTE_RFIND_INDICES: BuiltinFnId = 452;
    /// Integer square root rounded down. A negative value is an error.
    pub const INT_ISQRT: BuiltinFnId = 453;
    /// Integer square root rounded down. A negative value is null.
    pub const CHECKED_INT_ISQRT: BuiltinFnId = 454;
    /// Absolute value. The signed minimum is an error.
    pub const STRICT_ABS: BuiltinFnId = 455;
    /// Euclidean quotient, wrapping the signed minimum divided by -1.
    pub const WRAPPING_DIV_EUCLID: BuiltinFnId = 456;
    /// Euclidean remainder. The signed minimum divided by -1 is 0.
    pub const WRAPPING_REM_EUCLID: BuiltinFnId = 457;
    /// Unsigned difference as the signed width, or null when it does not fit.
    pub const CHECKED_SIGNED_DIFF: BuiltinFnId = 458;
    /// Exact quotient. A zero divisor or a remainder is null.
    pub const CHECKED_DIV_EXACT: BuiltinFnId = 459;
    /// Exact quotient. A remainder is an error.
    pub const STRICT_DIV_EXACT: BuiltinFnId = 460;
    /// Shift left that can be reversed. Lost bits or a large distance are null.
    pub const CHECKED_SHL_EXACT: BuiltinFnId = 461;
    /// Shift left that can be reversed. Lost bits are an error.
    pub const STRICT_SHL_EXACT: BuiltinFnId = 462;
    /// Shift right that can be reversed. Lost bits or a large distance are null.
    pub const CHECKED_SHR_EXACT: BuiltinFnId = 463;
    /// Shift right that can be reversed. Lost bits are an error.
    pub const STRICT_SHR_EXACT: BuiltinFnId = 464;
    /// Remove a prefix once, or return the original string.
    pub const STR_TRIM_PREFIX: BuiltinFnId = 465;
    /// Remove a suffix once, or return the original string.
    pub const STR_TRIM_SUFFIX: BuiltinFnId = 466;
    /// Remove a prefix and a suffix once. Overlap or a missing affix is null.
    pub const STR_STRIP_CIRCUMFIX: BuiltinFnId = 467;
    /// Overlapping shallow copies of `size` elements. A zero size is an error.
    pub const LIST_WINDOWS: BuiltinFnId = 468;
    /// Non-overlapping full groups. The short tail is omitted.
    pub const LIST_CHUNKS_EXACT: BuiltinFnId = 469;
    /// Groups from the end, including a short first group.
    pub const LIST_RCHUNKS: BuiltinFnId = 470;
    /// Full groups from the end. The short prefix is omitted.
    pub const LIST_RCHUNKS_EXACT: BuiltinFnId = 471;
    /// The short tail omitted by exact chunking.
    pub const LIST_CHUNKS_REMAINDER: BuiltinFnId = 472;
    /// The short prefix omitted by exact reverse chunking.
    pub const LIST_RCHUNKS_REMAINDER: BuiltinFnId = 473;
    /// Two shallow copies split at an index in `0..=len`.
    pub const LIST_SPLIT_AT: BuiltinFnId = 474;
    /// A new list with the first `distance` elements moved to the end.
    pub const LIST_ROTATE_LEFT: BuiltinFnId = 475;
    /// A new list with the last `distance` elements moved to the front.
    pub const LIST_ROTATE_RIGHT: BuiltinFnId = 476;
    /// A new list that repeats the source. A negative count is an error.
    pub const LIST_REPEAT: BuiltinFnId = 477;
    /// Shorten a list in place. A longer length leaves it unchanged.
    pub const LIST_TRUNCATE: BuiltinFnId = 478;
    /// Replace every element in place with the same value.
    pub const LIST_FILL: BuiltinFnId = 479;
    /// Remove an element by swapping it with the last, and return it.
    pub const LIST_SWAP_REMOVE: BuiltinFnId = 480;
    /// Split off the tail in place and return that tail as a new list.
    pub const LIST_SPLIT_OFF: BuiltinFnId = 481;
    /// Grow or shrink a list in place. New slots share the fill value.
    pub const LIST_RESIZE: BuiltinFnId = 482;
    /// Values in the same order as `map_keys`.
    pub const MAP_VALUES: BuiltinFnId = 483;
    /// Remove every entry and drop its key and value edges.
    pub const MAP_CLEAR: BuiltinFnId = 484;
    /// Copy entries from another map. Existing keys are overwritten.
    pub const MAP_MERGE: BuiltinFnId = 485;
    /// Return the stored value, or insert and return `value` when the key is absent.
    pub const MAP_GET_OR_INSERT: BuiltinFnId = 486;
    /// Insert only when the key is absent. Returns whether an entry was added.
    pub const MAP_INSERT_ABSENT: BuiltinFnId = 487;
    /// The golden ratio, φ.
    pub const GOLDEN_RATIO: BuiltinFnId = 488;
    /// The Euler-Mascheroni constant, γ.
    pub const EULER_GAMMA: BuiltinFnId = 489;
    /// `nan`, `infinite`, `zero`, `subnormal`, or `normal`.
    pub const F64_CLASSIFY: BuiltinFnId = 490;
    /// Every byte is an ASCII digit. An empty string is true.
    pub const STR_IS_ASCII_DIGIT: BuiltinFnId = 491;
    /// Every byte is an ASCII letter. An empty string is true.
    pub const STR_IS_ASCII_ALPHABETIC: BuiltinFnId = 492;
    /// Every byte is an ASCII letter or digit. An empty string is true.
    pub const STR_IS_ASCII_ALPHANUMERIC: BuiltinFnId = 493;
    /// Every byte is an ASCII lowercase letter. An empty string is true.
    pub const STR_IS_ASCII_LOWERCASE: BuiltinFnId = 494;
    /// Every byte is an ASCII uppercase letter. An empty string is true.
    pub const STR_IS_ASCII_UPPERCASE: BuiltinFnId = 495;
    /// Every byte is ASCII whitespace. An empty string is true.
    pub const STR_IS_ASCII_WHITESPACE: BuiltinFnId = 496;
    /// Every byte is an ASCII hex digit. An empty string is true.
    pub const STR_IS_ASCII_HEXDIGIT: BuiltinFnId = 497;
    /// Every Unicode scalar is numeric. An empty string is true.
    pub const STR_IS_NUMERIC: BuiltinFnId = 498;
    /// Every Unicode scalar is alphabetic. An empty string is true.
    pub const STR_IS_ALPHABETIC: BuiltinFnId = 499;
    /// Every Unicode scalar is alphanumeric. An empty string is true.
    pub const STR_IS_ALPHANUMERIC: BuiltinFnId = 500;
    /// Every Unicode scalar is lowercase. An empty string is true.
    pub const STR_IS_LOWERCASE: BuiltinFnId = 501;
    /// Every Unicode scalar is uppercase. An empty string is true.
    pub const STR_IS_UPPERCASE: BuiltinFnId = 502;
    /// Every Unicode scalar is whitespace. An empty string is true.
    pub const STR_IS_WHITESPACE: BuiltinFnId = 503;
    /// Every Unicode scalar is a control character. An empty string is true.
    pub const STR_IS_CONTROL: BuiltinFnId = 504;
    /// Every byte is ASCII graphic. An empty string is true.
    pub const STR_IS_ASCII_GRAPHIC: BuiltinFnId = 505;
    /// Every byte is ASCII punctuation. An empty string is true.
    pub const STR_IS_ASCII_PUNCTUATION: BuiltinFnId = 506;
    /// Every byte is an ASCII control character. An empty string is true.
    pub const STR_IS_ASCII_CONTROL: BuiltinFnId = 507;
    /// Shallow copy. Element objects stay shared.
    pub const LIST_COPY: BuiltinFnId = 508;
    /// A new list in reverse order. The source stays unchanged.
    pub const LIST_REVERSE: BuiltinFnId = 509;
    /// A new list with the elements of both sides. Sources stay unchanged.
    pub const LIST_CONCAT: BuiltinFnId = 510;
    /// Exchange two elements in place. The same index is a no-op.
    pub const LIST_SWAP: BuiltinFnId = 511;
    /// Remove every element in place.
    pub const LIST_CLEAR: BuiltinFnId = 512;
    /// Insert at `index`. `index == len` appends.
    pub const LIST_INSERT: BuiltinFnId = 513;
    /// Remove `index` and shift later elements down.
    pub const LIST_REMOVE: BuiltinFnId = 514;
    /// Copy the first `count` elements, or the whole list if it is shorter.
    pub const LIST_TAKE: BuiltinFnId = 515;
    /// Copy the elements after the first `count`, or an empty list if none remain.
    pub const LIST_DROP: BuiltinFnId = 516;
    /// The first element, or null when the list is empty.
    pub const LIST_FIRST: BuiltinFnId = 517;
    /// The last element, or null when the list is empty.
    pub const LIST_LAST: BuiltinFnId = 518;
    /// Exclusive range length. An empty range is 0.
    pub const RANGE_LEN: BuiltinFnId = 519;
    /// Inclusive range length. An empty range is 0.
    pub const RANGE_INCLUSIVE_LEN: BuiltinFnId = 520;
    /// Exclusive range emptiness.
    pub const RANGE_IS_EMPTY: BuiltinFnId = 521;
    /// Inclusive range emptiness.
    pub const RANGE_INCLUSIVE_IS_EMPTY: BuiltinFnId = 522;
    /// Exclusive range membership: `start <= value < end`.
    pub const RANGE_CONTAINS: BuiltinFnId = 523;
    /// Inclusive range membership: `start <= value <= end`.
    pub const RANGE_INCLUSIVE_CONTAINS: BuiltinFnId = 524;
    /// Unbounded-above membership: `start <= value`.
    pub const RANGE_FROM_CONTAINS: BuiltinFnId = 525;
    /// Open-ended membership: `value < end`.
    pub const RANGE_TO_CONTAINS: BuiltinFnId = 526;
    /// Closed-ended membership: `value <= end`.
    pub const RANGE_TO_INCLUSIVE_CONTAINS: BuiltinFnId = 527;
    /// Native-endian IEEE-754 bytes.
    pub const F64_TO_NE_BYTES: BuiltinFnId = 528;
    /// Interpret eight native-endian IEEE-754 bytes.
    pub const F64_FROM_NE_BYTES: BuiltinFnId = 529;
    /// Addition that may be reassociated.
    pub const ALGEBRAIC_ADD: BuiltinFnId = 530;
    /// Subtraction that may be reassociated.
    pub const ALGEBRAIC_SUB: BuiltinFnId = 531;
    /// Multiplication that may be reassociated.
    pub const ALGEBRAIC_MUL: BuiltinFnId = 532;
    /// Division that may be reassociated.
    pub const ALGEBRAIC_DIV: BuiltinFnId = 533;
    /// Remainder that may be reassociated.
    pub const ALGEBRAIC_REM: BuiltinFnId = 534;
    /// Replace a NaN. Other values, including infinities, stay themselves.
    pub const F64_NAN_TO: BuiltinFnId = 535;
    /// Absolute difference of two floats.
    pub const F64_ABS_DIFF: BuiltinFnId = 536;
    /// True for both +0.0 and -0.0.
    pub const F64_IS_ZERO: BuiltinFnId = 537;
    /// Logical not.
    pub const BOOL_NOT: BuiltinFnId = 538;
    /// Logical and. Both arguments are evaluated.
    pub const BOOL_AND: BuiltinFnId = 539;
    /// Logical or. Both arguments are evaluated.
    pub const BOOL_OR: BuiltinFnId = 540;
    /// Exclusive or.
    pub const BOOL_XOR: BuiltinFnId = 541;
    /// Return `value` when true, otherwise null.
    pub const BOOL_THEN: BuiltinFnId = 542;
    /// UTF-8 bytes as i64 values in `0..=255`.
    pub const STR_BYTES: BuiltinFnId = 543;
    /// Byte offsets of each Unicode scalar, in order.
    pub const STR_CHAR_INDICES: BuiltinFnId = 544;
    /// True when the string has no bytes.
    pub const STR_IS_EMPTY: BuiltinFnId = 545;
    /// Repeat a character `count` times.
    pub const STR_REPEAT_CHAR: BuiltinFnId = 546;
    /// The first `count` Unicode scalars. A larger count returns the whole string.
    pub const STR_TAKE: BuiltinFnId = 547;
    /// Drop the first `count` Unicode scalars. A larger count returns the empty string.
    pub const STR_DROP: BuiltinFnId = 548;
    /// The first Unicode scalar, or null when empty.
    pub const STR_FIRST: BuiltinFnId = 549;
    /// The last Unicode scalar, or null when empty.
    pub const STR_LAST: BuiltinFnId = 550;
    /// True when the string starts with that character.
    pub const STR_STARTS_WITH_CHAR: BuiltinFnId = 551;
    /// True when the string ends with that character.
    pub const STR_ENDS_WITH_CHAR: BuiltinFnId = 552;
    /// True when the string contains that character.
    pub const STR_CONTAINS_CHAR: BuiltinFnId = 553;
    /// Replace every occurrence of a character.
    pub const STR_REPLACE_CHAR: BuiltinFnId = 554;
    /// Append every element of another list in place.
    pub const LIST_APPEND: BuiltinFnId = 555;
    /// Join String elements with a separator.
    pub const LIST_JOIN: BuiltinFnId = 556;
    /// True when a list has no elements.
    pub const LIST_IS_EMPTY: BuiltinFnId = 557;
    /// Split on `\n`, `\r\n`, and `\r`, dropping the endings.
    pub const STR_LINES: BuiltinFnId = 558;
    /// Split at the first separator match into a `(String, String)`.
    pub const STR_SPLIT_ONCE: BuiltinFnId = 559;
    /// Split at the last separator match into a `(String, String)`.
    pub const STR_RSPLIT_ONCE: BuiltinFnId = 560;
    /// Remove a matching prefix, or null when it is absent.
    pub const STR_STRIP_PREFIX: BuiltinFnId = 561;
    /// Remove a matching suffix, or null when it is absent.
    pub const STR_STRIP_SUFFIX: BuiltinFnId = 562;
    /// Occupied `(String, Any)` pairs in the same order as `keys`.
    pub const MAP_ENTRIES: BuiltinFnId = 563;
    /// True when a map has no occupied keys.
    pub const MAP_IS_EMPTY: BuiltinFnId = 564;
    /// Consecutive groups of `size`, keeping a shorter final group.
    pub const LIST_CHUNKS: BuiltinFnId = 565;
    /// `(i64, Any)` pairs of each index and element.
    pub const LIST_ENUMERATE: BuiltinFnId = 566;
    /// Concatenate one level of nested lists.
    pub const LIST_FLATTEN: BuiltinFnId = 567;
    /// Pair elements until the shorter list ends.
    pub const LIST_ZIP: BuiltinFnId = 568;
    /// Scalar index of the first matching character, or null.
    pub const STR_FIND_CHAR: BuiltinFnId = 569;
    /// Scalar index of the last matching character, or null.
    pub const STR_RFIND_CHAR: BuiltinFnId = 570;
    /// Non-overlapping count of a character.
    pub const STR_COUNT_CHAR: BuiltinFnId = 571;
    /// Independent map with the same keys and values.
    pub const MAP_COPY: BuiltinFnId = 572;
}

/// Static metadata for a builtin function (no function pointer).
#[derive(Debug, Clone, Copy)]
pub struct BuiltinFnMeta {
    pub id: BuiltinFnId,
    pub name: &'static str,
}

/// All builtin functions known to the compiler and runtime.
pub const BUILTIN_FN_META: &[BuiltinFnMeta] = &[
    BuiltinFnMeta {
        id: ids::DISPLAY_QUOTE,
        name: "__display_quote",
    },
    BuiltinFnMeta {
        id: ids::DISPLAY_ENTER_TUPLE,
        name: "__display_enter_tuple",
    },
    BuiltinFnMeta {
        id: ids::DISPLAY_EXIT_TUPLE,
        name: "__display_exit_tuple",
    },
    BuiltinFnMeta {
        id: ids::SCALAR_CMP,
        name: "__scalar_cmp",
    },
    BuiltinFnMeta {
        id: ids::SCALAR_EQ,
        name: "__scalar_eq",
    },
    BuiltinFnMeta {
        id: ids::PRINT,
        name: "print",
    },
    BuiltinFnMeta {
        id: ids::PRINTLN,
        name: "println",
    },
    BuiltinFnMeta {
        id: ids::TYPE_OF,
        name: "type_of",
    },
    BuiltinFnMeta {
        id: ids::TO_I64,
        name: "to_i64",
    },
    BuiltinFnMeta {
        id: ids::TO_F64,
        name: "to_f64",
    },
    BuiltinFnMeta {
        id: ids::TO_STRING,
        name: "to_string",
    },
    BuiltinFnMeta {
        id: ids::ABS,
        name: "abs",
    },
    BuiltinFnMeta {
        id: ids::SIN,
        name: "sin",
    },
    BuiltinFnMeta {
        id: ids::COS,
        name: "cos",
    },
    BuiltinFnMeta {
        id: ids::SQRT,
        name: "sqrt",
    },
    BuiltinFnMeta {
        id: ids::FLOOR,
        name: "floor",
    },
    BuiltinFnMeta {
        id: ids::CEIL,
        name: "ceil",
    },
    BuiltinFnMeta {
        id: ids::ROUND,
        name: "round",
    },
    BuiltinFnMeta {
        id: ids::POW,
        name: "pow",
    },
    BuiltinFnMeta {
        id: ids::LOG,
        name: "log",
    },
    BuiltinFnMeta {
        id: ids::STR_LEN,
        name: "str_len",
    },
    BuiltinFnMeta {
        id: ids::STR_CONCAT,
        name: "str_concat",
    },
    BuiltinFnMeta {
        id: ids::EXIT,
        name: "exit",
    },
    BuiltinFnMeta {
        id: ids::PANIC,
        name: "panic",
    },
    BuiltinFnMeta {
        id: ids::LIST_INIT,
        name: "__list_init",
    },
    BuiltinFnMeta {
        id: ids::LIST_LEN,
        name: "__list_len",
    },
    BuiltinFnMeta {
        id: ids::LIST_GET,
        name: "__list_get",
    },
    BuiltinFnMeta {
        id: ids::LIST_SET,
        name: "__list_set",
    },
    BuiltinFnMeta {
        id: ids::LIST_PUSH,
        name: "__list_push",
    },
    BuiltinFnMeta {
        id: ids::LIST_POP,
        name: "__list_pop",
    },
    BuiltinFnMeta {
        id: ids::MAP_INIT,
        name: "__map_init",
    },
    BuiltinFnMeta {
        id: ids::MAP_LEN,
        name: "__map_len",
    },
    BuiltinFnMeta {
        id: ids::MAP_GET,
        name: "__map_get",
    },
    BuiltinFnMeta {
        id: ids::MAP_SET,
        name: "__map_set",
    },
    BuiltinFnMeta {
        id: ids::MAP_REMOVE,
        name: "__map_remove",
    },
    BuiltinFnMeta {
        id: ids::MAP_CONTAINS,
        name: "__map_contains",
    },
    BuiltinFnMeta {
        id: ids::MAP_KEYS,
        name: "__map_keys",
    },
    BuiltinFnMeta {
        id: ids::STR_CONTAINS,
        name: "str_contains",
    },
    BuiltinFnMeta {
        id: ids::STR_STARTS_WITH,
        name: "str_starts_with",
    },
    BuiltinFnMeta {
        id: ids::STR_ENDS_WITH,
        name: "str_ends_with",
    },
    BuiltinFnMeta {
        id: ids::STR_TRIM,
        name: "str_trim",
    },
    BuiltinFnMeta {
        id: ids::STR_TRIM_START,
        name: "str_trim_start",
    },
    BuiltinFnMeta {
        id: ids::STR_TRIM_END,
        name: "str_trim_end",
    },
    BuiltinFnMeta {
        id: ids::STR_TO_UPPER,
        name: "str_to_upper",
    },
    BuiltinFnMeta {
        id: ids::STR_TO_LOWER,
        name: "str_to_lower",
    },
    BuiltinFnMeta {
        id: ids::STR_REPEAT,
        name: "str_repeat",
    },
    BuiltinFnMeta {
        id: ids::STR_REPLACE,
        name: "str_replace",
    },
    BuiltinFnMeta {
        id: ids::STR_SPLIT,
        name: "str_split",
    },
    BuiltinFnMeta {
        id: ids::STR_CHARS,
        name: "str_chars",
    },
    BuiltinFnMeta {
        id: ids::ITERATION_STEP_DONE,
        name: "__iteration_step_done",
    },
    BuiltinFnMeta {
        id: ids::NATIVE_EQUAL,
        name: "__native_equal",
    },
    BuiltinFnMeta {
        id: ids::TUPLE_LEN,
        name: "__tuple_len",
    },
    BuiltinFnMeta {
        id: ids::TUPLE_GET,
        name: "__tuple_get",
    },
    BuiltinFnMeta {
        id: ids::STR_CHAR_LEN,
        name: "str_char_len",
    },
    BuiltinFnMeta {
        id: ids::STR_CHAR_AT,
        name: "str_char_at",
    },
    BuiltinFnMeta {
        id: ids::STR_FIND,
        name: "str_find",
    },
    BuiltinFnMeta {
        id: ids::STR_SLICE,
        name: "str_slice",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_WHITESPACE,
        name: "char_is_whitespace",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_ALPHABETIC,
        name: "char_is_alphabetic",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_NUMERIC,
        name: "char_is_numeric",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_ALPHANUMERIC,
        name: "char_is_alphanumeric",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_UPPERCASE,
        name: "char_is_uppercase",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_LOWERCASE,
        name: "char_is_lowercase",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_ASCII,
        name: "char_is_ascii",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_ASCII_DIGIT,
        name: "char_is_ascii_digit",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_ASCII_ALPHABETIC,
        name: "char_is_ascii_alphabetic",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_ASCII_HEXDIGIT,
        name: "char_is_ascii_hexdigit",
    },
    BuiltinFnMeta {
        id: ids::CHAR_TO_ASCII_UPPERCASE,
        name: "char_to_ascii_uppercase",
    },
    BuiltinFnMeta {
        id: ids::CHAR_TO_ASCII_LOWERCASE,
        name: "char_to_ascii_lowercase",
    },
    BuiltinFnMeta {
        id: ids::MIN,
        name: "min",
    },
    BuiltinFnMeta {
        id: ids::MAX,
        name: "max",
    },
    BuiltinFnMeta {
        id: ids::CLAMP,
        name: "clamp",
    },
    BuiltinFnMeta {
        id: ids::STR_PARSE_I64,
        name: "str_parse_i64",
    },
    BuiltinFnMeta {
        id: ids::STR_PARSE_F64,
        name: "str_parse_f64",
    },
    BuiltinFnMeta {
        id: ids::SIGN,
        name: "sign",
    },
    BuiltinFnMeta {
        id: ids::STR_PAD_START,
        name: "str_pad_start",
    },
    BuiltinFnMeta {
        id: ids::STR_PAD_END,
        name: "str_pad_end",
    },
    BuiltinFnMeta {
        id: ids::CHAR_TO_CODE,
        name: "char_to_code",
    },
    BuiltinFnMeta {
        id: ids::CHAR_FROM_CODE,
        name: "char_from_code",
    },
    BuiltinFnMeta {
        id: ids::STR_RFIND,
        name: "str_rfind",
    },
    BuiltinFnMeta {
        id: ids::STR_COUNT,
        name: "str_count",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_ASCII,
        name: "str_is_ascii",
    },
    BuiltinFnMeta {
        id: ids::CHAR_INTO_STRING,
        name: "char_into_string",
    },
    BuiltinFnMeta {
        id: ids::EXP,
        name: "exp",
    },
    BuiltinFnMeta {
        id: ids::LOG2,
        name: "log2",
    },
    BuiltinFnMeta {
        id: ids::LOG10,
        name: "log10",
    },
    BuiltinFnMeta {
        id: ids::TRUNC,
        name: "trunc",
    },
    BuiltinFnMeta {
        id: ids::FRACT,
        name: "fract",
    },
    BuiltinFnMeta {
        id: ids::COPY_SIGN,
        name: "copysign",
    },
    BuiltinFnMeta {
        id: ids::ATAN2,
        name: "atan2",
    },
    BuiltinFnMeta {
        id: ids::HYPOT,
        name: "hypot",
    },
    BuiltinFnMeta {
        id: ids::TAN,
        name: "tan",
    },
    BuiltinFnMeta {
        id: ids::ASIN,
        name: "asin",
    },
    BuiltinFnMeta {
        id: ids::ACOS,
        name: "acos",
    },
    BuiltinFnMeta {
        id: ids::ATAN,
        name: "atan",
    },
    BuiltinFnMeta {
        id: ids::IS_NAN,
        name: "is_nan",
    },
    BuiltinFnMeta {
        id: ids::IS_INFINITE,
        name: "is_infinite",
    },
    BuiltinFnMeta {
        id: ids::IS_FINITE,
        name: "is_finite",
    },
    BuiltinFnMeta {
        id: ids::IS_SIGN_NEGATIVE,
        name: "is_sign_negative",
    },
    BuiltinFnMeta {
        id: ids::SINH,
        name: "sinh",
    },
    BuiltinFnMeta {
        id: ids::COSH,
        name: "cosh",
    },
    BuiltinFnMeta {
        id: ids::TANH,
        name: "tanh",
    },
    BuiltinFnMeta {
        id: ids::CBRT,
        name: "cbrt",
    },
    BuiltinFnMeta {
        id: ids::EXP2,
        name: "exp2",
    },
    BuiltinFnMeta {
        id: ids::EXPM1,
        name: "expm1",
    },
    BuiltinFnMeta {
        id: ids::LOG1P,
        name: "log1p",
    },
    BuiltinFnMeta {
        id: ids::STR_TO_ASCII_UPPERCASE,
        name: "str_to_ascii_uppercase",
    },
    BuiltinFnMeta {
        id: ids::STR_TO_ASCII_LOWERCASE,
        name: "str_to_ascii_lowercase",
    },
    BuiltinFnMeta {
        id: ids::STR_SPLIT_WHITESPACE,
        name: "str_split_whitespace",
    },
    BuiltinFnMeta {
        id: ids::CHAR_TO_UPPERCASE,
        name: "char_to_uppercase",
    },
    BuiltinFnMeta {
        id: ids::CHAR_TO_LOWERCASE,
        name: "char_to_lowercase",
    },
    BuiltinFnMeta {
        id: ids::COUNT_ONES,
        name: "count_ones",
    },
    BuiltinFnMeta {
        id: ids::COUNT_ZEROS,
        name: "count_zeros",
    },
    BuiltinFnMeta {
        id: ids::LEADING_ZEROS,
        name: "leading_zeros",
    },
    BuiltinFnMeta {
        id: ids::TRAILING_ZEROS,
        name: "trailing_zeros",
    },
    BuiltinFnMeta {
        id: ids::LEADING_ONES,
        name: "leading_ones",
    },
    BuiltinFnMeta {
        id: ids::TRAILING_ONES,
        name: "trailing_ones",
    },
    BuiltinFnMeta {
        id: ids::REVERSE_BITS,
        name: "reverse_bits",
    },
    BuiltinFnMeta {
        id: ids::SWAP_BYTES,
        name: "swap_bytes",
    },
    BuiltinFnMeta {
        id: ids::ROTATE_LEFT,
        name: "rotate_left",
    },
    BuiltinFnMeta {
        id: ids::ROTATE_RIGHT,
        name: "rotate_right",
    },
    BuiltinFnMeta {
        id: ids::IS_POWER_OF_TWO,
        name: "is_power_of_two",
    },
    BuiltinFnMeta {
        id: ids::ILOG2,
        name: "ilog2",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_ASCII_CONTROL,
        name: "char_is_ascii_control",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_ASCII_GRAPHIC,
        name: "char_is_ascii_graphic",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_ASCII_PUNCTUATION,
        name: "char_is_ascii_punctuation",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_ASCII_WHITESPACE,
        name: "char_is_ascii_whitespace",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_ASCII_ALPHANUMERIC,
        name: "char_is_ascii_alphanumeric",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_ASCII_UPPERCASE,
        name: "char_is_ascii_uppercase",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_ASCII_LOWERCASE,
        name: "char_is_ascii_lowercase",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_CONTROL,
        name: "char_is_control",
    },
    BuiltinFnMeta {
        id: ids::STR_TRIM_START_MATCHES,
        name: "str_trim_start_matches",
    },
    BuiltinFnMeta {
        id: ids::STR_TRIM_END_MATCHES,
        name: "str_trim_end_matches",
    },
    BuiltinFnMeta {
        id: ids::STR_TRIM_MATCHES,
        name: "str_trim_matches",
    },
    BuiltinFnMeta {
        id: ids::STR_REPLACEN,
        name: "str_replacen",
    },
    BuiltinFnMeta {
        id: ids::STR_SPLITN,
        name: "str_splitn",
    },
    BuiltinFnMeta {
        id: ids::STR_RSPLIT,
        name: "str_rsplit",
    },
    BuiltinFnMeta {
        id: ids::STR_RSPLITN,
        name: "str_rsplitn",
    },
    BuiltinFnMeta {
        id: ids::SIGNUM,
        name: "signum",
    },
    BuiltinFnMeta {
        id: ids::RECIP,
        name: "recip",
    },
    BuiltinFnMeta {
        id: ids::MUL_ADD,
        name: "mul_add",
    },
    BuiltinFnMeta {
        id: ids::DIV_EUCLID,
        name: "div_euclid",
    },
    BuiltinFnMeta {
        id: ids::REM_EUCLID,
        name: "rem_euclid",
    },
    BuiltinFnMeta {
        id: ids::CHAR_UTF8_LEN,
        name: "char_utf8_len",
    },
    BuiltinFnMeta {
        id: ids::CHAR_UTF16_LEN,
        name: "char_utf16_len",
    },
    BuiltinFnMeta {
        id: ids::CHAR_ESCAPE_DEBUG,
        name: "char_escape_debug",
    },
    BuiltinFnMeta {
        id: ids::CHAR_ESCAPE_UNICODE,
        name: "char_escape_unicode",
    },
    BuiltinFnMeta {
        id: ids::CHAR_ESCAPE_DEFAULT,
        name: "char_escape_default",
    },
    BuiltinFnMeta {
        id: ids::STR_ESCAPE_DEBUG,
        name: "str_escape_debug",
    },
    BuiltinFnMeta {
        id: ids::STR_ESCAPE_UNICODE,
        name: "str_escape_unicode",
    },
    BuiltinFnMeta {
        id: ids::STR_ESCAPE_DEFAULT,
        name: "str_escape_default",
    },
    BuiltinFnMeta {
        id: ids::STR_SPLIT_INCLUSIVE,
        name: "str_split_inclusive",
    },
    BuiltinFnMeta {
        id: ids::STR_FIND_ALL,
        name: "str_find_all",
    },
    BuiltinFnMeta {
        id: ids::TO_RADIANS,
        name: "to_radians",
    },
    BuiltinFnMeta {
        id: ids::TO_DEGREES,
        name: "to_degrees",
    },
    BuiltinFnMeta {
        id: ids::NEXT_UP,
        name: "next_up",
    },
    BuiltinFnMeta {
        id: ids::NEXT_DOWN,
        name: "next_down",
    },
    BuiltinFnMeta {
        id: ids::MIDPOINT,
        name: "midpoint",
    },
    BuiltinFnMeta {
        id: ids::ROUND_TIES_EVEN,
        name: "round_ties_even",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_DIGIT,
        name: "char_is_digit",
    },
    BuiltinFnMeta {
        id: ids::CHAR_TO_DIGIT,
        name: "char_to_digit",
    },
    BuiltinFnMeta {
        id: ids::STR_SPLIT_ASCII_WHITESPACE,
        name: "str_split_ascii_whitespace",
    },
    BuiltinFnMeta {
        id: ids::STR_EQ_IGNORE_ASCII_CASE,
        name: "str_eq_ignore_ascii_case",
    },
    BuiltinFnMeta {
        id: ids::STR_TRIM_ASCII,
        name: "str_trim_ascii",
    },
    BuiltinFnMeta {
        id: ids::STR_TRIM_ASCII_START,
        name: "str_trim_ascii_start",
    },
    BuiltinFnMeta {
        id: ids::STR_TRIM_ASCII_END,
        name: "str_trim_ascii_end",
    },
    BuiltinFnMeta {
        id: ids::ILOG10,
        name: "ilog10",
    },
    BuiltinFnMeta {
        id: ids::ABS_DIFF,
        name: "abs_diff",
    },
    BuiltinFnMeta {
        id: ids::WRAPPING_ADD,
        name: "wrapping_add",
    },
    BuiltinFnMeta {
        id: ids::WRAPPING_SUB,
        name: "wrapping_sub",
    },
    BuiltinFnMeta {
        id: ids::WRAPPING_MUL,
        name: "wrapping_mul",
    },
    BuiltinFnMeta {
        id: ids::WRAPPING_NEG,
        name: "wrapping_neg",
    },
    BuiltinFnMeta {
        id: ids::SATURATING_ADD,
        name: "saturating_add",
    },
    BuiltinFnMeta {
        id: ids::SATURATING_SUB,
        name: "saturating_sub",
    },
    BuiltinFnMeta {
        id: ids::SATURATING_MUL,
        name: "saturating_mul",
    },
    BuiltinFnMeta {
        id: ids::SATURATING_NEG,
        name: "saturating_neg",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_ADD,
        name: "checked_add",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_SUB,
        name: "checked_sub",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_MUL,
        name: "checked_mul",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_DIV,
        name: "checked_div",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_REM,
        name: "checked_rem",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_NEG,
        name: "checked_neg",
    },
    BuiltinFnMeta {
        id: ids::CHAR_FROM_DIGIT,
        name: "char_from_digit",
    },
    BuiltinFnMeta {
        id: ids::INT_DIV_EUCLID,
        name: "int_div_euclid",
    },
    BuiltinFnMeta {
        id: ids::INT_REM_EUCLID,
        name: "int_rem_euclid",
    },
    BuiltinFnMeta {
        id: ids::UNSIGNED_ABS,
        name: "unsigned_abs",
    },
    BuiltinFnMeta {
        id: ids::INT_MIDPOINT,
        name: "int_midpoint",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_SHL,
        name: "checked_shl",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_SHR,
        name: "checked_shr",
    },
    BuiltinFnMeta {
        id: ids::WRAPPING_SHL,
        name: "wrapping_shl",
    },
    BuiltinFnMeta {
        id: ids::WRAPPING_SHR,
        name: "wrapping_shr",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_POW,
        name: "checked_pow",
    },
    BuiltinFnMeta {
        id: ids::WRAPPING_POW,
        name: "wrapping_pow",
    },
    BuiltinFnMeta {
        id: ids::SATURATING_POW,
        name: "saturating_pow",
    },
    BuiltinFnMeta {
        id: ids::ASINH,
        name: "asinh",
    },
    BuiltinFnMeta {
        id: ids::ACOSH,
        name: "acosh",
    },
    BuiltinFnMeta {
        id: ids::ATANH,
        name: "atanh",
    },
    BuiltinFnMeta {
        id: ids::IS_NORMAL,
        name: "is_normal",
    },
    BuiltinFnMeta {
        id: ids::WRAPPING_DIV,
        name: "wrapping_div",
    },
    BuiltinFnMeta {
        id: ids::WRAPPING_REM,
        name: "wrapping_rem",
    },
    BuiltinFnMeta {
        id: ids::SATURATING_ABS,
        name: "saturating_abs",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_ABS,
        name: "checked_abs",
    },
    BuiltinFnMeta {
        id: ids::WRAPPING_ABS,
        name: "wrapping_abs",
    },
    BuiltinFnMeta {
        id: ids::INT_SIGNUM,
        name: "int_signum",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_ILOG,
        name: "checked_ilog",
    },
    BuiltinFnMeta {
        id: ids::CAST_UNSIGNED,
        name: "cast_unsigned",
    },
    BuiltinFnMeta {
        id: ids::CAST_SIGNED,
        name: "cast_signed",
    },
    BuiltinFnMeta {
        id: ids::IS_SUBNORMAL,
        name: "is_subnormal",
    },
    BuiltinFnMeta {
        id: ids::TO_BITS,
        name: "to_bits",
    },
    BuiltinFnMeta {
        id: ids::FROM_BITS,
        name: "from_bits",
    },
    BuiltinFnMeta {
        id: ids::POWI,
        name: "powi",
    },
    BuiltinFnMeta {
        id: ids::CHAR_ENCODE_UTF8,
        name: "char_encode_utf8",
    },
    BuiltinFnMeta {
        id: ids::CHAR_ENCODE_UTF16,
        name: "char_encode_utf16",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_DIV_EUCLID,
        name: "checked_div_euclid",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_REM_EUCLID,
        name: "checked_rem_euclid",
    },
    BuiltinFnMeta {
        id: ids::WIDENING_MUL,
        name: "widening_mul",
    },
    BuiltinFnMeta {
        id: ids::NEXT_POWER_OF_TWO,
        name: "next_power_of_two",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_NEXT_POWER_OF_TWO,
        name: "checked_next_power_of_two",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_ADD_SIGNED,
        name: "checked_add_signed",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_SUB_SIGNED,
        name: "checked_sub_signed",
    },
    BuiltinFnMeta {
        id: ids::IS_MULTIPLE_OF,
        name: "is_multiple_of",
    },
    BuiltinFnMeta {
        id: ids::LOG_BASE,
        name: "log_base",
    },
    BuiltinFnMeta {
        id: ids::STR_ENCODE_UTF8,
        name: "str_encode_utf8",
    },
    BuiltinFnMeta {
        id: ids::STR_ENCODE_UTF16,
        name: "str_encode_utf16",
    },
    BuiltinFnMeta {
        id: ids::STR_FROM_UTF8,
        name: "str_from_utf8",
    },
    BuiltinFnMeta {
        id: ids::STR_FROM_UTF16,
        name: "str_from_utf16",
    },
    BuiltinFnMeta {
        id: ids::STR_FROM_UTF8_LOSSY,
        name: "str_from_utf8_lossy",
    },
    BuiltinFnMeta {
        id: ids::STR_FROM_UTF16_LOSSY,
        name: "str_from_utf16_lossy",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_CHAR_BOUNDARY,
        name: "str_is_char_boundary",
    },
    BuiltinFnMeta {
        id: ids::STR_FLOOR_CHAR_BOUNDARY,
        name: "str_floor_char_boundary",
    },
    BuiltinFnMeta {
        id: ids::STR_CEIL_CHAR_BOUNDARY,
        name: "str_ceil_char_boundary",
    },
    BuiltinFnMeta {
        id: ids::PI,
        name: "pi",
    },
    BuiltinFnMeta {
        id: ids::E,
        name: "euler",
    },
    BuiltinFnMeta {
        id: ids::TAU,
        name: "tau",
    },
    BuiltinFnMeta {
        id: ids::LN_2,
        name: "ln_2",
    },
    BuiltinFnMeta {
        id: ids::LN_10,
        name: "ln_10",
    },
    BuiltinFnMeta {
        id: ids::LOG2_E,
        name: "log2_e",
    },
    BuiltinFnMeta {
        id: ids::LOG10_E,
        name: "log10_e",
    },
    BuiltinFnMeta {
        id: ids::SQRT_2,
        name: "sqrt_2",
    },
    BuiltinFnMeta {
        id: ids::INFINITY,
        name: "infinity",
    },
    BuiltinFnMeta {
        id: ids::NEG_INFINITY,
        name: "neg_infinity",
    },
    BuiltinFnMeta {
        id: ids::NAN,
        name: "nan",
    },
    BuiltinFnMeta {
        id: ids::EPSILON,
        name: "epsilon",
    },
    BuiltinFnMeta {
        id: ids::STRICT_ADD,
        name: "strict_add",
    },
    BuiltinFnMeta {
        id: ids::STRICT_SUB,
        name: "strict_sub",
    },
    BuiltinFnMeta {
        id: ids::STRICT_MUL,
        name: "strict_mul",
    },
    BuiltinFnMeta {
        id: ids::STRICT_DIV,
        name: "strict_div",
    },
    BuiltinFnMeta {
        id: ids::STRICT_REM,
        name: "strict_rem",
    },
    BuiltinFnMeta {
        id: ids::STRICT_NEG,
        name: "strict_neg",
    },
    BuiltinFnMeta {
        id: ids::STRICT_SHL,
        name: "strict_shl",
    },
    BuiltinFnMeta {
        id: ids::STRICT_SHR,
        name: "strict_shr",
    },
    BuiltinFnMeta {
        id: ids::STRICT_POW,
        name: "strict_pow",
    },
    BuiltinFnMeta {
        id: ids::MIN_POSITIVE,
        name: "min_positive",
    },
    BuiltinFnMeta {
        id: ids::FRAC_PI_2,
        name: "frac_pi_2",
    },
    BuiltinFnMeta {
        id: ids::FRAC_PI_4,
        name: "frac_pi_4",
    },
    BuiltinFnMeta {
        id: ids::FRAC_1_SQRT_2,
        name: "frac_1_sqrt_2",
    },
    BuiltinFnMeta {
        id: ids::LOG2_10,
        name: "log2_10",
    },
    BuiltinFnMeta {
        id: ids::LOG10_2,
        name: "log10_2",
    },
    BuiltinFnMeta {
        id: ids::SATURATING_DIV,
        name: "saturating_div",
    },
    BuiltinFnMeta {
        id: ids::SATURATING_DIV_EUCLID,
        name: "saturating_div_euclid",
    },
    BuiltinFnMeta {
        id: ids::TRUNC_I64,
        name: "trunc_i64",
    },
    BuiltinFnMeta {
        id: ids::TRUNC_U64,
        name: "trunc_u64",
    },
    BuiltinFnMeta {
        id: ids::TRUNC_I128,
        name: "trunc_i128",
    },
    BuiltinFnMeta {
        id: ids::TRUNC_U128,
        name: "trunc_u128",
    },
    BuiltinFnMeta {
        id: ids::CHAR_IS_ASCII_OCTDIGIT,
        name: "char_is_ascii_octdigit",
    },
    BuiltinFnMeta {
        id: ids::FRAC_1_PI,
        name: "frac_1_pi",
    },
    BuiltinFnMeta {
        id: ids::FRAC_2_PI,
        name: "frac_2_pi",
    },
    BuiltinFnMeta {
        id: ids::FRAC_2_SQRT_PI,
        name: "frac_2_sqrt_pi",
    },
    BuiltinFnMeta {
        id: ids::FRAC_PI_3,
        name: "frac_pi_3",
    },
    BuiltinFnMeta {
        id: ids::FRAC_PI_6,
        name: "frac_pi_6",
    },
    BuiltinFnMeta {
        id: ids::FRAC_PI_8,
        name: "frac_pi_8",
    },
    BuiltinFnMeta {
        id: ids::F64_MIN,
        name: "f64_min",
    },
    BuiltinFnMeta {
        id: ids::F64_MAX,
        name: "f64_max",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_ADD_UNSIGNED,
        name: "checked_add_unsigned",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_SUB_UNSIGNED,
        name: "checked_sub_unsigned",
    },
    BuiltinFnMeta {
        id: ids::SATURATING_ADD_UNSIGNED,
        name: "saturating_add_unsigned",
    },
    BuiltinFnMeta {
        id: ids::SATURATING_SUB_UNSIGNED,
        name: "saturating_sub_unsigned",
    },
    BuiltinFnMeta {
        id: ids::WRAPPING_ADD_UNSIGNED,
        name: "wrapping_add_unsigned",
    },
    BuiltinFnMeta {
        id: ids::WRAPPING_SUB_UNSIGNED,
        name: "wrapping_sub_unsigned",
    },
    BuiltinFnMeta {
        id: ids::STRICT_ADD_UNSIGNED,
        name: "strict_add_unsigned",
    },
    BuiltinFnMeta {
        id: ids::STRICT_SUB_UNSIGNED,
        name: "strict_sub_unsigned",
    },
    BuiltinFnMeta {
        id: ids::INT_TO_STR_RADIX,
        name: "int_to_str_radix",
    },
    BuiltinFnMeta {
        id: ids::I64_FROM_STR_RADIX,
        name: "i64_from_str_radix",
    },
    BuiltinFnMeta {
        id: ids::U64_FROM_STR_RADIX,
        name: "u64_from_str_radix",
    },
    BuiltinFnMeta {
        id: ids::I128_FROM_STR_RADIX,
        name: "i128_from_str_radix",
    },
    BuiltinFnMeta {
        id: ids::U128_FROM_STR_RADIX,
        name: "u128_from_str_radix",
    },
    BuiltinFnMeta {
        id: ids::STR_CHAR_AT_BYTE,
        name: "str_char_at_byte",
    },
    BuiltinFnMeta {
        id: ids::STR_SPLIT_AT,
        name: "str_split_at",
    },
    BuiltinFnMeta {
        id: ids::DIV_CEIL,
        name: "div_ceil",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_DIV_CEIL,
        name: "checked_div_ceil",
    },
    BuiltinFnMeta {
        id: ids::NEXT_MULTIPLE_OF,
        name: "next_multiple_of",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_NEXT_MULTIPLE_OF,
        name: "checked_next_multiple_of",
    },
    BuiltinFnMeta {
        id: ids::INT_TO_BE_BYTES,
        name: "int_to_be_bytes",
    },
    BuiltinFnMeta {
        id: ids::INT_TO_LE_BYTES,
        name: "int_to_le_bytes",
    },
    BuiltinFnMeta {
        id: ids::I64_FROM_BE_BYTES,
        name: "i64_from_be_bytes",
    },
    BuiltinFnMeta {
        id: ids::I64_FROM_LE_BYTES,
        name: "i64_from_le_bytes",
    },
    BuiltinFnMeta {
        id: ids::U64_FROM_BE_BYTES,
        name: "u64_from_be_bytes",
    },
    BuiltinFnMeta {
        id: ids::U64_FROM_LE_BYTES,
        name: "u64_from_le_bytes",
    },
    BuiltinFnMeta {
        id: ids::I128_FROM_BE_BYTES,
        name: "i128_from_be_bytes",
    },
    BuiltinFnMeta {
        id: ids::I128_FROM_LE_BYTES,
        name: "i128_from_le_bytes",
    },
    BuiltinFnMeta {
        id: ids::U128_FROM_BE_BYTES,
        name: "u128_from_be_bytes",
    },
    BuiltinFnMeta {
        id: ids::U128_FROM_LE_BYTES,
        name: "u128_from_le_bytes",
    },
    BuiltinFnMeta {
        id: ids::TOTAL_CMP,
        name: "total_cmp",
    },
    BuiltinFnMeta {
        id: ids::DIV_FLOOR,
        name: "div_floor",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_DIV_FLOOR,
        name: "checked_div_floor",
    },
    BuiltinFnMeta {
        id: ids::IS_SIGN_POSITIVE,
        name: "is_sign_positive",
    },
    BuiltinFnMeta {
        id: ids::F64_TO_BE_BYTES,
        name: "f64_to_be_bytes",
    },
    BuiltinFnMeta {
        id: ids::F64_TO_LE_BYTES,
        name: "f64_to_le_bytes",
    },
    BuiltinFnMeta {
        id: ids::F64_FROM_BE_BYTES,
        name: "f64_from_be_bytes",
    },
    BuiltinFnMeta {
        id: ids::F64_FROM_LE_BYTES,
        name: "f64_from_le_bytes",
    },
    BuiltinFnMeta {
        id: ids::F64_RADIX,
        name: "f64_radix",
    },
    BuiltinFnMeta {
        id: ids::F64_MANTISSA_DIGITS,
        name: "f64_mantissa_digits",
    },
    BuiltinFnMeta {
        id: ids::F64_DIGITS,
        name: "f64_digits",
    },
    BuiltinFnMeta {
        id: ids::F64_MIN_EXP,
        name: "f64_min_exp",
    },
    BuiltinFnMeta {
        id: ids::F64_MAX_EXP,
        name: "f64_max_exp",
    },
    BuiltinFnMeta {
        id: ids::STR_BYTE_AT,
        name: "str_byte_at",
    },
    BuiltinFnMeta {
        id: ids::STR_GET,
        name: "str_get",
    },
    BuiltinFnMeta {
        id: ids::STR_SPLIT_AT_CHECKED,
        name: "str_split_at_checked",
    },
    BuiltinFnMeta {
        id: ids::STR_PARSE_U64,
        name: "str_parse_u64",
    },
    BuiltinFnMeta {
        id: ids::STR_PARSE_I128,
        name: "str_parse_i128",
    },
    BuiltinFnMeta {
        id: ids::STR_PARSE_U128,
        name: "str_parse_u128",
    },
    BuiltinFnMeta {
        id: ids::F64_MIN_10_EXP,
        name: "f64_min_10_exp",
    },
    BuiltinFnMeta {
        id: ids::F64_MAX_10_EXP,
        name: "f64_max_10_exp",
    },
    BuiltinFnMeta {
        id: ids::STR_SPLIT_TERMINATOR,
        name: "str_split_terminator",
    },
    BuiltinFnMeta {
        id: ids::STR_RSPLIT_TERMINATOR,
        name: "str_rsplit_terminator",
    },
    BuiltinFnMeta {
        id: ids::INT_GCD,
        name: "int_gcd",
    },
    BuiltinFnMeta {
        id: ids::INT_LCM,
        name: "int_lcm",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_INT_LCM,
        name: "checked_int_lcm",
    },
    BuiltinFnMeta {
        id: ids::STR_BYTE_FIND,
        name: "str_byte_find",
    },
    BuiltinFnMeta {
        id: ids::STR_BYTE_RFIND,
        name: "str_byte_rfind",
    },
    BuiltinFnMeta {
        id: ids::STR_BYTE_COUNT,
        name: "str_byte_count",
    },
    BuiltinFnMeta {
        id: ids::CHAR_REPLACEMENT,
        name: "char_replacement",
    },
    BuiltinFnMeta {
        id: ids::CHAR_MAX,
        name: "char_max",
    },
    BuiltinFnMeta {
        id: ids::SATURATING_ADD_SIGNED,
        name: "saturating_add_signed",
    },
    BuiltinFnMeta {
        id: ids::SATURATING_SUB_SIGNED,
        name: "saturating_sub_signed",
    },
    BuiltinFnMeta {
        id: ids::WRAPPING_ADD_SIGNED,
        name: "wrapping_add_signed",
    },
    BuiltinFnMeta {
        id: ids::WRAPPING_SUB_SIGNED,
        name: "wrapping_sub_signed",
    },
    BuiltinFnMeta {
        id: ids::STRICT_ADD_SIGNED,
        name: "strict_add_signed",
    },
    BuiltinFnMeta {
        id: ids::STRICT_SUB_SIGNED,
        name: "strict_sub_signed",
    },
    BuiltinFnMeta {
        id: ids::UNBOUNDED_SHL,
        name: "unbounded_shl",
    },
    BuiltinFnMeta {
        id: ids::UNBOUNDED_SHR,
        name: "unbounded_shr",
    },
    BuiltinFnMeta {
        id: ids::I64_MIN,
        name: "i64_min",
    },
    BuiltinFnMeta {
        id: ids::I64_MAX,
        name: "i64_max",
    },
    BuiltinFnMeta {
        id: ids::U64_MAX,
        name: "u64_max",
    },
    BuiltinFnMeta {
        id: ids::I128_MIN,
        name: "i128_min",
    },
    BuiltinFnMeta {
        id: ids::I128_MAX,
        name: "i128_max",
    },
    BuiltinFnMeta {
        id: ids::U128_MAX,
        name: "u128_max",
    },
    BuiltinFnMeta {
        id: ids::CHAR_EQ_IGNORE_ASCII_CASE,
        name: "char_eq_ignore_ascii_case",
    },
    BuiltinFnMeta {
        id: ids::HIGHEST_ONE,
        name: "highest_one",
    },
    BuiltinFnMeta {
        id: ids::LOWEST_ONE,
        name: "lowest_one",
    },
    BuiltinFnMeta {
        id: ids::ISOLATE_HIGHEST_ONE,
        name: "isolate_highest_one",
    },
    BuiltinFnMeta {
        id: ids::ISOLATE_LOWEST_ONE,
        name: "isolate_lowest_one",
    },
    BuiltinFnMeta {
        id: ids::INT_BIT_WIDTH,
        name: "int_bit_width",
    },
    BuiltinFnMeta {
        id: ids::I64_BITS,
        name: "i64_bits",
    },
    BuiltinFnMeta {
        id: ids::I128_BITS,
        name: "i128_bits",
    },
    BuiltinFnMeta {
        id: ids::ISIZE_BITS,
        name: "isize_bits",
    },
    BuiltinFnMeta {
        id: ids::U128_BITS,
        name: "u128_bits",
    },
    BuiltinFnMeta {
        id: ids::USIZE_BITS,
        name: "usize_bits",
    },
    BuiltinFnMeta {
        id: ids::CHAR_MIN,
        name: "char_min",
    },
    BuiltinFnMeta {
        id: ids::STR_FIND_INDICES,
        name: "str_find_indices",
    },
    BuiltinFnMeta {
        id: ids::STR_RFIND_INDICES,
        name: "str_rfind_indices",
    },
    BuiltinFnMeta {
        id: ids::STR_BYTE_FIND_INDICES,
        name: "str_byte_find_indices",
    },
    BuiltinFnMeta {
        id: ids::STR_BYTE_RFIND_INDICES,
        name: "str_byte_rfind_indices",
    },
    BuiltinFnMeta {
        id: ids::INT_ISQRT,
        name: "isqrt",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_INT_ISQRT,
        name: "checked_isqrt",
    },
    BuiltinFnMeta {
        id: ids::STRICT_ABS,
        name: "strict_abs",
    },
    BuiltinFnMeta {
        id: ids::WRAPPING_DIV_EUCLID,
        name: "wrapping_div_euclid",
    },
    BuiltinFnMeta {
        id: ids::WRAPPING_REM_EUCLID,
        name: "wrapping_rem_euclid",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_SIGNED_DIFF,
        name: "checked_signed_diff",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_DIV_EXACT,
        name: "checked_div_exact",
    },
    BuiltinFnMeta {
        id: ids::STRICT_DIV_EXACT,
        name: "strict_div_exact",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_SHL_EXACT,
        name: "checked_shl_exact",
    },
    BuiltinFnMeta {
        id: ids::STRICT_SHL_EXACT,
        name: "strict_shl_exact",
    },
    BuiltinFnMeta {
        id: ids::CHECKED_SHR_EXACT,
        name: "checked_shr_exact",
    },
    BuiltinFnMeta {
        id: ids::STRICT_SHR_EXACT,
        name: "strict_shr_exact",
    },
    BuiltinFnMeta {
        id: ids::STR_TRIM_PREFIX,
        name: "str_trim_prefix",
    },
    BuiltinFnMeta {
        id: ids::STR_TRIM_SUFFIX,
        name: "str_trim_suffix",
    },
    BuiltinFnMeta {
        id: ids::STR_STRIP_CIRCUMFIX,
        name: "str_strip_circumfix",
    },
    BuiltinFnMeta {
        id: ids::LIST_WINDOWS,
        name: "list_windows",
    },
    BuiltinFnMeta {
        id: ids::LIST_CHUNKS_EXACT,
        name: "list_chunks_exact",
    },
    BuiltinFnMeta {
        id: ids::LIST_RCHUNKS,
        name: "list_rchunks",
    },
    BuiltinFnMeta {
        id: ids::LIST_RCHUNKS_EXACT,
        name: "list_rchunks_exact",
    },
    BuiltinFnMeta {
        id: ids::LIST_CHUNKS_REMAINDER,
        name: "list_chunks_remainder",
    },
    BuiltinFnMeta {
        id: ids::LIST_RCHUNKS_REMAINDER,
        name: "list_rchunks_remainder",
    },
    BuiltinFnMeta {
        id: ids::LIST_SPLIT_AT,
        name: "list_split_at",
    },
    BuiltinFnMeta {
        id: ids::LIST_ROTATE_LEFT,
        name: "list_rotate_left",
    },
    BuiltinFnMeta {
        id: ids::LIST_ROTATE_RIGHT,
        name: "list_rotate_right",
    },
    BuiltinFnMeta {
        id: ids::LIST_REPEAT,
        name: "list_repeat",
    },
    BuiltinFnMeta {
        id: ids::LIST_TRUNCATE,
        name: "list_truncate",
    },
    BuiltinFnMeta {
        id: ids::LIST_FILL,
        name: "list_fill",
    },
    BuiltinFnMeta {
        id: ids::LIST_SWAP_REMOVE,
        name: "list_swap_remove",
    },
    BuiltinFnMeta {
        id: ids::LIST_SPLIT_OFF,
        name: "list_split_off",
    },
    BuiltinFnMeta {
        id: ids::LIST_RESIZE,
        name: "list_resize",
    },
    BuiltinFnMeta {
        id: ids::MAP_VALUES,
        name: "map_values",
    },
    BuiltinFnMeta {
        id: ids::MAP_CLEAR,
        name: "map_clear",
    },
    BuiltinFnMeta {
        id: ids::MAP_MERGE,
        name: "map_merge",
    },
    BuiltinFnMeta {
        id: ids::MAP_GET_OR_INSERT,
        name: "map_get_or_insert",
    },
    BuiltinFnMeta {
        id: ids::MAP_INSERT_ABSENT,
        name: "map_insert_absent",
    },
    BuiltinFnMeta {
        id: ids::GOLDEN_RATIO,
        name: "golden_ratio",
    },
    BuiltinFnMeta {
        id: ids::EULER_GAMMA,
        name: "euler_gamma",
    },
    BuiltinFnMeta {
        id: ids::F64_CLASSIFY,
        name: "f64_classify",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_ASCII_DIGIT,
        name: "str_is_ascii_digit",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_ASCII_ALPHABETIC,
        name: "str_is_ascii_alphabetic",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_ASCII_ALPHANUMERIC,
        name: "str_is_ascii_alphanumeric",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_ASCII_LOWERCASE,
        name: "str_is_ascii_lowercase",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_ASCII_UPPERCASE,
        name: "str_is_ascii_uppercase",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_ASCII_WHITESPACE,
        name: "str_is_ascii_whitespace",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_ASCII_HEXDIGIT,
        name: "str_is_ascii_hexdigit",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_NUMERIC,
        name: "str_is_numeric",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_ALPHABETIC,
        name: "str_is_alphabetic",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_ALPHANUMERIC,
        name: "str_is_alphanumeric",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_LOWERCASE,
        name: "str_is_lowercase",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_UPPERCASE,
        name: "str_is_uppercase",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_WHITESPACE,
        name: "str_is_whitespace",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_CONTROL,
        name: "str_is_control",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_ASCII_GRAPHIC,
        name: "str_is_ascii_graphic",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_ASCII_PUNCTUATION,
        name: "str_is_ascii_punctuation",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_ASCII_CONTROL,
        name: "str_is_ascii_control",
    },
    BuiltinFnMeta {
        id: ids::LIST_COPY,
        name: "list_copy",
    },
    BuiltinFnMeta {
        id: ids::LIST_REVERSE,
        name: "list_reverse",
    },
    BuiltinFnMeta {
        id: ids::LIST_CONCAT,
        name: "list_concat",
    },
    BuiltinFnMeta {
        id: ids::LIST_SWAP,
        name: "list_swap",
    },
    BuiltinFnMeta {
        id: ids::LIST_CLEAR,
        name: "list_clear",
    },
    BuiltinFnMeta {
        id: ids::LIST_INSERT,
        name: "list_insert",
    },
    BuiltinFnMeta {
        id: ids::LIST_REMOVE,
        name: "list_remove",
    },
    BuiltinFnMeta {
        id: ids::LIST_TAKE,
        name: "list_take",
    },
    BuiltinFnMeta {
        id: ids::LIST_DROP,
        name: "list_drop",
    },
    BuiltinFnMeta {
        id: ids::LIST_FIRST,
        name: "list_first",
    },
    BuiltinFnMeta {
        id: ids::LIST_LAST,
        name: "list_last",
    },
    BuiltinFnMeta {
        id: ids::RANGE_LEN,
        name: "range_len",
    },
    BuiltinFnMeta {
        id: ids::RANGE_INCLUSIVE_LEN,
        name: "range_inclusive_len",
    },
    BuiltinFnMeta {
        id: ids::RANGE_IS_EMPTY,
        name: "range_is_empty",
    },
    BuiltinFnMeta {
        id: ids::RANGE_INCLUSIVE_IS_EMPTY,
        name: "range_inclusive_is_empty",
    },
    BuiltinFnMeta {
        id: ids::RANGE_CONTAINS,
        name: "range_contains",
    },
    BuiltinFnMeta {
        id: ids::RANGE_INCLUSIVE_CONTAINS,
        name: "range_inclusive_contains",
    },
    BuiltinFnMeta {
        id: ids::RANGE_FROM_CONTAINS,
        name: "range_from_contains",
    },
    BuiltinFnMeta {
        id: ids::RANGE_TO_CONTAINS,
        name: "range_to_contains",
    },
    BuiltinFnMeta {
        id: ids::RANGE_TO_INCLUSIVE_CONTAINS,
        name: "range_to_inclusive_contains",
    },
    BuiltinFnMeta {
        id: ids::F64_TO_NE_BYTES,
        name: "f64_to_ne_bytes",
    },
    BuiltinFnMeta {
        id: ids::F64_FROM_NE_BYTES,
        name: "f64_from_ne_bytes",
    },
    BuiltinFnMeta {
        id: ids::ALGEBRAIC_ADD,
        name: "algebraic_add",
    },
    BuiltinFnMeta {
        id: ids::ALGEBRAIC_SUB,
        name: "algebraic_sub",
    },
    BuiltinFnMeta {
        id: ids::ALGEBRAIC_MUL,
        name: "algebraic_mul",
    },
    BuiltinFnMeta {
        id: ids::ALGEBRAIC_DIV,
        name: "algebraic_div",
    },
    BuiltinFnMeta {
        id: ids::ALGEBRAIC_REM,
        name: "algebraic_rem",
    },
    BuiltinFnMeta {
        id: ids::F64_NAN_TO,
        name: "f64_nan_to",
    },
    BuiltinFnMeta {
        id: ids::F64_ABS_DIFF,
        name: "f64_abs_diff",
    },
    BuiltinFnMeta {
        id: ids::F64_IS_ZERO,
        name: "f64_is_zero",
    },
    BuiltinFnMeta {
        id: ids::BOOL_NOT,
        name: "bool_not",
    },
    BuiltinFnMeta {
        id: ids::BOOL_AND,
        name: "bool_and",
    },
    BuiltinFnMeta {
        id: ids::BOOL_OR,
        name: "bool_or",
    },
    BuiltinFnMeta {
        id: ids::BOOL_XOR,
        name: "bool_xor",
    },
    BuiltinFnMeta {
        id: ids::BOOL_THEN,
        name: "bool_then",
    },
    BuiltinFnMeta {
        id: ids::STR_BYTES,
        name: "str_bytes",
    },
    BuiltinFnMeta {
        id: ids::STR_CHAR_INDICES,
        name: "str_char_indices",
    },
    BuiltinFnMeta {
        id: ids::STR_IS_EMPTY,
        name: "str_is_empty",
    },
    BuiltinFnMeta {
        id: ids::STR_REPEAT_CHAR,
        name: "str_repeat_char",
    },
    BuiltinFnMeta {
        id: ids::STR_TAKE,
        name: "str_take",
    },
    BuiltinFnMeta {
        id: ids::STR_DROP,
        name: "str_drop",
    },
    BuiltinFnMeta {
        id: ids::STR_FIRST,
        name: "str_first",
    },
    BuiltinFnMeta {
        id: ids::STR_LAST,
        name: "str_last",
    },
    BuiltinFnMeta {
        id: ids::STR_STARTS_WITH_CHAR,
        name: "str_starts_with_char",
    },
    BuiltinFnMeta {
        id: ids::STR_ENDS_WITH_CHAR,
        name: "str_ends_with_char",
    },
    BuiltinFnMeta {
        id: ids::STR_CONTAINS_CHAR,
        name: "str_contains_char",
    },
    BuiltinFnMeta {
        id: ids::STR_REPLACE_CHAR,
        name: "str_replace_char",
    },
    BuiltinFnMeta {
        id: ids::LIST_APPEND,
        name: "list_append",
    },
    BuiltinFnMeta {
        id: ids::LIST_JOIN,
        name: "list_join",
    },
    BuiltinFnMeta {
        id: ids::LIST_IS_EMPTY,
        name: "list_is_empty",
    },
    BuiltinFnMeta {
        id: ids::STR_LINES,
        name: "str_lines",
    },
    BuiltinFnMeta {
        id: ids::STR_SPLIT_ONCE,
        name: "str_split_once",
    },
    BuiltinFnMeta {
        id: ids::STR_RSPLIT_ONCE,
        name: "str_rsplit_once",
    },
    BuiltinFnMeta {
        id: ids::STR_STRIP_PREFIX,
        name: "str_strip_prefix",
    },
    BuiltinFnMeta {
        id: ids::STR_STRIP_SUFFIX,
        name: "str_strip_suffix",
    },
    BuiltinFnMeta {
        id: ids::MAP_ENTRIES,
        name: "map_entries",
    },
    BuiltinFnMeta {
        id: ids::MAP_IS_EMPTY,
        name: "map_is_empty",
    },
    BuiltinFnMeta {
        id: ids::LIST_CHUNKS,
        name: "list_chunks",
    },
    BuiltinFnMeta {
        id: ids::LIST_ENUMERATE,
        name: "list_enumerate",
    },
    BuiltinFnMeta {
        id: ids::LIST_FLATTEN,
        name: "list_flatten",
    },
    BuiltinFnMeta {
        id: ids::LIST_ZIP,
        name: "list_zip",
    },
    BuiltinFnMeta {
        id: ids::STR_FIND_CHAR,
        name: "str_find_char",
    },
    BuiltinFnMeta {
        id: ids::STR_RFIND_CHAR,
        name: "str_rfind_char",
    },
    BuiltinFnMeta {
        id: ids::STR_COUNT_CHAR,
        name: "str_count_char",
    },
    BuiltinFnMeta {
        id: ids::MAP_COPY,
        name: "map_copy",
    },
];

/// Look up a builtin function id by its language-facing name.
pub fn lookup_builtin_fn_id(name: &str) -> Option<BuiltinFnId> {
    BUILTIN_FN_META
        .iter()
        .find(|m| m.name == name)
        .map(|m| m.id)
}

/// Look up builtin function metadata by id.
pub fn lookup_builtin_fn_meta(id: BuiltinFnId) -> Option<&'static BuiltinFnMeta> {
    BUILTIN_FN_META.iter().find(|m| m.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fn_ids_are_stable_and_named() {
        assert_eq!(lookup_builtin_fn_id("print"), Some(ids::PRINT));
        assert_eq!(lookup_builtin_fn_id("str_concat"), Some(ids::STR_CONCAT));
        assert_eq!(lookup_builtin_fn_id("nope"), None);
        assert_eq!(
            lookup_builtin_fn_meta(ids::LIST_INIT).unwrap().name,
            "__list_init"
        );
    }
}
