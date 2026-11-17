//! Dedicated 32 MiB Immix moving-GC stress test under live JIT execution.
//! MMTk is initialized once per test binary, so this test runs in its own executable.

use driver::{Driver, JitConfig, JitMode};
use initialization::{Engine, EngineConfig};
use interpreter::VmResult;

#[test]
fn jit_hot_loops_preserve_live_heap_references_across_moving_immix_collections() {
    // Allocates > 50 MiB of short-lived Struct + String objects across a 32 MiB Immix
    // heap inside JIT-compiled functions, while keeping a live `Anchor` struct and
    // periodically replaced `Ephemeral` survivor in registers/slots across GC cycles.
    let source = r#"
use std.runtime

struct Anchor {
    prefix: String,
    magic: i64,
    checksum: i64,
}

struct Ephemeral {
    label: String,
    value: i64,
    weight: f64,
}

fn churn_step(anchor: Anchor, round: i64) -> i64 {
    let temp = Ephemeral {
        label: "chunk_payload_abcdefghijklmnopqrstuvwxyz_0123456789_" ++ round.to_string(),
        value: round * 3 + anchor.magic,
        weight: 0.1,
    }
    anchor.checksum = (anchor.checksum + temp.value + temp.label.len()) % 1073741824
    if round % 4096 == 0 {
        let _ = runtime.collect_garbage()
    }
    anchor.checksum
}

fn main() -> i64 {
    let anchor = Anchor {
        prefix: "surviving_anchor_header_string_across_immix_relocation",
        magic: 42,
        checksum: 7,
    }
    var survivor = Ephemeral {
        label: "initial_survivor",
        value: 99,
        weight: 0.25,
    }
    var i: i64 = 0
    var acc: i64 = 0
    while i < 80000 {
        acc = (acc + churn_step(anchor, i)) % 1073741824
        if i % 1500 == 750 {
            survivor = Ephemeral {
                label: "survivor_epoch_" ++ i.to_string(),
                value: i,
                weight: 1.5,
            }
        }
        i += 1
    }
    if anchor.prefix != "surviving_anchor_header_string_across_immix_relocation" {
        return -10
    }
    if anchor.magic != 42 {
        return -20
    }
    if not survivor.label.starts_with("survivor_epoch_") {
        return -30
    }
    acc + survivor.value + anchor.prefix.len()
}
"#;

    let compiled = Driver::new().without_cache().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let archive = nsbc_io::write_artifact(&compiled.into_artifact().unwrap()).unwrap();

    // Compute exact reference value under JitMode::Off first, then verify Tier1, Tier2, and Auto
    // all share the same 32 MiB Immix heap and trigger real moving collections!
    let mut expected_val: Option<i64> = None;

    for mode in [JitMode::Off, JitMode::Tier1, JitMode::Tier2, JitMode::Auto] {
        let artifact = nsbc_io::read_artifact(&archive).unwrap();
        let mut engine = Engine::new(EngineConfig {
            gc: gc::GcConfig {
                heap_size: 32 * 1024 * 1024,
                plan: "Immix".into(),
                gc_threads: 1,
            },
            jit: JitConfig::with_mode(mode),
            debug: false,
        });
        let entry = driver::install_artifact(engine.vm_mut(), artifact)
            .unwrap()
            .unwrap();
        let before_gc = engine.vm_mut().completed_collections();
        let task = engine.vm_mut().spawn_root(entry);
        let result = engine.vm_mut().run();
        assert!(
            matches!(result, VmResult::Finished),
            "GC stress failed in mode {mode:?}: {result:?}"
        );
        let after_gc = engine.vm_mut().completed_collections();
        assert!(
            after_gc > before_gc,
            "expected real Immix GC cycles in mode {mode:?} (before={before_gc}, after={after_gc})"
        );
        let val = engine.vm_mut().task_result_i64(task).unwrap();
        assert!(val > 0, "corrupted survivor state in mode {mode:?}: {val}");
        if let Some(exp) = expected_val {
            assert_eq!(val, exp, "GC stress value mismatch in mode {mode:?}");
        } else {
            expected_val = Some(val);
        }
        #[cfg(all(target_arch = "x86_64", any(unix, windows)))]
        if mode != JitMode::Off {
            let stats = engine.vm.jit_stats();
            assert!(
                stats.compiled_functions >= 1,
                "expected JIT compilation during GC stress in {mode:?}, got {stats:?}"
            );
        }
        assert_eq!(engine.vm_mut().active_stack_count(), 0);
    }
}
