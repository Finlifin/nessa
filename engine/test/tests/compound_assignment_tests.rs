//! Compound assignment writes back to the place it read, evaluating each
//! receiver and index key once.

mod common;

fn assert_rejected(source: &str, expected: &str) {
    let result = driver::Driver::new().compile(source);
    assert!(result.has_errors, "{source}: expected rejection");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains(expected)),
        "{source}: {:?}",
        result.diagnostics
    );
    assert!(result.codegen_output.functions.is_empty(), "{source}");
}

#[test]
fn compound_assignment_updates_variables_fields_and_collection_slots() {
    for (source, expected) in [
        (
            "fn main() { var x = 40; x += 4; x -= 2; x *= 3; x /= 3; x %= 100; x }",
            42,
        ),
        ("var total: i64 = 40\nfn main() { total += 2; total }", 42),
        (
            "struct P { x: i64, y: i32 }\nfn main() { let p = P { x: 40, y: 3 }; p.x += 2; p.y *= 2; p.y -= 1; p.x * 10 + to_i64(p.y) }",
            425,
        ),
        (
            "struct In { v: i64 }\nstruct Out { inner: In }\nfn main() { let o = Out { inner: In { v: 40 } }; o.inner.v += 2; o.inner.v }",
            42,
        ),
        (
            "fn main() { let xs = [10, 20, 30]; xs(1) += 22; xs(0) -= 10; xs(1).as(i64) + xs(0).as(i64) }",
            42,
        ),
        (
            "fn main() { let m = Map(); m.set(\"a\", 40); m(\"a\") += 2; m(\"a\").as(i64) }",
            42,
        ),
    ] {
        assert_eq!(common::run_value(source).unwrap(), expected, "{source}");
    }
}

#[test]
fn receivers_and_keys_are_evaluated_once_before_the_operand() {
    for (source, expected) in [
        // Order digits: receiver (1), then operand (2).
        (
            "struct P { x: i64 }\nvar order: i64 = 0\nlet p = P { x: 0 }\nfn pick() -> P { order = order * 10 + 1; p }\nfn two() -> i64 { order = order * 10 + 2; 2 }\nfn main() { pick().x += two(); order * 10 + p.x }",
            122,
        ),
        // Order digits: receiver (1), key (2), then operand (3).
        (
            "var order: i64 = 0\nlet xs = [0, 0]\nfn list() -> List { order = order * 10 + 1; xs }\nfn key() -> usize { order = order * 10 + 2; 1 }\nfn three() -> i64 { order = order * 10 + 3; 3 }\nfn main() { list()(key()) += three(); order * 10 + xs(1).as(i64) }",
            1233,
        ),
    ] {
        assert_eq!(common::run_value(source).unwrap(), expected, "{source}");
    }
}

#[test]
fn compound_results_are_checked_against_the_target_type() {
    let overflow = "struct P { x: i8 }\nfn main() { let p = P { x: 127 }; p.x += 1; 0 }";
    assert!(
        common::run_value(overflow)
            .unwrap_err()
            .contains("NumericOverflow")
    );
    let dynamic = "fn main() { let xs = [\"a\"]; xs(0) += 1; 0 }";
    assert!(common::run_value(dynamic).is_err());
}

#[test]
fn unsupported_assignment_targets_are_compile_errors() {
    for (source, expected) in [
        (
            "fn main() { 1 += 2 }",
            "compound assignment target must be a variable, field, or index",
        ),
        (
            "fn main() { let a: Any = 1; a.x += 5 }",
            "compound assignment to `x` requires a statically known struct field",
        ),
        (
            "fn main() { let a: Any = 1; a.x = 5 }",
            "assignment to `x` requires a statically known struct field",
        ),
        (
            "fn main() { let xs: Any = [1]; xs(0) += 1 }",
            "requires a statically known List or Map receiver",
        ),
        (
            "struct V {}\nimpl V { pub fn apply(self, i: i64) -> i64 { i }; pub fn update(self, i: i64, v: i64) {} }\nfn main() { let v = V {}; v(1) += 2 }",
            "requires a statically known List or Map receiver",
        ),
        (
            "struct P { x: String }\nfn main() { let p = P { x: \"a\" }; p.x += 1 }",
            "compound assignment requires numeric operands",
        ),
        (
            "fn main() { let xs = [1]; xs(true) += 1 }",
            "List index must have type",
        ),
    ] {
        assert_rejected(source, expected);
    }
}
