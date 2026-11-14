//! Bit positions, bit widths, and non-overlapping match indexes.

mod common;

#[test]
fn bit_positions_and_widths_match() {
    let source = r#"
        fn main() {
            let matched = (0b10100).highest_one() == 4
                and (0b10100).lowest_one() == 2
                and 0.highest_one() == null
                and 0.lowest_one() == null
                and (0b10100).isolate_highest_one() == 16
                and (0b10100).isolate_lowest_one() == 4
                and 0.isolate_highest_one() == 0
                and (-1).highest_one() == 63
                and (-1).lowest_one() == 0
                and (-1).isolate_highest_one() == i64_min()
                and (-1).isolate_lowest_one() == 1
                and (-2).lowest_one() == 1
                and (0b111).bit_width() == 3
                and 0.bit_width() == 0
                and (-1).bit_width() == 64
                and (1).as(u128).bit_width() == 1
                and i128_min().highest_one() == 127
                and i128_min().bit_width() == 128
                and i64_bits() == 64
                and i128_bits() == 128
                and u128_bits() == 128
                and isize_bits() == 64
                and usize_bits() == 64
                and (1).as(usize).bit_width() == 1
                and char_min().to_code() == 0
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn match_indexes_match() {
    let source = r#"
        fn main() {
            let matched = "aaa".find_indices("aa") == [0]
                and "aaa".rfind_indices("aa") == [1]
                and "aaaa".find_indices("aa") == [0, 2]
                and "aaaa".rfind_indices("aa") == [2, 0]
                and "ab".find_indices("") == [0, 1, 2]
                and "ab".rfind_indices("") == [2, 1, 0]
                and "\u00e9".find_indices("") == [0, 1]
                and "\u00e9".byte_find_indices("") == [0, 2]
                and "\u00e9".byte_rfind_indices("") == [2, 0]
                and "\u00e9\u00e9".rfind_indices("\u00e9") == [1, 0]
                and "\u00e9\u00e9".byte_rfind_indices("\u00e9") == [2, 0]
                and "".find_indices("").len() == 1
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn the_new_operations_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { true.highest_one() }"#,
        r#"fn main() { true.bit_width() }"#,
        r#"fn main() { true.find_indices("a") }"#,
        r#"fn main() { (1.0).isolate_lowest_one() }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
