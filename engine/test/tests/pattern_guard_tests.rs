//! Pattern guards end before the block of an `if`, `while` or `for` condition.

mod common;

#[test]
fn guards_in_conditions_stop_before_the_body_block() {
    let source = r#"
        fn classify(v: i64) -> i64 { if v matches x if x > 2 { 10 } else { 1 } }
        fn main() {
            var n = 0
            while n matches x if x < 3 { n += 1 }
            var total = 0
            for x if x > 1 in [1, 2, 3] { total += x }
            let any: Any = 7
            let typed = if any matches t: i64 if t > 5 { 7 } else { 0 }
            if classify(3) + classify(0) == 11 and n == 3 and total == 5 and typed == 7 { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn match_arm_guards_still_accept_brace_construction() {
    let source = r#"
        struct P { a: i64 }
        fn main() { 1 match { x if P { a: x }.a == 1 => 42, _ => 0 } }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}
