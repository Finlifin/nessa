//! `name: Type` patterns test the runtime type of Any and trait-typed inputs.

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

#[test]
fn match_arms_select_by_runtime_type_and_bind_the_converted_value() {
    let source = r#"
        struct D { a: i64 }
        derive Display for D
        struct P { a: i64 }
        fn code(v: Any) -> i64 {
            v match {
                p: P => 100 + p.a,
                n: i64 if n < 0 => 1,
                n: i64 => 2 + n,
                t: (i64, String) => t.0 + t.1.len(),
                shown: Display => shown.to_string().len(),
                _ => 0,
            }
        }
        fn main() {
            let first = code(P { a: 7 }) == 107 and code(-5) == 1 and code(5) == 7
            let second = code((1, "ab")) == 3 and code(D { a: 1 }) == "D {{ a: 1 }}".len()
            if first and second and code([1]) == 0 and code(1.5) == 3 { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn matches_for_alternatives_and_aliases_use_typed_bindings() {
    let source = r#"
        fn main() {
            let v: Any = 40
            let items: List = [1, "skip", 2, 3.5, -1]
            var total = 0
            for n: i64 in items { total += n }
            let alias = v match { (n: i64 or n: i64) as whole => n + whole.as(i64), _ => 0 }
            let real: Any = 2.5
            let wildcard = real match { _: f64 => 1, _ => 0 }
            if (v matches n: i64) and total == 2 and alias == 80 { 41 + wildcard } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn trait_typed_inputs_can_be_tested_against_concrete_types() {
    let source = r#"
        struct D { a: i64 }
        derive Display for D
        struct E { b: i64 }
        derive Display for E
        fn pick(shown: Display) -> i64 { shown match { d: D => d.a, e: E => e.b * 2, _ => 0 } }
        fn main() { pick(D { a: 2 }) + pick(E { b: 20 }) }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn trait_tests_respect_the_scope_of_the_pattern() {
    let source = r#"
        struct P { a: i64 }
        trait Score { fn score(self) -> i64 }
        mod inside {
            extend Score for P { pub fn score(self) -> i64 { 40 } }
            pub fn classify(v: Any) -> i64 { v match { s: Score => s.score(), _ => 0 } }
        }
        fn outside(v: Any) -> i64 { v match { s: Score => 1000, _ => 2 } }
        fn main() { inside.classify(P { a: 0 }) + outside(P { a: 0 }) }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn statically_known_inputs_are_rejected() {
    for source in [
        "fn main() { 3 match { n: i64 => n, _ => 0 } }",
        "struct P { a: i64 }\nfn main() { P { a: 1 } match { p: P => p.a, _ => 0 } }",
        "fn main() { let v: ?i64 = 1; v match { n: i64 => n, _ => 0 } }",
    ] {
        assert_rejected(source, "typed patterns test Any or trait-typed inputs");
    }
}

#[test]
fn typed_patterns_require_a_valid_type_and_consistent_alternatives() {
    assert_rejected(
        "fn main() { let v: Any = 1; v match { n: missing => 1, _ => 0 } }",
        "undefined name `missing`",
    );
    assert_rejected(
        "fn main() { let v: Any = 1; v match { n: i64 or n: String => 1, _ => 0 } }",
        "same type",
    );
}

#[test]
fn nested_list_payload_and_tuple_elements_test_their_any_values() {
    let source = r#"
        enum Slot { full(v: Any), empty }
        fn list(v: Any) -> i64 { v match { [n: i64, s: String, ...rest] => n + s.len() + rest.len(), _ => 0 } }
        fn main() {
            let payload = Slot.full(30) match { Slot.full(n: i64) => n, _ => 0 }
            let pair: (Any, i64) = ("ab", 1)
            let tuple = pair match { (s: String, n) => s.len() + n, _ => 0 }
            if list([5, "x", 1]) == 7 and list([1, 2]) == 0 { payload + tuple + 9 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn declarations_keep_reading_colon_as_their_own_annotation() {
    assert_rejected("fn main() { let (a: i64, b) = (1, 2); 0 }", "Expected `)`");
}
