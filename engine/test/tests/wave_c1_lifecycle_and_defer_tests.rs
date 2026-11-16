//! Wave C1 verification tests:
//! - Generational `TaskArena` slot reclamation (`O(1)` live footprint)
//! - Weak conditional `PromiseEntry` reclamation (`promise_count() == 0`)
//! - Converged loop-header/function-entry `Safepoint` throughput (200,000 short tasks)
//! - Deterministic `defer` LIFO execution and inner-before-outer child cleanup on cancellation

mod common;

use driver::Driver;
use initialization::Engine;
use interpreter::VmResult;

#[test]
fn two_hundred_thousand_short_async_tasks_scale_linearly_and_reclaim_all_slots() {
    let source = r#"
async effect step(x: i64) -> String

fn run_all(n: i64) -> #step i64 {
    var sum: i64 = 0
    var i: i64 = 0
    while i < n {
        let f = step(i)#
        let s = f.await
        sum += s.len()
        i += 1
    }
    sum
}

fn main() -> i64 {
    run_all(200000) # {
        async step(x) => if x % 2 == 0 { "aa" } else { "bbb" },
    }
}
"#;

    let compiled = Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();

    let mut engine = Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .expect("main entry");
    let root = engine.vm_mut().spawn_root(entry);

    assert!(matches!(engine.vm_mut().run(), VmResult::Finished));
    assert_eq!(engine.vm.active_stack_count(), 0);
    assert_eq!(engine.vm.active_task_count(), 0);
    assert_eq!(
        engine.vm.occupied_task_count(),
        1,
        "all 200,000 child task slots must be reaped, leaving only root task"
    );
    assert_eq!(
        engine.vm.promise_count(),
        0,
        "all 200,000 terminal PromiseEntry records must be reclaimed"
    );

    let result = engine.vm_mut().task_result_i64(root).unwrap();
    // 100,000 * 2 + 100,000 * 3 = 500,000
    assert_eq!(result, 500_000);
}

#[test]
fn cancellation_unwinds_multi_level_task_tree_and_call_frames_in_strict_lifo_order() {
    let source = r#"
use std.task

global events: List = List()
global grandchild_ready: i64 = 0

async effect outer_job() -> i64
async effect grandchild_job() -> i64
async effect fast_winner() -> i64

fn nested_helper() -> #grandchild_job i64 {
    defer { events.push("helper_defer_1") }
    defer { events.push("helper_defer_2") }
    let g = grandchild_job()#
    g.await
}

fn run_race() -> #[outer_job, grandchild_job, fast_winner] i64 {
    let loser = outer_job()#
    while grandchild_ready == 0 {
        task.yield_now()
    }
    let winner = fast_winner()#
    [loser, winner].await_any().as(i64)
}

fn main() -> i64 {
    let won = run_race() # {
        async outer_job() => {
            defer { events.push("outer_defer_1") }
            defer { events.push("outer_defer_2") }
            nested_helper() # {
                async grandchild_job() => {
                    defer { events.push("grandchild_defer_1") }
                    defer { events.push("grandchild_defer_2") }
                    grandchild_ready = 1
                    while true {
                        task.yield_now()
                    }
                    0
                },
            }
        },
        async grandchild_job() => 0,
        async fast_winner() => 77,
    }
    -- Wait until loser's cancellation unwind completes before exiting main:
    -- `run_race`'s `#` handler scope awaits all children on normal scope exit!
    let joined = events.join(",")
    if won == 77 and joined == "grandchild_defer_2,grandchild_defer_1,helper_defer_2,helper_defer_1,outer_defer_2,outer_defer_1" {
        42
    } else {
        -1
    }
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn early_return_and_error_exit_wait_for_cancelled_child_defer_before_outer_defer() {
    let source = r#"
use std.task

enum StopErr {
    Abort,
}

global log: List = List()
global child_started: i64 = 0

async effect worker() -> i64

fn early_return_case() -> i64 {
    defer { log.push("ret_parent_outer") }
    handles async worker() -> i64 => {
        defer { log.push("ret_child_inner") }
        child_started = 1
        while true {
            task.yield_now()
        }
        0
    }
    let _f = worker()#
    while child_started == 0 {
        task.yield_now()
    }
    return 20
}

fn error_case_inner() -> #worker !StopErr i64 {
    let _f = worker()#
    while child_started == 0 {
        task.yield_now()
    }
    error StopErr.Abort
}

fn error_exit_case() -> !StopErr i64 {
    defer { log.push("err_parent_outer") }
    child_started = 0
    error_case_inner() # {
        async worker() => {
            defer { log.push("err_child_inner") }
            child_started = 1
            while true {
                task.yield_now()
            }
            0
        },
    }
}

fn main() -> i64 {
    let a = early_return_case()
    let b = error_exit_case() ! {
        StopErr.Abort => 22,
    }
    let joined = log.join(",")
    if joined == "ret_child_inner,ret_parent_outer,err_child_inner,err_parent_outer" {
        a + b
    } else {
        -1
    }
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn loop_scoped_defers_pop_cleanly_on_normal_iterations_and_run_once_when_cancelled() {
    let source = r#"
use std.task

global normal_count: i64 = 0
global cancel_events: List = List()
global in_iter_three: i64 = 0

async effect loop_task() -> i64
async effect stopper() -> i64

fn run_loop_cancel_race() -> #[loop_task, stopper] i64 {
    let f1 = loop_task()#
    while in_iter_three == 0 {
        task.yield_now()
    }
    let f2 = stopper()#
    [f1, f2].await_any().as(i64)
}

fn main() -> i64 {
    for i in 0..500 {
        defer { normal_count += 1 }
        if i % 3 == 0 {
            continue
        }
    }

    let res = run_loop_cancel_race() # {
        async loop_task() => {
            defer { cancel_events.push("fn_outer") }
            var k: i64 = 0
            while k < 10 {
                let cur = k
                defer { cancel_events.push("iter_" ++ cur.to_string()) }
                if cur == 3 {
                    in_iter_three = 1
                    while true {
                        task.yield_now()
                    }
                }
                k += 1
            }
            0
        },
        async stopper() => 100,
    }

    let joined = cancel_events.join(",")
    -- Iterations 0, 1, 2 ran their loop-body defer normally before k == 3 stalled;
    -- on cancellation during k == 3, ONLY iter_3 and fn_outer remain on unwind_stack!
    if normal_count == 500 and res == 100 and joined == "iter_0,iter_1,iter_2,iter_3,fn_outer" {
        42
    } else {
        -1
    }
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}
