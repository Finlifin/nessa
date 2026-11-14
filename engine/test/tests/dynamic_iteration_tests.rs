//! `for` over an `Any` input selects IntoIterator, or else Iterator, from the
//! runtime type and binds `Any` items.

mod common;

fn assert_source_and_archive(source: &str, expected: i64) {
    assert_eq!(common::run_value(source).unwrap(), expected, "{source}");
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    assert!(
        artifact
            .builtins
            .iter()
            .any(|import| import.id == runtime::ids::ITERATION_STEP_DONE)
    );
    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
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

const COUNTER: &str = "struct Counter { cursor: i64, end: i64 }\nimpl Iterator for Counter {\n    assoc Item: Type = i64\n    pub fn next(self) -> IterationStep(i64) {\n        if self.cursor < self.end { let v = self.cursor; self.cursor = v + 1; IterationStep(i64).yielded(v) } else { IterationStep(i64).done }\n    }\n}\n";

#[test]
fn any_inputs_dispatch_on_their_runtime_type() {
    for (source, expected) in [
        ("fn main() { let xs: Any = [1, 2, 3]; var t = 0; for x in xs { t = t * 10 + x.as(i64) }; t }".to_owned(), 123),
        ("fn main() { let r: Any = 1..=3; var t = 0; for x in r { t = t * 10 + x.as(i64) }; t }".to_owned(), 123),
        ("fn count(v: Any) -> i64 { var t = 0; for x in v { t += 1 }; t }\nfn main() { let m = Map(); m.set(\"a\", 1); m.set(\"b\", 2); count(m) * 100 + count([7, 8, 9]) * 10 + count(0..0) }".to_owned(), 230),
        // A bare Iterator is used directly when it has no IntoIterator implementation.
        (format!("{COUNTER}fn main() {{ let c: Any = Counter {{ cursor: 1, end: 4 }}; var t = 0; for x in c {{ t = t * 10 + x.as(i64) }}; t }}"), 123),
        // Yielded null and Unit are elements; only the done tag stops the loop.
        ("fn main() { let xs: Any = [null, (), 3]; var n = 0; for x in xs { n += 1 }; n }".to_owned(), 3),
        ("fn main() { let xs: Any = [[1, 2], [3]]; var t = 0; for inner in xs { for x in inner { t += x.as(i64) } }; t }".to_owned(), 6),
        ("fn main() { let xs: Any = 0..; var t = 0; for x in xs { if x == 5 { break }; if x == 1 { continue }; t += x.as(i64) }; t }".to_owned(), 9),
    ] {
        assert_source_and_archive(&source, expected);
    }
}

#[test]
fn any_inputs_without_an_iterator_protocol_are_runtime_type_errors() {
    for value in ["42", "null", "\"text\"", "..3", "(1, 2)"] {
        let source = format!("fn main() {{ let x: Any = {value}; for i in x {{ }}; 0 }}");
        assert!(
            common::run_value(&source)
                .unwrap_err()
                .contains("TypeError"),
            "{source}"
        );
    }
}

#[test]
fn any_items_still_require_statically_known_tuple_inputs_for_tuple_patterns() {
    let result =
        driver::Driver::new().compile("fn main() { let m: Any = Map(); for (k, v) in m { } }");
    assert!(result.has_errors);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("tuple pattern requires"))
    );
}
