//! Wave C4 verification tests:
//! - 10,000 concurrent `time.sleep_ms(50)` tasks finish in `< 150 ms` wall-clock time without blocking compute
//! - Single-worker (`workers = 1`) zero-blocking scheduling latency (`< 5 ms` max heartbeat gap) across `time.sleep_ms`, `process.shell`, and `fs` I/O
//! - Structured cancellation of an `async` task running an external long-lived child process (`sleep 30`) terminates the entire OS process group immediately
//! - Global deadlock detection returns `VmError::DeadlockDetected` when tasks suspend cyclically with zero runnable tasks and zero active I/O

use std::sync::Mutex;
use std::time::{Duration, Instant};

use initialization::{Engine, EngineConfig};
use interpreter::{BuiltinCtx, VmError, VmResult};

static TIMING_SERIAL_LOCK: Mutex<()> = Mutex::new(());

fn compile_artifact(source: &str) -> nsbc::CompiledArtifact {
    let result = driver::Driver::new().compile(source);
    assert!(!result.has_errors, "{:?}", result.diagnostics);
    result.into_artifact().expect("compiled artifact")
}

#[test]
fn ten_thousand_concurrent_sleep_tasks_finish_under_150ms_without_blocking_compute() {
    let _guard = TIMING_SERIAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let source = r#"
use std.collections.*
use std.time
use std.task

async effect nap(idx: i64) -> i64
async effect spawn_shard(base: i64, count: i64) -> i64
async effect compute_side(iters: i64) -> i64

fn run_shard(base: i64, count: i64) -> #nap i64 {
    let futs = []
    var i: i64 = 0
    while i < count {
        futs.push(nap(base + i)#)
        i += 1
    }
    let results = task.await_all(futs)
    var sum: i64 = 0
    var j: u64 = 0
    let n = results.len().as(u64)
    while j < n {
        sum += results(j).as(i64)
        j += 1
    }
    sum
}

fn run_fanout(shards: i64, per_shard: i64, comp_iters: i64) -> #spawn_shard #compute_side i64 {
    let comp_fut = compute_side(comp_iters)#
    let shard_futs = []
    var s: i64 = 0
    while s < shards {
        shard_futs.push(spawn_shard(s * per_shard, per_shard)#)
        s += 1
    }
    let shard_sums = task.await_all(shard_futs)
    var total: i64 = 0
    var k: u64 = 0
    let m = shard_sums.len().as(u64)
    while k < m {
        total += shard_sums(k).as(i64)
        k += 1
    }
    total + comp_fut.await
}

pub fn main() -> i64 {
    let dur50 = time.Duration.from_millis(50)
    let _warm = run_fanout(16, 625, 16) # {
        async spawn_shard(base, count) => {
            run_shard(base, count) # {
                async nap(_idx) => {
                    time.sleep_ms(1)
                    1
                },
            }
        },
        async compute_side(_iters) => 1,
    }
    let t0 = time.monotonic_nanos()
    let total = run_fanout(16, 625, 4000) # {
        async spawn_shard(base, count) => {
            run_shard(base, count) # {
                async nap(idx) => {
                    if idx % 2 == 0 {
                        time.sleep_ms(50)
                    } else {
                        time.sleep(dur50)
                    }
                    1
                },
            }
        },
        async compute_side(iters) => {
            var acc: i64 = 0
            var k: i64 = 0
            while k < iters {
                acc += 1
                k += 1
            }
            acc
        },
    }
    let elapsed_ms = (time.monotonic_nanos() - t0) / 1000000
    if total != 14000 {
        return -1
    }
    elapsed_ms
}
"#;

    let artifact = compile_artifact(source);
    let mut engine = Engine::new(EngineConfig::default());
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .expect("main entry");
    let root = engine.vm_mut().spawn_root(entry);

    let outcome = engine.vm_mut().run();
    assert!(
        matches!(outcome, VmResult::Finished),
        "unexpected VM outcome: {outcome:?}"
    );
    let fanout_ms = engine.vm_mut().task_result_i64(root).unwrap();
    assert!(
        (45..150).contains(&fanout_ms),
        "expected 10,000 concurrent 50ms timers to finish in [45ms, 150ms), took {fanout_ms} ms"
    );
    assert_eq!(engine.vm.active_stack_count(), 0);
    assert_eq!(engine.vm.occupied_task_count(), 1);
    assert_eq!(engine.vm.promise_count(), 0);
}

#[test]
fn single_worker_heartbeat_maintains_sub_5ms_latency_across_sleep_process_and_fs_io() {
    let _guard = TIMING_SERIAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let tmp_dir = std::env::temp_dir().join(format!(
        "nessa_c4_hb_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp_dir).unwrap();
    let tmp_file = tmp_dir.join("payload.txt");
    let tmp_file_str = tmp_file.to_string_lossy().replace('\\', "/");

    let source = format!(
        r#"
use std.collections.*
use std.fs
use std.process
use std.task
use std.time

async effect io_sleeper() -> i64
async effect io_proc() -> i64
async effect io_disk() -> i64
async effect heartbeat(stop_flag: List) -> i64

fn orchestrate() -> #io_sleeper #io_proc #io_disk #heartbeat i64 {{
    let stop_flag = [false]
    let hb = heartbeat(stop_flag)#
    let f1 = io_sleeper()#
    let f2 = io_proc()#
    let f3 = io_disk()#
    let v1 = f1.await
    let v2 = f2.await
    let v3 = f3.await
    stop_flag.set(0, true)
    let max_gap_us = hb.await
    if v1 != 1 or v2 != 0 or v3 != 20 {{
        return -1
    }}
    max_gap_us
}}

pub fn main() -> i64 {{
    orchestrate() # {{
        async io_sleeper() => {{
            time.sleep_ms(220)
            1
        }},
        async io_proc() => {{
            let out = process.shell("sleep 0.18") ! {{
                _ => CommandOutput {{ status: -99, stdout: "", stderr: "" }},
            }}
            out.status
        }},
        async io_disk() => {{
            var round: i64 = 0
            while round < 20 {{
                let p = "{tmp_file_str}"
                fs.write_string(p, "nessa-nonblocking-fs-check\n") ! {{
                    _ => return -50,
                }}
                let txt = fs.read_to_string(p) ! {{
                    _ => return -51,
                }}
                if txt.len() == 0 {{
                    return -52
                }}
                round += 1
            }}
            round
        }},
        async heartbeat(stop_flag) => {{
            let flag = stop_flag.as(List)
            var last = time.monotonic_nanos()
            var max_gap_ns: i64 = 0
            var ticks: i64 = 0
            while not flag.get(0).as(bool) {{
                task.yield_now()
                let now = time.monotonic_nanos()
                let gap = now - last
                if gap > max_gap_ns {{
                    max_gap_ns = gap
                }}
                last = now
                ticks += 1
            }}
            if ticks < 50 {{
                return 999999999
            }}
            max_gap_ns / 1000
        }},
    }}
}}
"#
    );

    let artifact = compile_artifact(&source);
    let mut engine = Engine::new_with_workers(EngineConfig::default(), 1);
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .expect("main entry");
    let root = engine.vm_mut().spawn_root(entry);

    let outcome = engine.vm_mut().run();
    let _ = std::fs::remove_dir_all(&tmp_dir);

    assert!(
        matches!(outcome, VmResult::Finished),
        "unexpected VM outcome: {outcome:?}"
    );
    let max_gap_us = engine.vm_mut().task_result_i64(root).unwrap();
    assert!(
        (0..5_000).contains(&max_gap_us),
        "expected single-worker max heartbeat scheduling latency < 5000 us (5 ms), got {max_gap_us} us"
    );
}

#[cfg(unix)]
#[test]
fn cancelling_parent_scope_immediately_terminates_external_long_running_child_process() {
    // SAFETY: Standard POSIX `kill` syscall used with signal `0` to probe process-group liveness.
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }

    let _guard = TIMING_SERIAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let pid_file = std::env::temp_dir().join(format!(
        "nessa_c4_proc_cancel_{}_{}.pid",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_file(&pid_file);
    let pid_file_str = pid_file.to_string_lossy().replace('\\', "/");

    let source = format!(
        r#"
use std.collections.*
use std.fs
use std.process
use std.task
use std.time

async effect long_child(log: List) -> i64
async effect watcher() -> i64

fn race_and_cancel(log: List) -> #long_child #watcher i64 {{
    let slow = long_child(log)#
    let fast = watcher()#
    task.await_any([slow, fast]).as(i64)
}}

pub fn main() -> i64 {{
    let log = []
    let winner = race_and_cancel(log) # {{
        async long_child(l) => {{
            defer {{
                l.push(77)
            }}
            let out = process.shell("echo $$ > '{pid_file_str}'; sleep 30") ! {{
                _ => return -1,
            }}
            out.status
        }},
        async watcher() => {{
            while not fs.exists("{pid_file_str}") {{
                time.sleep_ms(5)
            }}
            time.sleep_ms(20)
            42
        }},
    }}
    if log.len() == 1 and log.get(0).as(i64) == 77 {{
        winner + 100
    }} else {{
        winner
    }}
}}
"#
    );

    let artifact = compile_artifact(&source);
    let mut engine = Engine::new_with_workers(EngineConfig::default(), 2);
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .expect("main entry");
    let root = engine.vm_mut().spawn_root(entry);

    let started = Instant::now();
    let outcome = engine.vm_mut().run();
    let elapsed = started.elapsed();

    assert!(
        matches!(outcome, VmResult::Finished),
        "unexpected VM outcome: {outcome:?}"
    );
    // Verifies both `watcher` won (`42`) AND `long_child`'s `defer` block executed (`+100` -> `142`).
    assert_eq!(engine.vm_mut().task_result_i64(root).unwrap(), 142);
    assert!(
        elapsed < Duration::from_millis(500),
        "cancelling `sleep 30` child task should finish promptly, took {elapsed:?}"
    );

    let pid_text = std::fs::read_to_string(&pid_file).expect("child shell wrote PID file");
    let _ = std::fs::remove_file(&pid_file);
    let child_pid: i32 = pid_text.trim().parse().expect("valid child PID");
    assert!(child_pid > 1);

    let wait_deadline = Instant::now() + Duration::from_millis(150);
    let mut group_alive = true;
    while Instant::now() < wait_deadline {
        // SAFETY: `kill(-child_pid, 0)` probes process-group existence without sending a signal.
        let rc = unsafe { kill(-child_pid, 0) };
        if rc != 0 {
            group_alive = false;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !group_alive,
        "detached process group -{child_pid} should have been terminated on task cancellation"
    );
}

fn deadlock_trap_builtin(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.suspend_retry();
    Ok(())
}

#[test]
fn cyclic_internal_suspension_without_active_io_deterministically_reports_deadlock() {
    let mut engine = Engine::new_with_workers(EngineConfig::default(), 2);
    engine
        .vm_mut()
        .register_builtin(runtime::ids::TIME_UNIX_SECS, deadlock_trap_builtin);

    let deadlock_source = r#"
use std.time

async effect stuck() -> i64

fn run_both() -> #stuck i64 {
    let f1 = stuck()#
    let f2 = stuck()#
    f1.await + f2.await
}

pub fn main() -> i64 {
    run_both() # {
        async stuck() => time.unix_secs(),
    }
}
"#;
    let artifact = compile_artifact(deadlock_source);
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .expect("main entry");
    let _root = engine.vm_mut().spawn_root(entry);

    let outcome = engine.vm_mut().run();
    assert!(
        matches!(outcome, VmResult::Error(VmError::DeadlockDetected)),
        "expected DeadlockDetected when all tasks suspend with zero active I/O, got {outcome:?}"
    );
}
