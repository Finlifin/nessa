//! The ABI9 string-search and floating-point batch.

mod common;

#[test]
fn search_encoding_and_exact_float_values() {
    let source = r#"
        fn main() {
            let matched = "héllo".rfind("l") == 3 and "héllo".rfind("") == 5 and "aaa".count("aa") == 1
                and "a".count("") == 2 and "ab".is_ascii() and "a\0".is_ascii() and not "é".is_ascii()
                and 'é'.into_string() == "é" and '\0'.into_string().char_at(0) == '\0'
                and exp(0) == 1 and log2(8) == 3 and log10(1000) == 3
                and trunc(1.9) == 1 and trunc(-1.9) == -1
                and fract(1.25) == 0.25 and fract(-1.25) == -0.25
                and copysign(2, -3) == -2 and copysign(-2, 3) == 2 and copysign(1, -0.0) == -1
                and atan2(0, 1) == 0 and hypot(3, 4) == 5
                and tan(0) == 0 and asin(0) == 0 and acos(1) == 0 and atan(0) == 0
                and not (log2(-1) == log2(-1)) and not (asin(2) == asin(2))
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn the_new_functions_reject_the_wrong_argument_type() {
    for source in [
        r#"fn main() { "ab".rfind(1) }"#,
        r#"fn main() { 1.is_ascii() }"#,
        r#"fn main() { exp("a") }"#,
        r#"fn main() { hypot(3, "4") }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
