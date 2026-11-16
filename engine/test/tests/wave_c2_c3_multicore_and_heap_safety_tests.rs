//! Wave C2 & C3 verification tests:
//! - True multicore parallel execution (`>= 2.8x` speedup on 4 workers vs 1 worker for 4 CPU-bound `async` tasks)
//! - Thread-safe shared heap containers (`List`, `Map`), `struct` fields, and `global` variables under concurrent MMTk moving GC
//! - Ancestor non-capturing effect handler dispatch across task boundaries, child-local `catch k` continuation capture, and deterministic `VmError::CrossTaskContinuationCapture` on cross-task `catch k`

mod common;

use std::sync::Mutex;
use std::time::Instant;

use driver::Driver;
use initialization::{Engine, EngineConfig};
use interpreter::{VmError, VmResult};

static BENCH_SERIAL_LOCK: Mutex<()> = Mutex::new(());

fn run_source_on_workers(source: &str, workers: usize) -> (i64, std::time::Duration, Vec<u64>) {
    let compiled = Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();

    let mut engine = Engine::new_with_workers(EngineConfig::default(), workers);
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .expect("main entry");
    let root = engine.vm_mut().spawn_root(entry);
    let started = Instant::now();
    let outcome = engine.vm_mut().run();
    let elapsed = started.elapsed();
    assert!(
        matches!(outcome, VmResult::Finished),
        "unexpected outcome: {outcome:?}"
    );
    let val = engine.vm_mut().task_result_i64(root).unwrap();
    let slices = engine.vm.worker_slice_counts();
    (val, elapsed, slices)
}

#[test]
fn four_cpu_bound_async_tasks_achieve_true_multicore_parallel_speedup() {
    let _guard = BENCH_SERIAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let hw_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    assert!(
        hw_threads >= 4,
        "multicore speedup gate requires >= 4 logical cores, found {hw_threads}"
    );

    let source = r#"
async effect crunch(seed: i64, iters: i64) -> i64

fn cpu_kernel(seed: i64, iters: i64) -> i64 {
    var acc: i64 = seed
    var i: i64 = 0
    while i < iters {
        acc = (acc * 1103 + 12345 + i) % 100000007
        i += 1
    }
    acc
}

fn run_four(iters: i64) -> #crunch i64 {
    let f0 = crunch(17, iters)#
    let f1 = crunch(31, iters)#
    let f2 = crunch(47, iters)#
    let f3 = crunch(73, iters)#
    let out = [f0, f1, f2, f3].await_all()
    out.get(0).as(i64) + out.get(1).as(i64) + out.get(2).as(i64) + out.get(3).as(i64)
}

fn main() -> i64 {
    -- Tiny warm-up wave so auxiliary worker threads are already spawned and parked.
    let _warm = run_four(200) # {
        async crunch(seed, iters) => cpu_kernel(seed, iters),
    }
    let iters = if std.runtime.jit_enabled() { 50000000 } else { 80000 }
    run_four(iters) # {
        async crunch(seed, iters) => cpu_kernel(seed, iters),
    }
}
"#;

    let mut best_speedup = 0.0f64;
    let mut expected_val = None;

    for attempt in 1..=3 {
        let (val_1, t1, slices_1) = run_source_on_workers(source, 1);
        let (val_4, t4, slices_4) = run_source_on_workers(source, 4);
        assert_eq!(val_1, val_4, "1-worker and 4-worker results must match");
        assert_eq!(slices_1.len(), 1);
        assert_eq!(slices_4.len(), 4);
        assert!(
            slices_4.iter().all(|&s| s > 0),
            "all 4 workers must execute slices, got {slices_4:?}"
        );
        if let Some(prev) = expected_val {
            assert_eq!(val_1, prev);
        } else {
            expected_val = Some(val_1);
        }
        let speedup = t1.as_secs_f64() / t4.as_secs_f64();
        eprintln!(
            "[attempt {attempt}] 1-worker={t1:?} (slices={slices_1:?}), 4-worker={t4:?} (slices={slices_4:?}), speedup={speedup:.2}x"
        );
        if speedup > best_speedup {
            best_speedup = speedup;
        }
        if best_speedup >= 2.8 {
            break;
        }
    }

    assert!(
        best_speedup >= 2.8,
        "expected >= 2.8x wall-clock speedup on 4 workers vs 1 worker, best observed was {best_speedup:.2}x"
    );
}

#[test]
fn eight_concurrent_tasks_mutate_shared_list_map_struct_and_globals_under_moving_gc() {
    let _guard = BENCH_SERIAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let source = r#"
use std.runtime

struct SharedState {
    count_hint: i64,
    tag_text: String,
    pair: (i64, String),
}

global shared_list: List = List()
global shared_map: Map = Map()
global last_banner: String = "initial"

async effect stress_worker(tid: i64, shared: SharedState) -> i64

fn run_chaos(shared: SharedState) -> #stress_worker i64 {
    let f0 = stress_worker(0, shared)#
    let f1 = stress_worker(1, shared)#
    let f2 = stress_worker(2, shared)#
    let f3 = stress_worker(3, shared)#
    let f4 = stress_worker(4, shared)#
    let f5 = stress_worker(5, shared)#
    let f6 = stress_worker(6, shared)#
    let f7 = stress_worker(7, shared)#
    let res = [f0, f1, f2, f3, f4, f5, f6, f7].await_all()
    var total_done: i64 = 0
    var i: usize = 0
    while i < res.len().as(usize) {
        total_done += res.get(i).as(i64)
        i += 1
    }
    total_done
}

fn main() -> i64 {
    let shared = SharedState {
        count_hint: 0,
        tag_text: "boot",
        pair: (0, "boot_pair"),
    }

    let done = run_chaos(shared) # {
        async stress_worker(tid, box_ref) => {
            var k: i64 = 0
            while k < 250 {
                let code = tid * 1000 + k
                shared_list.push(code)
                let key = "k_" ++ tid.to_string() ++ "_" ++ k.to_string()
                let val = "v_" ++ code.to_string()
                shared_map.set(key, val)

                box_ref.count_hint = code
                box_ref.tag_text = "tag_" ++ code.to_string()
                box_ref.pair = (code, "p_" ++ code.to_string())
                last_banner = "banner_" ++ tid.to_string() ++ "_" ++ k.to_string()

                -- Read back shared state concurrently to exercise read/write races under GC.
                let snap_pair = box_ref.pair
                if snap_pair.1.len() < 3 or box_ref.tag_text.len() < 4 or last_banner.len() < 6 {
                    return -999999
                }

                if k % 30 == 0 and tid % 2 == 0 {
                    let _ = runtime.collect_garbage()
                }
                k += 1
            }
            250
        },
    }

    if done != 2000 {
        return -1
    }
    if shared_list.len() != 2000 {
        return -2
    }
    if shared_map.len() != 2000 {
        return -3
    }

    var sum: i64 = 0
    var idx: usize = 0
    while idx < 2000 {
        sum += shared_list.get(idx).as(i64)
        idx += 1
    }
    -- Exact arithmetic sum across tid in 0..8, k in 0..250 of (tid * 1000 + k):
    -- 250 * 1000 * 28 + 8 * (249 * 250 / 2) = 7_000_000 + 249_000 = 7_249_000
    if sum != 7249000 {
        return -4
    }

    var t: i64 = 0
    while t < 8 {
        var j: i64 = 0
        while j < 250 {
            let expected_code = t * 1000 + j
            let key = "k_" ++ t.to_string() ++ "_" ++ j.to_string()
            if not shared_map.contains(key) {
                return -5
            }
            let actual = shared_map.get(key).as(String)
            if actual != ("v_" ++ expected_code.to_string()) {
                return -6
            }
            j += 1
        }
        t += 1
    }

    if shared.tag_text.len() < 4 or shared.pair.1.len() < 3 or last_banner.len() < 6 {
        return -7
    }
    42
}
"#;

    let compiled = Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();

    let mut engine = Engine::new_with_workers(EngineConfig::default(), 4);
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .expect("main entry");
    let root = engine.vm_mut().spawn_root(entry);

    assert!(matches!(engine.vm_mut().run(), VmResult::Finished));
    let slices = engine.vm.worker_slice_counts();
    assert!(
        slices.iter().all(|&s| s > 0),
        "all 4 workers must participate in 8-task container + moving GC chaos, got {slices:?}"
    );
    assert!(
        engine.vm.completed_collections() >= 10,
        "expected >= 10 completed STW moving GC cycles during 8-task container chaos, got {}",
        engine.vm.completed_collections()
    );
    assert_eq!(engine.vm_mut().task_result_i64(root).unwrap(), 42);
    assert_eq!(engine.vm.active_task_count(), 0);
    assert_eq!(engine.vm.occupied_task_count(), 1);
    assert_eq!(engine.vm.promise_count(), 0);
}

#[test]
fn ancestor_non_capturing_effects_work_across_tasks_and_cross_task_catch_k_is_rejected() {
    let ok_source = r#"
effect get_bias() -> i64
effect scale_val(x: i64) -> i64
effect local_choice(catch k) -> i64
async effect compute_branch(id: i64) -> i64

fn child_local_multishot(base: i64) -> i64 {
    (base + local_choice()#) # {
        local_choice(k) => k(10).as(i64) + k(20).as(i64),
    }
}

fn run_branches() -> #[get_bias, scale_val, compute_branch] i64 {
    let f1 = compute_branch(1)#
    let f2 = compute_branch(2)#
    let f3 = compute_branch(3)#
    let f4 = compute_branch(4)#
    let out = [f1, f2, f3, f4].await_all()
    out.get(0).as(i64) + out.get(1).as(i64) + out.get(2).as(i64) + out.get(3).as(i64)
}

fn main() -> i64 {
    let bias: i64 = 5
    run_branches() # {
        get_bias() => bias,
        scale_val(x) => x * 100,
        async compute_branch(id) => {
            -- Invoke parent's non-capturing sync handlers across task boundary,
            -- AND run a task-local multi-shot continuation inside this child task!
            let from_parent = scale_val(id)# + get_bias()#
            child_local_multishot(from_parent)
        },
    }
}
"#;
    // For id in 1..=4:
    // from_parent = 100*id + 5 -> {105, 205, 305, 405}
    // child_local_multishot(b) = (b + 10) + (b + 20) = 2*b + 30 -> {240, 440, 640, 840}
    // sum = 2160
    assert_eq!(common::run_value(ok_source).unwrap(), 2160);

    let forbidden_source = r#"
effect parent_capture(catch k) -> i64
async effect child_task() -> i64

fn run_bad() -> #[parent_capture, child_task] i64 {
    let f = child_task()#
    f.await
}

fn main() -> i64 {
    run_bad() # {
        parent_capture(k) => k(99).as(i64),
        async child_task() => parent_capture()#,
    }
}
"#;
    let compiled = Driver::new().compile(forbidden_source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();

    let mut engine = Engine::new_with_workers(EngineConfig::default(), 4);
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .expect("main entry");
    let _root = engine.vm_mut().spawn_root(entry);

    let outcome = engine.vm_mut().run();
    assert!(
        matches!(
            outcome,
            VmResult::Error(VmError::CrossTaskContinuationCapture)
        ),
        "expected CrossTaskContinuationCapture, got {outcome:?}"
    );
}
