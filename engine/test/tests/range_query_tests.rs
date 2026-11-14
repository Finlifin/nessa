//! Native list edits and range length/membership.

mod common;

#[test]
fn range_queries_match_the_operator_bounds() {
    let source = r#"
        fn main() {
            let matched = (0..5).len() == 5
                and (5..5).len() == 0
                and (5..1).len() == 0
                and (5..5).is_empty()
                and (not (0..5).is_empty())
                and (0..5).contains(0)
                and (0..5).contains(4)
                and (not (0..5).contains(5))
                and (0..=4).len() == 5
                and (4..=4).len() == 1
                and (5..=4).len() == 0
                and (0..=4).contains(4)
                and (not (0..=4).contains(5))
                and (3..).contains(3)
                and (not (3..).contains(2))
                and (..4).contains(3)
                and (not (..4).contains(4))
                and (..=4).contains(4)
                and (not (..=4).contains(5))
                and (..).contains(0)
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn native_insert_take_and_first_keep_existing_semantics() {
    let source = r#"
        fn main() {
            let xs = ["a", "b", "c"]
            xs.insert(0, "z")
            xs.insert(xs.len().as(usize), "w")
            let head = xs.first()
            let tail = xs.last()
            let prefix = xs.take(2)
            let rest = xs.drop(2)
            prefix.set(0, "q")
            let empty = List()
            let matched = xs == ["z", "a", "b", "c", "w"]
                and head == "z" and tail == "w"
                and prefix == ["q", "a"] and rest == ["b", "c", "w"]
                and xs.take(9) == xs
                and xs.drop(9).len() == 0
                and empty.first() == null
                and empty.last() == null
                and empty.take(3).len() == 0
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn an_inclusive_span_that_does_not_fit_overflows() {
    let source = r#"fn main() { (i64_min()..=i64_max()).len() }"#;
    let error = common::run_value(source).unwrap_err();
    assert!(error.contains("NumericOverflow"), "{error}");
}

#[test]
fn the_new_operations_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { 1.insert(0, 2) }"#,
        r#"fn main() { true.take(1) }"#,
        r#"fn main() { (0..1).len("a") }"#,
        r#"fn main() { (0..1).contains("a") }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
