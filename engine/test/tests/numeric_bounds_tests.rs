//! `min`, `max` and `clamp` keep an operand's numeric kind.

mod common;

#[test]
fn bounds_select_an_operand_without_rounding_mixed_numbers() {
    let source = r#"
        fn main() {
            let nan = sqrt(-1.0)
            let lower = clamp(5, 0.0, 3)
            let matched = min(3, 1) == 1 and max(3, 1) == 3 and min(-2, 5) == -2
                and max(1, 1.5) == 1.5
                and type_of(min(1, 1.0)) == i64 and type_of(min(1.0, 1)) == f64
                and clamp(5, 1, 3) == 3 and clamp(-1, 0, 3.5) == 0 and clamp(2, 2, 2) == 2
                and lower == 3 and type_of(lower) == i64
                and min(nan, 1.0) != 1.0 and min(1.0, nan) == 1.0
                and max(nan, 1.0) != 1.0 and max(1.0, nan) == 1.0
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn non_numbers_and_reversed_bounds_fail() {
    for (source, message) in [
        ("fn main() { min(\"a\", \"b\"); 0 }", "TypeError"),
        ("fn main() { max(true, false); 0 }", "TypeError"),
        (
            "fn main() { clamp(1, 3, 0); 0 }",
            "clamp bounds are out of order",
        ),
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains(message), "{source}: {error}");
    }
}
