mod common;

#[test]
fn prelude_imports_and_qualified_native_calls_execute_real_std_definitions() {
    for source in [
        "fn main() { str_len(to_string(123456)) + 36 }",
        "use std.io.to_i64 as integer\nfn main() { integer(42) }",
        "use std.math.{abs as positive, pow}\nfn main() { positive(-40) + pow(2, 1) }",
        "fn main() { std.io.to_i64(42) }",
        "fn main() { let t: std.builtin.Type = std.builtin.i64; if t == i64 { 42 } else { 0 } }",
        "mod api { pub use std.io.to_i64 as integer }\nuse api.integer\nfn main() { integer(42) }",
        "mod api { use @std.io.to_i64 as integer; pub fn result() -> i64 { integer(42) } }\nfn main() { api.result() }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn native_and_user_functions_are_values_with_checked_signatures() {
    for source in [
        "fn main() { let f = to_i64; f(42) }",
        "fn main() { let f = std.io.to_i64; f(42) }",
        "fn main() { let f: Any = to_i64; f(42) }",
        "fn main() { let f = to_i64; let g = || f(42); g() }",
        "fn apply(f: fn(Any) -> i64) -> i64 { f(42) }\nfn main() { apply(to_i64) }",
        "fn identity(x: i64) -> i64 { x }\nfn main() { let f = identity; f(42) }",
        "typealias Convert = fn(Any) -> i64\nfn apply(f: Convert) -> i64 { f(42) }\nfn main() { apply(to_i64) }",
        "typealias Number = Later\ntypealias Later = i64\nfn main() -> Number { 42 }",
        "fn apply(f: fn(i64) -> i64) -> i64 { f(42) }\nfn identity(x: i64) -> i64 { x }\nfn main() { apply(identity) }",
        "fn main() { let f = to_i64; if f'type == fn(Any) -> i64 { f(42) } else { 0 } }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
    let source = "fn apply(f: fn(i64) -> i64) -> i64 { f(42) }\nfn main() { let wrong: Any = |x: bool| 42; apply(wrong) }";
    assert!(common::run_value(source).unwrap_err().contains("TypeError"));
}

#[test]
fn local_definitions_shadow_prelude_and_only_the_root_main_is_the_entry() {
    for source in [
        "fn to_i64(x: i64) -> i64 { x + 40 }\nfn main() { to_i64(2) }",
        "mod hidden { pub fn main() { 99 } }\nfn main() { 42 }",
        "fn main() { 42 }\nmod hidden { pub fn main() { 99 } }",
        "mod api { pub fn second() -> i64 { first() }; fn first() -> i64 { 42 } }\nfn main() { api.second() }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
    let compiled = driver::Driver::new().compile("mod hidden { pub fn main() { 99 } }");
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    assert!(compiled.entry_func_id.is_none());
}

#[test]
fn bad_imports_native_arguments_and_forged_privilege_are_rejected() {
    for source in [
        "use missing.thing\nfn main() { 42 }",
        "use std.io.missing\nfn main() { 42 }",
        "use std.math.sin as f\nuse std.math.cos as f\nfn main() { 42 }",
        "fn main() { std.io.missing(42) }",
        "fn main() { sin(true) }",
        "fn main() { to_i64() }",
        "fn main() { to_i64(1, 2) }",
        "fn main() { .to_i64'builtin(42) }",
        "mod std { pub const fake = .to_i64'builtin }\nfn main() { fake(42) }",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "{source}");
        assert!(result.codegen_output.functions.is_empty(), "{source}");
    }
    let source = "fn main() { let x: Any = true; sin(x); 42 }";
    assert!(common::run_value(source).unwrap_err().contains("TypeError"));
}

#[test]
fn invalid_type_annotations_do_not_become_inferred_signatures() {
    for source in [
        "fn main() { let f: fn(42) -> i64 = to_i64; f(42) }",
        "fn main() { let f: fn(i64) -> 42 = to_i64; f(42) }",
        "fn main() { let x: (i64, 42) = (1, 2); 42 }",
        "fn main() { let x: 42 = 1; x }",
        "typealias Broken = fn(42) -> i64\nfn main() { 42 }",
        "typealias A = B\ntypealias B = A\nfn main() { 42 }",
        "fn main() { let x: !42 i64 = 1; x }",
        "fn main() { let x: #42 i64 = 1; x }",
        "fn bad(x: 42) { x }\nfn main() { bad(1) }",
        "fn bad() -> 42 { 1 }\nfn main() { bad() }",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "{source}");
        assert!(result.codegen_output.functions.is_empty(), "{source}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("valid type expression")),
            "{source}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn synchronous_and_asynchronous_effect_signatures_keep_distinct_type_identity() {
    let source = "fn main() { let sync = effect(Any) -> Unit; let asynchronous = async effect(Any) -> Unit; let sync_again = effect(Any) -> Unit; let async_again = async effect(Any) -> Unit; if sync != asynchronous and sync == sync_again and asynchronous == async_again { 42 } else { 0 } }";
    assert_eq!(common::run_value(source).unwrap(), 42);
}
