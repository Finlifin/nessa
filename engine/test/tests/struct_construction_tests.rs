mod common;

#[test]
fn struct_fields_bind_by_name_and_use_only_omitted_defaults() {
    for source in [
        "struct Pair { a: i64, b: i64 }\nfn main() { let p = Pair { b: 2, a: 4 }; p.a * 10 + p.b }",
        "struct Pair { a: i64, b: i64 = 2 }\nfn main() { let p = Pair { a: 4 }; p.a * 10 + p.b }",
        "struct Pair { a: i64, b: i64 = 0 }\nfn main() { let a: i64 = 4; let b: i64 = 2; let p = Pair { b, a }; p.a * 10 + p.b }",
        "struct Pair { a: i64, b: i64 }\ntypealias Alias = Pair\nfn main() { let p = Alias { b: 2, a: 4 }; p.a * 10 + p.b }",
        "fn main() { let p = Pair { a: 4 }; p.a * 10 + p.b }\nstruct Pair { a: i64, b: i64 = 2 }",
        "struct Pair { a: i8, b: u128 = 2 }\nfn main() { let p = Pair { a: 4 }; to_i64(p.a) * 10 + to_i64(p.b) }",
        "const base: i64 = 2\nstruct Pair { a: i64 = base }\nfn main() { let base: i64 = 99; let p = Pair {}; p.a + 40 }",
        "var calls: i64 = 0\nfn default() -> i64 { calls = calls + 1; 0 }\nstruct Pair { a: i64 = default() }\nfn main() { let p = Pair { a: 42 }; p.a + calls }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn struct_initializers_preserve_source_values_and_side_effect_order() {
    for (source, expected) in [
        (
            "struct Pair { a: Any, b: Any }\nfn main() { var x: i64 = 1; let p = Pair { a: x, b: if true { x = 2; x } else { 0 } }; p.a * 10 + p.b }",
            12,
        ),
        (
            "var order: i64 = 0\nfn step(x: i64) -> i64 { order = order * 10 + x; x }\nstruct Pair { a: i64, b: i64 }\nfn main() { let p = Pair { b: step(2), a: step(1) }; p.a * 10 + p.b + order }",
            33,
        ),
        (
            "var order: i64 = 0\nfn step(x: i64) -> i64 { order = order * 10 + x; x }\nstruct Pair { a: i64 = step(1), b: i64 = step(2), c: i64 }\nfn main() { let p = Pair { c: step(3) }; order }",
            312,
        ),
    ] {
        assert_eq!(common::run_value(source).unwrap(), expected, "{source}");
    }
}

#[test]
fn malformed_struct_constructions_and_defaults_are_rejected() {
    for source in [
        "struct Pair { a: i64 }\nfn main() { Pair { missing: 1 } }",
        "struct Pair { a: i64 }\nfn main() { Pair {} }",
        "struct Pair { a: i64 }\nfn main() { Pair { a: 1, a: 2 } }",
        "struct Pair { a: i64 }\nfn main() { Pair { a: true } }",
        "struct Pair { a: i8 }\nfn main() { Pair { a: 128 } }",
        "struct Pair { a: i64 = true }\nfn main() { 42 }",
        "struct Pair { a: i64 = a }\nfn main() { 42 }",
        "struct Pair { a: i64, b: i64 = a }\nfn main() { 42 }",
        "struct Pair { a: i64 }\nfn main() { Pair { 42 } }",
        "fn main() { i64 { value: 42 } }",
        "struct Pair { a: i64 = if true { return 42 } else { 0 } }\nfn main() { 42 }",
        "struct Recursive { child: Recursive = Recursive {} }\nfn main() { Recursive {} }",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "{source}");
        assert!(result.codegen_output.functions.is_empty(), "{source}");
    }
}

#[test]
fn gradual_struct_field_boundaries_check_values_at_runtime() {
    let source =
        "struct Pair { a: i64 }\nfn main() { let x: Any = true; let p = Pair { a: x }; p.a }";
    assert!(common::run_value(source).unwrap_err().contains("TypeError"));
}

#[test]
fn instance_methods_read_their_bound_receiver_and_same_type_private_fields() {
    assert_eq!(common::run_value(
        "struct Point { private x: i64 }\nimpl Point { pub fn new(x: i64) -> Point { Point { x } }; pub fn value(self) -> i64 { self.x } }\nfn main() { Point(42).value() }",
    ).unwrap(), 42);
}

#[test]
fn type_alias_uses_initialize_both_alias_and_underlying_type_modules() {
    for construction in ["Alias { x: 0 }", "Alias()"] {
        let source = format!(
            "global count: i64 = 0\nstruct Point {{ x: i64 }}\nimpl Point {{ pub fn new() -> Point {{ Point {{ x: 0 }} }} }}\nmod alias {{ pub typealias Alias = Point; fn __init__() {{ count = 42 }} }}\nuse alias.Alias\nfn main() -> i64 {{ let p = {construction}; count }}"
        );
        assert_eq!(common::run_value(&source).unwrap(), 42, "{source}");
    }
}
