//! `"text {expr}"` lowers to String concatenation of each part's Display text.

mod common;

fn compile_errors(source: &str) -> Vec<String> {
    let compiled = driver::Driver::new().compile(source);
    assert!(compiled.has_errors, "{source} should be rejected");
    compiled
        .diagnostics
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

fn assert_rejected(source: &str, message: &str) {
    let errors = compile_errors(source);
    assert!(
        errors.iter().any(|error| error.contains(message)),
        "{source}: {errors:?}"
    );
}

#[test]
fn interpolated_parts_use_their_display_text() {
    let source = r#"
        use std.ordering.*
        struct Named { a: i64 }
        impl Display for Named { pub fn to_string(self) -> String { "named" } }
        struct Derived { a: i64 }
        derive Display for Derived
        fn main() {
            let x = 3
            let name = "bob"
            let text = "hi {name}, {x + 1} {[1, 2].len()} {1.5} {true} {'c'} {()} {Ordering.less} {Named { a: 1 }} x}}"
            let derived = "<{Derived { a: 7 }}>"
            let expected = "hi bob, 4 2 1.5 true c () Ordering.less named x}}"
            if text == expected and derived.starts_with("<Derived ") and derived.ends_with(" a: 7 }}>") { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn braces_escapes_and_nested_strings_decode_in_every_segment() {
    let source = r#"
        fn wrap(text: String) -> String { "[{text}]" }
        fn main() {
            let x = 1
            let text = "a\t{x}|\u0041|{{x}}|{{{x}}}|}}|{wrap("{x}")}|{"in{x + 1}"}"
            let plain = "{{}}\n"
            if text == "a\t1|A|{{x}}|{{1}}|}}|[1]|in2" and plain.len() == 3 { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn values_without_static_display_are_rejected() {
    for value in [
        r#"(1, "a")"#,
        "[1, 2]",
        "Map()",
        "1..3",
        "P { a: 1 }",
        "null",
    ] {
        let source = format!(
            "use std.collections.*\nstruct P {{ a: i64 }}\nfn main() {{ let v = {value}; \"<{{v}}>\" }}"
        );
        assert_rejected(&source, "does not implement expected trait type `Display`");
    }
}

#[test]
fn any_values_check_display_at_runtime() {
    let source = r#"
        struct D { a: i64 }
        derive Display for D
        fn main() {
            let number: Any = 5
            let derived: Any = D { a: 2 }
            let text = "{number} {derived}"
            if text.starts_with("5 D ") { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);

    for value in [r#"(1, "a")"#, "P { a: 1 }", "[1]"] {
        let source = format!(
            "struct P {{ a: i64 }}\nfn main() {{ let v: Any = {value}; \"<{{v}}>\".len() }}"
        );
        let error = common::run_value(&source).unwrap_err();
        assert!(error.contains("TypeError"), "{value}: {error}");
    }
}

#[test]
fn empty_interpolation_is_a_parse_error() {
    for source in [
        "fn main() { \"{}\" }",
        "fn main() { \"a{}b\" }",
        "fn main() { \"{1}{}\" }",
    ] {
        assert_rejected(source, "requires an expression");
    }
}

#[test]
fn lone_braces_are_rejected_in_every_string() {
    assert_rejected("fn main() { \"a}b\" }", "must be written `}}`");
    assert_rejected("fn main() { \"{1} }\" }", "must be written `}}`");
}
