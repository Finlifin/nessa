//! Natively built String lists under real collections. MMTk is configured once
//! per process, so this small-heap test needs its own test executable.

#[test]
fn split_strings_survive_collections_triggered_while_the_list_is_built() {
    // Each split allocates 30000 Strings (over 1 MiB); ten rounds exceed the
    // small heap, so collections occur during the native allocation loop.
    let source = r#"
        fn main() {
            let text = "abcdefghijklmnop,".repeat(30000);
            let total: i64 = 0;
            let last: Any = null;
            for round in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
                let parts = text.split(",");
                total += parts.len();
                last = parts;
            }
            let parts = last.as(List);
            let intact = parts.fold(0, |acc, x| if x == "abcdefghijklmnop" { acc + 1 } else { acc }).as(i64);
            if total == 300010 and intact == 30000 and parts(30000) == "" { 42 } else { 0 }
        }
    "#;
    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let mut engine = initialization::Engine::new(initialization::EngineConfig {
        gc: gc::GcConfig {
            heap_size: 8 * 1024 * 1024,
            plan: "Immix".into(),
            gc_threads: 1,
        },
        debug: false,
        ..Default::default()
    });
    let entry = driver::install_artifact(engine.vm_mut(), compiled.into_artifact().unwrap())
        .unwrap()
        .unwrap();
    let before = engine.vm_mut().completed_collections();
    let task = engine.vm_mut().spawn_root(entry);
    let result = engine.vm_mut().run();
    assert!(
        matches!(result, interpreter::VmResult::Finished),
        "{result:?}"
    );
    assert!(engine.vm_mut().completed_collections() > before);
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
    assert_eq!(engine.vm_mut().active_stack_count(), 0);
}
