//! Structural `==` on lists and tuples, dynamic `==` on Any values, and the
//! basic List query helpers.

mod common;

fn assert_rejected(source: &str, message: &str) {
    let compiled = driver::Driver::new().compile(source);
    assert!(compiled.has_errors, "{source} should be rejected");
    assert!(
        compiled
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains(message)),
        "{source}: {:?}",
        compiled.diagnostics
    );
}

fn assert_type_error(source: &str) {
    let error = common::run_value(source).unwrap_err();
    assert!(error.contains("TypeError"), "{error}");
}

/// Each check contributes a distinct bit, so a wrong result names the check.
fn bits(checks: &[&str]) -> String {
    checks
        .iter()
        .enumerate()
        .map(|(index, check)| format!("(if {check} {{ {} }} else {{ 0 }})", 1_i64 << index))
        .collect::<Vec<_>>()
        .join(" + ")
}

fn all_bits(count: usize) -> i64 {
    (1_i64 << count) - 1
}

#[test]
fn lists_and_tuples_compare_element_wise() {
    let checks = [
        "[1, 2] == [1, 2]",
        "[1, 2] != [1, 3]",
        "[1, 2] != [1, 2, 3]",
        "[[1], [2, 3]] == [[1], [2, 3]]",
        "[1] == [1.0]",
        "[(1, \"a\")] == [(1, \"a\")]",
        "(1, \"a\") == (1, \"a\")",
        "(1, (2, 3)) != (1, (2, 4))",
        "(1, 2.5) == (1.0, 2.5)",
        "(1, [2]) != (1, [3])",
        "not ([] != [])",
    ];
    let source = format!("fn main() {{ {} }}", bits(&checks));
    assert_eq!(common::run_value(&source).unwrap(), all_bits(checks.len()));
}

#[test]
fn any_values_compare_by_runtime_type() {
    let source = r#"
        struct P { x: i64 }
        derive Eq for P
        struct Q { x: i64 }
        extend PartialEq for Q {
            fn eq(self, other: Self) -> bool { self.x == other.x }
        }
        fn main() {
            let one: Any = 1
            let real: Any = 1.0
            let p: Any = P { x: 1 }
            let q: Any = Q { x: 2 }
            let text: Any = "x"
            let tuple: Any = (1, [2])
            let list: Any = [1]
            var bits = 0
            if one == real { bits += 1 }
            if (p == P { x: 1 }) { bits += 2 }
            if (p != P { x: 2 }) { bits += 4 }
            if p != text { bits += 8 }
            if (q == Q { x: 2 }) { bits += 16 }
            if tuple == (1, [2]) { bits += 32 }
            if tuple != (1, [2], 3) { bits += 64 }
            if list == [1] and list != one { bits += 128 }
            bits
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 255);
}

#[test]
fn any_equality_uses_the_implementation_visible_at_the_comparison() {
    let source = r#"
        struct Q { x: i64 }
        mod inner {
            extend PartialEq for Q {
                fn eq(self, other: Self) -> bool { true }
            }
            pub fn compare(a: Any, b: Any) -> bool { a == b }
        }
        fn main() { if inner.compare(Q { x: 1 }, Q { x: 2 }) { 1 } else { 0 } }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 1);
}

#[test]
fn same_type_values_without_equality_raise_type_errors() {
    assert_type_error(
        r#"
        struct Q { x: i64 }
        fn main() {
            let q: Any = Q { x: 1 }
            if (q == Q { x: 1 }) { 1 } else { 0 }
        }
    "#,
    );
    assert_type_error(
        r#"
        struct Q { x: i64 }
        fn main() { if ([Q { x: 1 }] == [Q { x: 1 }]) { 1 } else { 0 } }
    "#,
    );
}

#[test]
fn tuples_of_different_shapes_are_rejected() {
    for source in [
        "fn main() { (1, 2) == (1, 2, 3) }",
        "fn main() { (1, \"a\") != (1, 2) }",
        "fn main() { (1, (2, \"a\")) == (1, (2, 3)) }",
    ] {
        assert_rejected(source, "cannot be compared: arity or element types differ");
    }
}

#[test]
fn maps_compare_keys_and_values_independent_of_order() {
    let source = r#"
        fn main() {
            let left = Map()
            let right = Map()
            left("x") = 1
            left("y") = [2]
            right("y") = [2]
            right("x") = 1.0
            let shorter = Map()
            shorter("x") = 1
            let different = Map()
            different("x") = 1
            different("y") = [3]
            let nested = Map()
            nested("inner") = left
            let nested_other = Map()
            nested_other("inner") = right
            let dynamic: Any = left
            var bits = 0
            if left == right { bits += 1 }
            if left != shorter { bits += 2 }
            if left != different { bits += 4 }
            if nested == nested_other { bits += 8 }
            if dynamic == right { bits += 16 }
            if Map().is_empty() and not left.is_empty() { bits += 32 }
            let stored = Map()
            stored("n") = null
            if stored != Map() and stored == stored { bits += 64 }
            bits
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 127);
}

#[test]
fn list_ends_and_string_join() {
    let checks = [
        "[1, 2, 3].first() == 1",
        "[1, 2, 3].last() == 3",
        "[].first() == null and [].last() == null",
        "[\"a\", \"b\", \"c\"].join(\"-\") == \"a-b-c\"",
        "[\"only\"].join(\",\") == \"only\"",
        "[].join(\"-\") == \"\"",
        "[\"a\", \"b\"].join(\"\") == \"ab\"",
    ];
    let source = format!("fn main() {{ {} }}", bits(&checks));
    assert_eq!(common::run_value(&source).unwrap(), all_bits(checks.len()));
    assert_type_error("fn main() { if [1, 2].join(\",\") == \"1,2\" { 1 } else { 0 } }");
}

#[test]
fn list_helpers_query_and_transform_lists() {
    let checks = [
        "[3, 1, 2].contains(1)",
        "not [3, 1, 2].contains(9)",
        "[3, 1, 2].index_of(2) == 2",
        "[3, 1, 2].index_of(7) == null",
        "[3, 1, 2].reverse() == [2, 1, 3]",
        "[3, 1].enumerate() == [(0, 3), (1, 1)]",
        "[3, 1, 2].any(|v: Any| v == 2)",
        "not [3, 1, 2].all(|v: Any| v == 2)",
        "[].all(|v: Any| false)",
        "not [].any(|v: Any| true)",
        "[].is_empty() and not [1].is_empty()",
        "[[1], (2, \"b\")].contains((2, \"b\"))",
    ];
    let source = format!("fn main() {{ {} }}", bits(&checks));
    assert_eq!(common::run_value(&source).unwrap(), all_bits(checks.len()));
}
