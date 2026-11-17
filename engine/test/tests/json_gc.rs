//! 32 MiB Immix GC stress test for `JsonTape`, `JsonWriter`, and `derive Json`.
//! MMTk initializes once per process, so this small-heap stress test runs in its own test binary.

#[test]
fn json_derive_and_dynamic_parse_survive_32mib_immix_collections() {
    let source = r#"
        use std.json

        enum EventKind {
            click,
            view(ms: u32),
            span(start: i64, end: i64),
        }
        derive Json, Eq, PartialEq for EventKind

        struct Record {
            id: i64,
            label: String,
            kind: EventKind,
            tags: (String, String),
            meta: ?String,
        }
        derive Json, Eq, PartialEq for Record

        fn main() -> i64 {
            let payload_pad = "x".repeat(512)
            var i = 0
            var checksum = 0
            while i < 12000 {
                let k = if i % 3 == 0 { EventKind.click } else if i % 3 == 1 { EventKind.view((250).as(u32)) } else { EventKind.span(i, i + 10) }
                let m: ?String = if i % 2 == 0 { null } else { "note" }
                let rec = Record {
                    id: i,
                    label: payload_pad ++ "_item",
                    kind: k,
                    tags: ("alpha", "beta"),
                    meta: m,
                }
                let encoded = rec.to_json().unwrap()
                let decoded = Record.from_json(encoded, allow_unknown_fields = false).unwrap()
                if decoded != rec {
                    return -1
                }
                if i % 10 == 0 {
                    let dyn_val = json.parse(encoded).unwrap()
                    let dyn_str = json.stringify(dyn_val).unwrap()
                    if Record.from_json(dyn_str).unwrap() != rec {
                        return -2
                    }
                }
                checksum = checksum + decoded.id
                i = i + 1
            }
            if checksum == (11999 * 12000) / 2 { 42 } else { 0 }
        }
    "#;

    let compiled = driver::Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let mut engine = initialization::Engine::new(initialization::EngineConfig {
        gc: gc::GcConfig {
            heap_size: 32 * 1024 * 1024,
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
    assert!(
        engine.vm_mut().completed_collections() > before,
        "expected real Immix GC collections under 32 MiB heap"
    );
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
    assert_eq!(engine.vm_mut().active_stack_count(), 0);
}
