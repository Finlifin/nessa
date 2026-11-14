//! Range operators build `std.range` values that `for` iterates by protocol.

mod common;

fn assert_source_and_archive(source: &str, expected: i64) {
    assert_eq!(common::run_value(source).unwrap(), expected, "{source}");
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let bytes = nsbc_io::write_artifact(&compiled.into_artifact().unwrap()).unwrap();
    let artifact = nsbc_io::read_artifact(&bytes).unwrap();
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .unwrap();
    let task = engine.vm_mut().spawn_root(entry);
    assert!(matches!(
        engine.vm_mut().run(),
        interpreter::VmResult::Finished
    ));
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), expected);
}

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
fn bounded_ranges_iterate_their_exact_elements() {
    for (source, expected) in [
        (
            "fn main() { var t = 0; for i in 0..4 { t = t * 10 + i }; t }",
            123,
        ),
        (
            "fn main() { var t = 0; for i in 1..=4 { t = t * 10 + i }; t }",
            1234,
        ),
        (
            "fn main() { var t = 0; for i in 5..1 { t += 1 }; for i in 3..3 { t += 1 }; for i in 5..=4 { t += 1 }; t }",
            0,
        ),
        ("fn main() { var t = 0; for i in 2..=2 { t += i }; t }", 2),
        (
            "fn main() { var t = 0; for i in -3..=-1 { t = t * 10 - i }; t }",
            321,
        ),
        // `..` binds below arithmetic; negation binds above it.
        (
            "fn main() { let n = 2; var t = 0; for i in -n..n + 1 { t += 1 }; t }",
            5,
        ),
        (
            "fn main() { let n: i32 = 3; let m: u8 = 5; var t = 0; for i in n..m { t += i }; t }",
            7,
        ),
        (
            "fn main() { var t = 0; for i in 9223372036854775806..=9223372036854775807 { t += 1 }; t }",
            2,
        ),
    ] {
        assert_source_and_archive(source, expected);
    }
}

#[test]
fn ranges_are_reusable_first_class_values() {
    for (source, expected) in [
        (
            "fn main() { let r = 1..4; var t = 0; for i in r { t += i }; for i in r { t += i * 10 }; t }",
            66,
        ),
        ("fn main() { let r = 2..=5; r.start * 10 + r.end }", 25),
        (
            "fn span() -> Any { 3..7 }\nfn main() { var t = 0; for i in span().as(std.range.Range) { t += i }; t }",
            18,
        ),
        (
            "use std.range.*\nfn total(r: Range) -> i64 { var t = 0; for i in r { t += i }; t }\nfn main() { total(0..5) }",
            10,
        ),
        (
            "use std.range.*\nfn main() { let r: RangeInclusive = 0..=2; let f: RangeFrom = 7..; let e: RangeTo = ..3; let i: RangeToInclusive = ..=3; let a: RangeFull = ..; r.end + f.start + e.end + i.end + 0 }",
            15,
        ),
        (
            "fn main() { var t = 0; for i in 7.. { if i == 10 { break }; t = t * 10 + i }; t }",
            789,
        ),
        (
            "fn main() { var t = 0; for i in 0..10 { if i % 2 == 0 { continue }; t += i }; t }",
            25,
        ),
        (
            "fn main() { var t = 0; for: outer i in 0..3 { for j in 0..3 { if i * j == 2 { break outer }; t += 1 } }; t }",
            5,
        ),
        (
            "fn main() { let fs = List(); for i in 0..3 { fs.push(|| i) }; var t = 0; for f in fs { t = t * 10 + f.as(fn() -> i64)() }; t }",
            12,
        ),
    ] {
        assert_source_and_archive(source, expected);
    }
}

#[test]
fn range_membership_follows_bound_inclusion() {
    let source = r#"
        fn main() {
            var bits = 0
            if (1..3).contains(1) { bits += 1 }
            if not (1..3).contains(3) { bits += 2 }
            if (1..=3).contains(3) { bits += 4 }
            if (5..).contains(9223372036854775807) { bits += 8 }
            if not (..5).contains(5) { bits += 16 }
            if (..=5).contains(5) { bits += 32 }
            if (..).contains(-9223372036854775807) { bits += 64 }
            if (3..3).is_empty() and not (3..=3).is_empty() and (4..=3).is_empty() { bits += 128 }
            bits
        }
    "#;
    assert_source_and_archive(source, 255);
}

#[test]
fn range_runtime_bounds_are_checked() {
    let overflow = "fn main() { var t = 0; for i in 9223372036854775806.. { t += 1 }; t }";
    assert!(
        common::run_value(overflow)
            .unwrap_err()
            .contains("NumericOverflow")
    );
    let dynamic = "fn main() { let x: Any = \"one\"; let r = x..2; r.end }";
    assert!(
        common::run_value(dynamic)
            .unwrap_err()
            .contains("TypeError")
    );
    assert_eq!(
        common::run_value("fn main() { let x: Any = 40; let r = x..2; r.start + r.end }").unwrap(),
        42
    );
}

#[test]
fn invalid_ranges_and_inputs_are_compile_errors() {
    for (source, expected) in [
        (
            "fn main() { let r = 0..true }",
            "range bound: expected `i64`",
        ),
        (
            "fn main() { let r = 0.5..2 }",
            "range bound: expected `i64`",
        ),
        (
            "fn main() { let x: i128 = 1; let r = x..2 }",
            "range bound: expected `i64`",
        ),
        (
            "fn main() { let r = ..\"z\" }",
            "range bound: expected `i64`",
        ),
        (
            "fn main() { for i in ..5 { } }",
            "for input `RangeTo` must implement",
        ),
        (
            "fn main() { for i in ..=5 { } }",
            "for input `RangeToInclusive` must implement",
        ),
        (
            "fn main() { for i in .. { } }",
            "for input `RangeFull` must implement",
        ),
        // A user type named Range never becomes the operator's result.
        (
            "struct Range { start: i64, end: i64 }\nfn main() { let mine: Range = 0..3 }",
            "type mismatch",
        ),
        (
            "fn main() { for i in 0..=3 { let s: String = i } }",
            "type mismatch",
        ),
    ] {
        assert_rejected(source, expected);
    }
}

#[test]
fn untyped_for_inputs_are_diagnosed_before_lowering() {
    assert_rejected(
        "fn main() { for i in undefined_name { } }",
        "for input has no statically known type",
    );
}
