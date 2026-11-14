//! Signed adjustment, unbounded shifts, and integer bounds.

mod common;

#[test]
fn signed_adjustment_and_bounds_match() {
    let source = r#"
        fn main() {
            let matched = (1).as(u64).saturating_add_signed(-2) == 0
                and (1).as(u64).wrapping_add_signed(-2) == u64_max()
                and (1).as(u64).strict_add_signed(-1) == 0
                and u64_max().saturating_add_signed(1) == u64_max()
                and (5).as(u64).saturating_sub_signed(8) == 0
                and (5).as(u64).wrapping_sub_signed(-3) == 8
                and (5).as(u64).strict_sub_signed(3) == 2
                and (0).as(u128).saturating_add_signed(-3) == 0
                and (0).as(u128).wrapping_sub_signed(1) == u128_max()
                and (1).as(usize).saturating_add_signed(-2) == 0
                and i64_min() == -9223372036854775808
                and i64_max() == 9223372036854775807
                and u64_max() == (-1).cast_unsigned()
                and i128_min() == (1).as(i128).unbounded_shl(127)
                and i128_max() == i128_min().wrapping_sub((1).as(i128))
                and isize_min() == i64_min().as(isize)
                and usize_max() == u64_max().as(usize)
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn unbounded_shifts_match() {
    let source = r#"
        fn main() {
            let huge = 9223372036854775807
            let matched = (1).unbounded_shl(63) == i64_min()
                and (1).unbounded_shl(64) == 0
                and (1).wrapping_shl(64) == 1
                and (1).unbounded_shl(huge) == 0
                and (-1).unbounded_shr(64) == -1
                and (-8).unbounded_shr(2) == -2
                and (-8).unbounded_shr(64) == -1
                and (1).unbounded_shr(64) == 0
                and (1).as(u64).unbounded_shl(63) == i64_min().cast_unsigned()
                and (1).as(u64).unbounded_shr(64) == 0
                and (-1).as(i128).unbounded_shr(128) == (-1).as(i128)
                and (1).as(i128).unbounded_shl(128) == 0
                and (1).as(usize).unbounded_shl(64) == 0
                and 'A'.eq_ignore_ascii_case('a')
                and (not ('A'.eq_ignore_ascii_case('B')))
                and (not ('\u00C9'.eq_ignore_ascii_case('\u00E9')))
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn a_signed_adjustment_overflows() {
    let source = r#"fn main() { (0).as(u64).strict_add_signed(-1) }"#;
    let error = common::run_value(source).unwrap_err();
    assert!(error.contains("NumericOverflow"), "{error}");
}

#[test]
fn a_negative_unbounded_shift_is_rejected() {
    let source = r#"fn main() { (1).unbounded_shl(-1) }"#;
    let error = common::run_value(source).unwrap_err();
    assert!(
        error.contains("shift distance must not be negative"),
        "{error}"
    );
}

#[test]
fn the_new_operations_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { 1.saturating_add_signed(1) }"#,
        r#"fn main() { (1).as(u64).saturating_add_signed((1).as(i128)) }"#,
        r#"fn main() { true.unbounded_shl(1) }"#,
        r#"fn main() { true.eq_ignore_ascii_case('a') }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
