//! Mixed unsigned arithmetic, radix text, and byte-index string splits.

mod common;

#[test]
fn mixed_unsigned_arithmetic_matches() {
    let source = r#"
        fn main() {
            let min = -9223372036854775808
            let max = 9223372036854775807
            let one = (1).as(u64)
            let matched = 5.checked_add_unsigned((3).as(u64)) == 8
                and 1.checked_sub_unsigned((2).as(u64)) == -1
                and max.checked_add_unsigned(one) == null
                and min.checked_sub_unsigned(one) == null
                and max.saturating_add_unsigned(one) == max
                and min.saturating_sub_unsigned(one) == min
                and max.wrapping_add_unsigned(one) == min
                and 0.wrapping_sub_unsigned(one) == -1
                and (1).as(i128).checked_add_unsigned((2).as(u128)) == (3).as(i128)
                and (1).as(isize).checked_add_unsigned((2).as(usize)) == (3).as(isize)
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn strict_unsigned_adjust_reports_overflow() {
    let source = r#"fn main() { 9223372036854775807.strict_add_unsigned((1).as(u64)) }"#;
    let error = common::run_value(source).unwrap_err();
    assert!(error.contains("NumericOverflow"), "{error}");
}

#[test]
fn radix_text_matches() {
    let source = r#"
        fn main() {
            let min = -9223372036854775808
            let matched = (255).to_str_radix(16) == "ff"
                and (-10).to_str_radix(10) == "-10"
                and (0).to_str_radix(2) == "0"
                and min.to_str_radix(16) == "-8000000000000000"
                and (255).as(u64).to_str_radix(16) == "ff"
                and i64_from_str_radix("-8000000000000000", 16) == min
                and i64_from_str_radix("+10", 10) == 10
                and i64_from_str_radix("g", 16) == null
                and i64_from_str_radix(" 1", 10) == null
                and i64_from_str_radix("", 10) == null
                and u64_from_str_radix("-1", 10) == null
                and u64_from_str_radix("FF", 16) == (255).as(u64)
                and i128_from_str_radix("-ff", 16) == (-255).as(i128)
                and u128_from_str_radix("ff", 16) == (255).as(u128)
                and isize_from_str_radix("-10", 10) == (-10).as(isize)
                and usize_from_str_radix("10", 10) == (10).as(usize)
                and usize_from_str_radix("-1", 10) == null
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn a_bad_radix_is_rejected() {
    for source in [
        r#"fn main() { (1).to_str_radix(1) }"#,
        r#"fn main() { (1).to_str_radix(37) }"#,
        r#"fn main() { i64_from_str_radix("1", 1) }"#,
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(
            error.contains("digit radix must be from 2 to 36"),
            "{source}: {error}"
        );
    }
}

#[test]
fn byte_indexes_match() {
    let source = r#"
        fn main() {
            let matched = "\u00e9".char_at_byte(0) == '\u00e9'
                and "\u00e9".char_at_byte(1) == null
                and "\u00e9".char_at_byte(2) == null
                and "".char_at_byte(0) == null
                and "ab".split_at(0) == ["", "ab"]
                and "ab".split_at(1) == ["a", "b"]
                and "ab".split_at(2) == ["ab", ""]
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn a_mid_character_split_is_rejected() {
    let source = r#"fn main() { "\u00e9".split_at(1) }"#;
    let error = common::run_value(source).unwrap_err();
    assert!(
        error.contains("string index is not a character boundary"),
        "{error}"
    );
}

#[test]
fn an_out_of_range_byte_index_is_rejected() {
    for source in [
        r#"fn main() { "ab".char_at_byte(-1) }"#,
        r#"fn main() { "ab".char_at_byte(3) }"#,
        r#"fn main() { "ab".split_at(-1) }"#,
        r#"fn main() { "ab".split_at(3) }"#,
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(
            error.contains("string index is out of range"),
            "{source}: {error}"
        );
    }
}

#[test]
fn the_new_operations_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { (1).as(u64).checked_add_unsigned((1).as(u64)) }"#,
        r#"fn main() { 1.checked_add_unsigned((1).as(u128)) }"#,
        r#"fn main() { true.to_str_radix(10) }"#,
        r#"fn main() { true.split_at(0) }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
