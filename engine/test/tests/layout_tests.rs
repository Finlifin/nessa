//! Floor division, f64 layout, and byte-index string access.

mod common;

#[test]
fn floor_division_matches() {
    let source = r#"
        fn main() {
            let min = -9223372036854775808
            let matched = 8.div_floor(3) == 2
                and 8.div_floor(-3) == -3
                and (-8).div_floor(3) == -3
                and (-8).div_floor(-3) == 2
                and 8.div_floor(4) == 2
                and (7).as(u64).div_floor((4).as(u64)) == 1
                and (8).as(i128).div_floor((3).as(i128)) == (2).as(i128)
                and min.checked_div_floor(-1) == null
                and 1.checked_div_floor(0) == null
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn a_zero_floor_divisor_is_rejected() {
    for (source, message) in [
        (
            r#"fn main() { 1.div_floor(0) }"#,
            "divisor must not be zero",
        ),
        (
            r#"fn main() { (-9223372036854775808).div_floor(-1) }"#,
            "NumericOverflow",
        ),
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains(message), "{source}: {error}");
    }
}

#[test]
fn float_layout_matches() {
    let source = r#"
        fn main() {
            let negative_zero = copysign(0.0, -1.0)
            let matched = (1.0).is_sign_positive()
                and (not ((-1.0).is_sign_positive()))
                and (0.0).is_sign_positive()
                and (not (negative_zero.is_sign_positive()))
                and infinity().is_sign_positive()
                and (not (neg_infinity().is_sign_positive()))
                and (1.0).to_le_bytes().len() == 8
                and f64_from_le_bytes((1.0).to_le_bytes()) == 1.0
                and f64_from_be_bytes((1.0).to_be_bytes()) == 1.0
                and negative_zero.to_be_bytes().get(0) == 128
                and f64_from_le_bytes(nan().to_le_bytes()).to_bits() == nan().to_bits()
                and f64_radix() == 2
                and f64_mantissa_digits() == 53
                and f64_digits() == 15
                and f64_min_exp() == -1021
                and f64_max_exp() == 1024
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn bad_float_bytes_are_rejected() {
    for (source, message) in [
        (
            r#"fn main() { f64_from_le_bytes([1]) }"#,
            "float byte length does not match",
        ),
        (
            r#"fn main() { f64_from_be_bytes([256, 0, 0, 0, 0, 0, 0, 0]) }"#,
            "float byte is out of range",
        ),
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains(message), "{source}: {error}");
    }
}

#[test]
fn byte_ranges_match() {
    let source = r#"
        fn text(value: ?String) -> String {
            value match {
                found? => found,
                _ => "none",
            }
        }
        fn pieces(value: ?List) -> String {
            value match {
                found? => found.join(","),
                _ => "none",
            }
        }
        fn main() {
            let matched = "ab".byte_at(0) == 97
                and "\u00e9".byte_at(0) == 195
                and "\u00e9".byte_at(1) == 169
                and text("ab".get(0, 1)) == "a"
                and text("ab".get(2, 2)) == ""
                and text("\u00e9".get(0, 2)) == "\u00e9"
                and text("\u00e9".get(1, 2)) == "none"
                and text("ab".get(2, 1)) == "none"
                and text("ab".get(0, 3)) == "none"
                and pieces("ab".split_at_checked(1)) == "a,b"
                and pieces("ab".split_at_checked(0)) == ",ab"
                and pieces("ab".split_at_checked(2)) == "ab,"
                and pieces("\u00e9".split_at_checked(1)) == "none"
                and pieces("ab".split_at_checked(3)) == "none"
                and pieces("ab".split_at_checked(-1)) == "none"
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn an_out_of_range_byte_is_rejected() {
    for source in [
        r#"fn main() { "ab".byte_at(-1) }"#,
        r#"fn main() { "ab".byte_at(2) }"#,
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
        r#"fn main() { true.div_floor(1) }"#,
        r#"fn main() { 1.div_floor((1).as(i128)) }"#,
        r#"fn main() { true.is_sign_positive() }"#,
        r#"fn main() { true.byte_at(0) }"#,
        r#"fn main() { true.split_at_checked(0) }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
