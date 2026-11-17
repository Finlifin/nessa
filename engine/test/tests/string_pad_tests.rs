//! Scalar-width padding. Strings that are already long enough stay unchanged.

mod common;

#[test]
fn pad_cycles_the_fill_and_does_not_truncate() {
    let source = r#"
        fn main() {
            let matched = "é".pad_start(3, "x") == "xxé" and "é".pad_start(3, "x").char_len() == 3
                and "ab".pad_end(5, "xy") == "abxyx" and "ab".pad_start(5, "xy") == "xyxab"
                and "hello".pad_start(3, "x") == "hello" and "abc".pad_end(1, "z") == "abc"
                and "".pad_start(0, "") == "" and "héllo".pad_end(6, "·") == "héllo·"
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn bad_width_fill_and_oversized_results_fail() {
    for (source, message) in [
        (
            r#"fn main() { "a".pad_start(-1, "x"); 0 }"#,
            "pad width must not be negative",
        ),
        (
            r#"fn main() { "".pad_end(2, ""); 0 }"#,
            "pad fill must not be empty",
        ),
        (
            r#"fn main() { "x".pad_end(1073741825, "y"); 0 }"#,
            "ObjectTooLarge",
        ),
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains(message), "{source}: {error}");
    }
}
