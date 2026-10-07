mod common;

#[test]
fn tuple_construction_projection_and_nested_bindings_execute() {
    for source in [
        "fn main() { let t = (40, 2); t.0 + t.1 }",
        "fn main() { let (a, b) = (40, 2); a + b }",
        "fn main() { let (a, (b, _)) = (40, (2, true)); a + b }",
        "fn main() { let t = ((40, 2), true); t.0.0 + t.0.1 }",
        "typealias Pair = (i64, i64)\nfn answer() -> Pair { (40, 2) }\nfn main() { let t: Pair = answer(); t.0 + t.1 }",
        "fn sum((a, b): (i64, i64)) -> i64 { a + b }\nfn main() { sum((40, 2)) }",
        "fn sum((a, b): (i64, i64), (c, d): (i64, i64)) -> i64 { a + b + c + d }\nfn main() { sum((30, 10), (1, 1)) }",
        "fn main() { let sum = |(a, b): (i64, i64)| a + b; sum((40, 2)) }",
        "fn main() { let t = (42,); t.0 }",
        "fn main() { let t = (40, 0); let alias = t; alias.1 = 2; t.0 + t.1 }",
        "fn change(t: (i64, i64)) { t.1 = 2 }\nfn main() { let t = (40, 0); change(t); t.0 + t.1 }",
        "struct Holder { value: i64 }\nfn main() { let h = Holder { value: 0 }; h.value = 42; h.value }",
        "fn main() { if ()'type == Unit { 42 } else { 0 } }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn tuples_evaluate_once_in_source_order_and_snapshot_mutable_elements() {
    for (source, expected) in [
        (
            "global count: i64 = 0\nfn bump() -> i64 { count = count + 1; count }\nfn main() { let (a, b) = (bump(), bump()); count * 100 + a * 10 + b }",
            212,
        ),
        (
            "fn main() { var x: i64 = 1; let t = (x, if true { x = 2; x } else { 0 }); t.0 * 10 + t.1 }",
            12,
        ),
        ("fn main() { var (a, b) = (40, 1); b = 2; a + b }", 42),
        (
            "global order: i64 = 0\nconst shared = (0, true)\nfn receiver() -> (i64, bool) { order = order * 10 + 1; shared }\nfn value() -> i64 { order = order * 10 + 2; 42 }\nfn main() { receiver().0 = value(); order }",
            12,
        ),
    ] {
        assert_eq!(common::run_value(source).unwrap(), expected, "{source}");
    }
}

#[test]
fn tuple_expected_types_and_gradual_elements_are_checked() {
    for source in [
        "fn main() { let t: (i8, i64) = (40, 2); t.0 + t.1 }",
        "fn main() { let a: Any = 40; let b: Any = 2; let t: (i64, i64) = (a, b); t.0 + t.1 }",
        "fn main() { let a: Any = 40; let raw: Any = (a, 2); let t: (i64, i64) = raw; t.0 + t.1 }",
        "fn sum(t: (i64, i64)) -> i64 { t.0 + t.1 }\nfn main() { let raw: Any = (40, 2); sum(raw) }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
    for source in [
        "fn main() { let raw: Any = (true, 2); let t: (i64, i64) = raw; t.0 + t.1 }",
        "fn main() { let raw: Any = (40, 2, 3); let t: (i64, i64) = raw; t.0 + t.1 }",
    ] {
        assert!(common::run_value(source).is_err(), "{source}");
    }
}

#[test]
fn invalid_tuple_shapes_projections_and_element_types_are_diagnosed() {
    for source in [
        "fn main() { let t = (1, 2); t.2 }",
        "fn main() { let t = (1, 2); t.999999999999999999999999999999 }",
        "fn main() { let t = 42; t.0 }",
        "fn main() { let (a, b) = (1, 2, 3); a + b }",
        "fn main() { let (a, b) = 42; a + b }",
        "fn main() { let raw: Any = (40, 2); let (a, b) = raw; a + b }",
        "fn main() { let t: (i64, bool) = (1, 2); t.0 }",
        "fn main() { let t: (i64, i64) = (1, 2, 3); t.0 }",
        "fn main() { let t = (1, true); t.1 = 2; t.0 }",
    ] {
        let output = driver::Driver::new().compile(source);
        assert!(output.has_errors, "unexpectedly compiled: {source}");
        assert!(output.into_artifact().is_err(), "{source}");
    }
}
