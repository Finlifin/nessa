//! Integer Euclidean division, shifts, powers, and hyperbolic inverses.

mod common;

#[test]
fn division_shifts_and_hyperbolic_inverses_match_their_domains() {
    let source = r#"
        fn main() {
            let wide: u64 = 9223372036854775808
            let matched = (-7).div_euclid(3) == -3 and (-7).rem_euclid(3) == 2
                and 7.div_euclid(-3) == -2 and 7.rem_euclid(-3) == 1
                and 1.midpoint(4) == 2 and (-7).midpoint(0) == -3 and 1.midpoint(2) == 1
                and (-5).unsigned_abs() == 5
                and (-9223372036854775808).unsigned_abs() == wide
                and 1.wrapping_shl(64) == 1 and 1.checked_shl(64) == null
                and 1.checked_shl(3) == 8 and (-8).wrapping_shr(1) == -4
                and 2.wrapping_pow(3) == 8 and 2.checked_pow(63) == null
                and (-2).saturating_pow(3) == -8 and 0.saturating_pow(0) == 1
                and (1).as(u64).div_euclid(2) == 0
                and (1).as(i128).midpoint(2) == 1
                and (2).as(u128).wrapping_pow(3) == 8
                and (0.0).asinh() == 0 and (1.0).acosh() == 0 and (0.0).atanh() == 0
                and (1.0).is_normal() and not ((0.0).is_normal())
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn zero_divisors_overflow_and_negative_distances_are_rejected() {
    let cases = [
        (
            r#"fn main() { (-7).div_euclid(0) }"#,
            "divisor must not be zero",
        ),
        (
            r#"fn main() { (-9223372036854775808).div_euclid(-1) }"#,
            "NumericOverflow",
        ),
        (
            r#"fn main() { (-9223372036854775808).rem_euclid(-1) }"#,
            "NumericOverflow",
        ),
        (
            r#"fn main() { 1.checked_shl(-1) }"#,
            "shift distance must not be negative",
        ),
        (
            r#"fn main() { 2.checked_pow(-1) }"#,
            "exponent must not be negative",
        ),
    ];
    for (source, expected) in cases {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains(expected), "{source}: {error}");
    }
}

#[test]
fn the_new_methods_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { (1).as(u64).unsigned_abs() }"#,
        r#"fn main() { true.wrapping_shl(1) }"#,
        r#"fn main() { true.is_normal() }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
