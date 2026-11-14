//! Ceiling division, directed multiples, endian bytes, and total float order.

mod common;

#[test]
fn ceiling_division_and_multiples_match() {
    let source = r#"
        fn main() {
            let min = -9223372036854775808
            let max = 9223372036854775807
            let matched = 8.div_ceil(3) == 3
                and 8.div_ceil(-3) == -2
                and (-8).div_ceil(3) == -2
                and (-8).div_ceil(-3) == 3
                and 8.div_ceil(4) == 2
                and (7).as(u64).div_ceil((4).as(u64)) == 2
                and (8).as(i128).div_ceil((3).as(i128)) == (3).as(i128)
                and min.checked_div_ceil(-1) == null
                and 1.checked_div_ceil(0) == null
                and 23.next_multiple_of(8) == 24
                and 16.next_multiple_of(8) == 16
                and 23.next_multiple_of(-8) == 16
                and (-23).next_multiple_of(8) == -16
                and (-23).next_multiple_of(-8) == -24
                and 5.next_multiple_of(-1) == 5
                and (23).as(u64).next_multiple_of((8).as(u64)) == 24
                and max.checked_next_multiple_of(2) == null
                and 1.checked_next_multiple_of(0) == null
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn a_zero_divisor_or_multiple_is_rejected() {
    for (source, message) in [
        (r#"fn main() { 1.div_ceil(0) }"#, "divisor must not be zero"),
        (
            r#"fn main() { 1.next_multiple_of(0) }"#,
            "multiple must not be zero",
        ),
        (
            r#"fn main() { (-9223372036854775808).div_ceil(-1) }"#,
            "NumericOverflow",
        ),
        (
            r#"fn main() { 9223372036854775807.next_multiple_of(2) }"#,
            "NumericOverflow",
        ),
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains(message), "{source}: {error}");
    }
}

#[test]
fn endian_bytes_round_trip() {
    let source = r#"
        fn main() {
            let min = -9223372036854775808
            let high: u64 = 9223372036854775808
            let matched = (1).to_le_bytes().len() == 8
                and (1).to_le_bytes().get(0) == 1
                and (1).to_be_bytes().get(7) == 1
                and min.to_be_bytes().get(0) == 128
                and i64_from_le_bytes((1).to_le_bytes()) == 1
                and i64_from_be_bytes(min.to_be_bytes()) == min
                and u64_from_le_bytes(high.to_le_bytes()) == high
                and (1).as(i128).to_be_bytes().len() == 16
                and i128_from_be_bytes((1).as(i128).to_be_bytes()) == (1).as(i128)
                and u128_from_le_bytes((1).as(u128).to_le_bytes()) == (1).as(u128)
                and isize_from_le_bytes((1).as(isize).to_le_bytes()) == (1).as(isize)
                and usize_from_be_bytes((1).as(usize).to_be_bytes()) == (1).as(usize)
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn bad_integer_bytes_are_rejected() {
    for (source, message) in [
        (
            r#"fn main() { i64_from_le_bytes([1]) }"#,
            "integer byte length does not match",
        ),
        (
            r#"fn main() { i64_from_le_bytes([256, 0, 0, 0, 0, 0, 0, 0]) }"#,
            "integer byte is out of range",
        ),
        (
            r#"fn main() { i64_from_be_bytes([-1, 0, 0, 0, 0, 0, 0, 0]) }"#,
            "integer byte is out of range",
        ),
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains(message), "{source}: {error}");
    }
}

#[test]
fn total_order_places_signed_zero_and_nan() {
    let source = r#"
        fn main() {
            let negative_zero = copysign(0.0, -1.0)
            let matched = (1.0).total_cmp(2.0) == -1
                and (1.0).total_cmp(1.0) == 0
                and (2.0).total_cmp(1.0) == 1
                and negative_zero.total_cmp(0.0) == -1
                and (0.0).total_cmp(negative_zero) == 1
                and negative_zero == 0.0
                and nan().total_cmp(infinity()) == 1
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn the_new_operations_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { true.div_ceil(1) }"#,
        r#"fn main() { 1.div_ceil((1).as(i128)) }"#,
        r#"fn main() { true.to_le_bytes() }"#,
        r#"fn main() { true.total_cmp(1.0) }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
