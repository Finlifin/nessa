//! Math builtins.

use std::cmp::Ordering;

use interpreter::{BuiltinCtx, VmError};
use nsbc::Opcode;
use runtime::{Number, TaggedValue};

pub fn abs(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let result = match ctx.arg_number(0)? {
        Number::I64(value) => match value.checked_abs() {
            Some(value) => Number::I64(value),
            None => Number::I128(-(value as i128)),
        },
        Number::I128(value) => Number::I128(value.checked_abs().ok_or(VmError::NumericOverflow)?),
        Number::U64(value) => Number::U64(value),
        Number::U128(value) => Number::U128(value),
        Number::F64(value) => Number::F64(value.abs()),
    };
    ctx.return_number(result)
}

pub fn sin(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.sin())?;
    Ok(())
}

pub fn cos(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.cos())?;
    Ok(())
}

pub fn sqrt(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.sqrt())?;
    Ok(())
}

pub fn floor(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.floor())?;
    Ok(())
}

pub fn ceil(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.ceil())?;
    Ok(())
}

pub fn round(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.round())?;
    Ok(())
}

pub fn pow(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let base = ctx.arg_number(0)?;
    let exponent = ctx.arg_number(1)?;
    if matches!(base, Number::F64(_)) || matches!(exponent, Number::F64(_)) {
        return ctx.return_f64(base.to_f64().powf(exponent.to_f64()));
    }
    let mut exponent = exponent.to_u64_checked().ok_or(VmError::NumericOverflow)?;
    let mut factor = base;
    let mut result = match base {
        Number::U64(_) => Number::U64(1),
        Number::U128(_) => Number::U128(1),
        Number::I128(_) => Number::I128(1),
        _ => Number::I64(1),
    };
    // Exponentiation by squaring bounds work by exponent bit width and uses
    // the same checked arithmetic and promotion rules as VM multiplication.
    while exponent != 0 {
        if exponent & 1 != 0 {
            result = result.binary(factor, Opcode::Mul)?;
        }
        exponent >>= 1;
        if exponent != 0 {
            factor = factor.binary(factor, Opcode::Mul)?;
        }
    }
    ctx.return_number(result)
}

pub fn log(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let v = ctx.arg_f64(0)?;
    ctx.return_f64(v.ln())?;
    Ok(())
}

fn unary_f64(ctx: &mut BuiltinCtx<'_>, function: fn(f64) -> f64) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_f64(function(ctx.arg_f64(0)?))?;
    Ok(())
}

fn binary_f64(ctx: &mut BuiltinCtx<'_>, function: fn(f64, f64) -> f64) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = ctx.arg_f64(0)?;
    let right = ctx.arg_f64(1)?;
    ctx.return_f64(function(left, right))?;
    Ok(())
}

pub fn exp(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::exp)
}

pub fn log2(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::log2)
}

pub fn log10(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::log10)
}

pub fn trunc(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::trunc)
}

pub fn fract(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::fract)
}

pub fn tan(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::tan)
}

pub fn asin(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::asin)
}

pub fn acos(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::acos)
}

pub fn atan(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::atan)
}

pub fn copysign(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_f64(ctx, f64::copysign)
}

/// `atan2(y, x)`: the angle of the vector `(x, y)`.
pub fn atan2(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_f64(ctx, f64::atan2)
}

pub fn hypot(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_f64(ctx, f64::hypot)
}

fn unary_f64_bool(ctx: &mut BuiltinCtx<'_>, function: fn(f64) -> bool) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_bool(function(ctx.arg_f64(0)?));
    Ok(())
}

pub fn is_nan(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64_bool(ctx, f64::is_nan)
}

pub fn is_infinite(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64_bool(ctx, f64::is_infinite)
}

pub fn is_finite(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64_bool(ctx, f64::is_finite)
}

/// The IEEE sign bit, so negative zero and a negative NaN are true.
pub fn is_sign_negative(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64_bool(ctx, f64::is_sign_negative)
}

pub fn sinh(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::sinh)
}

pub fn cosh(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::cosh)
}

pub fn tanh(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::tanh)
}

pub fn cbrt(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::cbrt)
}

pub fn exp2(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::exp2)
}

pub fn expm1(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::exp_m1)
}

pub fn log1p(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::ln_1p)
}

/// `1.0` or `-1.0`, including the sign of zero. NaN stays NaN.
pub fn signum(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::signum)
}

pub fn recip(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::recip)
}

/// `a * b + c` with one rounding.
pub fn mul_add(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let left = ctx.arg_f64(0)?;
    let factor = ctx.arg_f64(1)?;
    let addend = ctx.arg_f64(2)?;
    ctx.return_f64(left.mul_add(factor, addend))?;
    Ok(())
}

pub fn div_euclid(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_f64(ctx, f64::div_euclid)
}

pub fn rem_euclid(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_f64(ctx, f64::rem_euclid)
}

pub fn to_radians(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::to_radians)
}

pub fn to_degrees(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::to_degrees)
}

pub fn next_up(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::next_up)
}

pub fn next_down(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::next_down)
}

pub fn midpoint(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_f64(ctx, f64::midpoint)
}

pub fn round_ties_even(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::round_ties_even)
}

pub fn asinh(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::asinh)
}

pub fn acosh(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::acosh)
}

pub fn atanh(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64(ctx, f64::atanh)
}

pub fn is_normal(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64_bool(ctx, f64::is_normal)
}

pub fn is_subnormal(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_f64_bool(ctx, f64::is_subnormal)
}

pub fn to_bits(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_number(Number::U64(ctx.arg_f64(0)?.to_bits()))
}

pub fn from_bits(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let Number::U64(bits) = ctx.arg_number(0)? else {
        return Err(VmError::TypeError);
    };
    ctx.return_f64(f64::from_bits(bits))?;
    Ok(())
}

pub fn powi(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let base = ctx.arg_f64(0)?;
    let exponent = i32::try_from(ctx.arg_i64(1)?).map_err(|_| VmError::NumericOverflow)?;
    ctx.return_f64(base.powi(exponent))?;
    Ok(())
}

/// `log(value, base)`. A non-positive value or base is NaN or infinity.
pub fn log_base(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let value = ctx.arg_f64(0)?;
    let base = ctx.arg_f64(1)?;
    ctx.return_f64(value.log(base))?;
    Ok(())
}

fn return_constant(ctx: &mut BuiltinCtx<'_>, value: f64) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_f64(value)?;
    Ok(())
}

pub fn pi(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::PI)
}

pub fn euler(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::E)
}

pub fn tau(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::TAU)
}

pub fn ln_2(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::LN_2)
}

pub fn ln_10(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::LN_10)
}

pub fn log2_e(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::LOG2_E)
}

pub fn log10_e(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::LOG10_E)
}

pub fn sqrt_2(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::SQRT_2)
}

pub fn infinity(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, f64::INFINITY)
}

pub fn neg_infinity(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, f64::NEG_INFINITY)
}

pub fn nan(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, f64::NAN)
}

pub fn epsilon(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, f64::EPSILON)
}

pub fn min_positive(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, f64::MIN_POSITIVE)
}

pub fn frac_pi_2(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::FRAC_PI_2)
}

pub fn frac_pi_4(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::FRAC_PI_4)
}

pub fn frac_1_sqrt_2(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::FRAC_1_SQRT_2)
}

pub fn log2_10(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::LOG2_10)
}

pub fn log10_2(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::LOG10_2)
}

pub fn frac_1_pi(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::FRAC_1_PI)
}

pub fn frac_2_pi(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::FRAC_2_PI)
}

pub fn frac_2_sqrt_pi(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::FRAC_2_SQRT_PI)
}

pub fn frac_pi_3(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::FRAC_PI_3)
}

pub fn frac_pi_6(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::FRAC_PI_6)
}

pub fn frac_pi_8(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::FRAC_PI_8)
}

pub fn f64_min(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, f64::MIN)
}

pub fn f64_max(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, f64::MAX)
}

pub fn golden_ratio(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::GOLDEN_RATIO)
}

pub fn euler_gamma(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_constant(ctx, std::f64::consts::EULER_GAMMA)
}

pub fn f64_classify(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx.arg_f64(0)?;
    let class = match value.classify() {
        std::num::FpCategory::Nan => "nan",
        std::num::FpCategory::Infinite => "infinite",
        std::num::FpCategory::Zero => "zero",
        std::num::FpCategory::Subnormal => "subnormal",
        std::num::FpCategory::Normal => "normal",
    };
    ctx.return_string(class)
}

/// Total order: `-0.0` is below `0.0`, and every NaN is ordered by its sign bit.
pub fn total_cmp(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = ctx.arg_f64(0)?;
    let right = ctx.arg_f64(1)?;
    let order = left.total_cmp(&right);
    let value = match order {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    };
    ctx.return_i64(value)?;
    Ok(())
}

pub fn is_sign_positive(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_bool(ctx.arg_f64(0)?.is_sign_positive());
    Ok(())
}

fn f64_to_bytes(ctx: &mut BuiltinCtx<'_>, big_endian: bool) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx.arg_f64(0)?;
    let bytes = if big_endian {
        value.to_be_bytes()
    } else {
        value.to_le_bytes()
    };
    let values: Vec<i64> = bytes.into_iter().map(i64::from).collect();
    ctx.return_i64_list(&values)
}

pub fn f64_to_be_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    f64_to_bytes(ctx, true)
}

pub fn f64_to_le_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    f64_to_bytes(ctx, false)
}

fn float_byte(number: Number) -> Result<u8, VmError> {
    let value = match number {
        Number::I64(value) => i128::from(value),
        Number::U64(value) => i128::from(value),
        Number::I128(value) => value,
        Number::U128(value) => i128::try_from(value)
            .map_err(|_| VmError::InvalidArgument("float byte is out of range"))?,
        Number::F64(_) => return Err(VmError::TypeError),
    };
    if !(0..=255).contains(&value) {
        return Err(VmError::InvalidArgument("float byte is out of range"));
    }
    u8::try_from(value).map_err(|_| VmError::InvalidArgument("float byte is out of range"))
}

fn read_float_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<[u8; 8], VmError> {
    ctx.require_arity(1)?;
    let list = ctx.arg(0)?;
    let length = ctx.list_len(list)?;
    if length != 8 {
        return Err(VmError::InvalidArgument("float byte length does not match"));
    }
    let mut bytes = [0u8; 8];
    for index in 0..8 {
        let element = ctx.list_get(list, TaggedValue::from_u64(index))?;
        bytes[index as usize] = float_byte(ctx.number_of(element)?)?;
    }
    Ok(bytes)
}

fn f64_from_bytes(ctx: &mut BuiltinCtx<'_>, big_endian: bool) -> Result<(), VmError> {
    let bytes = read_float_bytes(ctx)?;
    let value = if big_endian {
        f64::from_be_bytes(bytes)
    } else {
        f64::from_le_bytes(bytes)
    };
    ctx.return_f64(value)?;
    Ok(())
}

pub fn f64_from_be_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    f64_from_bytes(ctx, true)
}

pub fn f64_from_le_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    f64_from_bytes(ctx, false)
}

pub fn f64_to_ne_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let bytes = ctx.arg_f64(0)?.to_ne_bytes();
    let values: Vec<i64> = bytes.into_iter().map(i64::from).collect();
    ctx.return_i64_list(&values)
}

pub fn f64_from_ne_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let bytes = read_float_bytes(ctx)?;
    ctx.return_f64(f64::from_ne_bytes(bytes))?;
    Ok(())
}

pub fn algebraic_add(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_f64(ctx, f64::algebraic_add)
}

pub fn algebraic_sub(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_f64(ctx, f64::algebraic_sub)
}

pub fn algebraic_mul(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_f64(ctx, f64::algebraic_mul)
}

pub fn algebraic_div(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_f64(ctx, f64::algebraic_div)
}

pub fn algebraic_rem(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_f64(ctx, f64::algebraic_rem)
}

pub fn f64_nan_to(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let value = ctx.arg_f64(0)?;
    let replacement = ctx.arg_f64(1)?;
    ctx.return_f64(if value.is_nan() { replacement } else { value })?;
    Ok(())
}

pub fn f64_abs_diff(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = ctx.arg_f64(0)?;
    let right = ctx.arg_f64(1)?;
    ctx.return_f64((left - right).abs())?;
    Ok(())
}

pub fn f64_is_zero(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_bool(ctx.arg_f64(0)? == 0.0);
    Ok(())
}

pub fn bool_not(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_bool(!ctx.arg_bool(0)?);
    Ok(())
}

pub fn bool_and(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.return_bool(ctx.arg_bool(0)? && ctx.arg_bool(1)?);
    Ok(())
}

pub fn bool_or(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.return_bool(ctx.arg_bool(0)? || ctx.arg_bool(1)?);
    Ok(())
}

pub fn bool_xor(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    ctx.return_bool(ctx.arg_bool(0)? ^ ctx.arg_bool(1)?);
    Ok(())
}

pub fn bool_then(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    if ctx.arg_bool(0)? {
        let value = ctx.arg(1)?;
        ctx.set_return(value)
    } else {
        ctx.return_null();
        Ok(())
    }
}

fn return_i64_constant(ctx: &mut BuiltinCtx<'_>, value: i64) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_i64(value)?;
    Ok(())
}

pub fn f64_radix(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_i64_constant(ctx, i64::from(f64::RADIX))
}

pub fn f64_mantissa_digits(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_i64_constant(ctx, i64::from(f64::MANTISSA_DIGITS))
}

pub fn f64_digits(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_i64_constant(ctx, i64::from(f64::DIGITS))
}

pub fn f64_min_exp(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_i64_constant(ctx, i64::from(f64::MIN_EXP))
}

pub fn f64_max_exp(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_i64_constant(ctx, i64::from(f64::MAX_EXP))
}

pub fn f64_min_10_exp(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_i64_constant(ctx, i64::from(f64::MIN_10_EXP))
}

pub fn f64_max_10_exp(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_i64_constant(ctx, i64::from(f64::MAX_10_EXP))
}

/// Truncate toward zero. The bound is the first power of two that does not fit,
/// so a saturating float-to-integer cast cannot round a too-large value back
/// into range. Non-finite values and results outside the destination are null.
fn trunc_integer(value: f64, lower: f64, upper: f64) -> Option<f64> {
    if !value.is_finite() {
        return None;
    }
    let truncated = value.trunc();
    if truncated >= lower && truncated < upper {
        Some(truncated)
    } else {
        None
    }
}

fn exact_signed(truncated: f64) -> Option<i128> {
    let converted = truncated as i128;
    (converted as f64 == truncated).then_some(converted)
}

fn exact_unsigned(truncated: f64) -> Option<u128> {
    let converted = truncated as u128;
    (converted as f64 == truncated).then_some(converted)
}

pub fn trunc_i64(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let limit = (1u64 << 63) as f64;
    let number = trunc_integer(ctx.arg_f64(0)?, -limit, limit)
        .and_then(exact_signed)
        .and_then(|bits| i64::try_from(bits).ok())
        .map(Number::I64);
    match number {
        Some(number) => ctx.return_number(number),
        None => {
            ctx.return_null();
            Ok(())
        }
    }
}

pub fn trunc_u64(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let number = trunc_integer(ctx.arg_f64(0)?, 0.0, (1u128 << 64) as f64)
        .and_then(exact_unsigned)
        .and_then(|bits| u64::try_from(bits).ok())
        .map(Number::U64);
    match number {
        Some(number) => ctx.return_number(number),
        None => {
            ctx.return_null();
            Ok(())
        }
    }
}

pub fn trunc_i128(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let limit = (1u128 << 127) as f64;
    let number = trunc_integer(ctx.arg_f64(0)?, -limit, limit)
        .and_then(exact_signed)
        .map(Number::I128);
    match number {
        Some(number) => ctx.return_number(number),
        None => {
            ctx.return_null();
            Ok(())
        }
    }
}

pub fn trunc_u128(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let limit = ((1u128 << 127) as f64) * 2.0;
    let number = trunc_integer(ctx.arg_f64(0)?, 0.0, limit)
        .and_then(exact_unsigned)
        .map(Number::U128);
    match number {
        Some(number) => ctx.return_number(number),
        None => {
            ctx.return_null();
            Ok(())
        }
    }
}

/// Return `right` only when `left` compares as `prefer_right`. Equal values and
/// unordered pairs (NaN) keep `left`, including its original numeric kind.
fn select_bound(left: Number, right: Number, prefer_right: Ordering) -> Number {
    match left.partial_cmp_numeric(right) {
        Some(order) if order == prefer_right => right,
        _ => left,
    }
}

pub fn min(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = ctx.arg_number(0)?;
    let right = ctx.arg_number(1)?;
    ctx.return_number(select_bound(left, right, Ordering::Greater))
}

pub fn max(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = ctx.arg_number(0)?;
    let right = ctx.arg_number(1)?;
    ctx.return_number(select_bound(left, right, Ordering::Less))
}

pub fn clamp(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let value = ctx.arg_number(0)?;
    let low = ctx.arg_number(1)?;
    let high = ctx.arg_number(2)?;
    if low.partial_cmp_numeric(high) == Some(Ordering::Greater) {
        return Err(VmError::InvalidArgument("clamp bounds are out of order"));
    }
    let raised = select_bound(value, low, Ordering::Less);
    ctx.return_number(select_bound(raised, high, Ordering::Greater))
}

/// `-1`, `0`, or `1` by the exact numeric order against zero. NaN is unordered
/// and returns null. Negative zero compares equal to zero, so its sign is `0`.
pub fn sign(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx.arg_number(0)?;
    match value.partial_cmp_numeric(Number::I64(0)) {
        Some(Ordering::Less) => ctx.return_i64(-1),
        Some(Ordering::Equal) => ctx.return_i64(0),
        Some(Ordering::Greater) => ctx.return_i64(1),
        None => {
            ctx.return_null();
            Ok(())
        }
    }
}
