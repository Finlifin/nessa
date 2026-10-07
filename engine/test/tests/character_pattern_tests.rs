//! Character patterns retain scalar identity through matching and persistence.

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

#[test]
fn character_patterns_match_unicode_and_escaped_scalars() {
    for literal in [
        r"'a'", "'界'", "'🦀'", r"'\n'", r"'\t'", r"'\r'", r"'\\'", r"'\''", r#"'\"'"#,
    ] {
        assert_source_and_archive(
            &format!("fn main() {{ {literal} match {{ 'z' => 0, {literal} => 42, _ => 1 }} }}"),
            42,
        );
    }
}

#[test]
fn character_patterns_work_in_nested_payloads_and_dynamic_matches() {
    for source in [
        "enum E { pair(a: char, b: (char, i64)), none }\nfn main() { E.pair('界', ('🦀', 42)) match { E.pair('界', ('🦀', n)) => n, _ => 0 } }",
        "fn main() { let x: Any = '界'; if x matches '界' { 42 } else { 0 } }",
        "fn main() { let x: Any = \"界\"; if x matches '界' { 0 } else { 42 } }",
        "fn main() { let x: Any = 30028; if x matches '界' { 0 } else { 42 } }",
        "fn main() { let x: Any = null; x match { '界' => 0, _ => 42 } }",
        "fn main() { '界' match { '界' if false => 0, '界' => 42, _ => 1 } }",
        "global count: i64 = 0\nfn next() -> char { count = count + 1; '界' }\nfn main() { let result = next() match { 'a' => 0, '界' => 41, _ => 0 }; result + count }",
    ] {
        assert_source_and_archive(source, 42);
    }
}

#[test]
fn incompatible_character_patterns_are_rejected_before_emission() {
    for source in [
        "fn main() { 42 match { 'a' => 0, _ => 1 } }",
        "fn main() { \"a\" matches 'a' }",
        "enum E { some(n: i64) }\nfn main() { E.some(42) matches E.some('a') }",
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "accepted {source}");
        assert!(compiled.codegen_output.functions.is_empty());
        assert!(
            compiled
                .diagnostics
                .iter()
                .any(|error| error.message.contains("pattern literal")),
            "{source}: {:?}",
            compiled.diagnostics
        );
    }
}

#[test]
fn unmatched_character_patterns_report_no_matching_case() {
    let error = common::run_value("fn main() { '界' match { 'a' => 42 } }").unwrap_err();
    assert!(error.contains("NoMatchingCase"), "{error}");
}
