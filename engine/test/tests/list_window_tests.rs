//! List windows copy elements. `zip` stops at the shorter side.

mod common;

#[test]
fn take_drop_and_zip_copy_without_changing_the_source() {
    let source = r#"
        fn main() {
            let xs = ["a", "b", "c"]
            let head = xs.take(2)
            let tail = xs.drop(1)
            let none = xs.drop(9)
            let all = xs.take(9)
            let empty = xs.take(0)
            head.set(0, "z")
            tail.set(0, "y")
            let pairs = [1, 2].zip([10, 20, 30])
            let first = pairs(0).as((i64, i64))
            let second = pairs(1).as((i64, i64))
            let matched = head.join(",") == "z,b" and tail.join(",") == "y,c"
                and xs.join(",") == "a,b,c"
                and none.join(",") == "" and all.join(",") == "a,b,c" and empty.len() == 0
                and pairs.len() == 2 and first.0 == 1 and first.1 == 10 and second.0 == 2 and second.1 == 20
                and [1].zip([]).len() == 0 and [].zip([1]).len() == 0
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn a_negative_window_count_is_rejected() {
    let compiled = driver::Driver::new().compile("fn main() { [1].take(-1) }");
    assert!(compiled.has_errors, "negative take should be rejected");
    assert!(
        compiled
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("out of range for `usize`")),
        "{:?}",
        compiled.diagnostics
    );
}
