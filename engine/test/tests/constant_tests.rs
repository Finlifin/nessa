//! Unicode scalar boundaries and exact f64 constants.

mod common;

#[test]
fn boundaries_and_constants_match_ieee_values() {
    let source = r#"
        fn main() {
            let matched = "\u00e9".is_char_boundary(0)
                and (not ("\u00e9".is_char_boundary(1)))
                and "\u00e9".is_char_boundary(2)
                and "\u00e9".floor_char_boundary(1) == 0
                and "\u00e9".ceil_char_boundary(1) == 2
                and "".is_char_boundary(0)
                and "a".floor_char_boundary(1) == 1
                and "a".ceil_char_boundary(0) == 0
                and pi() > 3.14 and pi() < 3.15
                and euler() > 2.718 and euler() < 2.719
                and tau() == pi() * 2.0
                and ln_2() > 0.693 and ln_2() < 0.694
                and ln_10() > 2.302 and ln_10() < 2.303
                and log2_e() > 1.442 and log2_e() < 1.443
                and log10_e() > 0.434 and log10_e() < 0.435
                and sqrt_2() > 1.414 and sqrt_2() < 1.415
                and infinity().is_infinite()
                and neg_infinity().is_sign_negative()
                and nan().is_nan()
                and epsilon() > 0
                and (1.0 + epsilon()) != 1.0
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn a_byte_index_outside_the_string_is_rejected() {
    for source in [
        r#"fn main() { "a".is_char_boundary(-1) }"#,
        r#"fn main() { "a".is_char_boundary(2) }"#,
        r#"fn main() { "a".floor_char_boundary(2) }"#,
        r#"fn main() { "a".ceil_char_boundary(-1) }"#,
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(
            error.contains("string index is out of range"),
            "{source}: {error}"
        );
    }
}

#[test]
fn boundaries_and_constants_reject_the_wrong_shape() {
    for source in [
        r#"fn main() { true.is_char_boundary(0) }"#,
        r#"fn main() { 1.floor_char_boundary(0) }"#,
        r#"fn main() { pi(1) }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
