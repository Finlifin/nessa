//! Full products, powers of two, and UTF-8/UTF-16 conversion.

mod common;

#[test]
fn products_powers_and_utf_conversion_match_their_domains() {
    let source = r#"
        fn main() {
            let min = -9223372036854775808
            let face = char_from_code(0x1F600) match {
                value? => value.into_string(),
                _ => "",
            }
            let matched = (-7).checked_div_euclid(3) == -3
                and (-7).checked_rem_euclid(3) == 2
                and (-7).checked_div_euclid(0) == null
                and min.checked_div_euclid(-1) == null
                and min.checked_rem_euclid(-1) == null
                and (-3).widening_mul(5) == -15
                and 9223372036854775807.widening_mul(2) == (9223372036854775807).as(i128) * (2).as(i128)
                and (3).as(u64).widening_mul((5).as(u64)) == 15
                and (5).as(u64).next_power_of_two() == 8
                and (0).as(u64).checked_next_power_of_two() == 1
                and (-1).cast_unsigned().checked_next_power_of_two() == null
                and (5).as(u64).checked_add_signed(-3) == 2
                and (1).as(u64).checked_add_signed(-2) == null
                and (1).as(u64).checked_sub_signed(2) == null
                and 6.is_multiple_of(3) and not (5.is_multiple_of(3))
                and not (5.is_multiple_of(0)) and not (0.is_multiple_of(0))
                and min.is_multiple_of(-1)
                and (100.0).log_base(10.0) == 2 and (1.0).log_base(10.0) == 0
                and "\u00e9".encode_utf8() == [195, 169]
                and "a".encode_utf16() == [97]
                and string_from_utf8([195, 169]) == "\u00e9"
                and string_from_utf8([255]) == null
                and string_from_utf8_lossy([255]) == "\uFFFD"
                and string_from_utf16([97, 55357, 56832]) == "a" ++ face
                and string_from_utf16([55296]) == null
                and string_from_utf16_lossy([55296]) == "\uFFFD"
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn overflow_and_out_of_range_units_are_rejected() {
    let overflow =
        common::run_value(r#"fn main() { (-1).cast_unsigned().next_power_of_two() }"#).unwrap_err();
    assert!(overflow.contains("NumericOverflow"), "{overflow}");
    let byte = common::run_value(r#"fn main() { string_from_utf8([256]) }"#).unwrap_err();
    assert!(byte.contains("utf-8 byte is out of range"), "{byte}");
    let unit = common::run_value(r#"fn main() { string_from_utf16([65536]) }"#).unwrap_err();
    assert!(unit.contains("utf-16 code unit is out of range"), "{unit}");
}

#[test]
fn the_new_operations_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { (1).as(i128).widening_mul((2).as(i128)) }"#,
        r#"fn main() { true.next_power_of_two() }"#,
        r#"fn main() { 1.checked_add_signed(-1) }"#,
        r#"fn main() { string_from_utf8(1) }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
