//! Integer bit operations on the runtime scalar width.
//!
//! `i64` and `u64` use 64 bits. `i128` and `u128` use 128 bits. `isize` and
//! `usize` share those 64-bit layouts. Narrower integers are not accepted:
//! their payload is a 64-bit scalar, so a 64-bit count would not be their width.

use interpreter::{BuiltinCtx, VmError};
use runtime::{Number, TaggedValue};

#[derive(Clone, Copy)]
enum IntBits {
    U64(u64),
    U128(u128),
}

#[derive(Clone, Copy)]
struct Integer {
    signed: bool,
    bits: IntBits,
}

fn integer(number: Number) -> Result<Integer, VmError> {
    match number {
        Number::I64(value) => Ok(Integer {
            signed: true,
            bits: IntBits::U64(value as u64),
        }),
        Number::U64(value) => Ok(Integer {
            signed: false,
            bits: IntBits::U64(value),
        }),
        Number::I128(value) => Ok(Integer {
            signed: true,
            bits: IntBits::U128(value as u128),
        }),
        Number::U128(value) => Ok(Integer {
            signed: false,
            bits: IntBits::U128(value),
        }),
        Number::F64(_) => Err(VmError::TypeError),
    }
}

fn width(bits: &IntBits) -> u32 {
    match bits {
        IntBits::U64(_) => 64,
        IntBits::U128(_) => 128,
    }
}

fn ones(bits: &IntBits) -> u32 {
    match bits {
        IntBits::U64(value) => value.count_ones(),
        IntBits::U128(value) => value.count_ones(),
    }
}

fn non_positive(value: &Integer) -> bool {
    match value.bits {
        IntBits::U64(bits) => (value.signed && (bits as i64) <= 0) || (!value.signed && bits == 0),
        IntBits::U128(bits) => {
            (value.signed && (bits as i128) <= 0) || (!value.signed && bits == 0)
        }
    }
}

fn restore(value: &Integer, bits: IntBits) -> Number {
    match (value.signed, bits) {
        (true, IntBits::U64(bits)) => Number::I64(bits as i64),
        (false, IntBits::U64(bits)) => Number::U64(bits),
        (true, IntBits::U128(bits)) => Number::I128(bits as i128),
        (false, IntBits::U128(bits)) => Number::U128(bits),
    }
}

fn unary_count(ctx: &mut BuiltinCtx<'_>, count: fn(&IntBits) -> u32) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    ctx.return_i64(i64::from(count(&value.bits)))?;
    Ok(())
}

pub fn count_ones(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_count(ctx, ones)
}

pub fn count_zeros(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_count(ctx, |bits| width(bits) - ones(bits))
}

pub fn leading_zeros(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_count(ctx, |bits| match bits {
        IntBits::U64(value) => value.leading_zeros(),
        IntBits::U128(value) => value.leading_zeros(),
    })
}

pub fn trailing_zeros(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_count(ctx, |bits| match bits {
        IntBits::U64(value) => value.trailing_zeros(),
        IntBits::U128(value) => value.trailing_zeros(),
    })
}

pub fn leading_ones(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_count(ctx, |bits| match bits {
        IntBits::U64(value) => value.leading_ones(),
        IntBits::U128(value) => value.leading_ones(),
    })
}

pub fn trailing_ones(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_count(ctx, |bits| match bits {
        IntBits::U64(value) => value.trailing_ones(),
        IntBits::U128(value) => value.trailing_ones(),
    })
}

fn unary_bits(ctx: &mut BuiltinCtx<'_>, map: fn(&IntBits) -> IntBits) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    ctx.return_number(restore(&value, map(&value.bits)))
}

pub fn reverse_bits(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_bits(ctx, |bits| match bits {
        IntBits::U64(value) => IntBits::U64(value.reverse_bits()),
        IntBits::U128(value) => IntBits::U128(value.reverse_bits()),
    })
}

pub fn swap_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_bits(ctx, |bits| match bits {
        IntBits::U64(value) => IntBits::U64(value.swap_bytes()),
        IntBits::U128(value) => IntBits::U128(value.swap_bytes()),
    })
}

fn rotate(ctx: &mut BuiltinCtx<'_>, left: bool) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let value = integer(ctx.arg_number(0)?)?;
    let distance = ctx.arg_i64(1)?;
    if distance < 0 {
        return Err(VmError::InvalidArgument(
            "rotate distance must not be negative",
        ));
    }
    let bits_width = u128::from(width(&value.bits));
    let distance = u32::try_from(
        u128::from(u64::try_from(distance).map_err(|_| VmError::NumericOverflow)?) % bits_width,
    )
    .map_err(|_| VmError::NumericOverflow)?;
    let rotated = match value.bits {
        IntBits::U64(bits) => IntBits::U64(if left {
            bits.rotate_left(distance)
        } else {
            bits.rotate_right(distance)
        }),
        IntBits::U128(bits) => IntBits::U128(if left {
            bits.rotate_left(distance)
        } else {
            bits.rotate_right(distance)
        }),
    };
    ctx.return_number(restore(&value, rotated))
}

pub fn rotate_left(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    rotate(ctx, true)
}

pub fn rotate_right(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    rotate(ctx, false)
}

pub fn is_power_of_two(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    ctx.return_bool(!non_positive(&value) && ones(&value.bits) == 1);
    Ok(())
}

pub fn ilog2(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    if non_positive(&value) {
        ctx.return_null();
        return Ok(());
    }
    let zeros = match value.bits {
        IntBits::U64(bits) => bits.leading_zeros(),
        IntBits::U128(bits) => bits.leading_zeros(),
    };
    ctx.return_i64(i64::from(width(&value.bits) - 1 - zeros))?;
    Ok(())
}

pub fn ilog10(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    let Some(log) = checked_ilog10(&value) else {
        ctx.return_null();
        return Ok(());
    };
    ctx.return_i64(i64::from(log))?;
    Ok(())
}

fn checked_ilog10(value: &Integer) -> Option<u32> {
    match value.bits {
        IntBits::U64(bits) => {
            if value.signed {
                (bits as i64).checked_ilog10()
            } else {
                bits.checked_ilog10()
            }
        }
        IntBits::U128(bits) => {
            if value.signed {
                (bits as i128).checked_ilog10()
            } else {
                bits.checked_ilog10()
            }
        }
    }
}

/// Absolute difference of two integers with the same runtime width.
/// The result is unsigned: `u64` for 64-bit inputs and `u128` for 128-bit inputs.
pub fn abs_diff(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    let difference = match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) if left.signed == right.signed => {
            let difference = if left.signed {
                (left_bits as i64).abs_diff(right_bits as i64)
            } else {
                left_bits.abs_diff(right_bits)
            };
            Number::U64(difference)
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) if left.signed == right.signed => {
            let difference = if left.signed {
                (left_bits as i128).abs_diff(right_bits as i128)
            } else {
                left_bits.abs_diff(right_bits)
            };
            Number::U128(difference)
        }
        _ => return Err(VmError::TypeError),
    };
    ctx.return_number(difference)
}

enum BinOp {
    Add,
    Sub,
    Mul,
}

#[derive(Clone, Copy)]
enum OverflowMode {
    Wrapping,
    Saturating,
    Checked,
    Strict,
}

fn require_overflow(
    ctx: &mut BuiltinCtx<'_>,
    signed: bool,
    bits: Option<IntBits>,
) -> Result<(), VmError> {
    if bits.is_none() {
        return Err(VmError::NumericOverflow);
    }
    finish_overflow(ctx, signed, bits)
}

fn finish_overflow(
    ctx: &mut BuiltinCtx<'_>,
    signed: bool,
    bits: Option<IntBits>,
) -> Result<(), VmError> {
    let Some(bits) = bits else {
        ctx.return_null();
        return Ok(());
    };
    ctx.return_number(restore(
        &Integer {
            signed,
            bits: IntBits::U64(0),
        },
        bits,
    ))
}

macro_rules! overflow_bin {
    ($left:expr, $right:expr, $op:expr, $mode:expr) => {
        match ($op, $mode) {
            (BinOp::Add, OverflowMode::Wrapping) => Some($left.wrapping_add($right)),
            (BinOp::Add, OverflowMode::Saturating) => Some($left.saturating_add($right)),
            (BinOp::Add, OverflowMode::Checked) => $left.checked_add($right),
            (BinOp::Sub, OverflowMode::Wrapping) => Some($left.wrapping_sub($right)),
            (BinOp::Sub, OverflowMode::Saturating) => Some($left.saturating_sub($right)),
            (BinOp::Sub, OverflowMode::Checked) => $left.checked_sub($right),
            (BinOp::Mul, OverflowMode::Wrapping) => Some($left.wrapping_mul($right)),
            (BinOp::Mul, OverflowMode::Saturating) => Some($left.saturating_mul($right)),
            (BinOp::Mul, OverflowMode::Checked) | (BinOp::Mul, OverflowMode::Strict) => {
                $left.checked_mul($right)
            }
            (BinOp::Add, OverflowMode::Strict) => $left.checked_add($right),
            (BinOp::Sub, OverflowMode::Strict) => $left.checked_sub($right),
        }
    };
}

fn binary_overflow(ctx: &mut BuiltinCtx<'_>, op: BinOp, mode: OverflowMode) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    if left.signed != right.signed {
        return Err(VmError::TypeError);
    }
    let bits = match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => {
            let bits = if left.signed {
                overflow_bin!(left_bits as i64, right_bits as i64, op, mode)
                    .map(|value| value as u64)
            } else {
                overflow_bin!(left_bits, right_bits, op, mode)
            };
            bits.map(IntBits::U64)
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => {
            let bits = if left.signed {
                overflow_bin!(left_bits as i128, right_bits as i128, op, mode)
                    .map(|value| value as u128)
            } else {
                overflow_bin!(left_bits, right_bits, op, mode)
            };
            bits.map(IntBits::U128)
        }
        _ => return Err(VmError::TypeError),
    };
    if matches!(mode, OverflowMode::Strict) {
        require_overflow(ctx, left.signed, bits)
    } else {
        finish_overflow(ctx, left.signed, bits)
    }
}

fn unary_neg(ctx: &mut BuiltinCtx<'_>, mode: OverflowMode) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    let bits = match value.bits {
        IntBits::U64(bits) => {
            let bits = if value.signed {
                match mode {
                    OverflowMode::Wrapping => Some((bits as i64).wrapping_neg() as u64),
                    OverflowMode::Saturating => Some((bits as i64).saturating_neg() as u64),
                    OverflowMode::Checked | OverflowMode::Strict => {
                        (bits as i64).checked_neg().map(|value| value as u64)
                    }
                }
            } else {
                match mode {
                    OverflowMode::Wrapping => Some(bits.wrapping_neg()),
                    OverflowMode::Saturating => Some(0),
                    OverflowMode::Checked | OverflowMode::Strict => bits.checked_neg(),
                }
            };
            bits.map(IntBits::U64)
        }
        IntBits::U128(bits) => {
            let bits = if value.signed {
                match mode {
                    OverflowMode::Wrapping => Some((bits as i128).wrapping_neg() as u128),
                    OverflowMode::Saturating => Some((bits as i128).saturating_neg() as u128),
                    OverflowMode::Checked | OverflowMode::Strict => {
                        (bits as i128).checked_neg().map(|value| value as u128)
                    }
                }
            } else {
                match mode {
                    OverflowMode::Wrapping => Some(bits.wrapping_neg()),
                    OverflowMode::Saturating => Some(0),
                    OverflowMode::Checked | OverflowMode::Strict => bits.checked_neg(),
                }
            };
            bits.map(IntBits::U128)
        }
    };
    if matches!(mode, OverflowMode::Strict) {
        require_overflow(ctx, value.signed, bits)
    } else {
        finish_overflow(ctx, value.signed, bits)
    }
}

fn checked_div_rem(ctx: &mut BuiltinCtx<'_>, division: bool, strict: bool) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    if left.signed != right.signed {
        return Err(VmError::TypeError);
    }
    if strict && is_zero(&right) {
        return Err(VmError::InvalidArgument("divisor must not be zero"));
    }
    let bits = match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => {
            let bits = if left.signed {
                let result = if division {
                    (left_bits as i64).checked_div(right_bits as i64)
                } else {
                    (left_bits as i64).checked_rem(right_bits as i64)
                };
                result.map(|value| value as u64)
            } else if division {
                left_bits.checked_div(right_bits)
            } else {
                left_bits.checked_rem(right_bits)
            };
            bits.map(IntBits::U64)
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => {
            let bits = if left.signed {
                let result = if division {
                    (left_bits as i128).checked_div(right_bits as i128)
                } else {
                    (left_bits as i128).checked_rem(right_bits as i128)
                };
                result.map(|value| value as u128)
            } else if division {
                left_bits.checked_div(right_bits)
            } else {
                left_bits.checked_rem(right_bits)
            };
            bits.map(IntBits::U128)
        }
        _ => return Err(VmError::TypeError),
    };
    if strict {
        require_overflow(ctx, left.signed, bits)
    } else {
        finish_overflow(ctx, left.signed, bits)
    }
}

pub fn wrapping_add(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_overflow(ctx, BinOp::Add, OverflowMode::Wrapping)
}

pub fn wrapping_sub(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_overflow(ctx, BinOp::Sub, OverflowMode::Wrapping)
}

pub fn wrapping_mul(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_overflow(ctx, BinOp::Mul, OverflowMode::Wrapping)
}

pub fn wrapping_neg(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_neg(ctx, OverflowMode::Wrapping)
}

pub fn saturating_add(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_overflow(ctx, BinOp::Add, OverflowMode::Saturating)
}

pub fn saturating_sub(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_overflow(ctx, BinOp::Sub, OverflowMode::Saturating)
}

pub fn saturating_mul(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_overflow(ctx, BinOp::Mul, OverflowMode::Saturating)
}

pub fn saturating_neg(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_neg(ctx, OverflowMode::Saturating)
}

pub fn checked_add(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_overflow(ctx, BinOp::Add, OverflowMode::Checked)
}

pub fn checked_sub(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_overflow(ctx, BinOp::Sub, OverflowMode::Checked)
}

pub fn checked_mul(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_overflow(ctx, BinOp::Mul, OverflowMode::Checked)
}

pub fn checked_div(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    checked_div_rem(ctx, true, false)
}

pub fn checked_rem(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    checked_div_rem(ctx, false, false)
}

pub fn strict_add(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_overflow(ctx, BinOp::Add, OverflowMode::Strict)
}

pub fn strict_sub(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_overflow(ctx, BinOp::Sub, OverflowMode::Strict)
}

pub fn strict_mul(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    binary_overflow(ctx, BinOp::Mul, OverflowMode::Strict)
}

pub fn strict_div(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    checked_div_rem(ctx, true, true)
}

pub fn strict_rem(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    checked_div_rem(ctx, false, true)
}

pub fn strict_neg(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_neg(ctx, OverflowMode::Strict)
}

pub fn checked_neg(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unary_neg(ctx, OverflowMode::Checked)
}

fn is_zero(value: &Integer) -> bool {
    match value.bits {
        IntBits::U64(bits) => bits == 0,
        IntBits::U128(bits) => bits == 0,
    }
}

/// `MIN / -1` and `MIN % -1` overflow the signed result.
fn euclid_overflows(left: &Integer, right: &Integer) -> bool {
    if !left.signed {
        return false;
    }
    match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => {
            (left_bits as i64) == i64::MIN && (right_bits as i64) == -1
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => {
            (left_bits as i128) == i128::MIN && (right_bits as i128) == -1
        }
        _ => false,
    }
}

fn require_same_sign(left: &Integer, right: &Integer) -> Result<(), VmError> {
    if left.signed == right.signed {
        Ok(())
    } else {
        Err(VmError::TypeError)
    }
}

fn int_euclid(ctx: &mut BuiltinCtx<'_>, division: bool) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    require_same_sign(&left, &right)?;
    if is_zero(&right) {
        return Err(VmError::InvalidArgument("divisor must not be zero"));
    }
    if euclid_overflows(&left, &right) {
        return Err(VmError::NumericOverflow);
    }
    let bits = match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => {
            let bits = if left.signed {
                if division {
                    (left_bits as i64).div_euclid(right_bits as i64) as u64
                } else {
                    (left_bits as i64).rem_euclid(right_bits as i64) as u64
                }
            } else if division {
                left_bits.div_euclid(right_bits)
            } else {
                left_bits.rem_euclid(right_bits)
            };
            IntBits::U64(bits)
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => {
            let bits = if left.signed {
                if division {
                    (left_bits as i128).div_euclid(right_bits as i128) as u128
                } else {
                    (left_bits as i128).rem_euclid(right_bits as i128) as u128
                }
            } else if division {
                left_bits.div_euclid(right_bits)
            } else {
                left_bits.rem_euclid(right_bits)
            };
            IntBits::U128(bits)
        }
        _ => return Err(VmError::TypeError),
    };
    finish_overflow(ctx, left.signed, Some(bits))
}

pub fn int_div_euclid(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_euclid(ctx, true)
}

pub fn int_rem_euclid(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_euclid(ctx, false)
}

pub fn unsigned_abs(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    if !value.signed {
        return Err(VmError::TypeError);
    }
    let number = match value.bits {
        IntBits::U64(bits) => Number::U64((bits as i64).unsigned_abs()),
        IntBits::U128(bits) => Number::U128((bits as i128).unsigned_abs()),
    };
    ctx.return_number(number)
}

pub fn int_midpoint(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    require_same_sign(&left, &right)?;
    let bits = match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => {
            let bits = if left.signed {
                (left_bits as i64).midpoint(right_bits as i64) as u64
            } else {
                left_bits.midpoint(right_bits)
            };
            IntBits::U64(bits)
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => {
            let bits = if left.signed {
                (left_bits as i128).midpoint(right_bits as i128) as u128
            } else {
                left_bits.midpoint(right_bits)
            };
            IntBits::U128(bits)
        }
        _ => return Err(VmError::TypeError),
    };
    finish_overflow(ctx, left.signed, Some(bits))
}

fn shift_distance(ctx: &mut BuiltinCtx<'_>) -> Result<u32, VmError> {
    let distance = ctx.arg_i64(1)?;
    if distance < 0 {
        return Err(VmError::InvalidArgument(
            "shift distance must not be negative",
        ));
    }
    u32::try_from(distance).map_err(|_| VmError::NumericOverflow)
}

#[derive(Clone, Copy)]
enum ShiftMode {
    Wrapping,
    Checked,
    Strict,
}

fn int_shift(ctx: &mut BuiltinCtx<'_>, leftward: bool, mode: ShiftMode) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let value = integer(ctx.arg_number(0)?)?;
    let distance = shift_distance(ctx)?;
    let checked = !matches!(mode, ShiftMode::Wrapping);
    if checked && distance >= width(&value.bits) {
        if matches!(mode, ShiftMode::Strict) {
            return Err(VmError::NumericOverflow);
        }
        ctx.return_null();
        return Ok(());
    }
    let bits = match value.bits {
        IntBits::U64(bits) => {
            let bits = if value.signed {
                let number = bits as i64;
                let shifted = if checked {
                    if leftward {
                        number.checked_shl(distance)
                    } else {
                        number.checked_shr(distance)
                    }
                } else if leftward {
                    Some(number.wrapping_shl(distance))
                } else {
                    Some(number.wrapping_shr(distance))
                };
                shifted.map(|shifted| shifted as u64)
            } else if checked {
                if leftward {
                    bits.checked_shl(distance)
                } else {
                    bits.checked_shr(distance)
                }
            } else if leftward {
                Some(bits.wrapping_shl(distance))
            } else {
                Some(bits.wrapping_shr(distance))
            };
            bits.map(IntBits::U64)
        }
        IntBits::U128(bits) => {
            let bits = if value.signed {
                let number = bits as i128;
                let shifted = if checked {
                    if leftward {
                        number.checked_shl(distance)
                    } else {
                        number.checked_shr(distance)
                    }
                } else if leftward {
                    Some(number.wrapping_shl(distance))
                } else {
                    Some(number.wrapping_shr(distance))
                };
                shifted.map(|shifted| shifted as u128)
            } else if checked {
                if leftward {
                    bits.checked_shl(distance)
                } else {
                    bits.checked_shr(distance)
                }
            } else if leftward {
                Some(bits.wrapping_shl(distance))
            } else {
                Some(bits.wrapping_shr(distance))
            };
            bits.map(IntBits::U128)
        }
    };
    finish_overflow(ctx, value.signed, bits)
}

pub fn checked_shl(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_shift(ctx, true, ShiftMode::Checked)
}

pub fn checked_shr(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_shift(ctx, false, ShiftMode::Checked)
}

pub fn wrapping_shl(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_shift(ctx, true, ShiftMode::Wrapping)
}

pub fn wrapping_shr(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_shift(ctx, false, ShiftMode::Wrapping)
}

pub fn strict_shl(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_shift(ctx, true, ShiftMode::Strict)
}

pub fn strict_shr(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_shift(ctx, false, ShiftMode::Strict)
}

fn power_exponent(ctx: &mut BuiltinCtx<'_>) -> Result<u32, VmError> {
    let exponent = ctx.arg_i64(1)?;
    if exponent < 0 {
        return Err(VmError::InvalidArgument("exponent must not be negative"));
    }
    u32::try_from(exponent).map_err(|_| VmError::NumericOverflow)
}

fn int_pow(ctx: &mut BuiltinCtx<'_>, mode: OverflowMode) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let value = integer(ctx.arg_number(0)?)?;
    let exponent = power_exponent(ctx)?;
    let bits = match value.bits {
        IntBits::U64(bits) => {
            let bits = if value.signed {
                let number = bits as i64;
                match mode {
                    OverflowMode::Wrapping => Some(number.wrapping_pow(exponent) as u64),
                    OverflowMode::Saturating => Some(number.saturating_pow(exponent) as u64),
                    OverflowMode::Checked | OverflowMode::Strict => {
                        number.checked_pow(exponent).map(|value| value as u64)
                    }
                }
            } else {
                match mode {
                    OverflowMode::Wrapping => Some(bits.wrapping_pow(exponent)),
                    OverflowMode::Saturating => Some(bits.saturating_pow(exponent)),
                    OverflowMode::Checked | OverflowMode::Strict => bits.checked_pow(exponent),
                }
            };
            bits.map(IntBits::U64)
        }
        IntBits::U128(bits) => {
            let bits = if value.signed {
                let number = bits as i128;
                match mode {
                    OverflowMode::Wrapping => Some(number.wrapping_pow(exponent) as u128),
                    OverflowMode::Saturating => Some(number.saturating_pow(exponent) as u128),
                    OverflowMode::Checked | OverflowMode::Strict => {
                        number.checked_pow(exponent).map(|value| value as u128)
                    }
                }
            } else {
                match mode {
                    OverflowMode::Wrapping => Some(bits.wrapping_pow(exponent)),
                    OverflowMode::Saturating => Some(bits.saturating_pow(exponent)),
                    OverflowMode::Checked | OverflowMode::Strict => bits.checked_pow(exponent),
                }
            };
            bits.map(IntBits::U128)
        }
    };
    if matches!(mode, OverflowMode::Strict) {
        require_overflow(ctx, value.signed, bits)
    } else {
        finish_overflow(ctx, value.signed, bits)
    }
}

pub fn strict_pow(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_pow(ctx, OverflowMode::Strict)
}

pub fn checked_pow(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_pow(ctx, OverflowMode::Checked)
}

pub fn wrapping_pow(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_pow(ctx, OverflowMode::Wrapping)
}

pub fn saturating_pow(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_pow(ctx, OverflowMode::Saturating)
}

fn wrapping_div_rem(ctx: &mut BuiltinCtx<'_>, division: bool) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    require_same_sign(&left, &right)?;
    if is_zero(&right) {
        return Err(VmError::InvalidArgument("divisor must not be zero"));
    }
    let bits = match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => {
            let bits = if left.signed {
                if division {
                    (left_bits as i64).wrapping_div(right_bits as i64) as u64
                } else {
                    (left_bits as i64).wrapping_rem(right_bits as i64) as u64
                }
            } else if division {
                left_bits.wrapping_div(right_bits)
            } else {
                left_bits.wrapping_rem(right_bits)
            };
            IntBits::U64(bits)
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => {
            let bits = if left.signed {
                if division {
                    (left_bits as i128).wrapping_div(right_bits as i128) as u128
                } else {
                    (left_bits as i128).wrapping_rem(right_bits as i128) as u128
                }
            } else if division {
                left_bits.wrapping_div(right_bits)
            } else {
                left_bits.wrapping_rem(right_bits)
            };
            IntBits::U128(bits)
        }
        _ => return Err(VmError::TypeError),
    };
    finish_overflow(ctx, left.signed, Some(bits))
}

pub fn wrapping_div(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wrapping_div_rem(ctx, true)
}

pub fn wrapping_rem(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wrapping_div_rem(ctx, false)
}

fn signed_abs(ctx: &mut BuiltinCtx<'_>, mode: OverflowMode) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    if !value.signed {
        return Err(VmError::TypeError);
    }
    let bits = match value.bits {
        IntBits::U64(bits) => {
            let number = bits as i64;
            let bits = match mode {
                OverflowMode::Wrapping => Some(number.wrapping_abs() as u64),
                OverflowMode::Saturating => Some(number.saturating_abs() as u64),
                OverflowMode::Checked | OverflowMode::Strict => {
                    number.checked_abs().map(|value| value as u64)
                }
            };
            bits.map(IntBits::U64)
        }
        IntBits::U128(bits) => {
            let number = bits as i128;
            let bits = match mode {
                OverflowMode::Wrapping => Some(number.wrapping_abs() as u128),
                OverflowMode::Saturating => Some(number.saturating_abs() as u128),
                OverflowMode::Checked | OverflowMode::Strict => {
                    number.checked_abs().map(|value| value as u128)
                }
            };
            bits.map(IntBits::U128)
        }
    };
    finish_overflow(ctx, true, bits)
}

pub fn saturating_abs(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    signed_abs(ctx, OverflowMode::Saturating)
}

pub fn checked_abs(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    signed_abs(ctx, OverflowMode::Checked)
}

pub fn wrapping_abs(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    signed_abs(ctx, OverflowMode::Wrapping)
}

pub fn int_signum(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    if !value.signed {
        return Err(VmError::TypeError);
    }
    let sign = match value.bits {
        IntBits::U64(bits) => (bits as i64).signum(),
        IntBits::U128(bits) => (bits as i128).signum() as i64,
    };
    ctx.return_i64(sign)?;
    Ok(())
}

pub fn checked_ilog(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let value = integer(ctx.arg_number(0)?)?;
    let base = ctx.arg_i64(1)?;
    if base < 2 {
        return Err(VmError::InvalidArgument(
            "logarithm base must be at least 2",
        ));
    }
    let base = u32::try_from(base).map_err(|_| VmError::NumericOverflow)?;
    let log = match value.bits {
        IntBits::U64(bits) => {
            if value.signed {
                (bits as i64).checked_ilog(i64::from(base))
            } else {
                bits.checked_ilog(u64::from(base))
            }
        }
        IntBits::U128(bits) => {
            if value.signed {
                (bits as i128).checked_ilog(i128::from(base))
            } else {
                bits.checked_ilog(u128::from(base))
            }
        }
    };
    match log {
        Some(log) => ctx.return_i64(i64::from(log))?,
        None => ctx.return_null(),
    }
    Ok(())
}

pub fn cast_unsigned(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    if !value.signed {
        return Err(VmError::TypeError);
    }
    let number = match value.bits {
        IntBits::U64(bits) => Number::U64((bits as i64).cast_unsigned()),
        IntBits::U128(bits) => Number::U128((bits as i128).cast_unsigned()),
    };
    ctx.return_number(number)
}

pub fn cast_signed(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    if value.signed {
        return Err(VmError::TypeError);
    }
    let number = match value.bits {
        IntBits::U64(bits) => Number::I64(bits.cast_signed()),
        IntBits::U128(bits) => Number::I128(bits.cast_signed()),
    };
    ctx.return_number(number)
}

fn checked_euclid(ctx: &mut BuiltinCtx<'_>, division: bool) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    require_same_sign(&left, &right)?;
    let bits = match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => {
            let bits = if left.signed {
                if division {
                    (left_bits as i64)
                        .checked_div_euclid(right_bits as i64)
                        .map(|value| value as u64)
                } else {
                    (left_bits as i64)
                        .checked_rem_euclid(right_bits as i64)
                        .map(|value| value as u64)
                }
            } else if division {
                left_bits.checked_div_euclid(right_bits)
            } else {
                left_bits.checked_rem_euclid(right_bits)
            };
            bits.map(IntBits::U64)
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => {
            let bits = if left.signed {
                if division {
                    (left_bits as i128)
                        .checked_div_euclid(right_bits as i128)
                        .map(|value| value as u128)
                } else {
                    (left_bits as i128)
                        .checked_rem_euclid(right_bits as i128)
                        .map(|value| value as u128)
                }
            } else if division {
                left_bits.checked_div_euclid(right_bits)
            } else {
                left_bits.checked_rem_euclid(right_bits)
            };
            bits.map(IntBits::U128)
        }
        _ => return Err(VmError::TypeError),
    };
    finish_overflow(ctx, left.signed, bits)
}

pub fn checked_div_euclid(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    checked_euclid(ctx, true)
}

pub fn checked_rem_euclid(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    checked_euclid(ctx, false)
}

/// Full product in the next width. 128-bit inputs have no wider result.
pub fn widening_mul(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    require_same_sign(&left, &right)?;
    let number = match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => {
            if left.signed {
                Number::I128((left_bits as i64 as i128) * (right_bits as i64 as i128))
            } else {
                Number::U128(u128::from(left_bits) * u128::from(right_bits))
            }
        }
        _ => return Err(VmError::TypeError),
    };
    ctx.return_number(number)
}

fn next_power(ctx: &mut BuiltinCtx<'_>, checked: bool) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    if value.signed {
        return Err(VmError::TypeError);
    }
    let power = match value.bits {
        IntBits::U64(bits) => bits.checked_next_power_of_two().map(Number::U64),
        IntBits::U128(bits) => bits.checked_next_power_of_two().map(Number::U128),
    };
    match power {
        Some(number) => ctx.return_number(number),
        None if checked => {
            ctx.return_null();
            Ok(())
        }
        None => Err(VmError::NumericOverflow),
    }
}

pub fn next_power_of_two(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    next_power(ctx, false)
}

pub fn checked_next_power_of_two(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    next_power(ctx, true)
}

fn adjust_u64_signed(left: u64, right: i64, op: AdjustOp, mode: OverflowMode) -> Option<u64> {
    match (op, mode) {
        (AdjustOp::Add, OverflowMode::Checked | OverflowMode::Strict) => {
            left.checked_add_signed(right)
        }
        (AdjustOp::Sub, OverflowMode::Checked | OverflowMode::Strict) => {
            left.checked_sub_signed(right)
        }
        (AdjustOp::Add, OverflowMode::Saturating) => Some(left.saturating_add_signed(right)),
        (AdjustOp::Sub, OverflowMode::Saturating) => Some(left.saturating_sub_signed(right)),
        (AdjustOp::Add, OverflowMode::Wrapping) => Some(left.wrapping_add_signed(right)),
        (AdjustOp::Sub, OverflowMode::Wrapping) => Some(left.wrapping_sub_signed(right)),
    }
}

fn adjust_u128_signed(left: u128, right: i128, op: AdjustOp, mode: OverflowMode) -> Option<u128> {
    match (op, mode) {
        (AdjustOp::Add, OverflowMode::Checked | OverflowMode::Strict) => {
            left.checked_add_signed(right)
        }
        (AdjustOp::Sub, OverflowMode::Checked | OverflowMode::Strict) => {
            left.checked_sub_signed(right)
        }
        (AdjustOp::Add, OverflowMode::Saturating) => Some(left.saturating_add_signed(right)),
        (AdjustOp::Sub, OverflowMode::Saturating) => Some(left.saturating_sub_signed(right)),
        (AdjustOp::Add, OverflowMode::Wrapping) => Some(left.wrapping_add_signed(right)),
        (AdjustOp::Sub, OverflowMode::Wrapping) => Some(left.wrapping_sub_signed(right)),
    }
}

/// The receiver is unsigned and the operand is signed, both of the same width.
fn signed_adjust(
    ctx: &mut BuiltinCtx<'_>,
    op: AdjustOp,
    mode: OverflowMode,
) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    if left.signed || !right.signed {
        return Err(VmError::TypeError);
    }
    let bits = match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => {
            adjust_u64_signed(left_bits, right_bits as i64, op, mode).map(IntBits::U64)
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => {
            adjust_u128_signed(left_bits, right_bits as i128, op, mode).map(IntBits::U128)
        }
        _ => return Err(VmError::TypeError),
    };
    if matches!(mode, OverflowMode::Strict) {
        require_overflow(ctx, false, bits)
    } else {
        finish_overflow(ctx, false, bits)
    }
}

pub fn checked_add_signed(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    signed_adjust(ctx, AdjustOp::Add, OverflowMode::Checked)
}

pub fn checked_sub_signed(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    signed_adjust(ctx, AdjustOp::Sub, OverflowMode::Checked)
}

pub fn is_multiple_of(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    require_same_sign(&left, &right)?;
    if is_zero(&right) {
        ctx.return_bool(false);
        return Ok(());
    }
    // `MIN % -1` overflows, and the minimum is a multiple of -1.
    if euclid_overflows(&left, &right) {
        ctx.return_bool(true);
        return Ok(());
    }
    let multiple = match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => {
            if left.signed {
                (left_bits as i64) % (right_bits as i64) == 0
            } else {
                left_bits % right_bits == 0
            }
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => {
            if left.signed {
                (left_bits as i128) % (right_bits as i128) == 0
            } else {
                left_bits % right_bits == 0
            }
        }
        _ => return Err(VmError::TypeError),
    };
    ctx.return_bool(multiple);
    Ok(())
}

fn saturating_signed_max(bits: &IntBits) -> IntBits {
    match bits {
        IntBits::U64(_) => IntBits::U64(i64::MAX as u64),
        IntBits::U128(_) => IntBits::U128(i128::MAX as u128),
    }
}

/// Toward-zero quotient. A zero divisor is an error. The signed minimum
/// divided by -1 saturates to the maximum.
pub fn saturating_div(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    require_same_sign(&left, &right)?;
    if is_zero(&right) {
        return Err(VmError::InvalidArgument("divisor must not be zero"));
    }
    let bits = match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => {
            let bits = if left.signed {
                (left_bits as i64).saturating_div(right_bits as i64) as u64
            } else {
                left_bits.saturating_div(right_bits)
            };
            IntBits::U64(bits)
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => {
            let bits = if left.signed {
                (left_bits as i128).saturating_div(right_bits as i128) as u128
            } else {
                left_bits.saturating_div(right_bits)
            };
            IntBits::U128(bits)
        }
        _ => return Err(VmError::TypeError),
    };
    finish_overflow(ctx, left.signed, Some(bits))
}

/// Euclidean quotient. A zero divisor is an error. The signed minimum divided
/// by -1 saturates to the maximum.
pub fn saturating_div_euclid(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    require_same_sign(&left, &right)?;
    if is_zero(&right) {
        return Err(VmError::InvalidArgument("divisor must not be zero"));
    }
    if euclid_overflows(&left, &right) {
        return finish_overflow(ctx, true, Some(saturating_signed_max(&left.bits)));
    }
    let bits = match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => {
            let bits = if left.signed {
                (left_bits as i64).div_euclid(right_bits as i64) as u64
            } else {
                left_bits.div_euclid(right_bits)
            };
            IntBits::U64(bits)
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => {
            let bits = if left.signed {
                (left_bits as i128).div_euclid(right_bits as i128) as u128
            } else {
                left_bits.div_euclid(right_bits)
            };
            IntBits::U128(bits)
        }
        _ => return Err(VmError::TypeError),
    };
    finish_overflow(ctx, left.signed, Some(bits))
}

#[derive(Clone, Copy)]
enum AdjustOp {
    Add,
    Sub,
}

fn adjust_i64(left: i64, right: u64, op: AdjustOp, mode: OverflowMode) -> Option<u64> {
    let value = match (op, mode) {
        (AdjustOp::Add, OverflowMode::Checked | OverflowMode::Strict) => {
            left.checked_add_unsigned(right)
        }
        (AdjustOp::Sub, OverflowMode::Checked | OverflowMode::Strict) => {
            left.checked_sub_unsigned(right)
        }
        (AdjustOp::Add, OverflowMode::Saturating) => Some(left.saturating_add_unsigned(right)),
        (AdjustOp::Sub, OverflowMode::Saturating) => Some(left.saturating_sub_unsigned(right)),
        (AdjustOp::Add, OverflowMode::Wrapping) => Some(left.wrapping_add_unsigned(right)),
        (AdjustOp::Sub, OverflowMode::Wrapping) => Some(left.wrapping_sub_unsigned(right)),
    };
    value.map(|number| number as u64)
}

fn adjust_i128(left: i128, right: u128, op: AdjustOp, mode: OverflowMode) -> Option<u128> {
    let value = match (op, mode) {
        (AdjustOp::Add, OverflowMode::Checked | OverflowMode::Strict) => {
            left.checked_add_unsigned(right)
        }
        (AdjustOp::Sub, OverflowMode::Checked | OverflowMode::Strict) => {
            left.checked_sub_unsigned(right)
        }
        (AdjustOp::Add, OverflowMode::Saturating) => Some(left.saturating_add_unsigned(right)),
        (AdjustOp::Sub, OverflowMode::Saturating) => Some(left.saturating_sub_unsigned(right)),
        (AdjustOp::Add, OverflowMode::Wrapping) => Some(left.wrapping_add_unsigned(right)),
        (AdjustOp::Sub, OverflowMode::Wrapping) => Some(left.wrapping_sub_unsigned(right)),
    };
    value.map(|number| number as u128)
}

/// The receiver is signed and the operand is unsigned, both of the same width.
fn unsigned_adjust(
    ctx: &mut BuiltinCtx<'_>,
    op: AdjustOp,
    mode: OverflowMode,
) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    if !left.signed || right.signed {
        return Err(VmError::TypeError);
    }
    let bits = match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => {
            adjust_i64(left_bits as i64, right_bits, op, mode).map(IntBits::U64)
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => {
            adjust_i128(left_bits as i128, right_bits, op, mode).map(IntBits::U128)
        }
        _ => return Err(VmError::TypeError),
    };
    if matches!(mode, OverflowMode::Strict) {
        require_overflow(ctx, true, bits)
    } else {
        finish_overflow(ctx, true, bits)
    }
}

pub fn checked_add_unsigned(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unsigned_adjust(ctx, AdjustOp::Add, OverflowMode::Checked)
}

pub fn checked_sub_unsigned(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unsigned_adjust(ctx, AdjustOp::Sub, OverflowMode::Checked)
}

pub fn saturating_add_unsigned(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unsigned_adjust(ctx, AdjustOp::Add, OverflowMode::Saturating)
}

pub fn saturating_sub_unsigned(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unsigned_adjust(ctx, AdjustOp::Sub, OverflowMode::Saturating)
}

pub fn wrapping_add_unsigned(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unsigned_adjust(ctx, AdjustOp::Add, OverflowMode::Wrapping)
}

pub fn wrapping_sub_unsigned(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unsigned_adjust(ctx, AdjustOp::Sub, OverflowMode::Wrapping)
}

pub fn strict_add_unsigned(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unsigned_adjust(ctx, AdjustOp::Add, OverflowMode::Strict)
}

pub fn strict_sub_unsigned(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unsigned_adjust(ctx, AdjustOp::Sub, OverflowMode::Strict)
}

fn digit_radix(value: i64) -> Result<u32, VmError> {
    if !(2..=36).contains(&value) {
        return Err(VmError::InvalidArgument("digit radix must be from 2 to 36"));
    }
    u32::try_from(value).map_err(|_| VmError::NumericOverflow)
}

fn signed_magnitude(value: &Integer) -> (bool, u128) {
    match value.bits {
        IntBits::U64(bits) if value.signed => {
            let signed = bits as i64;
            if signed < 0 {
                (true, u128::from(signed.unsigned_abs()))
            } else {
                (false, u128::from(signed as u64))
            }
        }
        IntBits::U64(bits) => (false, u128::from(bits)),
        IntBits::U128(bits) if value.signed => {
            let signed = bits as i128;
            if signed < 0 {
                (true, signed.unsigned_abs())
            } else {
                (false, signed as u128)
            }
        }
        IntBits::U128(bits) => (false, bits),
    }
}

fn format_magnitude(mut magnitude: u128, radix: u32) -> String {
    if magnitude == 0 {
        return "0".to_string();
    }
    let base = u128::from(radix);
    let mut text = String::new();
    while magnitude > 0 {
        let digit = (magnitude % base) as u32;
        let ch = if digit < 10 {
            char::from(b'0' + digit as u8)
        } else {
            char::from(b'a' + digit as u8 - 10)
        };
        text.insert(0, ch);
        magnitude /= base;
    }
    text
}

pub fn int_to_str_radix(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let value = integer(ctx.arg_number(0)?)?;
    let radix = digit_radix(ctx.arg_i64(1)?)?;
    let (negative, magnitude) = signed_magnitude(&value);
    let mut text = format_magnitude(magnitude, radix);
    if negative {
        text.insert(0, '-');
    }
    ctx.return_string(&text)
}

fn parsed_radix(ctx: &mut BuiltinCtx<'_>) -> Result<(String, u32), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let radix = digit_radix(ctx.arg_i64(1)?)?;
    Ok((text, radix))
}

pub fn i64_from_str_radix(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let (text, radix) = parsed_radix(ctx)?;
    match i64::from_str_radix(&text, radix) {
        Ok(value) => ctx.return_i64(value)?,
        Err(_) => ctx.return_null(),
    }
    Ok(())
}

pub fn u64_from_str_radix(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let (text, radix) = parsed_radix(ctx)?;
    match u64::from_str_radix(&text, radix) {
        Ok(value) => ctx.return_number(Number::U64(value))?,
        Err(_) => ctx.return_null(),
    }
    Ok(())
}

pub fn i128_from_str_radix(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let (text, radix) = parsed_radix(ctx)?;
    match i128::from_str_radix(&text, radix) {
        Ok(value) => ctx.return_number(Number::I128(value))?,
        Err(_) => ctx.return_null(),
    }
    Ok(())
}

pub fn u128_from_str_radix(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let (text, radix) = parsed_radix(ctx)?;
    match u128::from_str_radix(&text, radix) {
        Ok(value) => ctx.return_number(Number::U128(value))?,
        Err(_) => ctx.return_null(),
    }
    Ok(())
}

fn signed_div_ceil_i64(left: i64, right: i64) -> Option<i64> {
    let quotient = left.checked_div(right)?;
    let remainder = left.checked_rem(right)?;
    if remainder == 0 {
        return Some(quotient);
    }
    let correction = 1 + ((left ^ right) >> 63);
    quotient.checked_add(correction)
}

fn signed_div_ceil_i128(left: i128, right: i128) -> Option<i128> {
    let quotient = left.checked_div(right)?;
    let remainder = left.checked_rem(right)?;
    if remainder == 0 {
        return Some(quotient);
    }
    let correction = 1 + ((left ^ right) >> 127);
    quotient.checked_add(correction)
}

fn unsigned_div_ceil_u64(left: u64, right: u64) -> Option<u64> {
    let quotient = left.checked_div(right)?;
    let remainder = left.checked_rem(right)?;
    if remainder > 0 {
        quotient.checked_add(1)
    } else {
        Some(quotient)
    }
}

fn unsigned_div_ceil_u128(left: u128, right: u128) -> Option<u128> {
    let quotient = left.checked_div(right)?;
    let remainder = left.checked_rem(right)?;
    if remainder > 0 {
        quotient.checked_add(1)
    } else {
        Some(quotient)
    }
}

fn ceil_quotient(left: &Integer, right: &Integer) -> Option<IntBits> {
    match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) if left.signed => {
            signed_div_ceil_i64(left_bits as i64, right_bits as i64)
                .map(|value| IntBits::U64(value as u64))
        }
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => {
            unsigned_div_ceil_u64(left_bits, right_bits).map(IntBits::U64)
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) if left.signed => {
            signed_div_ceil_i128(left_bits as i128, right_bits as i128)
                .map(|value| IntBits::U128(value as u128))
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => {
            unsigned_div_ceil_u128(left_bits, right_bits).map(IntBits::U128)
        }
        _ => None,
    }
}

/// Toward positive infinity. A zero divisor is an error. Overflow, including
/// the signed minimum divided by -1, is `NumericOverflow`. The checked form
/// returns null for both a zero divisor and overflow.
fn div_ceil_op(ctx: &mut BuiltinCtx<'_>, checked: bool) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    require_same_sign(&left, &right)?;
    if is_zero(&right) {
        if checked {
            ctx.return_null();
            return Ok(());
        }
        return Err(VmError::InvalidArgument("divisor must not be zero"));
    }
    let bits = match (left.bits, right.bits) {
        (IntBits::U64(_), IntBits::U64(_)) | (IntBits::U128(_), IntBits::U128(_)) => {
            ceil_quotient(&left, &right)
        }
        _ => return Err(VmError::TypeError),
    };
    if checked {
        finish_overflow(ctx, left.signed, bits)
    } else {
        require_overflow(ctx, left.signed, bits)
    }
}

pub fn div_ceil(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    div_ceil_op(ctx, false)
}

pub fn checked_div_ceil(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    div_ceil_op(ctx, true)
}

fn signed_next_multiple_i64(left: i64, right: i64) -> Option<i64> {
    if right == -1 {
        return Some(left);
    }
    let remainder = left.checked_rem(right)?;
    let adjusted = if (remainder > 0 && right < 0) || (remainder < 0 && right > 0) {
        remainder + right
    } else {
        remainder
    };
    if adjusted == 0 {
        Some(left)
    } else {
        left.checked_add(right - adjusted)
    }
}

fn signed_next_multiple_i128(left: i128, right: i128) -> Option<i128> {
    if right == -1 {
        return Some(left);
    }
    let remainder = left.checked_rem(right)?;
    let adjusted = if (remainder > 0 && right < 0) || (remainder < 0 && right > 0) {
        remainder + right
    } else {
        remainder
    };
    if adjusted == 0 {
        Some(left)
    } else {
        left.checked_add(right - adjusted)
    }
}

fn next_multiple_bits(left: &Integer, right: &Integer) -> Option<IntBits> {
    match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) if left.signed => {
            signed_next_multiple_i64(left_bits as i64, right_bits as i64)
                .map(|value| IntBits::U64(value as u64))
        }
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => left_bits
            .checked_next_multiple_of(right_bits)
            .map(IntBits::U64),
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) if left.signed => {
            signed_next_multiple_i128(left_bits as i128, right_bits as i128)
                .map(|value| IntBits::U128(value as u128))
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => left_bits
            .checked_next_multiple_of(right_bits)
            .map(IntBits::U128),
        _ => None,
    }
}

/// The next multiple in the direction of `rhs`. A zero factor is an error.
/// Overflow is `NumericOverflow`. The checked form returns null for both.
fn next_multiple_op(ctx: &mut BuiltinCtx<'_>, checked: bool) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    require_same_sign(&left, &right)?;
    if is_zero(&right) {
        if checked {
            ctx.return_null();
            return Ok(());
        }
        return Err(VmError::InvalidArgument("multiple must not be zero"));
    }
    let bits = match (left.bits, right.bits) {
        (IntBits::U64(_), IntBits::U64(_)) | (IntBits::U128(_), IntBits::U128(_)) => {
            next_multiple_bits(&left, &right)
        }
        _ => return Err(VmError::TypeError),
    };
    if checked {
        finish_overflow(ctx, left.signed, bits)
    } else {
        require_overflow(ctx, left.signed, bits)
    }
}

pub fn next_multiple_of(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    next_multiple_op(ctx, false)
}

pub fn checked_next_multiple_of(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    next_multiple_op(ctx, true)
}

fn integer_byte_values(value: &Integer, big_endian: bool) -> Vec<u8> {
    match value.bits {
        IntBits::U64(bits) if value.signed => {
            let signed = bits as i64;
            if big_endian {
                signed.to_be_bytes().to_vec()
            } else {
                signed.to_le_bytes().to_vec()
            }
        }
        IntBits::U64(bits) => {
            if big_endian {
                bits.to_be_bytes().to_vec()
            } else {
                bits.to_le_bytes().to_vec()
            }
        }
        IntBits::U128(bits) if value.signed => {
            let signed = bits as i128;
            if big_endian {
                signed.to_be_bytes().to_vec()
            } else {
                signed.to_le_bytes().to_vec()
            }
        }
        IntBits::U128(bits) => {
            if big_endian {
                bits.to_be_bytes().to_vec()
            } else {
                bits.to_le_bytes().to_vec()
            }
        }
    }
}

fn int_to_bytes(ctx: &mut BuiltinCtx<'_>, big_endian: bool) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    let bytes = integer_byte_values(&value, big_endian);
    let values: Vec<i64> = bytes.into_iter().map(i64::from).collect();
    ctx.return_i64_list(&values)
}

pub fn int_to_be_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_to_bytes(ctx, true)
}

pub fn int_to_le_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_to_bytes(ctx, false)
}

fn read_integer_bytes(ctx: &mut BuiltinCtx<'_>, expected: usize) -> Result<Vec<u8>, VmError> {
    ctx.require_arity(1)?;
    let list = ctx.arg(0)?;
    let length = ctx.list_len(list)?;
    if length != expected {
        return Err(VmError::InvalidArgument(
            "integer byte length does not match",
        ));
    }
    let mut bytes = Vec::with_capacity(length);
    for index in 0..length {
        let element = ctx.list_get(list, TaggedValue::from_u64(index as u64))?;
        let number = ctx.number_of(element)?;
        let value = match number {
            Number::I64(value) => i128::from(value),
            Number::U64(value) => i128::from(value),
            Number::I128(value) => value,
            Number::U128(value) => i128::try_from(value)
                .map_err(|_| VmError::InvalidArgument("integer byte is out of range"))?,
            Number::F64(_) => return Err(VmError::TypeError),
        };
        if !(0..=255).contains(&value) {
            return Err(VmError::InvalidArgument("integer byte is out of range"));
        }
        bytes.push(
            u8::try_from(value)
                .map_err(|_| VmError::InvalidArgument("integer byte is out of range"))?,
        );
    }
    Ok(bytes)
}

fn bytes_to_number(bytes: &[u8], signed: bool, big_endian: bool) -> Result<Number, VmError> {
    if bytes.len() == 8 {
        let mut encoded = [0u8; 8];
        encoded.copy_from_slice(bytes);
        let bits = if big_endian {
            u64::from_be_bytes(encoded)
        } else {
            u64::from_le_bytes(encoded)
        };
        return Ok(if signed {
            Number::I64(bits as i64)
        } else {
            Number::U64(bits)
        });
    }
    let mut encoded = [0u8; 16];
    encoded.copy_from_slice(bytes);
    let bits = if big_endian {
        u128::from_be_bytes(encoded)
    } else {
        u128::from_le_bytes(encoded)
    };
    Ok(if signed {
        Number::I128(bits as i128)
    } else {
        Number::U128(bits)
    })
}

fn int_from_bytes(
    ctx: &mut BuiltinCtx<'_>,
    signed: bool,
    wide: bool,
    big_endian: bool,
) -> Result<(), VmError> {
    let expected = if wide { 16 } else { 8 };
    let bytes = read_integer_bytes(ctx, expected)?;
    ctx.return_number(bytes_to_number(&bytes, signed, big_endian)?)
}

pub fn i64_from_be_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_from_bytes(ctx, true, false, true)
}

pub fn i64_from_le_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_from_bytes(ctx, true, false, false)
}

pub fn u64_from_be_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_from_bytes(ctx, false, false, true)
}

pub fn u64_from_le_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_from_bytes(ctx, false, false, false)
}

pub fn i128_from_be_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_from_bytes(ctx, true, true, true)
}

pub fn i128_from_le_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_from_bytes(ctx, true, true, false)
}

pub fn u128_from_be_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_from_bytes(ctx, false, true, true)
}

pub fn u128_from_le_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_from_bytes(ctx, false, true, false)
}

fn signed_div_floor_i64(left: i64, right: i64) -> Option<i64> {
    let quotient = left.checked_div(right)?;
    let remainder = left.checked_rem(right)?;
    if remainder == 0 {
        return Some(quotient);
    }
    let correction = (left ^ right) >> 63;
    quotient.checked_add(correction)
}

fn signed_div_floor_i128(left: i128, right: i128) -> Option<i128> {
    let quotient = left.checked_div(right)?;
    let remainder = left.checked_rem(right)?;
    if remainder == 0 {
        return Some(quotient);
    }
    let correction = (left ^ right) >> 127;
    quotient.checked_add(correction)
}

fn floor_quotient(left: &Integer, right: &Integer) -> Option<IntBits> {
    match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) if left.signed => {
            signed_div_floor_i64(left_bits as i64, right_bits as i64)
                .map(|value| IntBits::U64(value as u64))
        }
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => {
            left_bits.checked_div(right_bits).map(IntBits::U64)
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) if left.signed => {
            signed_div_floor_i128(left_bits as i128, right_bits as i128)
                .map(|value| IntBits::U128(value as u128))
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => {
            left_bits.checked_div(right_bits).map(IntBits::U128)
        }
        _ => None,
    }
}

/// Toward negative infinity. A zero divisor is an error. Overflow, including
/// the signed minimum divided by -1, is `NumericOverflow`. The checked form
/// returns null for both a zero divisor and overflow.
fn div_floor_op(ctx: &mut BuiltinCtx<'_>, checked: bool) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    require_same_sign(&left, &right)?;
    if is_zero(&right) {
        if checked {
            ctx.return_null();
            return Ok(());
        }
        return Err(VmError::InvalidArgument("divisor must not be zero"));
    }
    let bits = match (left.bits, right.bits) {
        (IntBits::U64(_), IntBits::U64(_)) | (IntBits::U128(_), IntBits::U128(_)) => {
            floor_quotient(&left, &right)
        }
        _ => return Err(VmError::TypeError),
    };
    if checked {
        finish_overflow(ctx, left.signed, bits)
    } else {
        require_overflow(ctx, left.signed, bits)
    }
}

pub fn div_floor(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    div_floor_op(ctx, false)
}

pub fn checked_div_floor(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    div_floor_op(ctx, true)
}

fn unsigned_magnitude(value: &Integer) -> u128 {
    match value.bits {
        IntBits::U64(bits) if value.signed => u128::from((bits as i64).unsigned_abs()),
        IntBits::U64(bits) => u128::from(bits),
        IntBits::U128(bits) if value.signed => (bits as i128).unsigned_abs(),
        IntBits::U128(bits) => bits,
    }
}

fn gcd_u128(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

fn lcm_u128(left: u128, right: u128) -> Option<u128> {
    if left == 0 || right == 0 {
        return Some(0);
    }
    let divisor = gcd_u128(left, right);
    left.checked_div(divisor)?.checked_mul(right)
}

fn magnitude_pair(ctx: &mut BuiltinCtx<'_>) -> Result<(Integer, u128, u128), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    require_same_sign(&left, &right)?;
    if !matches!(
        (left.bits, right.bits),
        (IntBits::U64(_), IntBits::U64(_)) | (IntBits::U128(_), IntBits::U128(_))
    ) {
        return Err(VmError::TypeError);
    }
    Ok((left, unsigned_magnitude(&left), unsigned_magnitude(&right)))
}

fn return_unsigned(ctx: &mut BuiltinCtx<'_>, wide: bool, value: u128) -> Result<(), VmError> {
    if wide {
        ctx.return_number(Number::U128(value))
    } else {
        let value = u64::try_from(value).map_err(|_| VmError::NumericOverflow)?;
        ctx.return_number(Number::U64(value))
    }
}

pub fn int_gcd(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let (left, left_magnitude, right_magnitude) = magnitude_pair(ctx)?;
    let wide = matches!(left.bits, IntBits::U128(_));
    return_unsigned(ctx, wide, gcd_u128(left_magnitude, right_magnitude))
}

fn int_lcm_value(ctx: &mut BuiltinCtx<'_>, checked: bool) -> Result<(), VmError> {
    let (left, left_magnitude, right_magnitude) = magnitude_pair(ctx)?;
    let wide = matches!(left.bits, IntBits::U128(_));
    let Some(value) = lcm_u128(left_magnitude, right_magnitude) else {
        if checked {
            ctx.return_null();
            return Ok(());
        }
        return Err(VmError::NumericOverflow);
    };
    if !wide && u64::try_from(value).is_err() {
        if checked {
            ctx.return_null();
            return Ok(());
        }
        return Err(VmError::NumericOverflow);
    }
    return_unsigned(ctx, wide, value)
}

pub fn int_lcm(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_lcm_value(ctx, false)
}

pub fn checked_int_lcm(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_lcm_value(ctx, true)
}

pub fn saturating_add_signed(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    signed_adjust(ctx, AdjustOp::Add, OverflowMode::Saturating)
}

pub fn saturating_sub_signed(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    signed_adjust(ctx, AdjustOp::Sub, OverflowMode::Saturating)
}

pub fn wrapping_add_signed(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    signed_adjust(ctx, AdjustOp::Add, OverflowMode::Wrapping)
}

pub fn wrapping_sub_signed(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    signed_adjust(ctx, AdjustOp::Sub, OverflowMode::Wrapping)
}

pub fn strict_add_signed(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    signed_adjust(ctx, AdjustOp::Add, OverflowMode::Strict)
}

pub fn strict_sub_signed(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    signed_adjust(ctx, AdjustOp::Sub, OverflowMode::Strict)
}

fn is_negative(value: &Integer) -> bool {
    if !value.signed {
        return false;
    }
    match value.bits {
        IntBits::U64(bits) => (bits as i64) < 0,
        IntBits::U128(bits) => (bits as i128) < 0,
    }
}

fn shifted_off(bits: IntBits, ones: bool) -> IntBits {
    match bits {
        IntBits::U64(_) => IntBits::U64(if ones { u64::MAX } else { 0 }),
        IntBits::U128(_) => IntBits::U128(if ones { u128::MAX } else { 0 }),
    }
}

fn apply_unbounded(bits: IntBits, signed: bool, distance: u32, leftward: bool) -> IntBits {
    match bits {
        IntBits::U64(bits) if signed => {
            let number = bits as i64;
            let shifted = if leftward {
                number.unbounded_shl(distance)
            } else {
                number.unbounded_shr(distance)
            };
            IntBits::U64(shifted as u64)
        }
        IntBits::U64(bits) => {
            let shifted = if leftward {
                bits.unbounded_shl(distance)
            } else {
                bits.unbounded_shr(distance)
            };
            IntBits::U64(shifted)
        }
        IntBits::U128(bits) if signed => {
            let number = bits as i128;
            let shifted = if leftward {
                number.unbounded_shl(distance)
            } else {
                number.unbounded_shr(distance)
            };
            IntBits::U128(shifted as u128)
        }
        IntBits::U128(bits) => {
            let shifted = if leftward {
                bits.unbounded_shl(distance)
            } else {
                bits.unbounded_shr(distance)
            };
            IntBits::U128(shifted)
        }
    }
}

/// A distance at least the width yields 0. A negative value shifted right
/// yields -1. The distance is not reduced modulo the width.
fn unbounded_shift(ctx: &mut BuiltinCtx<'_>, leftward: bool) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let value = integer(ctx.arg_number(0)?)?;
    let distance = ctx.arg_i64(1)?;
    if distance < 0 {
        return Err(VmError::InvalidArgument(
            "shift distance must not be negative",
        ));
    }
    let bits = if distance >= i64::from(width(&value.bits)) {
        shifted_off(value.bits, !leftward && is_negative(&value))
    } else {
        apply_unbounded(value.bits, value.signed, distance as u32, leftward)
    };
    finish_overflow(ctx, value.signed, Some(bits))
}

pub fn unbounded_shl(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unbounded_shift(ctx, true)
}

pub fn unbounded_shr(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    unbounded_shift(ctx, false)
}

fn return_limit(ctx: &mut BuiltinCtx<'_>, number: Number) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_number(number)
}

pub fn i64_min(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_limit(ctx, Number::I64(i64::MIN))
}

pub fn i64_max(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_limit(ctx, Number::I64(i64::MAX))
}

pub fn u64_max(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_limit(ctx, Number::U64(u64::MAX))
}

pub fn i128_min(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_limit(ctx, Number::I128(i128::MIN))
}

pub fn i128_max(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_limit(ctx, Number::I128(i128::MAX))
}

pub fn u128_max(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_limit(ctx, Number::U128(u128::MAX))
}

fn one_index(value: &Integer, highest: bool) -> Option<u32> {
    match value.bits {
        IntBits::U64(bits) if value.signed => {
            let number = bits as i64;
            if highest {
                number.highest_one()
            } else {
                number.lowest_one()
            }
        }
        IntBits::U64(bits) => {
            if highest {
                bits.highest_one()
            } else {
                bits.lowest_one()
            }
        }
        IntBits::U128(bits) if value.signed => {
            let number = bits as i128;
            if highest {
                number.highest_one()
            } else {
                number.lowest_one()
            }
        }
        IntBits::U128(bits) => {
            if highest {
                bits.highest_one()
            } else {
                bits.lowest_one()
            }
        }
    }
}

fn return_one_index(ctx: &mut BuiltinCtx<'_>, highest: bool) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    match one_index(&value, highest) {
        Some(index) => ctx.return_i64(i64::from(index))?,
        None => ctx.return_null(),
    }
    Ok(())
}

pub fn highest_one(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_one_index(ctx, true)
}

pub fn lowest_one(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_one_index(ctx, false)
}

fn isolate_one(value: &Integer, highest: bool) -> IntBits {
    match value.bits {
        IntBits::U64(bits) if value.signed => {
            let number = bits as i64;
            let isolated = if highest {
                number.isolate_highest_one()
            } else {
                number.isolate_lowest_one()
            };
            IntBits::U64(isolated as u64)
        }
        IntBits::U64(bits) => {
            let isolated = if highest {
                bits.isolate_highest_one()
            } else {
                bits.isolate_lowest_one()
            };
            IntBits::U64(isolated)
        }
        IntBits::U128(bits) if value.signed => {
            let number = bits as i128;
            let isolated = if highest {
                number.isolate_highest_one()
            } else {
                number.isolate_lowest_one()
            };
            IntBits::U128(isolated as u128)
        }
        IntBits::U128(bits) => {
            let isolated = if highest {
                bits.isolate_highest_one()
            } else {
                bits.isolate_lowest_one()
            };
            IntBits::U128(isolated)
        }
    }
}

fn return_isolated(ctx: &mut BuiltinCtx<'_>, highest: bool) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    finish_overflow(ctx, value.signed, Some(isolate_one(&value, highest)))
}

pub fn isolate_highest_one(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_isolated(ctx, true)
}

pub fn isolate_lowest_one(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_isolated(ctx, false)
}

/// Bits of the two's-complement representation. A negative value uses the full width.
pub fn int_bit_width(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    let width = match value.bits {
        IntBits::U64(bits) => bits.bit_width(),
        IntBits::U128(bits) => bits.bit_width(),
    };
    ctx.return_i64(i64::from(width))?;
    Ok(())
}

fn return_i64_limit(ctx: &mut BuiltinCtx<'_>, value: i64) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_i64(value)?;
    Ok(())
}

pub fn i64_bits(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_i64_limit(ctx, 64)
}

pub fn i128_bits(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_i64_limit(ctx, 128)
}

pub fn isize_bits(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_i64_limit(ctx, 64)
}

pub fn u128_bits(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_i64_limit(ctx, 128)
}

pub fn usize_bits(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_i64_limit(ctx, 64)
}

fn square_root(value: &Integer) -> Option<IntBits> {
    match value.bits {
        IntBits::U64(bits) if value.signed => (bits as i64)
            .checked_isqrt()
            .map(|root| IntBits::U64(root as u64)),
        IntBits::U64(bits) => Some(IntBits::U64(bits.isqrt())),
        IntBits::U128(bits) if value.signed => (bits as i128)
            .checked_isqrt()
            .map(|root| IntBits::U128(root as u128)),
        IntBits::U128(bits) => Some(IntBits::U128(bits.isqrt())),
    }
}

fn int_square_root(ctx: &mut BuiltinCtx<'_>, checked: bool) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    match square_root(&value) {
        Some(bits) => finish_overflow(ctx, value.signed, Some(bits)),
        None if checked => {
            ctx.return_null();
            Ok(())
        }
        None => Err(VmError::InvalidArgument("square root of a negative value")),
    }
}

pub fn int_isqrt(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_square_root(ctx, false)
}

pub fn checked_int_isqrt(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    int_square_root(ctx, true)
}

pub fn strict_abs(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = integer(ctx.arg_number(0)?)?;
    if !value.signed {
        return Err(VmError::TypeError);
    }
    let bits = match value.bits {
        IntBits::U64(bits) => (bits as i64)
            .checked_abs()
            .map(|number| IntBits::U64(number as u64)),
        IntBits::U128(bits) => (bits as i128)
            .checked_abs()
            .map(|number| IntBits::U128(number as u128)),
    };
    require_overflow(ctx, true, bits)
}

fn wrapping_euclid(ctx: &mut BuiltinCtx<'_>, division: bool) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    require_same_sign(&left, &right)?;
    if is_zero(&right) {
        return Err(VmError::InvalidArgument("divisor must not be zero"));
    }
    let bits = match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => {
            let bits = if left.signed {
                let left_number = left_bits as i64;
                let right_number = right_bits as i64;
                if division {
                    left_number.wrapping_div_euclid(right_number) as u64
                } else {
                    left_number.wrapping_rem_euclid(right_number) as u64
                }
            } else if division {
                left_bits.wrapping_div_euclid(right_bits)
            } else {
                left_bits.wrapping_rem_euclid(right_bits)
            };
            IntBits::U64(bits)
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => {
            let bits = if left.signed {
                let left_number = left_bits as i128;
                let right_number = right_bits as i128;
                if division {
                    left_number.wrapping_div_euclid(right_number) as u128
                } else {
                    left_number.wrapping_rem_euclid(right_number) as u128
                }
            } else if division {
                left_bits.wrapping_div_euclid(right_bits)
            } else {
                left_bits.wrapping_rem_euclid(right_bits)
            };
            IntBits::U128(bits)
        }
        _ => return Err(VmError::TypeError),
    };
    finish_overflow(ctx, left.signed, Some(bits))
}

pub fn wrapping_div_euclid(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wrapping_euclid(ctx, true)
}

pub fn wrapping_rem_euclid(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    wrapping_euclid(ctx, false)
}

pub fn checked_signed_diff(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    if left.signed || right.signed {
        return Err(VmError::TypeError);
    }
    let bits = match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => left_bits
            .checked_signed_diff(right_bits)
            .map(|difference| IntBits::U64(difference as u64)),
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => left_bits
            .checked_signed_diff(right_bits)
            .map(|difference| IntBits::U128(difference as u128)),
        _ => return Err(VmError::TypeError),
    };
    finish_overflow(ctx, true, bits)
}

fn bits_are_zero(bits: IntBits) -> bool {
    match bits {
        IntBits::U64(bits) => bits == 0,
        IntBits::U128(bits) => bits == 0,
    }
}

fn truncating_div_rem(left: &Integer, right: &Integer) -> Result<(IntBits, IntBits), VmError> {
    match (left.bits, right.bits) {
        (IntBits::U64(left_bits), IntBits::U64(right_bits)) => {
            if left.signed {
                let left_number = left_bits as i64;
                let right_number = right_bits as i64;
                Ok((
                    IntBits::U64((left_number / right_number) as u64),
                    IntBits::U64((left_number % right_number) as u64),
                ))
            } else {
                Ok((
                    IntBits::U64(left_bits / right_bits),
                    IntBits::U64(left_bits % right_bits),
                ))
            }
        }
        (IntBits::U128(left_bits), IntBits::U128(right_bits)) => {
            if left.signed {
                let left_number = left_bits as i128;
                let right_number = right_bits as i128;
                Ok((
                    IntBits::U128((left_number / right_number) as u128),
                    IntBits::U128((left_number % right_number) as u128),
                ))
            } else {
                Ok((
                    IntBits::U128(left_bits / right_bits),
                    IntBits::U128(left_bits % right_bits),
                ))
            }
        }
        _ => Err(VmError::TypeError),
    }
}

fn div_exact(ctx: &mut BuiltinCtx<'_>, strict: bool) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = integer(ctx.arg_number(0)?)?;
    let right = integer(ctx.arg_number(1)?)?;
    require_same_sign(&left, &right)?;
    if is_zero(&right) {
        if strict {
            return Err(VmError::InvalidArgument("divisor must not be zero"));
        }
        ctx.return_null();
        return Ok(());
    }
    if euclid_overflows(&left, &right) {
        if strict {
            return Err(VmError::NumericOverflow);
        }
        ctx.return_null();
        return Ok(());
    }
    let (quotient, remainder) = truncating_div_rem(&left, &right)?;
    if !bits_are_zero(remainder) {
        if strict {
            return Err(VmError::InvalidArgument("division is not exact"));
        }
        ctx.return_null();
        return Ok(());
    }
    finish_overflow(ctx, left.signed, Some(quotient))
}

pub fn checked_div_exact(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    div_exact(ctx, false)
}

pub fn strict_div_exact(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    div_exact(ctx, true)
}

fn zero_count(value: &Integer, leading: bool) -> u32 {
    match value.bits {
        IntBits::U64(bits) if value.signed => {
            let number = bits as i64;
            if leading {
                number.leading_zeros()
            } else {
                number.trailing_zeros()
            }
        }
        IntBits::U64(bits) => {
            if leading {
                bits.leading_zeros()
            } else {
                bits.trailing_zeros()
            }
        }
        IntBits::U128(bits) if value.signed => {
            let number = bits as i128;
            if leading {
                number.leading_zeros()
            } else {
                number.trailing_zeros()
            }
        }
        IntBits::U128(bits) => {
            if leading {
                bits.leading_zeros()
            } else {
                bits.trailing_zeros()
            }
        }
    }
}

fn shifted_exactly(value: &Integer, distance: u32, leftward: bool) -> IntBits {
    match value.bits {
        IntBits::U64(bits) if value.signed => {
            let number = bits as i64;
            let shifted = if leftward {
                number.wrapping_shl(distance)
            } else {
                number.wrapping_shr(distance)
            };
            IntBits::U64(shifted as u64)
        }
        IntBits::U64(bits) => {
            let shifted = if leftward {
                bits.wrapping_shl(distance)
            } else {
                bits.wrapping_shr(distance)
            };
            IntBits::U64(shifted)
        }
        IntBits::U128(bits) if value.signed => {
            let number = bits as i128;
            let shifted = if leftward {
                number.wrapping_shl(distance)
            } else {
                number.wrapping_shr(distance)
            };
            IntBits::U128(shifted as u128)
        }
        IntBits::U128(bits) => {
            let shifted = if leftward {
                bits.wrapping_shl(distance)
            } else {
                bits.wrapping_shr(distance)
            };
            IntBits::U128(shifted)
        }
    }
}

fn exact_shift(ctx: &mut BuiltinCtx<'_>, leftward: bool, strict: bool) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let value = integer(ctx.arg_number(0)?)?;
    let distance = shift_distance(ctx)?;
    if distance >= width(&value.bits) {
        if strict {
            return Err(VmError::NumericOverflow);
        }
        ctx.return_null();
        return Ok(());
    }
    let available = zero_count(&value, leftward);
    if distance > available {
        if strict {
            return Err(VmError::InvalidArgument("shift is not exact"));
        }
        ctx.return_null();
        return Ok(());
    }
    finish_overflow(
        ctx,
        value.signed,
        Some(shifted_exactly(&value, distance, leftward)),
    )
}

pub fn checked_shl_exact(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    exact_shift(ctx, true, false)
}

pub fn strict_shl_exact(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    exact_shift(ctx, true, true)
}

pub fn checked_shr_exact(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    exact_shift(ctx, false, false)
}

pub fn strict_shr_exact(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    exact_shift(ctx, false, true)
}
