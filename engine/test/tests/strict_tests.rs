//! Strict integer arithmetic and the remaining f64 constants.

mod common;

#[test]
fn strict_arithmetic_and_constants_keep_exact_results() {
    let source = r#"
        fn main() {
            let matched = 2.strict_add(3) == 5
                and 2.strict_sub(5) == -3
                and 2.strict_mul(4) == 8
                and (-7).strict_div(3) == -2
                and (-7).strict_rem(3) == -1
                and (-3).strict_neg() == 3
                and (0).as(u64).strict_neg() == 0
                and 1.strict_shl(2) == 4
                and (-8).strict_shr(1) == -4
                and 2.strict_pow(3) == 8
                and min_positive() > 0 and min_positive().is_normal()
                and frac_pi_2() == pi() / 2.0
                and frac_pi_4() == pi() / 4.0
                and frac_1_sqrt_2() > 0.707 and frac_1_sqrt_2() < 0.708
                and log2_10() * log10_2() == 1.0
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn strict_overflow_and_bad_divisors_are_errors() {
    let cases = [
        ("9223372036854775807.strict_add(1)", "NumericOverflow"),
        ("(-9223372036854775808).strict_div(-1)", "NumericOverflow"),
        ("(-9223372036854775808).strict_neg()", "NumericOverflow"),
        ("1.strict_div(0)", "divisor must not be zero"),
        ("1.strict_shl(64)", "NumericOverflow"),
        ("1.strict_shl(-1)", "shift distance must not be negative"),
        ("2.strict_pow(63)", "NumericOverflow"),
        ("2.strict_pow(-1)", "exponent must not be negative"),
        ("(1).as(u64).strict_neg()", "NumericOverflow"),
    ];
    for (expression, expected) in cases {
        let error = common::run_value(&format!("fn main() {{ {expression} }}")).unwrap_err();
        assert!(error.contains(expected), "{expression}: {error}");
    }
}

#[test]
fn strict_operations_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { true.strict_add(1) }"#,
        r#"fn main() { 1.strict_add((1).as(i128)) }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
