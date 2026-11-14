//! Wrapping division, bit reinterpretation, and character encodings.

mod common;

#[test]
fn wrapping_division_bit_casts_and_encodings_match_their_domains() {
    let source = r#"
        fn main() {
            let high: u64 = 9223372036854775808
            let min = -9223372036854775808
            let face = char_from_code(0x1F600) match {
                value? => value.encode_utf16() == [55357, 56832]
                    and value.encode_utf8() == [240, 159, 152, 128],
                _ => false,
            }
            let matched = (-7).wrapping_div(3) == -2 and (-7).wrapping_rem(3) == -1
                and min.wrapping_div(-1) == min and min.wrapping_rem(-1) == 0
                and min.saturating_abs() == 9223372036854775807
                and min.checked_abs() == null and min.wrapping_abs() == min
                and (-5).saturating_abs() == 5
                and (-3).signum() == -1 and 0.signum() == 0 and 4.signum() == 1
                and 1000.checked_ilog(10) == 3 and 0.checked_ilog(10) == null
                and (-1).checked_ilog(10) == null
                and (1000).as(u64).checked_ilog(10) == 3
                and (-1).cast_unsigned().count_ones() == 64
                and high.cast_signed() == min
                and (1.0).to_bits() == 4607182418800017408
                and high.f64_from_bits().is_sign_negative()
                and (1).as(u64).f64_from_bits().is_subnormal()
                and not ((0.0).is_subnormal())
                and (2.0).powi(-1) == 0.5 and (2.0).powi(3) == 8
                and '\u00e9'.encode_utf8() == [195, 169]
                and 'a'.encode_utf16() == [97]
                and face
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn zero_divisors_small_bases_and_wide_exponents_are_rejected() {
    let cases = [
        (
            r#"fn main() { (-7).wrapping_div(0) }"#,
            "divisor must not be zero",
        ),
        (
            r#"fn main() { 8.checked_ilog(1) }"#,
            "logarithm base must be at least 2",
        ),
        (r#"fn main() { (2.0).powi(2147483648) }"#, "NumericOverflow"),
    ];
    for (source, expected) in cases {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains(expected), "{source}: {error}");
    }
}

#[test]
fn the_new_methods_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { (1).as(u64).saturating_abs() }"#,
        r#"fn main() { true.signum() }"#,
        r#"fn main() { (1.0).cast_unsigned() }"#,
        r#"fn main() { true.encode_utf8() }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
