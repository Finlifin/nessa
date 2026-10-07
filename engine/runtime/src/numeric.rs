//! Exact scalar arithmetic through 128 bits, independent of GC allocation.

use std::cmp::Ordering;
use std::fmt;

use gc::ObjectHeader;
use nsbc::Opcode;
use type_pool::Intrinsic;

use crate::TaggedValue;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Number {
    I64(i64),
    U64(u64),
    I128(i128),
    U128(u128),
    F64(f64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumericError {
    Overflow,
    DivisionByZero,
    TypeError,
    InvalidShift,
}

impl Number {
    /// Decode immediate numbers or the exact scalar heap layouts.
    ///
    /// # Safety
    /// A heap value must reference a live, initialized GC object. The caller
    /// must hold a mutator operation and keep the value rooted across GC.
    pub unsafe fn from_tagged(value: TaggedValue) -> Option<Self> {
        if let Some(value) = value.as_i64() {
            return Some(Self::I64(value));
        }
        if let Some(value) = value.as_u64() {
            return Some(Self::U64(value));
        }
        if let Some(value) = value.as_f64() {
            return Some(Self::F64(value));
        }
        let pointer = value.as_heap_ptr()?;
        // SAFETY: the caller guarantees a live object with its aligned header.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        if !header.is_ordinary() {
            return None;
        }
        let kind = header.type_index;
        let words = header.payload_words();
        if words == 1 {
            let intrinsic = Intrinsic::ALL
                .iter()
                .copied()
                .find(|intrinsic| intrinsic.type_index() == kind)?;
            if let Some(width) = signed_width(intrinsic)
                && width <= 64
            {
                // SAFETY: the checked layout contains one initialized word.
                let value = unsafe { pointer.cast::<i64>().read() };
                return Self::I64(value).cast_to(intrinsic).ok();
            }
            if let Some(width) = unsigned_width(intrinsic)
                && width <= 64
            {
                // SAFETY: the checked layout contains one initialized word.
                let value = unsafe { pointer.cast::<u64>().read() };
                return Self::U64(value).cast_to(intrinsic).ok();
            }
            if matches!(intrinsic, Intrinsic::F32 | Intrinsic::F64) {
                // SAFETY: the checked layout contains the initialized f64 bits.
                let value = f64::from_bits(unsafe { pointer.cast::<u64>().read() });
                if intrinsic == Intrinsic::F32
                    && !value.is_nan()
                    && f64::from(value as f32) != value
                {
                    return None;
                }
                return Some(Self::F64(value));
            }
        } else if words == 2
            && (kind == Intrinsic::I128.type_index() || kind == Intrinsic::U128.type_index())
        {
            // Read u64 words rather than requiring 16-byte payload alignment.
            // SAFETY: both words are initialized and the payload is 8-byte aligned.
            let bits = unsafe {
                u128::from(pointer.cast::<u64>().read())
                    | (u128::from(pointer.cast::<u64>().add(1).read()) << 64)
            };
            return Some(if kind == Intrinsic::I128.type_index() {
                Self::I128(bits as i128)
            } else {
                Self::U128(bits)
            });
        }
        None
    }

    /// Convert to a numeric intrinsic with checked integer ranges. Finite
    /// floats truncate toward zero before range checking. F32 rounds once to
    /// its precision and is returned as an exactly widened F64 scalar.
    /// The caller retains the target's type identity when allocating its value.
    pub fn cast_to(self, target: Intrinsic) -> Result<Self, NumericError> {
        if let Some(width) = signed_width(target) {
            let value = match self {
                Self::F64(value) => {
                    let value = value.trunc();
                    if !value.is_finite()
                        || value < i128::MIN as f64
                        || value >= -(i128::MIN as f64)
                    {
                        return Err(NumericError::Overflow);
                    }
                    value as i128
                }
                value => value.signed()?,
            };
            if width < 128 {
                let limit = 1i128 << (width - 1);
                if value < -limit || value >= limit {
                    return Err(NumericError::Overflow);
                }
                return Ok(Self::I64(value as i64));
            }
            return Ok(Self::I128(value));
        }
        if let Some(width) = unsigned_width(target) {
            let value = match self {
                Self::I64(value) => u128::try_from(value).map_err(|_| NumericError::Overflow)?,
                Self::U64(value) => u128::from(value),
                Self::I128(value) => u128::try_from(value).map_err(|_| NumericError::Overflow)?,
                Self::U128(value) => value,
                Self::F64(value) => {
                    let value = value.trunc();
                    if !value.is_finite() || value < 0.0 || value >= u128::MAX as f64 {
                        return Err(NumericError::Overflow);
                    }
                    value as u128
                }
            };
            if width < 128 {
                if value >= 1u128 << width {
                    return Err(NumericError::Overflow);
                }
                return Ok(Self::U64(value as u64));
            }
            return Ok(Self::U128(value));
        }
        match target {
            Intrinsic::F64 => Ok(Self::F64(self.to_f64())),
            Intrinsic::F32 => {
                let original = self.to_f64();
                let rounded = f64::from(original as f32);
                if original.is_finite() && rounded.is_infinite() {
                    return Err(NumericError::Overflow);
                }
                Ok(Self::F64(rounded))
            }
            _ => Err(NumericError::TypeError),
        }
    }

    /// Arithmetic promotes overflowing 64-bit results to 128 bits. Mixed
    /// integer signs use i128; an operand outside that domain is an overflow.
    pub fn binary(self, other: Self, opcode: Opcode) -> Result<Self, NumericError> {
        if !matches!(
            opcode,
            Opcode::Add
                | Opcode::Sub
                | Opcode::Mul
                | Opcode::Div
                | Opcode::Mod
                | Opcode::BitAnd
                | Opcode::BitOr
                | Opcode::BitXor
                | Opcode::Shl
                | Opcode::Shr
                | Opcode::UShr
        ) {
            return Err(NumericError::TypeError);
        }
        if matches!(opcode, Opcode::Div | Opcode::Mod) && other.is_zero() {
            return Err(NumericError::DivisionByZero);
        }
        if matches!(opcode, Opcode::Shl | Opcode::Shr | Opcode::UShr) {
            return self.shift(other, opcode);
        }
        if matches!(self, Self::F64(_)) || matches!(other, Self::F64(_)) {
            let left = self.to_f64();
            let right = other.to_f64();
            let result = match opcode {
                Opcode::Add => left + right,
                Opcode::Sub => left - right,
                Opcode::Mul => left * right,
                Opcode::Div | Opcode::Mod if right == 0.0 => {
                    return Err(NumericError::DivisionByZero);
                }
                Opcode::Div => left / right,
                Opcode::Mod => left % right,
                _ => return Err(NumericError::TypeError),
            };
            return Ok(Self::F64(result));
        }
        match (self, other) {
            (Self::I64(left), Self::I64(right)) => {
                let result = signed_binary(i128::from(left), i128::from(right), opcode)?;
                Ok(match i64::try_from(result) {
                    Ok(result) => Self::I64(result),
                    Err(_) => Self::I128(result),
                })
            }
            (Self::U64(left), Self::U64(right)) => {
                let result = unsigned_binary(u128::from(left), u128::from(right), opcode)?;
                Ok(match u64::try_from(result) {
                    Ok(result) => Self::U64(result),
                    Err(_) => Self::U128(result),
                })
            }
            (Self::U64(left), Self::U128(right)) => {
                unsigned_binary(u128::from(left), right, opcode).map(Self::U128)
            }
            (Self::U128(left), Self::U64(right)) => {
                unsigned_binary(left, u128::from(right), opcode).map(Self::U128)
            }
            (Self::U128(left), Self::U128(right)) => {
                unsigned_binary(left, right, opcode).map(Self::U128)
            }
            (left, right) => signed_binary(left.signed()?, right.signed()?, opcode).map(Self::I128),
        }
    }

    pub fn checked_neg(self) -> Result<Self, NumericError> {
        match self {
            Self::I64(value) => Ok(match value.checked_neg() {
                Some(value) => Self::I64(value),
                None => Self::I128(-i128::from(value)),
            }),
            Self::U64(value) => {
                let value = -i128::from(value);
                Ok(match i64::try_from(value) {
                    Ok(value) => Self::I64(value),
                    Err(_) => Self::I128(value),
                })
            }
            Self::I128(value) => value
                .checked_neg()
                .map(Self::I128)
                .ok_or(NumericError::Overflow),
            Self::U128(value) if value == 1u128 << 127 => Ok(Self::I128(i128::MIN)),
            Self::U128(value) => i128::try_from(value)
                .ok()
                .and_then(i128::checked_neg)
                .map(Self::I128)
                .ok_or(NumericError::Overflow),
            Self::F64(value) => Ok(Self::F64(-value)),
        }
    }

    pub fn bit_not(self) -> Result<Self, NumericError> {
        match self {
            Self::I64(value) => Ok(Self::I64(!value)),
            Self::U64(value) => Ok(Self::U64(!value)),
            Self::I128(value) => Ok(Self::I128(!value)),
            Self::U128(value) => Ok(Self::U128(!value)),
            Self::F64(_) => Err(NumericError::TypeError),
        }
    }

    /// Integer comparisons never round through floating point. Float versus
    /// integer comparisons also retain the integer's exact magnitude.
    pub fn partial_cmp_numeric(self, other: Self) -> Option<Ordering> {
        match (self, other) {
            (Self::F64(left), Self::F64(right)) => left.partial_cmp(&right),
            (Self::F64(left), right) => compare_float_integer(left, right),
            (left, Self::F64(right)) => compare_float_integer(right, left).map(Ordering::reverse),
            (left, right) => match (left.integer(), right.integer()) {
                (Integer::Signed(left), Integer::Signed(right)) => Some(left.cmp(&right)),
                (Integer::Unsigned(left), Integer::Unsigned(right)) => Some(left.cmp(&right)),
                (Integer::Signed(left), Integer::Unsigned(right)) => Some(if left < 0 {
                    Ordering::Less
                } else {
                    (left as u128).cmp(&right)
                }),
                (Integer::Unsigned(left), Integer::Signed(right)) => Some(if right < 0 {
                    Ordering::Greater
                } else {
                    left.cmp(&(right as u128))
                }),
            },
        }
    }

    /// Finite floats within range truncate toward zero; out-of-range values,
    /// infinities and NaN fail instead of saturating.
    pub fn to_i64_checked(self) -> Option<i64> {
        match self {
            Self::I64(value) => Some(value),
            Self::U64(value) => i64::try_from(value).ok(),
            Self::I128(value) => i64::try_from(value).ok(),
            Self::U128(value) => i64::try_from(value).ok(),
            Self::F64(value) if value >= i64::MIN as f64 && value < -(i64::MIN as f64) => {
                Some(value as i64)
            }
            Self::F64(_) => None,
        }
    }

    /// The same checked, truncating policy as `to_i64_checked`; negative floats
    /// are rejected even when truncation would produce zero.
    pub fn to_u64_checked(self) -> Option<u64> {
        match self {
            Self::I64(value) => u64::try_from(value).ok(),
            Self::U64(value) => Some(value),
            Self::I128(value) => u64::try_from(value).ok(),
            Self::U128(value) => u64::try_from(value).ok(),
            Self::F64(value) if value >= 0.0 && value < u64::MAX as f64 => Some(value as u64),
            Self::F64(_) => None,
        }
    }

    pub fn to_f64(self) -> f64 {
        match self {
            Self::I64(value) => value as f64,
            Self::U64(value) => value as f64,
            Self::I128(value) => value as f64,
            Self::U128(value) => value as f64,
            Self::F64(value) => value,
        }
    }

    fn signed(self) -> Result<i128, NumericError> {
        match self {
            Self::I64(value) => Ok(i128::from(value)),
            Self::U64(value) => Ok(i128::from(value)),
            Self::I128(value) => Ok(value),
            Self::U128(value) => i128::try_from(value).map_err(|_| NumericError::Overflow),
            Self::F64(_) => Err(NumericError::TypeError),
        }
    }

    fn is_zero(self) -> bool {
        match self {
            Self::I64(value) => value == 0,
            Self::U64(value) => value == 0,
            Self::I128(value) => value == 0,
            Self::U128(value) => value == 0,
            Self::F64(value) => value == 0.0,
        }
    }

    fn integer(self) -> Integer {
        match self {
            Self::I64(value) => Integer::Signed(i128::from(value)),
            Self::U64(value) => Integer::Unsigned(u128::from(value)),
            Self::I128(value) => Integer::Signed(value),
            Self::U128(value) => Integer::Unsigned(value),
            Self::F64(_) => unreachable!("float cases are handled before integer comparison"),
        }
    }

    fn shift(self, other: Self, opcode: Opcode) -> Result<Self, NumericError> {
        if matches!(self, Self::F64(_)) {
            return Err(NumericError::TypeError);
        }
        let count = match other {
            Self::I64(value) => u32::try_from(value).ok(),
            Self::U64(value) => u32::try_from(value).ok(),
            Self::I128(value) => u32::try_from(value).ok(),
            Self::U128(value) => u32::try_from(value).ok(),
            Self::F64(_) => return Err(NumericError::TypeError),
        }
        .ok_or(NumericError::InvalidShift)?;
        macro_rules! shift {
            ($value:expr, $signed:ty, $unsigned:ty, $variant:ident) => {{
                let value = $value;
                if count >= <$signed>::BITS {
                    return Err(NumericError::InvalidShift);
                }
                let result = match opcode {
                    Opcode::Shl => {
                        let shifted = value << count;
                        if shifted >> count != value {
                            return Err(NumericError::Overflow);
                        }
                        shifted
                    }
                    Opcode::Shr => value >> count,
                    Opcode::UShr => ((value as $unsigned) >> count) as $signed,
                    _ => unreachable!("shift dispatch accepts only shift opcodes"),
                };
                Ok(Self::$variant(result))
            }};
        }
        match self {
            Self::I64(value) => shift!(value, i64, u64, I64),
            Self::U64(value) => shift!(value, u64, u64, U64),
            Self::I128(value) => shift!(value, i128, u128, I128),
            Self::U128(value) => shift!(value, u128, u128, U128),
            Self::F64(_) => Err(NumericError::TypeError),
        }
    }
}

enum Integer {
    Signed(i128),
    Unsigned(u128),
}

fn signed_width(kind: Intrinsic) -> Option<u32> {
    match kind {
        Intrinsic::I8 => Some(8),
        Intrinsic::I16 => Some(16),
        Intrinsic::I32 => Some(32),
        Intrinsic::I64 => Some(64),
        Intrinsic::I128 => Some(128),
        Intrinsic::Isize => Some(isize::BITS),
        _ => None,
    }
}

fn unsigned_width(kind: Intrinsic) -> Option<u32> {
    match kind {
        Intrinsic::U8 => Some(8),
        Intrinsic::U16 => Some(16),
        Intrinsic::U32 => Some(32),
        Intrinsic::U64 => Some(64),
        Intrinsic::U128 => Some(128),
        Intrinsic::Usize => Some(usize::BITS),
        _ => None,
    }
}

fn signed_binary(left: i128, right: i128, opcode: Opcode) -> Result<i128, NumericError> {
    let value = match opcode {
        Opcode::Add => left.checked_add(right),
        Opcode::Sub => left.checked_sub(right),
        Opcode::Mul => left.checked_mul(right),
        Opcode::Div | Opcode::Mod if right == 0 => return Err(NumericError::DivisionByZero),
        Opcode::Div => left.checked_div(right),
        // MIN % -1 is mathematically zero, although Rust's checked_rem rejects it.
        Opcode::Mod if right == -1 => Some(0),
        Opcode::Mod => left.checked_rem(right),
        Opcode::BitAnd => Some(left & right),
        Opcode::BitOr => Some(left | right),
        Opcode::BitXor => Some(left ^ right),
        _ => return Err(NumericError::TypeError),
    };
    value.ok_or(NumericError::Overflow)
}

fn unsigned_binary(left: u128, right: u128, opcode: Opcode) -> Result<u128, NumericError> {
    let value = match opcode {
        Opcode::Add => left.checked_add(right),
        Opcode::Sub => left.checked_sub(right),
        Opcode::Mul => left.checked_mul(right),
        Opcode::Div | Opcode::Mod if right == 0 => return Err(NumericError::DivisionByZero),
        Opcode::Div => left.checked_div(right),
        Opcode::Mod => left.checked_rem(right),
        Opcode::BitAnd => Some(left & right),
        Opcode::BitOr => Some(left | right),
        Opcode::BitXor => Some(left ^ right),
        _ => return Err(NumericError::TypeError),
    };
    value.ok_or(NumericError::Overflow)
}

fn compare_float_integer(float: f64, integer: Number) -> Option<Ordering> {
    if float.is_nan() {
        return None;
    }
    match integer.integer() {
        Integer::Signed(integer) => {
            if float < i128::MIN as f64 {
                return Some(Ordering::Less);
            }
            if float >= -(i128::MIN as f64) {
                return Some(Ordering::Greater);
            }
            let integral = (float as i128).cmp(&integer);
            if integral != Ordering::Equal {
                return Some(integral);
            }
        }
        Integer::Unsigned(integer) => {
            if float < 0.0 {
                return Some(Ordering::Less);
            }
            if float >= u128::MAX as f64 {
                return Some(Ordering::Greater);
            }
            let integral = (float as u128).cmp(&integer);
            if integral != Ordering::Equal {
                return Some(integral);
            }
        }
    }
    float.fract().partial_cmp(&0.0)
}

impl fmt::Display for Number {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::I64(value) => value.fmt(formatter),
            Self::U64(value) => value.fmt(formatter),
            Self::I128(value) => value.fmt(formatter),
            Self::U128(value) => value.fmt(formatter),
            Self::F64(value) => value.fmt(formatter),
        }
    }
}

impl std::ops::Neg for Number {
    type Output = Result<Self, NumericError>;

    fn neg(self) -> Self::Output {
        Number::checked_neg(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn casts_check_every_integer_width_and_signedness() {
        for target in [
            Intrinsic::I8,
            Intrinsic::I16,
            Intrinsic::I32,
            Intrinsic::I64,
            Intrinsic::I128,
            Intrinsic::Isize,
        ] {
            let width = signed_width(target).unwrap();
            let (minimum, maximum) = if width == 128 {
                (i128::MIN, i128::MAX)
            } else {
                let limit = 1i128 << (width - 1);
                (-limit, limit - 1)
            };
            for value in [minimum, maximum] {
                let expected = if width == 128 {
                    Number::I128(value)
                } else {
                    Number::I64(value as i64)
                };
                assert_eq!(Number::I128(value).cast_to(target), Ok(expected));
            }
            if width < 128 {
                assert_eq!(
                    Number::I128(minimum - 1).cast_to(target),
                    Err(NumericError::Overflow)
                );
            }
            assert_eq!(
                Number::U128(maximum as u128 + 1).cast_to(target),
                Err(NumericError::Overflow)
            );
        }
        for target in [
            Intrinsic::U8,
            Intrinsic::U16,
            Intrinsic::U32,
            Intrinsic::U64,
            Intrinsic::U128,
            Intrinsic::Usize,
        ] {
            let width = unsigned_width(target).unwrap();
            let maximum = if width == 128 {
                u128::MAX
            } else {
                (1u128 << width) - 1
            };
            let expected = if width == 128 {
                Number::U128(maximum)
            } else {
                Number::U64(maximum as u64)
            };
            assert_eq!(Number::U128(maximum).cast_to(target), Ok(expected));
            assert_eq!(Number::I64(-1).cast_to(target), Err(NumericError::Overflow));
            if width < 128 {
                assert_eq!(
                    Number::U128(maximum + 1).cast_to(target),
                    Err(NumericError::Overflow)
                );
            }
        }
        assert_eq!(
            Number::I64(1).cast_to(Intrinsic::Bool),
            Err(NumericError::TypeError)
        );
    }

    #[test]
    fn float_casts_truncate_check_bounds_and_round_f32_once() {
        assert_eq!(
            Number::F64(127.9).cast_to(Intrinsic::I8),
            Ok(Number::I64(127))
        );
        assert_eq!(
            Number::F64(-128.9).cast_to(Intrinsic::I8),
            Ok(Number::I64(-128))
        );
        assert_eq!(
            Number::F64(128.0).cast_to(Intrinsic::I8),
            Err(NumericError::Overflow)
        );
        assert_eq!(
            Number::F64(-129.0).cast_to(Intrinsic::I8),
            Err(NumericError::Overflow)
        );
        assert_eq!(Number::F64(-0.5).cast_to(Intrinsic::U8), Ok(Number::U64(0)));
        assert_eq!(
            Number::F64(-1.0).cast_to(Intrinsic::U8),
            Err(NumericError::Overflow)
        );
        assert_eq!(
            Number::F64(i128::MIN as f64).cast_to(Intrinsic::I128),
            Ok(Number::I128(i128::MIN))
        );
        assert_eq!(
            Number::F64(-(i128::MIN as f64)).cast_to(Intrinsic::I128),
            Err(NumericError::Overflow)
        );
        assert_eq!(
            Number::F64(u128::MAX as f64).cast_to(Intrinsic::U128),
            Err(NumericError::Overflow)
        );
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for target in [
                Intrinsic::I8,
                Intrinsic::U8,
                Intrinsic::I128,
                Intrinsic::U128,
            ] {
                assert_eq!(
                    Number::F64(invalid).cast_to(target),
                    Err(NumericError::Overflow)
                );
            }
        }
        assert_eq!(
            Number::F64(0.1).cast_to(Intrinsic::F32),
            Ok(Number::F64(f64::from(0.1f32)))
        );
        assert_eq!(
            Number::F64(f64::from(f32::MAX)).cast_to(Intrinsic::F32),
            Ok(Number::F64(f64::from(f32::MAX)))
        );
        assert_eq!(
            Number::F64(f64::from(f32::MAX) * 2.0).cast_to(Intrinsic::F32),
            Err(NumericError::Overflow)
        );
        assert_eq!(
            Number::F64(f64::INFINITY).cast_to(Intrinsic::F32),
            Ok(Number::F64(f64::INFINITY))
        );
        assert!(
            matches!(Number::F64(f64::NAN).cast_to(Intrinsic::F32), Ok(Number::F64(value)) if value.is_nan())
        );
        assert_eq!(
            Number::U128(u128::MAX).cast_to(Intrinsic::F64),
            Ok(Number::F64(u128::MAX as f64))
        );
    }

    #[test]
    fn narrow_heap_scalars_validate_range_and_f32_representation() {
        #[repr(C)]
        struct Fixture {
            header: ObjectHeader,
            word: u64,
        }
        let decode = |target: Intrinsic, word: u64| {
            let fixture = Fixture {
                header: ObjectHeader::new(target.type_index(), 1),
                word,
            };
            // SAFETY: the immutable fixture has an aligned header/word layout,
            // remains live during decoding and cannot be moved by collection.
            let value = unsafe { TaggedValue::from_heap_ptr((&fixture.word as *const u64).cast()) };
            unsafe { Number::from_tagged(value) }
        };
        for target in [
            Intrinsic::I8,
            Intrinsic::I16,
            Intrinsic::I32,
            Intrinsic::Isize,
        ] {
            assert_eq!(decode(target, (-42i64) as u64), Some(Number::I64(-42)));
            let width = signed_width(target).unwrap();
            if width < 64 {
                assert_eq!(decode(target, 1u64 << (width - 1)), None);
                assert_eq!(decode(target, (-(1i64 << (width - 1)) - 1) as u64), None);
            }
        }
        for target in [
            Intrinsic::U8,
            Intrinsic::U16,
            Intrinsic::U32,
            Intrinsic::Usize,
        ] {
            assert_eq!(decode(target, 42), Some(Number::U64(42)));
            let width = unsigned_width(target).unwrap();
            if width < 64 {
                assert_eq!(decode(target, 1u64 << width), None);
            }
        }
        assert_eq!(
            decode(Intrinsic::F32, f64::from(0.1f32).to_bits()),
            Some(Number::F64(f64::from(0.1f32)))
        );
        assert_eq!(decode(Intrinsic::F32, 0.1f64.to_bits()), None);
        assert_eq!(
            decode(Intrinsic::F32, (f64::from(f32::MAX) * 2.0).to_bits()),
            None
        );
        assert!(
            matches!(decode(Intrinsic::F32, f64::NAN.to_bits()), Some(Number::F64(value)) if value.is_nan())
        );
        assert_eq!(
            decode(Intrinsic::F32, f64::INFINITY.to_bits()),
            Some(Number::F64(f64::INFINITY))
        );
    }

    #[test]
    fn arithmetic_promotes_without_losing_bits() {
        assert_eq!(
            Number::I64(i64::MAX).binary(Number::I64(1), Opcode::Add),
            Ok(Number::I128(i128::from(i64::MAX) + 1))
        );
        assert_eq!(
            Number::U64(u64::MAX).binary(Number::U64(2), Opcode::Mul),
            Ok(Number::U128(u128::from(u64::MAX) * 2))
        );
        assert_eq!(
            Number::I64(-1).binary(Number::U64(u64::MAX), Opcode::Add),
            Ok(Number::I128(i128::from(u64::MAX) - 1))
        );
        assert_eq!(
            Number::I128(i128::MIN).binary(Number::I128(-1), Opcode::Mod),
            Ok(Number::I128(0))
        );
        assert_eq!(
            Number::I128(i128::MAX).binary(Number::I64(1), Opcode::Add),
            Err(NumericError::Overflow)
        );
        assert_eq!(
            Number::U128(u128::MAX).binary(Number::U64(1), Opcode::Add),
            Err(NumericError::Overflow)
        );
        assert_eq!(
            Number::U64(0).binary(Number::U64(1), Opcode::Sub),
            Err(NumericError::Overflow)
        );
    }

    #[test]
    fn zero_division_and_invalid_operations_are_errors() {
        for value in [
            Number::I64(1),
            Number::U64(1),
            Number::I128(1),
            Number::U128(1),
            Number::F64(1.0),
        ] {
            for opcode in [Opcode::Div, Opcode::Mod] {
                assert_eq!(
                    value.binary(Number::I64(0), opcode),
                    Err(NumericError::DivisionByZero)
                );
            }
            assert_eq!(
                value.binary(Number::I64(1), Opcode::Mov),
                Err(NumericError::TypeError)
            );
        }
        assert_eq!(
            Number::F64(3.5).binary(Number::I64(2), Opcode::Add),
            Ok(Number::F64(5.5))
        );
        assert_eq!(
            Number::I128(i128::MIN).binary(Number::I64(-1), Opcode::Div),
            Err(NumericError::Overflow)
        );
        assert_eq!(Number::F64(1.0).bit_not(), Err(NumericError::TypeError));
        assert_eq!(
            Number::U128(u128::MAX).binary(Number::I64(0), Opcode::Div),
            Err(NumericError::DivisionByZero)
        );
        assert_eq!(
            Number::U128(u128::MAX).binary(Number::I64(-1), Opcode::Mov),
            Err(NumericError::TypeError)
        );
    }

    #[test]
    fn unary_and_shifts_preserve_width_and_reject_lost_bits() {
        assert_eq!(
            Number::I64(i64::MIN).checked_neg(),
            Ok(Number::I128(1i128 << 63))
        );
        assert_eq!(
            Number::U128(1u128 << 127).checked_neg(),
            Ok(Number::I128(i128::MIN))
        );
        assert_eq!(
            Number::I128(i128::MIN).checked_neg(),
            Err(NumericError::Overflow)
        );
        assert_eq!(Number::U64(0).bit_not(), Ok(Number::U64(u64::MAX)));
        assert_eq!(
            Number::I64(-1).binary(Number::I64(63), Opcode::Shl),
            Ok(Number::I64(i64::MIN))
        );
        assert_eq!(
            Number::U64(1).binary(Number::I64(63), Opcode::Shl),
            Ok(Number::U64(1 << 63))
        );
        assert_eq!(
            Number::U64(2).binary(Number::I64(63), Opcode::Shl),
            Err(NumericError::Overflow)
        );
        assert_eq!(
            Number::I64(-1).binary(Number::I64(1), Opcode::UShr),
            Ok(Number::I64(i64::MAX))
        );
        assert_eq!(
            Number::I64(-1).binary(Number::I64(1), Opcode::Shr),
            Ok(Number::I64(-1))
        );
        for count in [Number::I64(-1), Number::U64(64), Number::U128(u128::MAX)] {
            assert_eq!(
                Number::U64(1).binary(count, Opcode::Shl),
                Err(NumericError::InvalidShift)
            );
        }
        assert_eq!(
            Number::U128(1).binary(Number::U64(127), Opcode::Shl),
            Ok(Number::U128(1u128 << 127))
        );
        assert_eq!(
            Number::F64(1.0).binary(Number::I64(-1), Opcode::Shl),
            Err(NumericError::TypeError)
        );
        for (opcode, expected) in [
            (Opcode::BitAnd, 0b1000),
            (Opcode::BitOr, 0b1110),
            (Opcode::BitXor, 0b0110),
        ] {
            assert_eq!(
                Number::U128(0b1010).binary(Number::U64(0b1100), opcode),
                Ok(Number::U128(expected))
            );
        }
    }

    #[test]
    fn comparisons_and_conversions_respect_exact_boundaries() {
        assert_eq!(
            Number::U128(u128::MAX).partial_cmp_numeric(Number::I128(i128::MAX)),
            Some(Ordering::Greater)
        );
        assert_eq!(
            Number::U64((1 << 53) + 1).partial_cmp_numeric(Number::F64((1u64 << 53) as f64)),
            Some(Ordering::Greater)
        );
        assert_eq!(
            Number::F64(-0.5).partial_cmp_numeric(Number::I64(0)),
            Some(Ordering::Less)
        );
        assert_eq!(
            Number::F64(f64::NAN).partial_cmp_numeric(Number::U128(0)),
            None
        );
        assert_eq!(
            Number::F64(f64::INFINITY).partial_cmp_numeric(Number::U128(u128::MAX)),
            Some(Ordering::Greater)
        );
        assert_eq!(
            Number::F64(i64::MIN as f64).to_i64_checked(),
            Some(i64::MIN)
        );
        assert_eq!(Number::F64(-(i64::MIN as f64)).to_i64_checked(), None);
        assert_eq!(Number::F64(u64::MAX as f64).to_u64_checked(), None);
        assert_eq!(Number::F64(3.75).to_i64_checked(), Some(3));
        assert_eq!(Number::F64(-0.5).to_u64_checked(), None);
        assert_eq!(Number::U128(u128::MAX).to_string(), u128::MAX.to_string());
    }

    #[test]
    fn heap_decoding_checks_kind_size_and_word_order() {
        #[repr(C)]
        struct Fixture {
            header: ObjectHeader,
            words: [u64; 2],
        }
        for (kind, count, words, expected) in [
            (
                Intrinsic::I64,
                1,
                [i64::MIN as u64, 0],
                Some(Number::I64(i64::MIN)),
            ),
            (
                Intrinsic::U64,
                1,
                [u64::MAX, 0],
                Some(Number::U64(u64::MAX)),
            ),
            (
                Intrinsic::F64,
                1,
                [0.1f64.to_bits(), 0],
                Some(Number::F64(0.1)),
            ),
            (
                Intrinsic::I128,
                2,
                [0, 1 << 63],
                Some(Number::I128(i128::MIN)),
            ),
            (
                Intrinsic::U128,
                2,
                [42, 1],
                Some(Number::U128((1u128 << 64) + 42)),
            ),
            (Intrinsic::I128, 1, [0, 0], None),
            (Intrinsic::I64, 2, [0, 0], None),
            (Intrinsic::Closure, 2, [0, 0], None),
        ] {
            let fixture = Fixture {
                header: ObjectHeader::new(kind.type_index(), count),
                words,
            };
            // SAFETY: repr(C) gives the exact header/payload layout. The fixture
            // is live and immutable throughout decoding; no GC is involved.
            let value = unsafe { TaggedValue::from_heap_ptr(fixture.words.as_ptr().cast()) };
            assert_eq!(unsafe { Number::from_tagged(value) }, expected);
        }
        assert_eq!(
            unsafe { Number::from_tagged(TaggedValue::from_i64(-42)) },
            Some(Number::I64(-42))
        );
        assert_eq!(unsafe { Number::from_tagged(TaggedValue::TRUE) }, None);
    }
}
