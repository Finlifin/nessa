//! Float classification, hyperbolic functions, ASCII case, and Unicode char case.

mod common;

#[test]
fn classification_case_and_exact_float_values() {
    let source = r#"
        fn main() {
            let matched = is_nan(log2(-1)) and not (is_nan(0)) and not (is_infinite(0)) and is_finite(0)
                and is_infinite(log2(0)) and is_sign_negative(log2(0)) and not (is_finite(log2(0)))
                and is_infinite(-log2(0)) and not (is_sign_negative(-log2(0)))
                and is_sign_negative(-0.0) and not (is_sign_negative(0.0)) and is_finite(-0.0)
                and is_sign_negative(copysign(log2(-1), -1))
                and sinh(0) == 0 and cosh(0) == 1 and tanh(0) == 0
                and cbrt(8) == 2 and cbrt(-8) == -2 and exp2(3) == 8
                and expm1(0) == 0 and log1p(0) == 0
                and is_infinite(log1p(-1)) and not (log1p(-2) == log1p(-2))
                and "Abé".to_ascii_uppercase() == "ABé" and "AbÉ".to_ascii_lowercase() == "abÉ"
                and "é".to_ascii_uppercase() == "é"
                and "a\t\nb  é".split_whitespace() == ["a", "b", "é"]
                and "".split_whitespace().is_empty() and " \r\n\t ".split_whitespace().is_empty()
                and 'a'.to_uppercase() == "A" and 'É'.to_lowercase() == "é"
                and 'ß'.to_uppercase() == "SS" and 'İ'.to_lowercase() == "i\u0307"
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn the_new_functions_reject_the_wrong_argument_type() {
    for source in [
        r#"fn main() { is_nan("a") }"#,
        r#"fn main() { sinh("a") }"#,
        r#"fn main() { 1.to_ascii_uppercase() }"#,
        r#"fn main() { 1.to_uppercase() }"#,
        r#"fn main() { 'a'.split_whitespace() }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
