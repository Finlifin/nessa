mod common;

#[test]
fn optional_and_named_arguments_bind_without_overwriting_supplied_values() {
    for source in [
        "fn value(.x: i64 = 42) -> i64 { x }\nfn main() { value() }",
        "fn main() { (|.x: i64 = 42| x)() }",
        "fn value(.x: i64 = if true { while true { break }; 42 } else { 0 }) -> i64 { x }\nfn main() { value() }",
        "fn value(.f: fn() -> i64 = || { return 42 }) -> i64 { f() }\nfn main() { value() }",
        "const base: i64 = 40\nfn value(.x: i64 = base + 2) -> i64 { x }\nfn main() { let base: i64 = 0; value() }",
        "fn value(.x: i64 = 7) -> i64 { x }\nfn main() { value(x = 42) }",
        "fn combine(a: i64, b: i64) -> i64 { a * 10 + b }\nfn main() { combine(b = 2, a = 4) }",
        "fn value(a: i64, .b: i64 = a + 2) -> i64 { b }\nfn main() { value(40) }",
        "fn value(a: i64, .b: i64 = a + 1, .c: i64 = b + 1) -> i64 { c }\nfn main() { value(40) }",
        "var calls: i64 = 0\nfn default() -> i64 { calls = calls + 1; 7 }\nfn value(.x: i64 = default()) -> i64 { x }\nfn main() { value(x = 42) + calls }",
        "fn value(a: i64, .b: i64 = a + 1) -> i64 { if a == 0 { b } else { value(a - 1) + a } }\nfn main() { value(8) + 5 }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn calls_preserve_callee_and_source_argument_evaluation_order() {
    for (source, expected) in [
        (
            "var order: i64 = 0\nfn factory() -> fn(i64) -> i64 { order = order * 10 + 1; |x: i64| order }\nfn arg() -> i64 { order = order * 10 + 2; 0 }\nfn main() { factory()(arg()) }",
            12,
        ),
        (
            "fn pair(a, b) { a * 10 + b }\nfn main() { var x: i64 = 1; pair(x, if true { x = 2; x } else { 0 }) }",
            12,
        ),
        (
            "var order: i64 = 0\nfn step(x: i64) -> i64 { order = order * 10 + x; x }\nfn pair(a: i64, b: i64) -> i64 { a * 10 + b }\nfn main() { pair(b = step(2), a = step(1)) + order }",
            33,
        ),
        (
            "fn first(x: i64) -> i64 { 1 }\nfn second(x: i64) -> i64 { 2 }\nfn main() { var f: fn(i64) -> i64 = first; f(if true { f = second; 0 } else { 0 }) }",
            1,
        ),
        (
            "fn main() { var x: i64 = 1; x + if true { x = 2; x } else { 0 } }",
            3,
        ),
    ] {
        assert_eq!(common::run_value(source).unwrap(), expected, "{source}");
    }
}

#[test]
fn malformed_argument_bindings_and_defaults_are_compile_errors() {
    for source in [
        "fn value(.x: i64 = 0) { x }\nfn main() { value(42) }",
        "fn value(a: i64) { a }\nfn main() { value(a = 1, a = 2) }",
        "fn value(a: i64) { a }\nfn main() { value(1, a = 2) }",
        "fn value(a: i64) { a }\nfn main() { value(missing = 1) }",
        "fn value(a: i64) { a }\nfn main() { value() }",
        "fn value(.a: i64 = true) { a }\nfn main() { value() }",
        "fn unused(.a: i64 = true) { a }\nfn main() { 42 }",
        "fn value(.a: i64 = value()) { a }\nfn main() { 42 }",
        "fn value(.a: i64 = if true { return 42 } else { 0 }) -> i64 { a }\nfn main() { value() + 1 }",
        "fn value(.a: i64 = b, .b: i64 = 42) { a }\nfn main() { value() }",
        "fn value(.a: i64 = a) { a }\nfn main() { value() }",
        "fn value(.a: i64 = 0) { a }\nfn main() { value(a = true) }",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "{source}");
        assert!(result.codegen_output.functions.is_empty(), "{source}");
    }
}
