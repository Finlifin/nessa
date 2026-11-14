//! `assert` and `assert_eq` panic through the existing process builtin.

mod common;

fn assert_panic(source: &str, message: &str) {
    let error = common::run_value(source).unwrap_err();
    assert!(error.contains(message), "{source}: {error}");
}

#[test]
fn passing_assertions_continue_and_named_messages_are_evaluated() {
    let source = r#"
        global seen: i64 = 0
        fn mark() -> String { seen = 1; "used" }
        fn main() {
            assert(true)
            assert(1 == 1, message = mark())
            assert_eq([1, 2], [1, 2])
            assert_eq((1, "a"), (1, "a"))
            assert_eq(1, 1.0, message = "numbers")
            if seen == 1 { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn failing_assertions_panic_with_the_default_or_named_message() {
    assert_panic(
        "fn main() { assert(false); 0 }",
        "Panic(\"assertion failed\")",
    );
    assert_panic(
        "fn main() { assert(1 == 2, message = \"need a value\"); 0 }",
        "Panic(\"need a value\")",
    );
    assert_panic(
        "fn main() { assert_eq([1], [2]); 0 }",
        "Panic(\"values are not equal\")",
    );
    assert_panic(
        "fn main() { assert_eq(1, 2, message = 7); 0 }",
        "Panic(\"7\")",
    );
}

#[test]
fn assertion_calls_reject_bad_arguments() {
    for (source, message) in [
        (
            "fn main() { assert(false, \"x\") }",
            "optional parameters must be supplied by name",
        ),
        ("fn main() { assert(1) }", "expected `bool`, found `i64`"),
        (
            "fn main() { let f = assert; f(true) }",
            "expects 2 arguments, but 1 was supplied",
        ),
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source}");
        assert!(
            compiled
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(message)),
            "{source}: {:?}",
            compiled.diagnostics
        );
    }
}
