mod common;

#[test]
fn all_default_variants_construct_with_an_empty_call() {
    for source in [
        "enum E { value(.answer: i64 = 42) }\nfn main() { E.value() match { E.value(n) => n } }",
        "enum E { value(.a: i64 = 40, .b: i64 = 2) }\nfn main() { E.value() match { E.value(a, b) => a + b } }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
    assert_rejected("enum E { value(.answer: i64 = 42) }\nfn main() { E.value }");
}

#[test]
fn explicit_arguments_run_in_source_order_before_declaration_order_defaults() {
    for (source, expected) in [
        (
            "global trace: i64 = 0\nfn step(n: i64) -> i64 { trace = trace * 10 + n; n }\nenum E { value(.a: i64 = step(1), .b: i64 = step(2), .c: i64 = step(3)) }\nfn main() { let value = E.value(c = step(9), a = step(8)); value match { E.value(a, b, c) => if a == 8 and b == 2 and c == 9 { trace } else { 0 } } }",
            982,
        ),
        (
            "global trace: i64 = 0\nfn step(n: i64) -> i64 { trace = trace * 10 + n; n }\nenum E { value(.a: i64 = step(1), .b: i64 = step(2), .c: i64 = step(3)) }\nfn main() { let value = E.value(c = step(9)); value match { E.value(a, b, c) => if a == 1 and b == 2 and c == 9 { trace } else { 0 } } }",
            912,
        ),
    ] {
        assert_eq!(common::run_value(source).unwrap(), expected, "{source}");
    }
}

#[test]
fn supplied_values_are_snapshots_and_their_defaults_are_skipped() {
    for (source, expected) in [
        (
            "enum E { value(a: i64, .b: i64 = a + 10, .c: i64 = 0) }\nfn main() { var x: i64 = 1; E.value(x, c = if true { x = 2; x } else { 0 }) match { E.value(a, b, c) => a * 100 + b * 10 + c } }",
            212,
        ),
        (
            "global calls: i64 = 0\nfn fallback() -> i64 { calls += 1; 0 }\nenum E { value(.answer: i64 = fallback()) }\nfn main() { E.value(answer = 42) match { E.value(n) => n + calls } }",
            42,
        ),
    ] {
        assert_eq!(common::run_value(source).unwrap(), expected, "{source}");
    }
}

#[test]
fn defaults_bind_prior_fields_in_the_declaration_namespace() {
    for source in [
        "enum E { value(a: i64, .b: i64 = a + 1, .c: i64 = b + 1) }\nfn main() { E.value(40) match { E.value(a, b, c) => c } }",
        "enum E { value(a: i64, .b: i64 = a + 2) }\ntypealias Alias = E\nfn main() { Alias.value(40) match { Alias.value(a, b) => b } }",
        "const base: i64 = 40\nenum E { value(.answer: i64 = base + 2) }\nfn main() { let base: i64 = 0; E.value() match { E.value(n) => n } }",
        "enum E { value(.answer: i64 = helper() + base), const base: i64 = 40; fn helper() -> i64 { 2 } }\nfn main() { let base: i64 = 0; let helper = || 0; E.value() match { E.value(n) => n } }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn each_variant_has_its_own_parameter_scope() {
    let source = "enum E { first(seed: i64, .answer: i64 = seed + 1), second(seed: i64, .answer: i64 = seed + 2) }\nfn main() { let a = E.first(19) match { E.first(seed, answer) => answer, _ => 0 }; let b = E.second(20) match { E.second(seed, answer) => answer, _ => 0 }; a + b }";
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn variadic_fields_store_one_list_and_defaults_observe_it() {
    for source in [
        "enum E { values(...items: List, .answer: i64 = items.len()) }\nfn main() { E.values() match { E.values(items, answer) => if items.len() == 0 and answer == 0 { 42 } else { 0 } } }",
        "enum E { values(...items: List, .answer: i64 = items.len()) }\nfn main() { E.values(40, 2) match { E.values(items, answer) => if answer == 2 { items(0) + items(1) } else { 0 } } }",
        "enum E { values(...items: List, .answer: i64 = items.len()) }\nfn main() { E.values(\"heap\", [42], null, ()) match { E.values(items, answer) => if answer == 4 and items(0) == \"heap\" and items(2) == null and items(3) == () { let nested: List = items(1); nested(0) } else { 0 } } }",
        "typealias Values = List\nenum E { values(...items: Values, .answer: i64 = 0) }\nfn main() { E.values(40, answer = 2) match { E.values(items, answer) => items(0) + answer } }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn default_closures_capture_prior_fields_after_construction_returns() {
    let source = "enum E { value(seed: i64, .answer: fn() -> i64 = || seed + 2) }\nfn make() -> E { E.value(40) }\nfn main() { let value = make(); value match { E.value(seed, answer) => answer() } }";
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn defaults_keep_local_loop_break_and_continue_targets() {
    let source = "enum E { value(.answer: i64 = if true { var n: i64 = 0; var total: i64 = 0; while true { n += 1; if n == 1 { continue }; total += n; if n == 9 { break } }; total - 2 } else { 0 }) }\nfn main() { E.value() match { E.value(n) => n } }";
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn unused_defaults_are_checked_and_cannot_read_self_or_later_fields() {
    for source in [
        "enum E { value(.answer: i64 = true) }\nfn main() { 42 }",
        "enum E { value(.answer: i64 = answer) }\nfn main() { 42 }",
        "enum E { value(.a: i64 = b, .b: i64 = 42) }\nfn main() { 42 }",
        "enum E { first(a: i64), second(.b: i64 = a) }\nfn main() { 42 }",
        "enum E { value(.answer: i64 = if true { return 42 } else { 0 }) }\nfn main() { E.value() }",
    ] {
        assert_rejected(source);
    }
}

#[test]
fn invalid_constructor_bindings_and_variadic_declarations_emit_no_artifact() {
    for source in [
        "enum E { value(...a: List, ...b: List) }\nfn main() { 42 }",
        "enum E { value(...items: List, answer: i64) }\nfn main() { 42 }",
        "enum E { value(...items: i64) }\nfn main() { 42 }",
        "enum E { value(...items: List) }\nfn main() { E.value(items = []) }",
        "enum E { value(.answer: i64 = 42) }\nfn main() { E.value(42) }",
        "enum E { value(.answer: i64 = 42) }\nfn main() { E.value(answer = 1, answer = 2) }",
        "enum E { value(.answer: i64 = 42) }\nfn main() { E.value(missing = 42) }",
        "enum E { value(.answer: i64 = 42) }\nfn main() { E.value(answer = true) }",
        "enum E { value(required: i64, .answer: i64 = 42) }\nfn main() { E.value() }",
    ] {
        assert_rejected(source);
    }
}

#[test]
fn numeric_defaults_preserve_exact_types_and_reject_overflow() {
    for (source, expected) in [
        (
            "enum E { value(.answer: i8 = -128) }\nfn main() { E.value() match { E.value(n) => if n'type == i8 { n } else { 0 } } }",
            runtime::Number::I64(-128),
        ),
        (
            "enum E { value(.answer: u128 = 340282366920938463463374607431768211455) }\nfn main() { E.value() match { E.value(n) => if n'type == u128 { n } else { 0 } } }",
            runtime::Number::U128(u128::MAX),
        ),
    ] {
        assert_eq!(common::run_number(source).unwrap(), expected, "{source}");
    }
    for source in [
        "enum E { value(.answer: i8 = 128) }\nfn main() { 42 }",
        "enum E { value(.answer: i8 = -129) }\nfn main() { 42 }",
        "enum E { value(.answer: u128 = -1) }\nfn main() { 42 }",
        "enum E { value(.answer: u128 = 340282366920938463463374607431768211456) }\nfn main() { 42 }",
    ] {
        assert_rejected(source);
    }
}

fn assert_rejected(source: &str) {
    let result = driver::Driver::new().compile(source);
    assert!(result.has_errors, "accepted {source}");
    assert!(result.codegen_output.functions.is_empty(), "{source}");
    assert!(
        result.into_artifact().is_err(),
        "produced artifact for {source}"
    );
}
