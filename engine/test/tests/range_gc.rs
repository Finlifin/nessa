//! Range iterators and dynamically selected iterators under real collections.
//! MMTk is configured once per process, so this small-heap test needs its own
//! test executable.

#[test]
fn range_and_dynamic_iterators_survive_collections_inside_loops() {
    // Each round natively allocates a 256 KiB String, so 64 rounds exceed the
    // heap with few interpreted steps. Collections run while the outer range
    // iterator and the dynamically selected List iterator are live.
    let source = r#"
        fn main() {
            var total = 0
            var kept: Any = null
            for round in 0..64 {
                let values: Any = [round, 0..=2]
                for value in values {
                    let chunk = "abcdefgh".repeat(32768)
                    if chunk.len() == 262144 { total += 1 }
                }
                for i in 1..=2 { total += i }
                kept = values
            }
            let last: Any = kept
            var count = 0
            for value in last { count += 1 }
            if total == 320 and count == 2 { 42 } else { 0 }
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
