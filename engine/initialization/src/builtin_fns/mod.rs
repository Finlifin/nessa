//! Builtin function implementations registered at engine init.

mod convert;
mod int;
mod io;
mod list;
mod map;
mod math;
mod process;
mod string;

use interpreter::Vm;
use runtime::ids;

/// Register all builtin functions into the VM.
pub fn register_all(vm: &mut Vm) {
    vm.register_builtin(ids::PRINT, io::print);
    vm.register_builtin(ids::PRINTLN, io::println);

    vm.register_builtin(ids::TYPE_OF, convert::type_of);
    vm.register_builtin(ids::TO_I64, convert::to_i64);
    vm.register_builtin(ids::TO_F64, convert::to_f64);
    vm.register_builtin(ids::TO_STRING, convert::to_string);
    vm.register_builtin(ids::SCALAR_EQ, convert::scalar_eq);
    vm.register_builtin(ids::SCALAR_CMP, convert::scalar_cmp);
    vm.register_builtin(ids::ITERATION_STEP_DONE, convert::iteration_step_done);
    vm.register_builtin(ids::NATIVE_EQUAL, convert::native_equal);
    vm.register_builtin(ids::TUPLE_LEN, convert::tuple_len);
    vm.register_builtin(ids::TUPLE_GET, convert::tuple_get);

    vm.register_builtin(ids::ABS, math::abs);
    vm.register_builtin(ids::SIN, math::sin);
    vm.register_builtin(ids::COS, math::cos);
    vm.register_builtin(ids::SQRT, math::sqrt);
    vm.register_builtin(ids::FLOOR, math::floor);
    vm.register_builtin(ids::CEIL, math::ceil);
    vm.register_builtin(ids::ROUND, math::round);
    vm.register_builtin(ids::POW, math::pow);
    vm.register_builtin(ids::LOG, math::log);
    vm.register_builtin(ids::MIN, math::min);
    vm.register_builtin(ids::MAX, math::max);
    vm.register_builtin(ids::CLAMP, math::clamp);
    vm.register_builtin(ids::SIGN, math::sign);
    vm.register_builtin(ids::EXP, math::exp);
    vm.register_builtin(ids::LOG2, math::log2);
    vm.register_builtin(ids::LOG10, math::log10);
    vm.register_builtin(ids::TRUNC, math::trunc);
    vm.register_builtin(ids::FRACT, math::fract);
    vm.register_builtin(ids::COPY_SIGN, math::copysign);
    vm.register_builtin(ids::ATAN2, math::atan2);
    vm.register_builtin(ids::HYPOT, math::hypot);
    vm.register_builtin(ids::TAN, math::tan);
    vm.register_builtin(ids::ASIN, math::asin);
    vm.register_builtin(ids::ACOS, math::acos);
    vm.register_builtin(ids::ATAN, math::atan);
    vm.register_builtin(ids::IS_NAN, math::is_nan);
    vm.register_builtin(ids::IS_INFINITE, math::is_infinite);
    vm.register_builtin(ids::IS_FINITE, math::is_finite);
    vm.register_builtin(ids::IS_SIGN_NEGATIVE, math::is_sign_negative);
    vm.register_builtin(ids::SINH, math::sinh);
    vm.register_builtin(ids::COSH, math::cosh);
    vm.register_builtin(ids::TANH, math::tanh);
    vm.register_builtin(ids::CBRT, math::cbrt);
    vm.register_builtin(ids::EXP2, math::exp2);
    vm.register_builtin(ids::EXPM1, math::expm1);
    vm.register_builtin(ids::LOG1P, math::log1p);
    vm.register_builtin(ids::SIGNUM, math::signum);
    vm.register_builtin(ids::RECIP, math::recip);
    vm.register_builtin(ids::MUL_ADD, math::mul_add);
    vm.register_builtin(ids::DIV_EUCLID, math::div_euclid);
    vm.register_builtin(ids::REM_EUCLID, math::rem_euclid);
    vm.register_builtin(ids::TO_RADIANS, math::to_radians);
    vm.register_builtin(ids::TO_DEGREES, math::to_degrees);
    vm.register_builtin(ids::NEXT_UP, math::next_up);
    vm.register_builtin(ids::NEXT_DOWN, math::next_down);
    vm.register_builtin(ids::MIDPOINT, math::midpoint);
    vm.register_builtin(ids::ROUND_TIES_EVEN, math::round_ties_even);
    vm.register_builtin(ids::ASINH, math::asinh);
    vm.register_builtin(ids::ACOSH, math::acosh);
    vm.register_builtin(ids::ATANH, math::atanh);
    vm.register_builtin(ids::IS_NORMAL, math::is_normal);
    vm.register_builtin(ids::IS_SUBNORMAL, math::is_subnormal);
    vm.register_builtin(ids::TO_BITS, math::to_bits);
    vm.register_builtin(ids::FROM_BITS, math::from_bits);
    vm.register_builtin(ids::POWI, math::powi);
    vm.register_builtin(ids::LOG_BASE, math::log_base);
    vm.register_builtin(ids::PI, math::pi);
    vm.register_builtin(ids::E, math::euler);
    vm.register_builtin(ids::TAU, math::tau);
    vm.register_builtin(ids::LN_2, math::ln_2);
    vm.register_builtin(ids::LN_10, math::ln_10);
    vm.register_builtin(ids::LOG2_E, math::log2_e);
    vm.register_builtin(ids::LOG10_E, math::log10_e);
    vm.register_builtin(ids::SQRT_2, math::sqrt_2);
    vm.register_builtin(ids::INFINITY, math::infinity);
    vm.register_builtin(ids::NEG_INFINITY, math::neg_infinity);
    vm.register_builtin(ids::NAN, math::nan);
    vm.register_builtin(ids::EPSILON, math::epsilon);
    vm.register_builtin(ids::MIN_POSITIVE, math::min_positive);
    vm.register_builtin(ids::FRAC_PI_2, math::frac_pi_2);
    vm.register_builtin(ids::FRAC_PI_4, math::frac_pi_4);
    vm.register_builtin(ids::FRAC_1_SQRT_2, math::frac_1_sqrt_2);
    vm.register_builtin(ids::LOG2_10, math::log2_10);
    vm.register_builtin(ids::LOG10_2, math::log10_2);
    vm.register_builtin(ids::FRAC_1_PI, math::frac_1_pi);
    vm.register_builtin(ids::FRAC_2_PI, math::frac_2_pi);
    vm.register_builtin(ids::FRAC_2_SQRT_PI, math::frac_2_sqrt_pi);
    vm.register_builtin(ids::FRAC_PI_3, math::frac_pi_3);
    vm.register_builtin(ids::FRAC_PI_6, math::frac_pi_6);
    vm.register_builtin(ids::FRAC_PI_8, math::frac_pi_8);
    vm.register_builtin(ids::F64_MIN, math::f64_min);
    vm.register_builtin(ids::F64_MAX, math::f64_max);
    vm.register_builtin(ids::GOLDEN_RATIO, math::golden_ratio);
    vm.register_builtin(ids::EULER_GAMMA, math::euler_gamma);
    vm.register_builtin(ids::F64_CLASSIFY, math::f64_classify);
    vm.register_builtin(ids::TRUNC_I64, math::trunc_i64);
    vm.register_builtin(ids::TRUNC_U64, math::trunc_u64);
    vm.register_builtin(ids::TRUNC_I128, math::trunc_i128);
    vm.register_builtin(ids::TRUNC_U128, math::trunc_u128);

    vm.register_builtin(ids::STR_LEN, string::str_len);
    vm.register_builtin(ids::STR_CONCAT, string::str_concat);
    vm.register_builtin(ids::STR_CONTAINS, string::str_contains);
    vm.register_builtin(ids::STR_STARTS_WITH, string::str_starts_with);
    vm.register_builtin(ids::STR_ENDS_WITH, string::str_ends_with);
    vm.register_builtin(ids::STR_TRIM, string::str_trim);
    vm.register_builtin(ids::STR_TRIM_START, string::str_trim_start);
    vm.register_builtin(ids::STR_TRIM_END, string::str_trim_end);
    vm.register_builtin(ids::STR_TRIM_ASCII, string::str_trim_ascii);
    vm.register_builtin(ids::STR_TRIM_ASCII_START, string::str_trim_ascii_start);
    vm.register_builtin(ids::STR_TRIM_ASCII_END, string::str_trim_ascii_end);
    vm.register_builtin(
        ids::STR_EQ_IGNORE_ASCII_CASE,
        string::str_eq_ignore_ascii_case,
    );
    vm.register_builtin(
        ids::STR_SPLIT_ASCII_WHITESPACE,
        string::str_split_ascii_whitespace,
    );
    vm.register_builtin(ids::STR_TO_UPPER, string::str_to_upper);
    vm.register_builtin(ids::STR_TO_LOWER, string::str_to_lower);
    vm.register_builtin(ids::STR_REPEAT, string::str_repeat);
    vm.register_builtin(ids::STR_PAD_START, string::str_pad_start);
    vm.register_builtin(ids::STR_PAD_END, string::str_pad_end);
    vm.register_builtin(ids::STR_REPLACE, string::str_replace);
    vm.register_builtin(ids::STR_SPLIT, string::str_split);
    vm.register_builtin(ids::STR_TRIM_START_MATCHES, string::str_trim_start_matches);
    vm.register_builtin(ids::STR_TRIM_END_MATCHES, string::str_trim_end_matches);
    vm.register_builtin(ids::STR_TRIM_MATCHES, string::str_trim_matches_both);
    vm.register_builtin(ids::STR_REPLACEN, string::str_replacen);
    vm.register_builtin(ids::STR_SPLITN, string::str_splitn);
    vm.register_builtin(ids::STR_RSPLIT, string::str_rsplit);
    vm.register_builtin(ids::STR_RSPLITN, string::str_rsplitn);
    vm.register_builtin(ids::STR_SPLIT_INCLUSIVE, string::str_split_inclusive);
    vm.register_builtin(ids::STR_FIND_ALL, string::str_find_all);
    vm.register_builtin(ids::STR_ESCAPE_DEBUG, string::str_escape_debug);
    vm.register_builtin(ids::STR_ESCAPE_UNICODE, string::str_escape_unicode);
    vm.register_builtin(ids::STR_ESCAPE_DEFAULT, string::str_escape_default);
    vm.register_builtin(ids::STR_CHARS, string::str_chars);
    vm.register_builtin(ids::STR_CHAR_LEN, string::str_char_len);
    vm.register_builtin(ids::STR_CHAR_AT, string::str_char_at);
    vm.register_builtin(ids::STR_FIND, string::str_find);
    vm.register_builtin(ids::STR_RFIND, string::str_rfind);
    vm.register_builtin(ids::STR_COUNT, string::str_count);
    vm.register_builtin(ids::STR_IS_ASCII, string::str_is_ascii);
    vm.register_builtin(ids::STR_IS_ASCII_DIGIT, string::str_is_ascii_digit);
    vm.register_builtin(
        ids::STR_IS_ASCII_ALPHABETIC,
        string::str_is_ascii_alphabetic,
    );
    vm.register_builtin(
        ids::STR_IS_ASCII_ALPHANUMERIC,
        string::str_is_ascii_alphanumeric,
    );
    vm.register_builtin(ids::STR_IS_ASCII_LOWERCASE, string::str_is_ascii_lowercase);
    vm.register_builtin(ids::STR_IS_ASCII_UPPERCASE, string::str_is_ascii_uppercase);
    vm.register_builtin(
        ids::STR_IS_ASCII_WHITESPACE,
        string::str_is_ascii_whitespace,
    );
    vm.register_builtin(ids::STR_IS_ASCII_HEXDIGIT, string::str_is_ascii_hexdigit);
    vm.register_builtin(ids::STR_IS_ASCII_GRAPHIC, string::str_is_ascii_graphic);
    vm.register_builtin(
        ids::STR_IS_ASCII_PUNCTUATION,
        string::str_is_ascii_punctuation,
    );
    vm.register_builtin(ids::STR_IS_ASCII_CONTROL, string::str_is_ascii_control);
    vm.register_builtin(ids::STR_IS_NUMERIC, string::str_is_numeric);
    vm.register_builtin(ids::STR_IS_ALPHABETIC, string::str_is_alphabetic);
    vm.register_builtin(ids::STR_IS_ALPHANUMERIC, string::str_is_alphanumeric);
    vm.register_builtin(ids::STR_IS_LOWERCASE, string::str_is_lowercase);
    vm.register_builtin(ids::STR_IS_UPPERCASE, string::str_is_uppercase);
    vm.register_builtin(ids::STR_IS_WHITESPACE, string::str_is_whitespace);
    vm.register_builtin(ids::STR_IS_CONTROL, string::str_is_control);
    vm.register_builtin(ids::STR_SLICE, string::str_slice);
    vm.register_builtin(ids::STR_PARSE_I64, string::str_parse_i64);
    vm.register_builtin(ids::STR_PARSE_F64, string::str_parse_f64);
    vm.register_builtin(ids::CHAR_IS_WHITESPACE, string::char_is_whitespace);
    vm.register_builtin(ids::CHAR_IS_ALPHABETIC, string::char_is_alphabetic);
    vm.register_builtin(ids::CHAR_IS_NUMERIC, string::char_is_numeric);
    vm.register_builtin(ids::CHAR_IS_ALPHANUMERIC, string::char_is_alphanumeric);
    vm.register_builtin(ids::CHAR_IS_UPPERCASE, string::char_is_uppercase);
    vm.register_builtin(ids::CHAR_IS_LOWERCASE, string::char_is_lowercase);
    vm.register_builtin(ids::CHAR_IS_ASCII, string::char_is_ascii);
    vm.register_builtin(ids::CHAR_IS_ASCII_DIGIT, string::char_is_ascii_digit);
    vm.register_builtin(
        ids::CHAR_IS_ASCII_ALPHABETIC,
        string::char_is_ascii_alphabetic,
    );
    vm.register_builtin(ids::CHAR_IS_ASCII_HEXDIGIT, string::char_is_ascii_hexdigit);
    vm.register_builtin(ids::CHAR_IS_ASCII_OCTDIGIT, string::char_is_ascii_octdigit);
    vm.register_builtin(ids::CHAR_IS_ASCII_CONTROL, string::char_is_ascii_control);
    vm.register_builtin(ids::CHAR_IS_ASCII_GRAPHIC, string::char_is_ascii_graphic);
    vm.register_builtin(
        ids::CHAR_IS_ASCII_PUNCTUATION,
        string::char_is_ascii_punctuation,
    );
    vm.register_builtin(
        ids::CHAR_IS_ASCII_WHITESPACE,
        string::char_is_ascii_whitespace,
    );
    vm.register_builtin(
        ids::CHAR_IS_ASCII_ALPHANUMERIC,
        string::char_is_ascii_alphanumeric,
    );
    vm.register_builtin(
        ids::CHAR_IS_ASCII_UPPERCASE,
        string::char_is_ascii_uppercase,
    );
    vm.register_builtin(
        ids::CHAR_IS_ASCII_LOWERCASE,
        string::char_is_ascii_lowercase,
    );
    vm.register_builtin(ids::CHAR_IS_CONTROL, string::char_is_control);
    vm.register_builtin(ids::CHAR_UTF8_LEN, string::char_utf8_len);
    vm.register_builtin(ids::CHAR_UTF16_LEN, string::char_utf16_len);
    vm.register_builtin(ids::CHAR_ESCAPE_DEBUG, string::char_escape_debug);
    vm.register_builtin(ids::CHAR_ESCAPE_UNICODE, string::char_escape_unicode);
    vm.register_builtin(ids::CHAR_ESCAPE_DEFAULT, string::char_escape_default);
    vm.register_builtin(ids::CHAR_IS_DIGIT, string::char_is_digit);
    vm.register_builtin(ids::CHAR_TO_DIGIT, string::char_to_digit);
    vm.register_builtin(ids::CHAR_FROM_DIGIT, string::char_from_digit);
    vm.register_builtin(ids::CHAR_ENCODE_UTF8, string::char_encode_utf8);
    vm.register_builtin(ids::CHAR_ENCODE_UTF16, string::char_encode_utf16);
    vm.register_builtin(ids::STR_ENCODE_UTF8, string::str_encode_utf8);
    vm.register_builtin(ids::STR_ENCODE_UTF16, string::str_encode_utf16);
    vm.register_builtin(ids::STR_FROM_UTF8, string::str_from_utf8);
    vm.register_builtin(ids::STR_FROM_UTF16, string::str_from_utf16);
    vm.register_builtin(ids::STR_FROM_UTF8_LOSSY, string::str_from_utf8_lossy);
    vm.register_builtin(ids::STR_FROM_UTF16_LOSSY, string::str_from_utf16_lossy);
    vm.register_builtin(ids::STR_IS_CHAR_BOUNDARY, string::str_is_char_boundary);
    vm.register_builtin(
        ids::STR_FLOOR_CHAR_BOUNDARY,
        string::str_floor_char_boundary,
    );
    vm.register_builtin(ids::STR_CEIL_CHAR_BOUNDARY, string::str_ceil_char_boundary);
    vm.register_builtin(ids::STR_CHAR_AT_BYTE, string::str_char_at_byte);
    vm.register_builtin(ids::STR_SPLIT_AT, string::str_split_at);
    vm.register_builtin(ids::STR_BYTE_AT, string::str_byte_at);
    vm.register_builtin(ids::STR_GET, string::str_get);
    vm.register_builtin(ids::STR_SPLIT_AT_CHECKED, string::str_split_at_checked);
    vm.register_builtin(ids::STR_PARSE_U64, string::str_parse_u64);
    vm.register_builtin(ids::STR_PARSE_I128, string::str_parse_i128);
    vm.register_builtin(ids::STR_PARSE_U128, string::str_parse_u128);
    vm.register_builtin(ids::STR_SPLIT_TERMINATOR, string::str_split_terminator);
    vm.register_builtin(ids::STR_RSPLIT_TERMINATOR, string::str_rsplit_terminator);
    vm.register_builtin(ids::STR_BYTE_FIND, string::str_byte_find);
    vm.register_builtin(ids::STR_BYTE_RFIND, string::str_byte_rfind);
    vm.register_builtin(ids::STR_BYTE_COUNT, string::str_byte_count);
    vm.register_builtin(ids::CHAR_REPLACEMENT, string::char_replacement);
    vm.register_builtin(ids::CHAR_MAX, string::char_max);
    vm.register_builtin(
        ids::CHAR_EQ_IGNORE_ASCII_CASE,
        string::char_eq_ignore_ascii_case,
    );
    vm.register_builtin(ids::CHAR_MIN, string::char_min);
    vm.register_builtin(ids::STR_FIND_INDICES, string::str_find_indices);
    vm.register_builtin(ids::STR_RFIND_INDICES, string::str_rfind_indices);
    vm.register_builtin(ids::STR_BYTE_FIND_INDICES, string::str_byte_find_indices);
    vm.register_builtin(ids::STR_BYTE_RFIND_INDICES, string::str_byte_rfind_indices);
    vm.register_builtin(ids::STR_TRIM_PREFIX, string::str_trim_prefix);
    vm.register_builtin(ids::STR_TRIM_SUFFIX, string::str_trim_suffix);
    vm.register_builtin(ids::STR_STRIP_CIRCUMFIX, string::str_strip_circumfix);
    vm.register_builtin(
        ids::CHAR_TO_ASCII_UPPERCASE,
        string::char_to_ascii_uppercase,
    );
    vm.register_builtin(
        ids::CHAR_TO_ASCII_LOWERCASE,
        string::char_to_ascii_lowercase,
    );
    vm.register_builtin(ids::CHAR_TO_CODE, string::char_to_code);
    vm.register_builtin(ids::CHAR_FROM_CODE, string::char_from_code);
    vm.register_builtin(ids::CHAR_INTO_STRING, string::char_into_string);
    vm.register_builtin(ids::STR_TO_ASCII_UPPERCASE, string::str_to_ascii_uppercase);
    vm.register_builtin(ids::STR_TO_ASCII_LOWERCASE, string::str_to_ascii_lowercase);
    vm.register_builtin(ids::STR_SPLIT_WHITESPACE, string::str_split_whitespace);
    vm.register_builtin(ids::CHAR_TO_UPPERCASE, string::char_to_uppercase);
    vm.register_builtin(ids::CHAR_TO_LOWERCASE, string::char_to_lowercase);
    vm.register_builtin(ids::COUNT_ONES, int::count_ones);
    vm.register_builtin(ids::COUNT_ZEROS, int::count_zeros);
    vm.register_builtin(ids::LEADING_ZEROS, int::leading_zeros);
    vm.register_builtin(ids::TRAILING_ZEROS, int::trailing_zeros);
    vm.register_builtin(ids::LEADING_ONES, int::leading_ones);
    vm.register_builtin(ids::TRAILING_ONES, int::trailing_ones);
    vm.register_builtin(ids::REVERSE_BITS, int::reverse_bits);
    vm.register_builtin(ids::SWAP_BYTES, int::swap_bytes);
    vm.register_builtin(ids::ROTATE_LEFT, int::rotate_left);
    vm.register_builtin(ids::ROTATE_RIGHT, int::rotate_right);
    vm.register_builtin(ids::IS_POWER_OF_TWO, int::is_power_of_two);
    vm.register_builtin(ids::ILOG2, int::ilog2);
    vm.register_builtin(ids::ILOG10, int::ilog10);
    vm.register_builtin(ids::ABS_DIFF, int::abs_diff);
    vm.register_builtin(ids::WRAPPING_ADD, int::wrapping_add);
    vm.register_builtin(ids::WRAPPING_SUB, int::wrapping_sub);
    vm.register_builtin(ids::WRAPPING_MUL, int::wrapping_mul);
    vm.register_builtin(ids::WRAPPING_NEG, int::wrapping_neg);
    vm.register_builtin(ids::SATURATING_ADD, int::saturating_add);
    vm.register_builtin(ids::SATURATING_SUB, int::saturating_sub);
    vm.register_builtin(ids::SATURATING_MUL, int::saturating_mul);
    vm.register_builtin(ids::SATURATING_NEG, int::saturating_neg);
    vm.register_builtin(ids::CHECKED_ADD, int::checked_add);
    vm.register_builtin(ids::CHECKED_SUB, int::checked_sub);
    vm.register_builtin(ids::CHECKED_MUL, int::checked_mul);
    vm.register_builtin(ids::CHECKED_DIV, int::checked_div);
    vm.register_builtin(ids::CHECKED_REM, int::checked_rem);
    vm.register_builtin(ids::CHECKED_NEG, int::checked_neg);
    vm.register_builtin(ids::INT_DIV_EUCLID, int::int_div_euclid);
    vm.register_builtin(ids::INT_REM_EUCLID, int::int_rem_euclid);
    vm.register_builtin(ids::UNSIGNED_ABS, int::unsigned_abs);
    vm.register_builtin(ids::INT_MIDPOINT, int::int_midpoint);
    vm.register_builtin(ids::CHECKED_SHL, int::checked_shl);
    vm.register_builtin(ids::CHECKED_SHR, int::checked_shr);
    vm.register_builtin(ids::WRAPPING_SHL, int::wrapping_shl);
    vm.register_builtin(ids::WRAPPING_SHR, int::wrapping_shr);
    vm.register_builtin(ids::CHECKED_POW, int::checked_pow);
    vm.register_builtin(ids::WRAPPING_POW, int::wrapping_pow);
    vm.register_builtin(ids::SATURATING_POW, int::saturating_pow);
    vm.register_builtin(ids::WRAPPING_DIV, int::wrapping_div);
    vm.register_builtin(ids::WRAPPING_REM, int::wrapping_rem);
    vm.register_builtin(ids::SATURATING_ABS, int::saturating_abs);
    vm.register_builtin(ids::CHECKED_ABS, int::checked_abs);
    vm.register_builtin(ids::WRAPPING_ABS, int::wrapping_abs);
    vm.register_builtin(ids::INT_SIGNUM, int::int_signum);
    vm.register_builtin(ids::CHECKED_ILOG, int::checked_ilog);
    vm.register_builtin(ids::CAST_UNSIGNED, int::cast_unsigned);
    vm.register_builtin(ids::CAST_SIGNED, int::cast_signed);
    vm.register_builtin(ids::CHECKED_DIV_EUCLID, int::checked_div_euclid);
    vm.register_builtin(ids::CHECKED_REM_EUCLID, int::checked_rem_euclid);
    vm.register_builtin(ids::WIDENING_MUL, int::widening_mul);
    vm.register_builtin(ids::NEXT_POWER_OF_TWO, int::next_power_of_two);
    vm.register_builtin(
        ids::CHECKED_NEXT_POWER_OF_TWO,
        int::checked_next_power_of_two,
    );
    vm.register_builtin(ids::CHECKED_ADD_SIGNED, int::checked_add_signed);
    vm.register_builtin(ids::CHECKED_SUB_SIGNED, int::checked_sub_signed);
    vm.register_builtin(ids::SATURATING_ADD_SIGNED, int::saturating_add_signed);
    vm.register_builtin(ids::SATURATING_SUB_SIGNED, int::saturating_sub_signed);
    vm.register_builtin(ids::WRAPPING_ADD_SIGNED, int::wrapping_add_signed);
    vm.register_builtin(ids::WRAPPING_SUB_SIGNED, int::wrapping_sub_signed);
    vm.register_builtin(ids::STRICT_ADD_SIGNED, int::strict_add_signed);
    vm.register_builtin(ids::STRICT_SUB_SIGNED, int::strict_sub_signed);
    vm.register_builtin(ids::UNBOUNDED_SHL, int::unbounded_shl);
    vm.register_builtin(ids::UNBOUNDED_SHR, int::unbounded_shr);
    vm.register_builtin(ids::I64_MIN, int::i64_min);
    vm.register_builtin(ids::I64_MAX, int::i64_max);
    vm.register_builtin(ids::U64_MAX, int::u64_max);
    vm.register_builtin(ids::I128_MIN, int::i128_min);
    vm.register_builtin(ids::I128_MAX, int::i128_max);
    vm.register_builtin(ids::U128_MAX, int::u128_max);
    vm.register_builtin(ids::HIGHEST_ONE, int::highest_one);
    vm.register_builtin(ids::LOWEST_ONE, int::lowest_one);
    vm.register_builtin(ids::ISOLATE_HIGHEST_ONE, int::isolate_highest_one);
    vm.register_builtin(ids::ISOLATE_LOWEST_ONE, int::isolate_lowest_one);
    vm.register_builtin(ids::INT_BIT_WIDTH, int::int_bit_width);
    vm.register_builtin(ids::I64_BITS, int::i64_bits);
    vm.register_builtin(ids::I128_BITS, int::i128_bits);
    vm.register_builtin(ids::ISIZE_BITS, int::isize_bits);
    vm.register_builtin(ids::U128_BITS, int::u128_bits);
    vm.register_builtin(ids::USIZE_BITS, int::usize_bits);
    vm.register_builtin(ids::INT_ISQRT, int::int_isqrt);
    vm.register_builtin(ids::CHECKED_INT_ISQRT, int::checked_int_isqrt);
    vm.register_builtin(ids::STRICT_ABS, int::strict_abs);
    vm.register_builtin(ids::WRAPPING_DIV_EUCLID, int::wrapping_div_euclid);
    vm.register_builtin(ids::WRAPPING_REM_EUCLID, int::wrapping_rem_euclid);
    vm.register_builtin(ids::CHECKED_SIGNED_DIFF, int::checked_signed_diff);
    vm.register_builtin(ids::CHECKED_DIV_EXACT, int::checked_div_exact);
    vm.register_builtin(ids::STRICT_DIV_EXACT, int::strict_div_exact);
    vm.register_builtin(ids::CHECKED_SHL_EXACT, int::checked_shl_exact);
    vm.register_builtin(ids::STRICT_SHL_EXACT, int::strict_shl_exact);
    vm.register_builtin(ids::CHECKED_SHR_EXACT, int::checked_shr_exact);
    vm.register_builtin(ids::STRICT_SHR_EXACT, int::strict_shr_exact);
    vm.register_builtin(ids::IS_MULTIPLE_OF, int::is_multiple_of);
    vm.register_builtin(ids::STRICT_ADD, int::strict_add);
    vm.register_builtin(ids::STRICT_SUB, int::strict_sub);
    vm.register_builtin(ids::STRICT_MUL, int::strict_mul);
    vm.register_builtin(ids::STRICT_DIV, int::strict_div);
    vm.register_builtin(ids::STRICT_REM, int::strict_rem);
    vm.register_builtin(ids::STRICT_NEG, int::strict_neg);
    vm.register_builtin(ids::STRICT_SHL, int::strict_shl);
    vm.register_builtin(ids::STRICT_SHR, int::strict_shr);
    vm.register_builtin(ids::STRICT_POW, int::strict_pow);
    vm.register_builtin(ids::SATURATING_DIV, int::saturating_div);
    vm.register_builtin(ids::SATURATING_DIV_EUCLID, int::saturating_div_euclid);
    vm.register_builtin(ids::CHECKED_ADD_UNSIGNED, int::checked_add_unsigned);
    vm.register_builtin(ids::CHECKED_SUB_UNSIGNED, int::checked_sub_unsigned);
    vm.register_builtin(ids::SATURATING_ADD_UNSIGNED, int::saturating_add_unsigned);
    vm.register_builtin(ids::SATURATING_SUB_UNSIGNED, int::saturating_sub_unsigned);
    vm.register_builtin(ids::WRAPPING_ADD_UNSIGNED, int::wrapping_add_unsigned);
    vm.register_builtin(ids::WRAPPING_SUB_UNSIGNED, int::wrapping_sub_unsigned);
    vm.register_builtin(ids::STRICT_ADD_UNSIGNED, int::strict_add_unsigned);
    vm.register_builtin(ids::STRICT_SUB_UNSIGNED, int::strict_sub_unsigned);
    vm.register_builtin(ids::INT_TO_STR_RADIX, int::int_to_str_radix);
    vm.register_builtin(ids::I64_FROM_STR_RADIX, int::i64_from_str_radix);
    vm.register_builtin(ids::U64_FROM_STR_RADIX, int::u64_from_str_radix);
    vm.register_builtin(ids::I128_FROM_STR_RADIX, int::i128_from_str_radix);
    vm.register_builtin(ids::U128_FROM_STR_RADIX, int::u128_from_str_radix);
    vm.register_builtin(ids::DIV_CEIL, int::div_ceil);
    vm.register_builtin(ids::CHECKED_DIV_CEIL, int::checked_div_ceil);
    vm.register_builtin(ids::NEXT_MULTIPLE_OF, int::next_multiple_of);
    vm.register_builtin(ids::CHECKED_NEXT_MULTIPLE_OF, int::checked_next_multiple_of);
    vm.register_builtin(ids::INT_TO_BE_BYTES, int::int_to_be_bytes);
    vm.register_builtin(ids::INT_TO_LE_BYTES, int::int_to_le_bytes);
    vm.register_builtin(ids::I64_FROM_BE_BYTES, int::i64_from_be_bytes);
    vm.register_builtin(ids::I64_FROM_LE_BYTES, int::i64_from_le_bytes);
    vm.register_builtin(ids::U64_FROM_BE_BYTES, int::u64_from_be_bytes);
    vm.register_builtin(ids::U64_FROM_LE_BYTES, int::u64_from_le_bytes);
    vm.register_builtin(ids::I128_FROM_BE_BYTES, int::i128_from_be_bytes);
    vm.register_builtin(ids::I128_FROM_LE_BYTES, int::i128_from_le_bytes);
    vm.register_builtin(ids::U128_FROM_BE_BYTES, int::u128_from_be_bytes);
    vm.register_builtin(ids::U128_FROM_LE_BYTES, int::u128_from_le_bytes);
    vm.register_builtin(ids::TOTAL_CMP, math::total_cmp);
    vm.register_builtin(ids::DIV_FLOOR, int::div_floor);
    vm.register_builtin(ids::CHECKED_DIV_FLOOR, int::checked_div_floor);
    vm.register_builtin(ids::INT_GCD, int::int_gcd);
    vm.register_builtin(ids::INT_LCM, int::int_lcm);
    vm.register_builtin(ids::CHECKED_INT_LCM, int::checked_int_lcm);
    vm.register_builtin(ids::IS_SIGN_POSITIVE, math::is_sign_positive);
    vm.register_builtin(ids::F64_TO_BE_BYTES, math::f64_to_be_bytes);
    vm.register_builtin(ids::F64_TO_LE_BYTES, math::f64_to_le_bytes);
    vm.register_builtin(ids::F64_FROM_BE_BYTES, math::f64_from_be_bytes);
    vm.register_builtin(ids::F64_FROM_LE_BYTES, math::f64_from_le_bytes);
    vm.register_builtin(ids::F64_TO_NE_BYTES, math::f64_to_ne_bytes);
    vm.register_builtin(ids::F64_FROM_NE_BYTES, math::f64_from_ne_bytes);
    vm.register_builtin(ids::ALGEBRAIC_ADD, math::algebraic_add);
    vm.register_builtin(ids::ALGEBRAIC_SUB, math::algebraic_sub);
    vm.register_builtin(ids::ALGEBRAIC_MUL, math::algebraic_mul);
    vm.register_builtin(ids::ALGEBRAIC_DIV, math::algebraic_div);
    vm.register_builtin(ids::ALGEBRAIC_REM, math::algebraic_rem);
    vm.register_builtin(ids::F64_NAN_TO, math::f64_nan_to);
    vm.register_builtin(ids::F64_ABS_DIFF, math::f64_abs_diff);
    vm.register_builtin(ids::F64_IS_ZERO, math::f64_is_zero);
    vm.register_builtin(ids::BOOL_NOT, math::bool_not);
    vm.register_builtin(ids::BOOL_AND, math::bool_and);
    vm.register_builtin(ids::BOOL_OR, math::bool_or);
    vm.register_builtin(ids::BOOL_XOR, math::bool_xor);
    vm.register_builtin(ids::BOOL_THEN, math::bool_then);
    vm.register_builtin(ids::STR_BYTES, string::str_bytes);
    vm.register_builtin(ids::STR_CHAR_INDICES, string::str_char_indices);
    vm.register_builtin(ids::STR_IS_EMPTY, string::str_is_empty);
    vm.register_builtin(ids::STR_REPEAT_CHAR, string::str_repeat_char);
    vm.register_builtin(ids::STR_TAKE, string::str_take);
    vm.register_builtin(ids::STR_DROP, string::str_drop);
    vm.register_builtin(ids::STR_FIRST, string::str_first);
    vm.register_builtin(ids::STR_LAST, string::str_last);
    vm.register_builtin(ids::STR_STARTS_WITH_CHAR, string::str_starts_with_char);
    vm.register_builtin(ids::STR_ENDS_WITH_CHAR, string::str_ends_with_char);
    vm.register_builtin(ids::STR_CONTAINS_CHAR, string::str_contains_char);
    vm.register_builtin(ids::STR_REPLACE_CHAR, string::str_replace_char);
    vm.register_builtin(ids::LIST_APPEND, list::list_append);
    vm.register_builtin(ids::LIST_JOIN, list::list_join);
    vm.register_builtin(ids::LIST_IS_EMPTY, list::list_is_empty);
    vm.register_builtin(ids::F64_RADIX, math::f64_radix);
    vm.register_builtin(ids::F64_MANTISSA_DIGITS, math::f64_mantissa_digits);
    vm.register_builtin(ids::F64_DIGITS, math::f64_digits);
    vm.register_builtin(ids::F64_MIN_EXP, math::f64_min_exp);
    vm.register_builtin(ids::F64_MAX_EXP, math::f64_max_exp);
    vm.register_builtin(ids::F64_MIN_10_EXP, math::f64_min_10_exp);
    vm.register_builtin(ids::F64_MAX_10_EXP, math::f64_max_10_exp);
    vm.register_builtin(ids::DISPLAY_QUOTE, string::display_quote);
    vm.register_builtin(ids::DISPLAY_ENTER_TUPLE, string::display_enter_tuple);
    vm.register_builtin(ids::DISPLAY_EXIT_TUPLE, string::display_exit_tuple);

    vm.register_builtin(ids::EXIT, process::exit);
    vm.register_builtin(ids::PANIC, process::panic);

    vm.register_builtin(ids::LIST_INIT, list::list_init);
    vm.register_builtin(ids::LIST_LEN, list::list_len);
    vm.register_builtin(ids::LIST_GET, list::list_get);
    vm.register_builtin(ids::LIST_SET, list::list_set);
    vm.register_builtin(ids::LIST_PUSH, list::list_push);
    vm.register_builtin(ids::LIST_POP, list::list_pop);
    vm.register_builtin(ids::LIST_WINDOWS, list::list_windows);
    vm.register_builtin(ids::LIST_CHUNKS_EXACT, list::list_chunks_exact);
    vm.register_builtin(ids::LIST_RCHUNKS, list::list_rchunks);
    vm.register_builtin(ids::LIST_RCHUNKS_EXACT, list::list_rchunks_exact);
    vm.register_builtin(ids::LIST_CHUNKS_REMAINDER, list::list_chunks_remainder);
    vm.register_builtin(ids::LIST_RCHUNKS_REMAINDER, list::list_rchunks_remainder);
    vm.register_builtin(ids::LIST_SPLIT_AT, list::list_split_at);
    vm.register_builtin(ids::LIST_ROTATE_LEFT, list::list_rotate_left);
    vm.register_builtin(ids::LIST_ROTATE_RIGHT, list::list_rotate_right);
    vm.register_builtin(ids::LIST_REPEAT, list::list_repeat);
    vm.register_builtin(ids::LIST_TRUNCATE, list::list_truncate);
    vm.register_builtin(ids::LIST_FILL, list::list_fill);
    vm.register_builtin(ids::LIST_SWAP_REMOVE, list::list_swap_remove);
    vm.register_builtin(ids::LIST_SPLIT_OFF, list::list_split_off);
    vm.register_builtin(ids::LIST_RESIZE, list::list_resize);
    vm.register_builtin(ids::LIST_COPY, list::list_copy);
    vm.register_builtin(ids::LIST_REVERSE, list::list_reverse);
    vm.register_builtin(ids::LIST_CONCAT, list::list_concat);
    vm.register_builtin(ids::LIST_SWAP, list::list_swap);
    vm.register_builtin(ids::LIST_CLEAR, list::list_clear);
    vm.register_builtin(ids::LIST_INSERT, list::list_insert);
    vm.register_builtin(ids::LIST_REMOVE, list::list_remove);
    vm.register_builtin(ids::LIST_TAKE, list::list_take);
    vm.register_builtin(ids::LIST_DROP, list::list_drop);
    vm.register_builtin(ids::LIST_FIRST, list::list_first);
    vm.register_builtin(ids::LIST_LAST, list::list_last);
    vm.register_builtin(ids::RANGE_LEN, list::range_len);
    vm.register_builtin(ids::RANGE_INCLUSIVE_LEN, list::range_inclusive_len);
    vm.register_builtin(ids::RANGE_IS_EMPTY, list::range_is_empty);
    vm.register_builtin(
        ids::RANGE_INCLUSIVE_IS_EMPTY,
        list::range_inclusive_is_empty,
    );
    vm.register_builtin(ids::RANGE_CONTAINS, list::range_contains);
    vm.register_builtin(
        ids::RANGE_INCLUSIVE_CONTAINS,
        list::range_inclusive_contains,
    );
    vm.register_builtin(ids::RANGE_FROM_CONTAINS, list::range_from_contains);
    vm.register_builtin(ids::RANGE_TO_CONTAINS, list::range_to_contains);
    vm.register_builtin(
        ids::RANGE_TO_INCLUSIVE_CONTAINS,
        list::range_to_inclusive_contains,
    );

    vm.register_builtin(ids::MAP_INIT, map::map_init);
    vm.register_builtin(ids::MAP_LEN, map::map_len);
    vm.register_builtin(ids::MAP_GET, map::map_get);
    vm.register_builtin(ids::MAP_SET, map::map_set);
    vm.register_builtin(ids::MAP_REMOVE, map::map_remove);
    vm.register_builtin(ids::MAP_CONTAINS, map::map_contains);
    vm.register_builtin(ids::MAP_KEYS, map::map_keys);
    vm.register_builtin(ids::MAP_VALUES, map::map_values);
    vm.register_builtin(ids::MAP_CLEAR, map::map_clear);
    vm.register_builtin(ids::MAP_MERGE, map::map_merge);
    vm.register_builtin(ids::MAP_GET_OR_INSERT, map::map_get_or_insert);
    vm.register_builtin(ids::MAP_INSERT_ABSENT, map::map_insert_absent);
    vm.register_builtin(ids::STR_LINES, string::str_lines);
    vm.register_builtin(ids::STR_SPLIT_ONCE, string::str_split_once);
    vm.register_builtin(ids::STR_RSPLIT_ONCE, string::str_rsplit_once);
    vm.register_builtin(ids::STR_STRIP_PREFIX, string::str_strip_prefix);
    vm.register_builtin(ids::STR_STRIP_SUFFIX, string::str_strip_suffix);
    vm.register_builtin(ids::MAP_ENTRIES, map::map_entries);
    vm.register_builtin(ids::MAP_IS_EMPTY, map::map_is_empty);
    vm.register_builtin(ids::LIST_CHUNKS, list::list_chunks);
    vm.register_builtin(ids::LIST_ENUMERATE, list::list_enumerate);
    vm.register_builtin(ids::LIST_FLATTEN, list::list_flatten);
    vm.register_builtin(ids::LIST_ZIP, list::list_zip);
    vm.register_builtin(ids::STR_FIND_CHAR, string::str_find_char);
    vm.register_builtin(ids::STR_RFIND_CHAR, string::str_rfind_char);
    vm.register_builtin(ids::STR_COUNT_CHAR, string::str_count_char);
    vm.register_builtin(ids::MAP_COPY, map::map_copy);
}
