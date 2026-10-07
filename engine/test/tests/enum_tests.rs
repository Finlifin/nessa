mod common;

#[test]
fn enum_values_preserve_nominal_identity_and_reflected_types() {
    for source in [
        "enum A { one, two }\nfn main() { if A.one != A.two and A.one == A.one { 42 } else { 0 } }",
        "enum A { one }\nenum B { one }\nfn main() { if A.one != B.one and A.one'type == A and B.one'type == B { 42 } else { 0 } }",
        "enum E { some(value: i64), none }\nfn main() { if E.some(42)'type == E and E.none'type == E { 42 } else { 0 } }",
        "enum E { none }\ntypealias Alias = E\nfn main() { if Alias.none == E.none { 42 } else { 0 } }",
        "global count: i64 = 0\nenum E { none, fn __init__() { count = 42 } }\nfn main() { let value = E.none; count }",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn enum_construction_and_recursive_patterns_extract_actual_payloads() {
    for source in [
        "enum E { some(value: i64), none }\nfn main() { E.some(42) match { E.none => 0, E.some(n) => n } }",
        "enum E { some(value: i64), none }\nconst shared = E.some(42)\nfn main() { shared match { E.some(n) => n, E.none => 0 } }",
        "enum E { some(value: i64), none, pub fn value(self) -> i64 { self match { E.some(n) => n, E.none => 0 } } }\nfn main() { E.some(42).value() }",
        "enum E { pair(a: i64, b: i64), none }\nfn main() { E.pair(b = 2, a = 40) match { E.pair(a, b) => a + b, _ => 0 } }",
        "enum E { node(value: (i64, i64)), none }\nfn main() { E.node((40, 2)) match { E.node((a, b)) => a + b, _ => 0 } }",
        "enum Tree { leaf(value: i64), node(left: Tree, right: Tree) }\nfn main() { Tree.node(Tree.leaf(40), Tree.leaf(2)) match { Tree.node(Tree.leaf(a), Tree.leaf(b)) => a + b, _ => 0 } }",
        "fn main() { (40, 2) match { (0, _) => 0, (a, b) => a + b } }",
        "enum E { some(value: i8), none }\nfn main() { let narrow: i8 = 42; let x: Any = narrow; E.some(x) match { E.some(n) => n, _ => 0 } }",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
}

#[test]
fn enum_patterns_check_all_arms_guards_and_dynamic_values() {
    for source in [
        "enum E { some(value: i64), none }\nfn main() { E.some(42) match { E.some(0) => 0, E.some(n) if n > 40 => n, _ => 0 } }",
        "enum E { some(value: i64), none }\nfn main() { if E.some(42) matches E.some(42) { 42 } else { 0 } }",
        "enum E { some(value: i64), none }\nfn main() { if E.none matches E.some(_) { 0 } else { 42 } }",
        "enum E { some(value: i64), none }\nfn main() { let x: Any = E.some(42); x match { E.none => 0, E.some(n) => n, _ => 0 } }",
        "enum A { one }\nenum B { one }\nfn main() { let x: Any = B.one; x match { A.one => 0, B.one => 42, _ => 0 } }",
        "fn main() { 42 match { 0 => 0, n => n } }",
        "fn main() { (-3.5) match { -3.5 => 42, _ => 0 } }",
        "fn main() { let x: Any = null; x match { () => 0, null => 42, _ => 0 } }",
        "fn main() { var x: i64 = 42; x match { _ if if true { x = 0; false } else { false } => 0, 42 => 42, _ => 0 } }",
        "enum E { some(value: i64), none }\nfn main() { E.some(42) match { E.none => { return 0 }, E.some(n) => { return n } } }",
    ] {
        assert_eq!(
            common::run_value(source).unwrap_or_else(|error| panic!("{source}: {error}")),
            42,
            "{source}"
        );
    }
    for source in [
        "enum E { some(value: i64), none }\nfn main() { E.none match { E.some(_) => 42 } }",
        "fn main() { 42 match { 0 => 0, 1 => 42 } }",
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains("NoMatchingCase"), "{source}: {error}");
    }
}

#[test]
fn enum_fields_evaluate_in_source_order_and_freeze_previous_values() {
    for (source, expected) in [
        (
            "global count: i64 = 0\nenum E { none, fn __init__() { count = count * 10 + 1 } }\nmod alias { pub typealias Alias = E; fn __init__() { count = count * 10 + 2 } }\nuse alias.Alias\nfn main() { let value = Alias.none; count }",
            12,
        ),
        (
            "enum E { pair(a: i64, b: i64) }\nfn main() { var x: i64 = 1; E.pair(x, if true { x = 2; x } else { 0 }) match { E.pair(a, b) => a * 10 + b } }",
            12,
        ),
        (
            "global count: i64 = 0\nfn next() -> i64 { count = count + 1; count }\nenum E { pair(a: i64, b: i64) }\nfn main() { E.pair(b = next(), a = next()) match { E.pair(a, b) => a * 10 + b } }",
            21,
        ),
    ] {
        assert_eq!(common::run_value(source).unwrap(), expected, "{source}");
    }
}

#[test]
fn invalid_enum_construction_and_patterns_are_rejected() {
    for source in [
        "enum E { some(value: i64), none }\nfn main() { E.some(true) }",
        "enum E { some(value: i64), none }\nfn main() { E.some() }",
        "enum E { some(value: i64), none }\nfn main() { E.some(1, 2) }",
        "enum E { some(value: i64), none }\nfn main() { let f = E.some; f(42) }",
        "enum E { some(value: i64), none }\nfn main() { E.some(value = 1, value = 2) }",
        "enum E { some(value: i64), none }\nfn main() { E.some(42) match { E.some(a, b) => a } }",
        "enum A { one }\nenum B { one }\nfn main() { A.one match { B.one => 42 } }",
        "enum E { some(value: i64), none }\nfn main() { E.some(42) match { E.some(n) => n }; n }",
        "enum E { private hidden, visible }\nfn main() { E.hidden }",
        "enum E { some(value: i64), none }\nfn main() { E.none matches E.some(n); n }",
        "enum E { some(value: i64), none }\nfn main() { false and (E.some(42) matches E.some(n)); n }",
        "fn value() -> i64 { false match { true => 42, _ => false } }\nfn main() { value() }",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "unexpectedly compiled: {source}");
        assert!(result.into_artifact().is_err(), "{source}");
    }
}

#[test]
fn enum_runtime_boundaries_and_unsupported_payload_equality_report_errors() {
    for (source, expected) in [
        (
            "enum E { some(value: i64), none }\nfn main() { let x: Any = true; E.some(x) match { E.some(n) => n, _ => 0 } }",
            "TypeError",
        ),
        (
            "enum E { some(value: i8), none }\nfn main() { let x: Any = 42; E.some(x) match { E.some(n) => n, _ => 0 } }",
            "TypeError",
        ),
        (
            "enum E { some(value: i64), none }\nfn main() { if E.some(42) == E.some(42) { 42 } else { 0 } }",
            "UnsupportedEnumEquality",
        ),
    ] {
        let error = common::run_value(source).unwrap_err();
        assert!(error.contains(expected), "{source}: {error}");
    }
}

#[test]
fn match_arms_share_a_checked_numeric_result_representation() {
    assert_eq!(
        common::run_number("fn main() { true match { true => 42, _ => 0.5 } }").unwrap(),
        runtime::Number::F64(42.0),
    );
    assert_eq!(
        common::run_number("fn main() { false match { true => 42, _ => 0.5 } }").unwrap(),
        runtime::Number::F64(0.5),
    );
}
