//! Wrapping, saturating, and checked integer arithmetic, plus digit characters.

mod common;

#[test]
fn overflow_arithmetic_keeps_width_and_reports_checked_failure_as_null() {
    let source = r#"
        fn main() {
            let min = -9223372036854775808
            let max = 9223372036854775807
            let matched = max.wrapping_add(1) == min
                and max.saturating_add(1) == max
                and max.checked_add(1) == null
                and 5.wrapping_sub(8) == -3
                and 4.wrapping_mul(3) == 12
                and (-3).wrapping_neg() == 3
                and min.wrapping_neg() == min
                and min.saturating_neg() == max
                and min.checked_neg() == null
                and min.checked_div(-1) == null
                and 8.checked_div(2) == 4
                and 8.checked_div(0) == null
                and 5.checked_rem(2) == 1
                and 5.checked_rem(0) == null
                and (1).as(u64).saturating_sub(3) == 0
                and (0).as(u64).checked_neg() == 0
                and (1).as(u64).checked_neg() == null
                and (1).as(u64).wrapping_neg().count_ones() == 64
                and (5).as(u64).saturating_neg() == 0
                and (1).as(i128).wrapping_mul(3) == 3
                and (1).as(isize).wrapping_sub(2) == -1
                and (1).as(usize).saturating_sub(2) == 0
                and char_from_digit(10, 16) == 'a'
                and char_from_digit(9, 10) == '9'
                and char_from_digit(35, 36) == 'z'
                and char_from_digit(10, 10) == null
                and char_from_digit(-1, 10) == null
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn mixed_widths_and_bad_digit_radixes_are_rejected() {
    let mixed = driver::Driver::new().compile(r#"fn main() { 5.wrapping_add((1).as(i128)) }"#);
    assert!(mixed.has_errors);
    let radix = common::run_value(r#"fn main() { char_from_digit(0, 1) }"#).unwrap_err();
    assert!(
        radix.contains("digit radix must be from 2 to 36"),
        "{radix}"
    );
    let high = common::run_value(r#"fn main() { char_from_digit(0, 37) }"#).unwrap_err();
    assert!(high.contains("digit radix must be from 2 to 36"), "{high}");
}

#[test]
fn overflow_methods_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { (1.0).wrapping_add(1.0) }"#,
        r#"fn main() { true.checked_neg() }"#,
        r#"fn main() { 'a'.saturating_add('b') }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
