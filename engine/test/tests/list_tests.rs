mod common;

#[test]
fn list_literals_empty_constructors_nested_values_and_type_identity_execute() {
    for source in [
        "fn main() { let xs = [40, 2]; xs(0) + xs(1) }",
        "fn main() { let xs = List(); xs.push(42); xs.get(0) }",
        "fn main() { let xs: Any = [0]; xs(0) = 42; xs(0) }",
        "fn main() { let xs: Any = [42]; let index: Any = 0; xs(index) }",
        "fn main() { let xs = [40, \"hello\", [2]]; let inner: List = xs(2); xs(0) + inner(0) }",
        "fn main() { let xs = []; if xs'type == List { xs.push(42); xs.pop() } else { 0 } }",
        "typealias Values = List\nfn main() { let xs: Values = [42]; xs.get(0) }",
        "struct Holder { items: List = [] }\nconst holder = Holder {}\nfn main() { holder.items.push(42); holder.items(0) }",
        "fn value(.items: List = [42]) -> i64 { items(0) }\nfn main() { value() }",
        "fn main() { let xs = [null, ()]; if xs.len() == 2 { 42 } else { 0 } }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn list_updates_growth_and_pop_preserve_shared_identity() {
    for source in [
        "fn main() { let xs = [0]; let alias = xs; alias(0) = 42; xs(0) }",
        "fn main() { let xs = [0]; xs.set(0, 42); xs.get(0) }",
        "fn main() { let xs = List(); var n: i64 = 0; while n < 100 { xs.push(n); n += 1 }; xs.set(42, xs.pop()); xs.len() - xs.get(42) + 42 }",
        "fn main() { let xs = []; if xs.pop() == null { 42 } else { 0 } }",
        "fn main() { let xs = [()]; if xs.pop() == () and xs.pop() == null { 42 } else { 0 } }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn list_construction_and_index_updates_preserve_source_evaluation_order() {
    for (source, expected) in [
        (
            "fn main() { var x: i64 = 1; let xs = [x, if true { x = 2; x } else { 0 }]; xs(0) * 10 + xs(1) }",
            12,
        ),
        (
            "global order: i64 = 0\nfn receiver() -> List { order = order * 10 + 1; [0] }\nfn index() -> usize { order = order * 10 + 2; 0 }\nfn value() -> i64 { order = order * 10 + 3; 42 }\nfn main() { receiver()(index()) = value(); order }",
            123,
        ),
    ] {
        assert_eq!(common::run_value(source).unwrap(), expected, "{source}");
    }
}

#[test]
fn single_list_variadic_parameters_collect_all_extra_arguments() {
    for source in [
        "fn collect(...args: List) -> i64 { args(0) + args(1) }\nfn main() { collect(40, 2) }",
        "fn collect(prefix: i64, ...args: List) -> i64 { prefix + args(0) + args(1) }\nfn main() { collect(1, 40, 1) }",
        "fn collect(...args: List) -> i64 { args.len() + 42 }\nfn main() { collect() }",
        "fn collect(...args: List, .extra: i64 = 2) -> i64 { args(0) + extra }\nfn main() { collect(40) }",
        "typealias Values = List\nfn collect(...args: Values) -> i64 { args(0) }\nfn main() { collect(42) }",
        "fn collect(...args: List, .answer: Any = args(0)) -> Any { answer }\nfn main() { collect(42) }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn list_runtime_rejects_invalid_indices_without_mutating_values() {
    for source in [
        "fn main() { let xs = []; xs(0) }",
        "fn main() { let xs = [42]; xs(1) }",
        "fn main() { let xs = [42]; let index: Any = -1; xs(index) }",
        "fn main() { let xs = [42]; let index: Any = 0.0; xs(index) }",
        "fn main() { let xs = [42]; let index: Any = true; xs(index) }",
        "fn main() { let xs = [42]; xs.set(2, 0) }",
    ] {
        assert!(common::run_value(source).is_err(), "{source}");
    }
}

#[test]
fn unsupported_or_malformed_collection_operations_are_compile_errors() {
    for source in [
        "fn main() { let xs = [42]; xs() }",
        "fn main() { let xs = [42]; xs(0, 1) }",
        "fn main() { let xs = [42]; xs(true) }",
        "fn main() { List { len: 0, capacity: 0, buffer: null } }",
        "impl List { fn fake() {} }\nfn main() { 42 }",
        "fn main() { let xs = [42]; xs.buffer }",
        "fn main() { let xs = [42]; let f = xs.len; f() }",
        "fn collect(...args: List) {}\nfn main() { collect(args = []) }",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "{source}");
        assert!(result.codegen_output.functions.is_empty(), "{source}");
    }
}
