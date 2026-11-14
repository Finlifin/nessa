//! `unique` keeps the first equal element. `merge` overwrites keys from a snapshot.

mod common;

#[test]
fn unique_and_merge_keep_identity_and_stored_null() {
    let source = r#"
        fn main() {
            let xs = [1, 1.0, 2, 1, "a", "a"]
            let ys = xs.unique()
            let left = Map()
            left("a") = 1
            left("c") = 3
            let right = Map()
            right("a") = 2
            right("b") = null
            left.merge(right)
            left.merge(left)
            let matched = ys == [1, 2, "a"] and xs == [1, 1.0, 2, 1, "a", "a"]
                and [].unique().len() == 0
                and left("a") == 2 and left.contains("b") and left("b") == null and left("c") == 3
                and left.len() == 3 and right("a") == 2
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn unique_rejects_values_without_equality() {
    let source = r#"
        struct P { x: i64 }
        fn main() { [P { x: 1 }, P { x: 1 }].unique(); 0 }
    "#;
    let error = common::run_value(source).unwrap_err();
    assert!(error.contains("TypeError"), "{error}");
}
