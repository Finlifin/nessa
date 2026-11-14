//! Wider integer parsing, gcd, terminator splits, and byte-index search.

mod common;

#[test]
fn wider_integer_parses_match() {
    let source = r#"
        fn main() {
            let high: u64 = 9223372036854775808
            let min128 = (1).as(i128).wrapping_shl(127)
            let max128 = (0).as(u128).wrapping_sub((1).as(u128))
            let matched = "9223372036854775808".parse_u64() == high
                and "+10".parse_u64() == 10
                and "0xff".parse_u64() == 255
                and "0b10".parse_u64() == 2
                and "0o10".parse_u64() == 8
                and "-1".parse_u64() == null
                and "-0".parse_u64() == null
                and "0x".parse_u64() == null
                and " 1".parse_u64() == null
                and "-0x80000000000000000000000000000000".parse_i128() == min128
                and "255".parse_i128() == (255).as(i128)
                and "0xffffffffffffffffffffffffffffffff".parse_u128() == max128
                and "0".parse_u128() == 0
                and "-1".parse_u128() == null
                and "10".parse_usize() == (10).as(usize)
                and "-10".parse_isize() == (-10).as(isize)
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn an_oversized_unsigned_parse_overflows() {
    let source = r#"fn main() { "18446744073709551616".parse_u64() }"#;
    let error = common::run_value(source).unwrap_err();
    assert!(error.contains("NumericOverflow"), "{error}");
}

#[test]
fn gcd_and_lcm_match() {
    let source = r#"
        fn main() {
            let high: u64 = 9223372036854775808
            let min = -9223372036854775808
            let matched = 12.gcd(18) == 6
                and (-12).gcd(18) == 6
                and 0.gcd(0) == 0
                and 0.lcm(5) == 0
                and 12.lcm(18) == 36
                and (-4).lcm(6) == 12
                and min.gcd(min) == high
                and (12).as(u64).gcd((18).as(u64)) == 6
                and (12).as(i128).lcm((18).as(i128)) == (36).as(u128)
                and (-1).cast_unsigned().checked_lcm((2).as(u64)) == null
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn an_oversized_lcm_overflows() {
    let source = r#"fn main() { (-1).cast_unsigned().lcm((2).as(u64)) }"#;
    let error = common::run_value(source).unwrap_err();
    assert!(error.contains("NumericOverflow"), "{error}");
}

#[test]
fn terminator_splits_and_byte_search_match() {
    let source = r#"
        fn main() {
            let matched = "A.B.".split_terminator(".") == ["A", "B"]
                and "A.B".split_terminator(".") == ["A", "B"]
                and "".split_terminator(".").len() == 0
                and "A.B.".rsplit_terminator(".") == ["B", "A"]
                and "A..B..".split_terminator(".") == ["A", "", "B", ""]
                and "\u00e9\u00e9".byte_find("\u00e9") == 0
                and "\u00e9\u00e9".byte_rfind("\u00e9") == 2
                and "\u00e9\u00e9".rfind("\u00e9") == 1
                and "ab".byte_find("") == 0
                and "ab".byte_rfind("") == 2
                and "ab".byte_find("c") == null
                and "ab".byte_count("") == 3
                and "\u00e9".byte_count("") == 3
                and "\u00e9".count("") == 2
                and f64_min_10_exp() == -307
                and f64_max_10_exp() == 308
                and char_replacement() == '\uFFFD'
                and char_max().to_code() == 1114111
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn an_empty_terminator_is_rejected() {
    let source = r#"fn main() { "A".split_terminator("") }"#;
    let error = common::run_value(source).unwrap_err();
    assert!(
        error.contains("split separator must not be empty"),
        "{error}"
    );
}

#[test]
fn the_new_operations_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { true.gcd(1) }"#,
        r#"fn main() { 1.gcd((1).as(i128)) }"#,
        r#"fn main() { true.byte_find("a") }"#,
        r#"fn main() { true.parse_u64() }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
