mod common;

#[test]
fn module_values_are_initialized_before_main_and_shared_across_functions() {
    for source in [
        "const value: i64 = 42\nfn main() -> i64 { value }",
        "global value: i8 = 40\nfn main() -> i8 { value += 2; value }",
        "global value: i64 = 50\nfn main() -> i64 { value -= 8; value *= 2; value /= 2; value %= 100; value }",
        "fn main() -> i64 { value }\nglobal value: i64 = 42",
        "fn read() -> i64 { value }\nlet value: i64 = 42\nfn main() -> i64 { read() }",
        "global count: i64 = 1\nfn __init__() { count = 41; }\nfn main() -> i64 { count + 1 }",
        "global count: i64 = 1\nfn add() { count += 1; }\nfn main() -> i64 { add(); count = count + 40; count }",
        "mod api { pub const value: i64 = 42 }\nuse api.value as answer\nfn main() -> i64 { answer }",
        "mod api { pub let value: i64 = 1; fn __init__() { value = 41; } }\nfn main() -> i64 { api.value = api.value + 1; api.value }",
        "const f = identity\nfn identity(x) { x }\nfn main() { f(42) }",
        "const f = identity\nfn identity(x: i64) { x }\nfn main() { f(42) }",
        "mod api { pub const f = |x: i64| x + 1 }\nfn main() { api.f(41) }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn initialization_orders_dependencies_including_called_helpers() {
    for source in [
        "mod a { pub const value: i64 = b.seed; }\nmod b { pub const seed: i64 = 42; }\nfn main() -> i64 { a.value }",
        "mod a { const read = || b.seed; pub const value: i64 = read(); }\nmod b { pub const seed: i64 = 42; }\nfn main() -> i64 { a.value }",
        "mod a { pub const value: i64 = read(); fn read() -> i64 { b.seed } }\nmod b { pub const seed: i64 = 42; }\nfn main() -> i64 { a.value }",
        "global count: i64 = 0\nmod a { fn __init__() { count = b.read() + 1; }; pub fn read() -> i64 { count } }\nmod b { global value: i64 = 40; fn __init__() { value = value + 1; }; pub fn read() -> i64 { value } }\nfn main() -> i64 { a.read() }",
        "global count: i64 = 0\nmod used { fn __init__() { count = count + 1; }; pub fn read() -> i64 { count } }\nuse used.read as first\nuse used.read as second\nfn main() -> i64 { first() + second() + 40 }",
        "global count: i64 = 0\nmod a { fn __init__() { count = count * 10 + 2; }; pub fn read() -> i64 { count }; fn dependency() { b.read() } }\nmod b { fn __init__() { count = count * 10 + 4; }; pub fn read() -> i64 { count } }\nfn main() -> i64 { a.read() }",
        "mod unused { fn __init__() { panic(\"must stay unloaded\"); } }\nfn main() -> i64 { 42 }",
        "mod a { use @b.second; pub fn first() -> i64 { second() } }\nmod b { use @a.first; pub fn second() -> i64 { 42 } }\nfn main() -> i64 { a.first() }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn closures_read_shared_globals_instead_of_capturing_old_values() {
    for source in [
        "global count: i64 = 0\nfn main() -> i64 { let delta = 42; let f = || { count = delta; }; f(); count }",
        "global count: i64 = 1\nfn main() -> i64 { let delta = 2; let f = || count + delta; count = 40; f() }",
        "mod api { pub global count: i64 = 1; pub const read = || count; }\nuse api.count as counter\nfn main() -> i64 { counter = 42; api.read() }",
        "global read: fn() -> i64 = || later\nglobal later: i64 = 42\nfn main() -> i64 { read() }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn uninitialized_values_and_failed_initializers_never_reach_main() {
    for source in [
        "const first: i64 = second\nconst second: i64 = 42\nfn main() -> i64 { first }",
        "global first: i64 = read()\nfn read() -> i64 { later }\nglobal later: i64 = 42\nfn main() -> i64 { first }",
    ] {
        assert!(
            common::run_value(source)
                .unwrap_err()
                .contains("UninitializedGlobal"),
            "{source}"
        );
    }
    assert!(
        common::run_value("global value: i8 = 127\nfn main() { value += 1; value }")
            .unwrap_err()
            .contains("NumericOverflow")
    );
    let source = "fn __init__() { panic(\"init failed\"); }\nfn main() { panic(\"main ran\"); }";
    let result = driver::Driver::new().run(source);
    assert!(
        matches!(result, driver::RunResult::RuntimeError(ref error) if error.contains("init failed") && !error.contains("main ran")),
        "{result:?}"
    );
}

#[test]
fn cyclic_runtime_initialization_and_invalid_init_signatures_are_diagnosed() {
    for source in [
        "mod a { pub const value: i64 = b.seed; }\nmod b { pub const seed: i64 = a.value; }\nfn main() -> i64 { a.value }",
        "fn __init__(x: i64) {}\nfn main() { 42 }",
        "fn __init__() -> i64 { 42 }\nfn main() { 42 }",
        "fn __init__() { 42 }\nfn main() { 42 }",
        "fn main() { global local: i64 = 42; local }",
        "fn main() { let value = 42; struct State { global captured: i64 = value; }; State.captured }",
        "fn main() { let outside: i64 = 42; fn make() -> i64 { outside }; struct Holder { pub const value: Any = make() }; Holder.value }",
        "fn main() { let outside: i64 = 42; fn read() -> i64 { outside }; fn make() -> i64 { read() }; struct Holder { pub const value: Any = make() }; Holder.value }",
        "if true { return 42; }\nfn main() { 42 }",
        "fn main() { let target = 42; while true { break target; } }",
        "break\nfn main() { 42 }",
        "fn main() { while true { continue if 1; } }",
        "global value: bool = true\nfn main() { value += false; 42 }",
        "const value = 1\nfn main() { value = 42; value }",
        "mod api { pub const value = 1 }\nfn main() { api.value += 41; api.value }",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "{source}");
        assert!(result.codegen_output.functions.is_empty(), "{source}");
    }
}

#[test]
fn archives_reject_global_schema_until_metadata_can_preserve_it() {
    let result =
        driver::Driver::new().compile("global value: i64 = 42\nfn main() -> i64 { value }");
    assert!(!result.has_errors, "{:?}", result.diagnostics);
    let error = nsbc_io::write_archive(&result.codegen_output).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
}

#[test]
fn executable_top_level_expressions_and_loop_control_preserve_init_order() {
    for source in [
        "global count: i64 = 0\ncount = count + 1\nfn __init__() { count += 40; }\nfn main() -> i64 { count + 1 }",
        "global count: i64 = 0\nwhile true { count += 1; break; }\nfn __init__() { count += 40; }\nfn main() -> i64 { count + 1 }",
        "global count: i64 = 0\nwhile count < 10 { count += 1; continue if count < 2; break; }\nfn main() -> i64 { count + 40 }",
        "global count: i64 = 0\nwhile :outer true { while true { count += 1; break outer if count == 2; break; }; }\nfn main() -> i64 { count + 40 }",
        "let identity = |x| x\nlet value: i64 = identity(42)\nfn main() -> i64 { value }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn associated_type_state_and_initializers_are_loaded_when_referenced() {
    for source in [
        "struct State { global value: i64 = 1; fn __init__() { value += 40; }; pub fn read() -> i64 { value } }\nfn main() -> i64 { State.read() + 1 }",
        "struct State { pub const value: i64 = 42; }\nfn main() -> i64 { State.value }",
        "struct State { pub const value: i64 = 42; }\ntypealias Alias = State\nfn main() -> i64 { Alias.value }",
        "struct State { pub fn read() -> i64 { value }; global value: i64 = 42; }\nfn main() -> i64 { State.read() }",
        "fn make(x: i64) -> i64 { let offset = 2; x + offset }\nstruct State { pub const value: i64 = make(40); }\nfn main() -> i64 { State.value }",
        "global count: i64 = 0\nstruct State { fn __init__() { count = 42; } }\nfn main() -> i64 { let t = State; count }",
        "struct Unused { fn __init__() { panic(\"unused type\"); } }\nfn main() -> i64 { 42 }",
        "enum State { ready, global value: i64 = 1; fn __init__() { value += 41; }; pub fn read() -> i64 { value } }\nfn main() -> i64 { State.read() }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn statically_unreachable_reads_do_not_create_initialization_cycles() {
    let source = "mod a { pub const value: i64 = 42; fn __init__() { while false { b.touch(); }; } }\nmod b { pub const value: i64 = a.value; pub fn touch() {} }\nfn main() -> i64 { b.value }";
    assert_eq!(common::run_value(source).unwrap(), 42);
    for expression in [
        "false and b.value",
        "true or b.value",
        "if false { b.value } else { false }",
    ] {
        let source = format!(
            "mod a {{ pub const value: bool = {expression}; }}\nmod b {{ pub const value: bool = a.value; }}\nfn main() -> i64 {{ if a.value == b.value {{ 42 }} else {{ 0 }} }}"
        );
        assert_eq!(common::run_value(&source).unwrap(), 42, "{source}");
    }
}
