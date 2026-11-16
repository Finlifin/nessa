//! End-to-end integration tests for Wave 3 async algebraic effects, `Future(T)` / `.await`,
//! task-tree structured concurrency, safepoint preemption, cancellation, and `std.task`.

mod common;

use diagnostic::Level;
use driver::Driver;

fn assert_rejected_msg(source: &str, expected_substring: &str) {
    let result = Driver::new().compile(source);
    assert!(
        result.has_errors,
        "expected compilation error containing {expected_substring:?} for:\n{source}\ngot diagnostics: {:?}",
        result.diagnostics
    );
    assert!(
        result.codegen_output.functions.is_empty(),
        "invalid source emitted bytecode:\n{source}"
    );
    assert!(
        result.diagnostics.iter().any(|d| {
            d.level == Level::Error
                && d.message
                    .to_lowercase()
                    .contains(&expected_substring.to_lowercase())
        }),
        "expected error containing {expected_substring:?} for:\n{source}\ngot diagnostics: {:?}",
        result.diagnostics
    );
}

#[test]
fn async_effect_spawns_concurrent_futures_and_awaits_typed_results() {
    let source = r#"
async effect fetch_num(key: String) -> i64

fn load_pair() -> #fetch_num i64 {
    let f1: Future(i64) = fetch_num("alpha")#
    let f2: Future = fetch_num("beta")#
    let a = f1.await
    let b = f2.await.as(i64)
    a + b
}

fn main() -> i64 {
    load_pair() # {
        async fetch_num(key) => if key == "alpha" { 40 } else { 2 },
    }
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn multiline_postfix_effect_await_and_error_chains_parse_and_execute() {
    let source = r#"
enum NetErr {
    BadUrl,
}

async effect fetch_code(ok: bool) -> !NetErr i64

fn run_query(ok: bool) -> #fetch_code !NetErr i64 {
    let v = fetch_code(ok)#
        .await!
    v + 7
}

fn run_handled(ok: bool) -> !NetErr i64 {
    run_query(ok)
        # {
            async fetch_code(flag) => if flag { 35 } else { error NetErr.BadUrl },
        }
}

fn main() -> i64 {
    let good = run_handled(true)
        ! {
            NetErr.BadUrl => -1,
        }
    let bad = run_handled(false)
        ! {
            NetErr.BadUrl => 100,
        }
    good + bad
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 142);
}

#[test]
fn mixed_sync_and_async_effects_eliminate_independently_and_together() {
    let source = r#"
global events: List = List()

effect log_msg(msg: String) -> Unit
async effect compute_double(x: i64) -> i64

fn workflow() -> #[log_msg, compute_double] i64 {
    log_msg("start")#
    let f1 = compute_double(10)#
    let f2 = compute_double(11)#
    log_msg("dispatched")#
    let sum = f1.await + f2.await
    log_msg("done")#
    sum
}

fn main() -> i64 {
    let total = workflow() # {
        log_msg(m) => { events.push(m) },
        async compute_double(x) => x * 2,
    }
    total + events.len()
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 45);
}

#[test]
fn capability_handles_async_works_with_dot_use_handles_stmt_and_let_handler() {
    let source = r#"
async effect calc(n: i64) -> i64

struct WorkerPool {
    factor: i64
}

impl WorkerPool {
    pub fn serve(self, n: i64) -> i64 handles(calc) {
        n * self.factor
    }
}

fn job(x: i64) -> #calc i64 {
    let f = calc(x)#
    f.await
}

fn via_dot_use() -> i64 {
    let pool = WorkerPool { factor: 3 }
    job(10).use(pool)
}

fn via_handles_stmt() -> i64 {
    handles async calc(n: i64) -> i64 => n * 4
    job(10)#
}

fn via_let_handler() -> i64 {
    handles(calc) let h = WorkerPool { factor: 5 }
    let res = job(10)#
    res + h.factor - 5
}

fn main() -> i64 {
    via_dot_use() + via_handles_stmt() + via_let_handler()
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 30 + 40 + 50);
}

#[test]
fn safepoint_preemption_interleaves_compute_loops_without_explicit_yield() {
    let source = r#"
global child_progress: i64 = 0
global saw_interleave: i64 = 0

async effect bg_work(iters: i64) -> i64

fn run_interleaved() -> #bg_work i64 {
    let f = bg_work(300)#
    var acc: i64 = 0
    for i in 0..300 {
        acc += 1
        if child_progress > 0 and child_progress < 300 {
            saw_interleave = 1
        }
    }
    acc + f.await
}

fn main() -> i64 {
    let sum = run_interleaved() # {
        async bg_work(iters) => {
            var c: i64 = 0
            for j in 0..iters {
                child_progress += 1
                c += 1
            }
            c
        },
    }
    if saw_interleave == 1 { sum } else { -1 }
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 600);
}

#[test]
fn nested_task_tree_resolves_ancestor_sync_and_async_handlers() {
    let source = r#"
global audit_count: i64 = 0

effect audit(tag: i64) -> Unit
async effect inner_step(x: i64) -> i64
async effect outer_step(x: i64) -> i64

fn top_call() -> #[audit, inner_step, outer_step] i64 {
    let f = outer_step(20)#
    f.await
}

fn main() -> i64 {
    let res = top_call() # {
        audit(tag) => { audit_count += tag },
        async inner_step(x) => {
            audit(2)#
            x + 1
        },
        async outer_step(x) => {
            audit(5)#
            let sub1 = inner_step(x)#
            let sub2 = inner_step(x)#
            sub1.await + sub2.await
        },
    }
    -- sub1=21, sub2=21 => res=42; audit_count = 5 + 2 + 2 = 9
    res * 100 + audit_count
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 4209);
}

#[test]
fn structured_concurrency_normal_exit_awaits_unawaited_children() {
    let source = r#"
use std.task

global completed_side_effects: i64 = 0

async effect fire_and_forget(delta: i64) -> Unit

struct AsyncRunner {}

impl AsyncRunner {
    pub fn handle_ff(self, delta: i64) -> Unit handles(fire_and_forget) {
        for i in 0..120 {
            if i == 60 {
                task.yield_now()
            }
        }
        completed_side_effects += delta
    }
}

fn spawn_for_hash() -> #fire_and_forget i64 {
    let _a = fire_and_forget(3)#
    let _b = fire_and_forget(4)#
    5
}

fn spawn_two() -> #fire_and_forget i64 {
    let _f1 = fire_and_forget(10)#
    let _f2 = fire_and_forget(20)#
    12
}

fn with_handles_stmt() -> i64 {
    handles async fire_and_forget(delta: i64) -> Unit => {
        for i in 0..120 {
            if i == 50 {
                task.yield_now()
            }
        }
        completed_side_effects += delta
    }
    let _f3 = fire_and_forget(100)#
    0
}

fn with_let_handler() -> i64 {
    handles(fire_and_forget) let _runner = AsyncRunner {}
    let _f4 = fire_and_forget(1000)#
    0
}

fn main() -> i64 {
    -- Normal exit from # block must await both un-awaited child tasks before reading global.
    let v1 = spawn_for_hash() # {
        async fire_and_forget(delta) => {
            for i in 0..120 {
                if i == 50 {
                    task.yield_now()
                }
            }
            completed_side_effects += delta
        },
    }
    let snap_after_hash = completed_side_effects

    -- Normal exit from .use(provider) must also await both un-awaited child tasks.
    let v2 = spawn_two().use(AsyncRunner {})
    let snap_after_use = completed_side_effects

    -- Normal exit from `handles` statement scope must also await _f3.
    let _ = with_handles_stmt()
    let snap_after_handles = completed_side_effects

    -- Normal exit from `handles(...) let` scope must also await _f4.
    let _ = with_let_handler()
    let snap_after_let = completed_side_effects

    if snap_after_hash == 7 and snap_after_use == 37 and snap_after_handles == 137 and snap_after_let == 1137 {
        v1 + v2 + snap_after_let
    } else {
        -1
    }
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 5 + 12 + 1137);
}

#[test]
fn structured_concurrency_early_return_and_error_unwind_cancel_infinite_subtrees() {
    let source = r#"
enum Bail {
    StopNow,
}

global child_ticks: i64 = 0
global grandchild_ticks: i64 = 0

async effect spin_forever() -> i64
async effect spin_tree() -> i64

fn early_return_cancels_infinite_child() -> i64 {
    handles async spin_forever() -> i64 => {
        while true {
            child_ticks += 1
        }
        0
    }
    let _bg = spin_forever()#
    for i in 0..200 {
        if child_ticks > 0 {
            return 40
        }
    }
    return 40
}

fn trigger_tree_then_error() -> #[spin_forever, spin_tree] !Bail i64 {
    let _tree = spin_tree()#
    for i in 0..300 {
        if grandchild_ticks > 0 {
            error Bail.StopNow
        }
    }
    error Bail.StopNow
}

fn error_unwind_cancels_nested_subtree() -> !Bail i64 {
    trigger_tree_then_error() # {
        async spin_forever() => {
            while true {
                grandchild_ticks += 1
            }
            0
        },
        async spin_tree() => {
            let _gc = spin_forever()#
            while true {
                child_ticks += 1
            }
            0
        },
    }
}

fn main() -> i64 {
    let a = early_return_cancels_infinite_child()
    let b = error_unwind_cancels_nested_subtree() ! {
        Bail.StopNow => 2,
    }
    a + b
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn std_task_combinators_yield_is_ready_await_all_await_any_and_await_first() {
    let source = r#"
use std.task
use std.task.{Either, Left, Right}

enum TimeoutErr {
    Expired,
}

enum NetErr {
    Down,
}

async effect delayed_val(steps: i64, out: i64) -> i64
async effect fetch_data(steps: i64, fail: bool) -> !NetErr i64
async effect sleep_ticks(steps: i64) -> Unit

fn run_all_body() -> #delayed_val i64 {
    let f1 = delayed_val(2, 10)#
    let f2 = delayed_val(1, 20)#
    let f3 = delayed_val(3, 30)#
    let initially_ready = task.is_ready(f3)
    let list = [f1, f2, f3]
    let results = list.await_all()
    let finally_ready = task.is_ready(f3)
    if not initially_ready and finally_ready {
        results.get(0).as(i64) + results.get(1).as(i64) + results.get(2).as(i64)
    } else {
        -1000
    }
}

fn run_any_body() -> #delayed_val i64 {
    let slow = delayed_val(1000000, 999)#
    let fast = delayed_val(1, 77)#
    [slow, fast].await_any().as(i64)
}

fn fetch_with_timeout(data_steps: i64, timeout_steps: i64, fail: bool) -> #[fetch_data, sleep_ticks] !TimeoutErr !NetErr i64 {
    let data_f = fetch_data(data_steps, fail)#
    let timeout_f = sleep_ticks(timeout_steps)#
    task.await_first(data_f, timeout_f) match {
        Left(resp) => {
            let typed: !NetErr i64 = resp.as(!NetErr i64)
            typed!
        },
        Right(_) => error TimeoutErr.Expired,
    }
}

fn main() -> i64 {
    let sum_all = run_all_body() # {
        async delayed_val(steps, out) => {
            var s: i64 = 0
            while s < steps {
                task.yield_now()
                s += 1
            }
            out
        },
    }

    let fastest = run_any_body() # {
        async delayed_val(steps, out) => {
            var s: i64 = 0
            while s < steps {
                task.yield_now()
                s += 1
            }
            out
        },
    }

    let fast_ok = (fetch_with_timeout(1, 50, false) # {
        async fetch_data(steps, fail) => {
            var s: i64 = 0
            while s < steps {
                task.yield_now()
                s += 1
            }
            if fail { error NetErr.Down } else { 100 }
        },
        async sleep_ticks(steps) => {
            var s: i64 = 0
            while s < steps {
                task.yield_now()
                s += 1
            }
        },
    }) ! {
        NetErr.Down => -10,
    } ! {
        TimeoutErr.Expired => -20,
    }

    let timed_out = (fetch_with_timeout(1000000, 2, false) # {
        async fetch_data(steps, fail) => {
            var s: i64 = 0
            while s < steps {
                task.yield_now()
                s += 1
            }
            if fail { error NetErr.Down } else { 100 }
        },
        async sleep_ticks(steps) => {
            var s: i64 = 0
            while s < steps {
                task.yield_now()
                s += 1
            }
        },
    }) ! {
        NetErr.Down => -10,
    } ! {
        TimeoutErr.Expired => 5,
    }

    let e1 = Left(12)
    let e2 = Right("hi")
    let either_ok = if e1.is_left() and e2.is_right() and e1 == Either.Left(12) and "{e1}" == "Left(12)" {
        e1.unwrap_left().as(i64)
    } else {
        -999
    }

    -- 60 + 77 + 100 + 5 + 12 = 254
    sum_all + fastest + fast_ok + timed_out + either_ok
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 254);
}

#[test]
fn completed_future_heap_payloads_survive_moving_gc_before_await() {
    let source = r#"
use std.runtime
use std.task

async effect build_greeting(name: String) -> String

fn churn_then_await() -> #build_greeting String {
    let f = build_greeting("nessa-concurrency")#
    while not task.is_ready(f) {
        task.yield_now()
    }
    var garbage = List()
    for i in 0..200 {
        garbage.push("padding-" ++ i.to_string())
    }
    runtime.collect_garbage()
    garbage = List()
    runtime.collect_garbage()
    f.await
}

fn main() -> i64 {
    let msg = churn_then_await() # {
        async build_greeting(name) => "hello, " ++ name ++ "!",
    }
    if msg == "hello, nessa-concurrency!" { 42 } else { 0 }
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn future_types_and_await_opcodes_roundtrip_through_nsbc_archive_and_disassembler() {
    let source = r#"
async effect compute(x: i64) -> i64

fn run_single(a: i64) -> #compute i64 {
    let f = compute(a)#
    f.await
}

fn run_pair(a: i64, b: i64) -> #compute i64 {
    let fa: Future(i64) = compute(a)#
    let fb: Future(i64) = compute(b)#
    [fa, fb].await_all().fold(0) do |acc, item| acc + item.as(i64)
}

fn main() -> i64 {
    let v = run_single(12) # {
        async compute(x) => x + 30,
    }
    let pair = run_pair(10, 20) # {
        async compute(x) => x * 2,
    }
    v + pair
}
"#;
    let compiled = Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();

    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
    let loaded = nsbc_io::read_artifact(&bytes).unwrap();

    let disasm = nsbc_io::disassemble_artifact(&loaded);
    assert!(
        disasm.contains("AwaitFuture"),
        "disassembly missing AwaitFuture:\n{disasm}"
    );
    assert!(
        disasm.contains("AwaitPopHandler"),
        "disassembly missing AwaitPopHandler:\n{disasm}"
    );

    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), loaded).unwrap();
    let root = engine.vm_mut().spawn_root(entry.unwrap());
    assert!(matches!(
        engine.vm_mut().run(),
        interpreter::VmResult::Finished
    ));
    assert_eq!(engine.vm.active_stack_count(), 0);
    let out = engine
        .vm_mut()
        .task_result_number(root)
        .unwrap()
        .to_i64_checked();
    assert_eq!(out, Some(42 + 60));
}

#[test]
fn async_effect_and_await_negative_diagnostics_reject_invalid_programs() {
    // 1. `.await` on a non-Future value
    assert_rejected_msg(
        r#"
fn main() -> i64 {
    let x = 42
    x.await
}
"#,
        "await",
    );

    // 2. Non-`async` handler arm for an `async effect`
    assert_rejected_msg(
        r#"
async effect fetch(x: i64) -> i64

fn main() -> i64 {
    (fetch(1)#.await) # {
        fetch(x) => x + 1,
    }
}
"#,
        "async",
    );

    // 3. `async` handler arm for a synchronous `effect`
    assert_rejected_msg(
        r#"
effect sync_op(x: i64) -> i64

fn main() -> i64 {
    (sync_op(1)#) # {
        async sync_op(x) => x + 1,
    }
}
"#,
        "async",
    );

    // 4. `catch` continuation parameter on an `async effect`
    assert_rejected_msg(
        r#"
async effect bad_async(x: i64, catch k) -> i64
fn main() {}
"#,
        "async",
    );

    // 5. `.await` directly on `#fetch Future(i64)` without propagating/handling `#` first
    assert_rejected_msg(
        r#"
async effect fetch(x: i64) -> i64

fn get_fut() -> #fetch Future(i64) {
    fetch(1)#
}

fn bad_forget_hash() -> #fetch i64 {
    get_fut().await
}

fn main() -> i64 { 0 }
"#,
        "await",
    );

    // 6. Future(T) inner type mismatch on assignment (`Future(String)` awaited into `i64`)
    assert_rejected_msg(
        r#"
async effect fetch_text() -> String

fn bad_type() -> #fetch_text i64 {
    let n: i64 = fetch_text()#.await
    n
}

fn main() -> i64 { 0 }
"#,
        "mismatch",
    );

    // 7. Unhandled `async effect` at runtime fails cleanly with `UnhandledEffect`
    let err = common::run_value(
        r#"
async effect fetch(x: i64) -> i64

fn main() -> #fetch i64 {
    fetch(1)#.await
}
"#,
    )
    .unwrap_err();
    assert!(
        err.contains("UnhandledEffect"),
        "expected UnhandledEffect, got {err}"
    );
}
