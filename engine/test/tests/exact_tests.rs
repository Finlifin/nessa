//! Integer square roots, exact division and shifts, and string affixes.

mod common;

#[test]
fn roots_and_euclidean_wrapping_match() {
    let source = r#"
        fn main() {
            let matched = 10.isqrt() == 3
                and 0.isqrt() == 0
                and 16.isqrt() == 4
                and (-1).checked_isqrt() == null
                and (10).as(u64).isqrt() == 3
                and (-5).strict_abs() == 5
                and 0.strict_abs() == 0
                and (-8).wrapping_div_euclid(3) == -3
                and (-8).wrapping_div(3) == -2
                and (-8).wrapping_rem_euclid(3) == 1
                and i64_min().wrapping_div_euclid(-1) == i64_min()
                and i64_min().wrapping_rem_euclid(-1) == 0
                and (5).as(u64).checked_signed_diff((8).as(u64)) == -3
                and (5).as(u64).checked_signed_diff((3).as(u64)) == 2
                and (0).as(u64).checked_signed_diff((1).as(u64)) == -1
                and u64_max().checked_signed_diff((0).as(u64)) == null
                and (0).as(u128).checked_signed_diff((1).as(u128)) == (-1).as(i128)
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn exact_division_and_shifts_match() {
    let source = r#"
        fn main() {
            let matched = (64).checked_div_exact(2) == 32
                and (65).checked_div_exact(2) == null
                and (10).checked_div_exact(0) == null
                and i64_min().checked_div_exact(-1) == null
                and (1).checked_shl_exact(3) == 8
                and (1).checked_shl_exact(63) == i64_min()
                and (1).checked_shl_exact(64) == null
                and (-1).checked_shl_exact(1) == null
                and (0x10).checked_shr_exact(4) == 1
                and (0x10).checked_shr_exact(5) == null
                and (-2).checked_shr_exact(1) == -1
                and (-1).checked_shr_exact(1) == null
                and (8).as(u64).checked_div_exact((2).as(u64)) == 4
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn affixes_match() {
    let source = r#"
        fn text(value: ?String) -> String { value match { found? => found, _ => "none" } }
        fn main() {
            let matched = "foo:bar".trim_prefix("foo:") == "bar"
                and "foo:bar".trim_prefix("bar") == "foo:bar"
                and "bar:foo".trim_suffix(":foo") == "bar"
                and "foo".trim_suffix("bar") == "foo"
                and text("bar:hello:foo".strip_circumfix("bar:", ":foo")) == "hello"
                and text("foo".strip_circumfix("foo", "foo")) == "none"
                and text("ab".strip_circumfix("a", "b")) == ""
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn a_negative_square_root_is_rejected() {
    let source = r#"fn main() { (-1).isqrt() }"#;
    let error = common::run_value(source).unwrap_err();
    assert!(error.contains("square root of a negative value"), "{error}");
}

#[test]
fn the_signed_minimum_abs_overflows() {
    let source = r#"fn main() { i64_min().strict_abs() }"#;
    let error = common::run_value(source).unwrap_err();
    assert!(error.contains("NumericOverflow"), "{error}");
}

#[test]
fn an_inexact_division_is_rejected() {
    let source = r#"fn main() { (10).strict_div_exact(4) }"#;
    let error = common::run_value(source).unwrap_err();
    assert!(error.contains("division is not exact"), "{error}");
}

#[test]
fn an_inexact_shift_is_rejected() {
    let source = r#"fn main() { (3).strict_shl_exact(63) }"#;
    let error = common::run_value(source).unwrap_err();
    assert!(error.contains("shift is not exact"), "{error}");
}

#[test]
fn the_new_operations_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { true.isqrt() }"#,
        r#"fn main() { 1.checked_signed_diff(1) }"#,
        r#"fn main() { (1).as(u64).strict_abs() }"#,
        r#"fn main() { true.trim_prefix("a") }"#,
        r#"fn main() { (1).as(u64).checked_signed_diff((1).as(u128)) }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
