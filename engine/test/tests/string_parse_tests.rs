//! Whole-string numeric parsing. Malformed text is null; range failures overflow.

mod common;

#[test]
fn decimal_and_radix_integers_keep_their_exact_values() {
    let source = r#"
        fn main() {
            let matched = "42".parse_i64() == 42 and "+10".parse_i64() == 10 and "-0".parse_i64() == 0
                and "0xff".parse_i64() == 255 and "-0X10".parse_i64() == -16
                and "0b1010".parse_i64() == 10 and "0B0".parse_i64() == 0
                and "0o17".parse_i64() == 15 and "0O10".parse_i64() == 8
                and "08".parse_i64() == 8
                and "9223372036854775807".parse_i64() == 9223372036854775807
                and "-9223372036854775808".parse_i64() == -9223372036854775808
                and type_of("3".parse_i64()) == i64
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn floats_accept_literal_forms_and_plain_integers() {
    let source = r#"
        fn main() {
            let matched = "1.5".parse_f64() == 1.5 and "1".parse_f64() == 1.0
                and "-1.25e1".parse_f64() == -12.5 and "+1E2".parse_f64() == 100.0
                and type_of("3".parse_f64()) == f64
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn malformed_text_is_null_and_out_of_range_values_overflow() {
    for source in [
        r#"fn main() { if "no".parse_i64() == null and " 1".parse_i64() == null and "1 ".parse_i64() == null and "1.0".parse_i64() == null and "0x".parse_i64() == null and "0xG".parse_i64() == null and "".parse_i64() == null and "-".parse_i64() == null { 42 } else { 0 } }"#,
        r#"fn main() { if "1.".parse_f64() == null and ".5".parse_f64() == null and "inf".parse_f64() == null and "NaN".parse_f64() == null and "1e".parse_f64() == null and " 1.0".parse_f64() == null { 42 } else { 0 } }"#,
        "fn main() { if \"\u{0660}\".parse_i64() == null { 42 } else { 0 } }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
    for (source, message) in [
        (
            r#"fn main() { "9223372036854775808".parse_i64(); 0 }"#,
            "NumericOverflow",
        ),
        (
            r#"fn main() { "-9223372036854775809".parse_i64(); 0 }"#,
            "NumericOverflow",
        ),
        (r#"fn main() { "1e309".parse_f64(); 0 }"#, "NumericOverflow"),
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains(message), "{source}: {error}");
    }
}
