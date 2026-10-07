//! Numeric promotion follows representable integer ranges, never enum order.

use crate::Intrinsic;

fn integer_layout(kind: Intrinsic) -> Option<(bool, u32)> {
    use Intrinsic::*;
    Some(match kind {
        U8 => (false, 8),
        U16 => (false, 16),
        U32 => (false, 32),
        U64 => (false, 64),
        U128 => (false, 128),
        Usize => (false, usize::BITS),
        I8 => (true, 8),
        I16 => (true, 16),
        I32 => (true, 32),
        I64 => (true, 64),
        I128 => (true, 128),
        Isize => (true, isize::BITS),
        _ => return None,
    })
}

impl Intrinsic {
    /// Whether a literal's magnitude and sign fit this integer type's range.
    /// Negative literals require a signed type, including the spelling `-0`.
    pub fn contains_integer_literal(self, magnitude: u128, negative: bool) -> bool {
        let Some((signed, bits)) = integer_layout(self) else {
            return false;
        };
        if negative {
            signed && magnitude <= (1_u128 << (bits - 1))
        } else if signed {
            magnitude < (1_u128 << (bits - 1))
        } else {
            bits == 128 || magnitude < (1_u128 << bits)
        }
    }
}

pub(crate) fn can_widen(source: Intrinsic, target: Intrinsic) -> bool {
    if !source.is_numeric() || !target.is_numeric() {
        return false;
    }
    if matches!(target, Intrinsic::F32 | Intrinsic::F64) {
        return source != Intrinsic::F64 || target == Intrinsic::F64;
    }
    let Some((source_signed, source_bits)) = integer_layout(source) else {
        return false;
    };
    let Some((target_signed, target_bits)) = integer_layout(target) else {
        return false;
    };
    match (source_signed, target_signed) {
        (true, false) => false,
        (false, true) => target_bits > source_bits,
        _ => target_bits >= source_bits,
    }
}

pub(crate) fn lift(a: Intrinsic, b: Intrinsic) -> Option<Intrinsic> {
    if !a.is_numeric() || !b.is_numeric() {
        return None;
    }
    if a == b {
        return Some(a);
    }
    if a == Intrinsic::F64 || b == Intrinsic::F64 {
        return Some(Intrinsic::F64);
    }
    if a == Intrinsic::F32 || b == Intrinsic::F32 {
        return Some(Intrinsic::F32);
    }
    let (a_signed, a_bits) = integer_layout(a)?;
    let (b_signed, b_bits) = integer_layout(b)?;
    let signed = a_signed || b_signed;
    let required = a_bits
        .saturating_add(u32::from(signed && !a_signed))
        .max(b_bits.saturating_add(u32::from(signed && !b_signed)));
    let candidates = if signed {
        [
            Intrinsic::I8,
            Intrinsic::I16,
            Intrinsic::I32,
            Intrinsic::I64,
            Intrinsic::I128,
        ]
    } else {
        [
            Intrinsic::U8,
            Intrinsic::U16,
            Intrinsic::U32,
            Intrinsic::U64,
            Intrinsic::U128,
        ]
    };
    candidates
        .into_iter()
        .find(|&kind| integer_layout(kind).is_some_and(|(_, bits)| bits >= required))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_literal_ranges_include_signed_minima_without_overflow() {
        for kind in [
            Intrinsic::U8,
            Intrinsic::U16,
            Intrinsic::U32,
            Intrinsic::U64,
            Intrinsic::U128,
            Intrinsic::Usize,
            Intrinsic::I8,
            Intrinsic::I16,
            Intrinsic::I32,
            Intrinsic::I64,
            Intrinsic::I128,
            Intrinsic::Isize,
        ] {
            let (signed, bits) = integer_layout(kind).unwrap();
            let maximum = if signed {
                (1_u128 << (bits - 1)) - 1
            } else {
                u128::MAX >> (128 - bits)
            };
            assert!(kind.contains_integer_literal(maximum, false));
            if let Some(overflow) = maximum.checked_add(1) {
                assert!(!kind.contains_integer_literal(overflow, false));
            }
            if signed {
                let minimum_magnitude = maximum + 1;
                assert!(kind.contains_integer_literal(minimum_magnitude, true));
                assert!(!kind.contains_integer_literal(minimum_magnitude + 1, true));
            } else {
                assert!(!kind.contains_integer_literal(0, true));
                assert!(!kind.contains_integer_literal(1, true));
            }
        }
        assert!(!Intrinsic::F64.contains_integer_literal(42, false));
    }

    #[test]
    fn mixed_signedness_requires_room_for_the_entire_unsigned_range() {
        for (a, b, expected) in [
            (Intrinsic::U8, Intrinsic::I8, Some(Intrinsic::I16)),
            (Intrinsic::U32, Intrinsic::I64, Some(Intrinsic::I64)),
            (Intrinsic::U64, Intrinsic::I8, Some(Intrinsic::I128)),
            (Intrinsic::U128, Intrinsic::I128, None),
        ] {
            assert_eq!(lift(a, b), expected);
            assert_eq!(lift(b, a), expected);
        }
        assert!(!can_widen(Intrinsic::U64, Intrinsic::I8));
        assert!(!can_widen(Intrinsic::U64, Intrinsic::I64));
        assert!(!can_widen(Intrinsic::I8, Intrinsic::U64));
        assert!(can_widen(Intrinsic::U64, Intrinsic::I128));
    }

    #[test]
    fn promotion_preserves_ranges_for_all_fixed_width_integer_pairs() {
        let integers = [
            Intrinsic::U8,
            Intrinsic::U16,
            Intrinsic::U32,
            Intrinsic::U64,
            Intrinsic::U128,
            Intrinsic::Usize,
            Intrinsic::I8,
            Intrinsic::I16,
            Intrinsic::I32,
            Intrinsic::I64,
            Intrinsic::I128,
            Intrinsic::Isize,
        ];
        for a in integers {
            for b in integers {
                let lifted = lift(a, b);
                assert_eq!(lifted, lift(b, a));
                if let Some(common) = lifted {
                    assert!(can_widen(a, common), "{a:?} -> {common:?}");
                    assert!(can_widen(b, common), "{b:?} -> {common:?}");
                }
            }
        }
        assert!(!can_widen(Intrinsic::F64, Intrinsic::F32));
        assert!(!can_widen(Intrinsic::F32, Intrinsic::I128));
        assert!(!can_widen(Intrinsic::Bool, Intrinsic::I64));
    }
}
