//! Map snapshots, conditional inserts, float class, and ASCII span checks.

mod common;

#[test]
fn values_follow_keys_and_conditional_inserts_keep_stored_null() {
    let source = r#"
        fn main() {
            let map = Map()
            let inserted = map.insert_absent("a", 1)
            let again = map.insert_absent("a", 9)
            let first = map.get_or_insert("a", 2)
            let second = map.get_or_insert("b", 3)
            map.set("n", null)
            let kept = map.get_or_insert("n", 8)
            let absent_null = map.insert_absent("n", 8)
            let keys = map.keys()
            let values = map.values()
            let paired = keys.len() == values.len()
                and map.get(keys.get(0).as(String)) == values.get(0)
                and map.get(keys.get(1).as(String)) == values.get(1)
                and map.get(keys.get(2).as(String)) == values.get(2)
            let matched = inserted and (not again) and first == 1 and second == 3
                and kept == null and map.get("n") == null and (not absent_null)
                and map.get("a") == 1 and paired
            if matched { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn clear_and_merge_keep_identity() {
    let source = r#"
        fn main() {
            let left = Map()
            left.set("x", 1)
            left.set("y", 2)
            let right = Map()
            right.set("y", 9)
            right.set("z", 4)
            left.merge(right)
            let merged = left.len() == 3 and left.get("x") == 1 and left.get("y") == 9
                and left.get("z") == 4
            left.merge(left)
            let stable = left.len() == 3 and left.get("y") == 9
            left.clear()
            let cleared = left.len() == 0 and left.is_empty() and (not left.contains("x"))
                and left.get("z") == null
            left.set("w", 5)
            let reused = left.len() == 1 and left.get("w") == 5
            if merged and stable and cleared and reused { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn float_class_and_ascii_spans_match() {
    let source = r#"
        fn main() {
            let classes = 1.0.classify() == "normal"
                and classify(0.0) == "zero"
                and copysign(0.0, -1.0).classify() == "zero"
                and infinity().classify() == "infinite"
                and neg_infinity().classify() == "infinite"
                and nan().classify() == "nan"
                and min_positive().next_down().classify() == "subnormal"
                and golden_ratio() > 1.618 and golden_ratio() < 1.619
                and euler_gamma() > 0.577 and euler_gamma() < 0.578
            let ascii = "".is_ascii_digit() and "012".is_ascii_digit() and (not "12a".is_ascii_digit())
                and "Ab".is_ascii_alphabetic() and (not "A1".is_ascii_alphabetic())
                and (not "\u00E9".is_ascii_alphabetic())
                and "A1".is_ascii_alphanumeric() and (not "A ".is_ascii_alphanumeric())
                and "ab".is_ascii_lowercase() and (not "Ab".is_ascii_lowercase())
                and "AB".is_ascii_uppercase() and (not "Ab".is_ascii_uppercase())
                and " \t\n".is_ascii_whitespace() and (not "a ".is_ascii_whitespace())
                and "aF09".is_ascii_hexdigit() and (not "ag".is_ascii_hexdigit())
            if classes and ascii { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn the_new_operations_reject_the_wrong_receiver() {
    for source in [
        r#"fn main() { 1.get_or_insert("a", 1) }"#,
        r#"fn main() { true.is_ascii_digit() }"#,
        r#"fn main() { classify("a") }"#,
        r#"fn main() { "a".classify() }"#,
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
    }
}
