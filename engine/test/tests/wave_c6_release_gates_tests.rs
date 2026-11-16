//! Wave C6 Release Gate Checklist verification suite (`docs/dev/concurrency-vision-and-roadmap.md` §4):
//! - Gate 1: True multicore parallel speedup (`>= 2.5x` on 4 workers vs 1 worker for 4 CPU-bound `async effect` tasks)
//! - Gate 2: Single-worker (`workers = 1`) zero-blocking scheduling (`< 5 ms` max heartbeat latency) across `sleep_ms`, `process.shell`, and `std.net` TCP I/O
//! - Gate 3: Long-run zero leaks across 500,000 heap-allocating (`String` + `List`) `async effect` tasks under moving Immix GC (`active_stack_count == 0`, `active_task_count == 0`, `promise_count == 0`)
//! - Gate 4: 4-level task tree structured concurrency & cancellation safety across normal exit, `!` error unwind, early `return`, `await_any`, and `with_timeout` (100% `defer` + `File` / `TcpListener` / `Mutex` release)
//! - Gate 5: 8 concurrent tasks mutating shared `List`, `Map`, `Channel`, `Mutex`, and `global` under repeated moving Immix GC
//! - Gate 6: Concurrent local `catch k` + `k.clone()` + `std.coroutine` (`Coroutine` & `Generator`) orthogonality under moving GC, plus deterministic `CrossTaskContinuationCapture` & cross-task resume rejection
//! - Gate 7: `.nsbc` bytecode archive roundtrip (`BUILTIN_ABI_VERSION`, disassembly, corrupt/tampered archive rejection, standalone execution without source)

use std::sync::Mutex;
use std::time::{Duration, Instant};

use driver::Driver;
use initialization::{Engine, EngineConfig};
use interpreter::{VmError, VmResult};
use runtime::StackError;

static RELEASE_GATE_TIMING_LOCK: Mutex<()> = Mutex::new(());

fn compile_checked(source: &str) -> nsbc::CompiledArtifact {
    let compiled = Driver::new().compile(source);
    assert!(
        !compiled.has_errors,
        "unexpected compile errors: {:?}",
        compiled.diagnostics
    );
    compiled.into_artifact().expect("compiled artifact")
}

fn run_on_workers(source: &str, workers: usize) -> (i64, Duration, Vec<u64>) {
    let artifact = compile_checked(source);
    let mut engine = Engine::new_with_workers(EngineConfig::default(), workers);
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .expect("main entry");
    let root = engine.vm_mut().spawn_root(entry);
    let t0 = Instant::now();
    let outcome = engine.vm_mut().run();
    let elapsed = t0.elapsed();
    assert!(
        matches!(outcome, VmResult::Finished),
        "expected VmResult::Finished, got {outcome:?}"
    );
    let result = engine.vm_mut().task_result_i64(root).unwrap();
    let slices = engine.vm.worker_slice_counts();
    assert_eq!(engine.vm.active_stack_count(), 0);
    assert_eq!(engine.vm.active_task_count(), 0);
    assert_eq!(engine.vm.promise_count(), 0);
    (result, elapsed, slices)
}

#[test]
fn gate_1_multicore_parallel_speedup_four_cpu_async_tasks() {
    let _guard = RELEASE_GATE_TIMING_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    assert!(
        cores >= 4,
        "Release Gate 1 requires >= 4 logical cores, found {cores}"
    );

    let source = r#"
async effect work_shard(seed: i64, iters: i64) -> i64

fn cpu_loop(seed: i64, iters: i64) -> i64 {
    var acc: i64 = seed
    var i: i64 = 0
    while i < iters {
        acc = (acc * 1664525 + 1013904223 + i) % 1000000007
        i += 1
    }
    acc
}

fn dispatch_four(iters: i64) -> #work_shard i64 {
    let futs = List()
    futs.push(work_shard(11, iters)#)
    futs.push(work_shard(23, iters)#)
    futs.push(work_shard(37, iters)#)
    futs.push(work_shard(53, iters)#)
    let done = futs.await_all()
    futs.clear()
    done(0) + done(1) + done(2) + done(3)
}

fn main() -> i64 {
    let _warm = dispatch_four(250) # {
        async work_shard(seed, iters) => cpu_loop(seed, iters),
    }
    std.runtime.collect_garbage()
    let iters = if std.runtime.jit_enabled() { 50000000 } else { 85000 }
    let out = dispatch_four(iters) # {
        async work_shard(seed, iters) => cpu_loop(seed, iters),
    }
    std.runtime.collect_garbage()
    out
}
"#;

    let mut best_speedup = 0.0f64;
    for attempt in 1..=3 {
        let (v1, t1, s1) = run_on_workers(source, 1);
        let (v4, t4, s4) = run_on_workers(source, 4);
        assert_eq!(v1, v4, "1-worker and 4-worker results must be identical");
        assert_eq!(s1.len(), 1);
        assert_eq!(s4.len(), 4);
        assert!(
            s4.iter().all(|&c| c > 0),
            "all 4 workers must participate: {s4:?}"
        );
        let speedup = t1.as_secs_f64() / t4.as_secs_f64();
        eprintln!("[Gate 1 attempt {attempt}] t1={t1:?}, t4={t4:?}, speedup={speedup:.2}x");
        if speedup > best_speedup {
            best_speedup = speedup;
        }
        if best_speedup >= 2.5 {
            break;
        }
    }
    assert!(
        best_speedup >= 2.5,
        "Release Gate 1 failed: expected >= 2.5x 4-worker speedup, best was {best_speedup:.2}x"
    );
}

#[test]
fn gate_2_single_worker_zero_blocking_across_sleep_process_and_tcp() {
    let _guard = RELEASE_GATE_TIMING_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    let source = r#"
use std.io.IoError
use std.net.{TcpListener, TcpStream}
use std.process
use std.process.CommandOutput
use std.sync.AtomicBool
use std.task
use std.time

async effect io_sleep() -> i64
async effect io_proc() -> !IoError i64
async effect io_tcp_server(srv: TcpListener) -> !IoError i64
async effect io_tcp_client(port: i64) -> !IoError String
async effect heartbeat(done: AtomicBool) -> (i64, i64)

fn run_io_gate() -> !IoError i64 {
    let listener = TcpListener.bind("127.0.0.1", 0)!
    defer { listener.close() }
    let port = listener.local_addr()!.port
    let done = AtomicBool.new(false)

    handles async io_sleep() => {
        time.sleep_ms(120)
        10
    }
    handles async io_proc() => {
        let out: CommandOutput = process.shell("sleep 0.10 && printf ok")!
        if out.stdout == "ok" { 20 } else { 0 }
    }
    handles async io_tcp_server(srv: TcpListener) => {
        let conn = srv.accept()!
        defer { conn.close() }
        time.sleep_ms(80)
        let msg = conn.read_line()! match {
            s? => s,
            null => "",
        }
        conn.write_line("pong:" ++ msg)!
        30
    }
    handles async io_tcp_client(p: i64) => {
        let cli = TcpStream.connect("127.0.0.1", p)!
        defer { cli.close() }
        cli.write_line("ping")!
        let reply = cli.read_line()! match {
            s? => s,
            null => "",
        }
        reply
    }
    handles async heartbeat(flag: AtomicBool) => {
        var ticks: i64 = 0
        var max_gap_ns: i64 = 0
        var last = time.monotonic_nanos()
        while not flag.load() {
            task.yield_now()
            let now = time.monotonic_nanos()
            let gap = now - last
            if gap > max_gap_ns {
                max_gap_ns = gap
            }
            last = now
            ticks += 1
        }
        (ticks, max_gap_ns)
    }

    let hb_fut = heartbeat(done)#
    let f_sleep = io_sleep()#
    let f_proc = io_proc()#
    let f_srv = io_tcp_server(listener)#
    let f_cli = io_tcp_client(port)#

    let v_sleep = f_sleep.await
    let v_proc = f_proc.await!
    let v_srv = f_srv.await!
    let v_cli = f_cli.await!
    done.store(true)
    let (ticks, max_gap_ns) = hb_fut.await

    if v_sleep != 10 or v_proc != 20 or v_srv != 30 or v_cli != "pong:ping" or ticks < 50 {
        return -1
    }
    max_gap_ns
}

fn main() -> i64 {
    let out = run_io_gate().unwrap()
    std.runtime.collect_garbage()
    out
}
"#;

    let mut min_max_gap_ns = i64::MAX;
    for attempt in 1..=3 {
        let (max_gap_ns, elapsed, slices) = run_on_workers(source, 1);
        assert!(
            max_gap_ns >= 0,
            "concurrent I/O tasks failed verification on single worker"
        );
        assert_eq!(slices.len(), 1);
        eprintln!(
            "[Gate 2 attempt {attempt}] single-worker elapsed={elapsed:?}, max_heartbeat_gap={} us",
            max_gap_ns / 1_000
        );
        if max_gap_ns < min_max_gap_ns {
            min_max_gap_ns = max_gap_ns;
        }
        if min_max_gap_ns < 5_000_000 {
            break;
        }
    }
    assert!(
        min_max_gap_ns < 5_000_000,
        "Release Gate 2 failed: expected < 5 ms single-worker max scheduling latency, got {:.2} ms",
        min_max_gap_ns as f64 / 1_000_000.0
    );
}

fn current_rss_kib() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let num = rest.split_whitespace().next()?;
            return num.parse::<u64>().ok();
        }
    }
    None
}

#[test]
fn gate_3_five_hundred_thousand_heap_allocating_async_tasks_zero_leak() {
    // 4 shards * 125,000 child tasks = 500,000 spawned, awaited, and dropped async tasks,
    // each returning a freshly heap-allocated `[String, i64]` List while triggering moving GC.
    let source = r#"
use std.runtime

async effect make_payload(idx: i64) -> List
async effect run_driver(shard_id: i64, count: i64) -> i64

fn drive_shard(shard_id: i64, count: i64) -> #make_payload i64 {
    var acc: i64 = 0
    var i: i64 = 0
    while i < count {
        let pair = make_payload(i)#.await
        let tag = pair(0).as(String)
        let n = pair(1).as(i64)
        acc += tag.len() + n
        if shard_id == 0 and i > 0 and i % 25000 == 0 {
            runtime.collect_garbage()
        }
        i += 1
    }
    acc
}

fn main() -> i64 {
    handles async run_driver(shard_id: i64, count: i64) => {
        drive_shard(shard_id, count) # {
            async make_payload(idx) => {
                let s = if idx % 2 == 0 {
                    "alpha".take(3)
                } else {
                    "bravo".take(4)
                }
                [s, idx % 7]
            },
        }
    }

    let warm = run_driver(99, 1000)#.await
    runtime.collect_garbage()

    let futs = List()
    futs.push(run_driver(0, 125000)#)
    futs.push(run_driver(1, 125000)#)
    futs.push(run_driver(2, 125000)#)
    futs.push(run_driver(3, 125000)#)
    let shards = futs.await_all()
    futs.clear()
    let total = warm * 0 + shards(0) + shards(1) + shards(2) + shards(3)
    runtime.collect_garbage()
    total
}
"#;

    let rss_before = current_rss_kib();
    let artifact = compile_checked(source);
    let mut engine = Engine::new_with_workers(EngineConfig::default(), 4);
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .expect("main entry");
    let root = engine.vm_mut().spawn_root(entry);

    assert!(matches!(engine.vm_mut().run(), VmResult::Finished));
    assert_eq!(
        engine.vm.active_stack_count(),
        0,
        "active stack segments must return to 0 after 500,000 tasks"
    );
    assert_eq!(
        engine.vm.active_task_count(),
        0,
        "active tasks must return to 0"
    );
    assert_eq!(
        engine.vm.occupied_task_count(),
        1,
        "all 500,004 spawned child TaskState slots must be reclaimed"
    );
    assert_eq!(
        engine.vm.promise_count(),
        0,
        "all 500,004 PromiseEntry records must be reclaimed"
    );

    let total = engine.vm_mut().task_result_i64(root).unwrap();
    // Per shard of 125,000 (0..124,999):
    // tag.len(): 62,500 evens * 3 + 62,500 odds * 4 = 187,500 + 250,000 = 437,500
    // idx % 7: 125,000 = 17,857 * 7 + 1 (rem 0 adds 0), sum(0..=6) = 21 -> 17,857 * 21 = 374,997
    // per shard = 812,497; * 4 shards = 3,249,988.
    assert_eq!(total, 3_249_988);

    if let (Some(before), Some(after)) = (rss_before, current_rss_kib()) {
        let growth_mib = after.saturating_sub(before) / 1024;
        eprintln!("[Gate 3] RSS before={before} KiB, after={after} KiB, delta={growth_mib} MiB");
        assert!(
            growth_mib < 256,
            "RSS grew unexpectedly across 500,000 reclaimed tasks: +{growth_mib} MiB"
        );
    }
}

#[test]
fn gate_4_four_level_task_tree_cancellation_defer_and_resource_release() {
    let source = r#"
use std.fs
use std.io.{File, IoError}
use std.net.TcpListener
use std.sync.{AtomicInt, Mutex}
use std.task
use std.task.TimeoutError

enum AbortErr {
    StopNow,
}

async effect spawn_l1(mode: i64) -> i64
async effect spawn_l2(mode: i64) -> i64
async effect spawn_l3(mode: i64) -> i64
async effect spawn_l4(mode: i64) -> i64

fn run_four_level_handlers(
    defers: AtomicInt,
    shared_mu: Mutex,
    tmp_path: String,
    body: fn() -> i64
) -> i64 {
    handles async spawn_l4(mode: i64) => {
        defer { defers.fetch_add(1000) }
        if mode == 0 {
            task.sleep_ms(5)
            return 4
        }
        shared_mu.with_lock(|v| {
            task.sleep_ms(500)
            v.as(i64) + 1
        }).as(i64)
    }
    handles async spawn_l3(mode: i64) => {
        let f = File.open(tmp_path, mode = "a") ! { _ => return -100 }
        defer {
            let _ = f.write("L3;") ! { _ => () }
            let _ = f.close() ! { _ => () }
            defers.fetch_add(100)
        }
        let c4 = spawn_l4(mode)#
        if mode == 0 {
            return c4.await + 3
        }
        task.sleep_ms(500)
        c4.await
    }
    handles async spawn_l2(mode: i64) => {
        let srv = TcpListener.bind("127.0.0.1", 0) ! { _ => return -200 }
        defer {
            srv.close()
            defers.fetch_add(10)
        }
        let c3 = spawn_l3(mode)#
        if mode == 0 {
            return c3.await + 2
        }
        task.sleep_ms(500)
        c3.await
    }
    handles async spawn_l1(mode: i64) => {
        defer { defers.fetch_add(1) }
        let c2 = spawn_l2(mode)#
        if mode == 0 {
            return c2.await + 1
        }
        task.sleep_ms(500)
        c2.await
    }
    body()
}

fn path_error_unwind(defers: AtomicInt) -> !AbortErr i64 {
    handles async spawn_l4(_m: i64) => {
        defer { defers.fetch_add(1000) }
        task.sleep_ms(500)
        0
    }
    handles async spawn_l3(_m: i64) => {
        defer { defers.fetch_add(100) }
        let _c4 = spawn_l4(1)#
        task.sleep_ms(500)
        0
    }
    handles async spawn_l2(_m: i64) => {
        let srv = TcpListener.bind("127.0.0.1", 0) ! { _ => return -1 }
        defer {
            srv.close()
            defers.fetch_add(10)
        }
        let _c3 = spawn_l3(1)#
        task.sleep_ms(500)
        0
    }
    handles async spawn_l1(_m: i64) => {
        defer { defers.fetch_add(1) }
        let _c2 = spawn_l2(1)#
        task.sleep_ms(500)
        0
    }
    let _f = spawn_l1(1)#
    -- Let all 4 levels spawn and arm their defers before unwinding with `!`
    task.sleep_ms(25)
    error AbortErr.StopNow
}

fn path_early_return(defers: AtomicInt, mu: Mutex, p: String) -> i64 {
    run_four_level_handlers(defers, mu, p, || -> i64 {
        let _f = spawn_l1(1)#
        task.sleep_ms(25)
        return 77
    })
}

fn run_gate_4() -> !IoError i64 {
    let tmp = fs.temp_file_path()!
    fs.write_string(tmp, "")!
    defer {
        let _ = fs.remove(tmp) ! { _ => () }
    }
    let mu = Mutex.new(42)

    -- Path 1: Normal scope exit implicitly awaits un-awaited 4-level tree and runs all defers.
    let d_normal = AtomicInt.zero()
    let normal_out = run_four_level_handlers(d_normal, mu, tmp, || -> i64 {
        let _unawaited = spawn_l1(0)#
        100
    })
    assert_eq(normal_out, 100)
    assert_eq(d_normal.load(), 1111)

    -- Path 2: Checked `!` error unwinding cancels 4-level tree and runs all 4 defers.
    let d_err = AtomicInt.zero()
    let err_tag = path_error_unwind(d_err) ! {
        AbortErr.StopNow => 88,
    }
    assert_eq(err_tag, 88)
    assert_eq(d_err.load(), 1111)

    -- Path 3: Early `return` cancels 4-level tree, releases Mutex/File/Socket, and runs all 4 defers.
    let d_ret = AtomicInt.zero()
    let ret_val = path_early_return(d_ret, mu, tmp)
    assert_eq(ret_val, 77)
    assert_eq(d_ret.load(), 1111)
    assert_eq(mu.is_locked(), false)

    -- Path 4a: `await_any` loser cancellation across 4-level tree.
    let d_race = AtomicInt.zero()
    let winner = run_four_level_handlers(d_race, mu, tmp, || -> i64 {
        let slow = spawn_l1(1)#
        task.sleep_ms(25)
        let fast = spawn_l1(0)#
        [slow, fast].await_any()
    })
    assert_eq(winner, 10)
    -- Both fast (1111) and cancelled slow (1111) 4-level trees executed 100% of their defers!
    assert_eq(d_race.load(), 2222)
    assert_eq(mu.is_locked(), false)

    -- Path 4b: `task.with_timeout` expiry cancels 4-level tree and releases Mutex/File/Socket.
    let d_to = AtomicInt.zero()
    let to_code = run_four_level_handlers(d_to, mu, tmp, || -> i64 {
        task.with_timeout(30, || -> i64 {
            spawn_l1(1)#.await
        }) ! {
            TimeoutError { millis } => millis,
        }
    })
    assert_eq(to_code, 30)
    assert_eq(d_to.load(), 1111)
    assert_eq(mu.is_locked(), false)

    -- Verify Mutex is still usable and File received flushed L3 defer markers cleanly.
    let mu_after = mu.with_lock(|v| v.as(i64) + 8).as(i64)
    let log_text = fs.read_to_string(tmp)!
    assert_eq(mu_after, 50)
    -- Path 1 (1) + Path 3 (1) + Path 4a (2) + Path 4b (1) = 5 file-backed L3 defers ("L3;" * 5 = 15 chars)
    assert_eq(log_text.len(), 15)
    9001
}

fn main() -> i64 {
    let out = run_gate_4().unwrap()
    std.runtime.collect_garbage()
    out
}
"#;

    let (val, _, _) = run_on_workers(source, 4);
    assert_eq!(val, 9001);
}

#[test]
fn gate_5_eight_concurrent_tasks_shared_containers_sync_and_moving_gc_chaos() {
    let source = r#"
use std.runtime
use std.sync.{AtomicBool, Channel, ChannelError, Mutex}
use std.task

global g_shared_counter: i64 = 0

async effect worker_task(id: i64) -> i64
async effect gc_agitator(stop: AtomicBool) -> i64

fn main() -> i64 {
    let shared_list = List()
    let shared_map = Map()
    let ch = Channel.buffered(16)
    let mu = Mutex.new(0)
    let stop_gc = AtomicBool.new(false)

    handles async gc_agitator(stop: AtomicBool) => {
        var collections: i64 = 0
        while not stop.load() {
            runtime.collect_garbage()
            collections += 1
            task.sleep_ms(2)
        }
        collections
    }

    handles async worker_task(id: i64) => {
        var local_consumed: i64 = 0
        var step: i64 = 0
        while step < 120 {
            let val = id * 1000 + step
            let key = id.to_string() ++ ":" ++ step.to_string()
            shared_list.push(val)
            shared_map.set(key, val)
            let _ = mu.update(|acc| acc.as(i64) + val)
            -- Serialized via Mutex for exact global lost-update-free check alongside direct atomic reads:
            mu.with_lock(|_| {
                g_shared_counter = g_shared_counter + 1
                0
            })
            if id < 4 {
                ch.send(val)! {}
            } else {
                let got = ch.recv() ! {
                    ChannelError.Closed => 0,
                    _ => 0,
                }
                local_consumed += got.as(i64)
            }
            step += 1
        }
        local_consumed
    }

    let gc_fut = gc_agitator(stop_gc)#
    let tasks = List()
    var w: i64 = 0
    while w < 8 {
        tasks.push(worker_task(w)#)
        w += 1
    }
    let consumed = tasks.await_all()
    tasks.clear()
    ch.close()
    stop_gc.store(true)
    let _gc_runs = gc_fut.await
    runtime.collect_garbage()

    assert_eq(shared_list.len().as(i64), 960)
    assert_eq(shared_map.len().as(i64), 960)
    assert_eq(g_shared_counter, 960)

    var list_sum: i64 = 0
    var idx: usize = 0
    while idx < shared_list.len() {
        list_sum += shared_list.get(idx).as(i64)
        idx += 1
    }

    var map_sum: i64 = 0
    var check_w: i64 = 0
    while check_w < 8 {
        var check_s: i64 = 0
        while check_s < 120 {
            let k = check_w.to_string() ++ ":" ++ check_s.to_string()
            map_sum += shared_map.get(k).as(i64)
            check_s += 1
        }
        check_w += 1
    }

    -- Expected sum across w in 0..8, step in 0..120 of (w * 1000 + step):
    -- sum(w) = 28 -> 28 * 120 * 1000 = 3,360,000
    -- sum(step) = 119 * 120 / 2 = 7,140 -> 8 * 7,140 = 57,120
    -- total = 3,417,120
    assert_eq(list_sum, 3417120)
    assert_eq(map_sum, 3417120)
    assert_eq(mu.with_lock(|v| v).as(i64), 3417120)

    -- Producers w in 0..4 sent sum:
    -- sum(0..4)=6 -> 6 * 120 * 1000 = 720,000 + 4 * 7,140 (28,560) = 748,560
    let chan_total = consumed(4) + consumed(5) + consumed(6) + consumed(7)
    assert_eq(chan_total, 748560)
    list_sum + chan_total
}
"#;

    let (val, _, _) = run_on_workers(source, 4);
    assert_eq!(val, 3_417_120 + 748_560);
}

#[test]
fn gate_6_continuation_and_coroutine_orthogonality_under_multicore_gc() {
    // Part A: 6 concurrent tasks each using multi-shot `catch k` + `k.clone()` AND `std.coroutine`
    // (`Coroutine` + `Generator`) while moving Immix GC runs concurrently across 4 workers.
    let source_valid = r#"
use std.coroutine
use std.runtime

effect choose(tag: String, catch k) -> i64
async effect cont_worker(seed: i64) -> i64

fn branch_calc(base: i64) -> #choose i64 {
    let hold = "prefix-" ++ base.to_string()
    let a = choose(hold)#
    let b = choose(hold)#
    hold.len() + base + a * 10 + b
}

fn run_task_body(seed: i64) -> i64 {
    -- 1. Multi-shot continuation with `k.clone()` + moving GC inside handler
    let multi = (branch_calc(seed)) # {
        choose(_tag, k) => {
            let k2 = k.clone()
            runtime.collect_garbage()
            let r1 = k(1).as(i64)
            let r2 = k2(2).as(i64)
            r1 + r2
        },
    }

    -- 2. Stateful asymmetric Coroutine + Generator on the same task
    let co = coroutine.create(|init: Any| -> Any {
        let start = init.as(i64)
        let r1 = coroutine.yield_val(value = start + 1).as(i64)
        runtime.collect_garbage()
        let r2 = coroutine.yield_val(value = start + r1).as(i64)
        start + r1 + r2
    })
    let y1 = co.step(arg = seed).as(i64)
    let y2 = co.step(arg = 5).as(i64)
    let y3 = co.step(arg = 7).as(i64)

    let gen = coroutine.generator(|| {
        var i: i64 = 1
        while i <= 4 {
            let _ = coroutine.yield_val(value = i * seed)
            i += 1
        }
    })
    var gen_sum: i64 = 0
    for item in gen {
        gen_sum += item.as(i64)
    }

    multi + y1 + y2 + y3 + gen_sum
}

fn main() -> i64 {
    handles async cont_worker(seed: i64) => run_task_body(seed)
    let futs = List()
    futs.push(cont_worker(1)#)
    futs.push(cont_worker(2)#)
    futs.push(cont_worker(3)#)
    futs.push(cont_worker(4)#)
    futs.push(cont_worker(5)#)
    futs.push(cont_worker(6)#)
    let res = futs.await_all()
    futs.clear()
    let total = res(0) + res(1) + res(2) + res(3) + res(4) + res(5)
    runtime.collect_garbage()
    total
}
"#;

    let (total, _, _) = run_on_workers(source_valid, 4);
    // For seed s in 1..=6:
    // hold = "prefix-<s>" -> len = 8
    // branch_calc(s) has 4 leaf evaluations (a,b) in {(1,1),(1,2),(2,1),(2,2)}:
    //   sum of 4 leaves = 4*(8 + s) + 10*(1+1+2+2) + (1+2+1+2) = 32 + 4s + 60 + 6 = 98 + 4s
    // co: y1 = s+1, y2 = s+5, y3 = s+12 -> y1+y2+y3 = 3s + 18
    // gen: (1+2+3+4)*s = 10s
    // per seed = 98 + 4s + 3s + 18 + 10s = 116 + 17s
    // sum s=1..=6 (sum s = 21): 6*116 + 17*21 = 696 + 357 = 1053
    assert_eq!(total, 1053);

    // Part B: Cross-task dynamic capture of ancestor's `catch k` handler fails deterministically
    // with `VmError::CrossTaskContinuationCapture`.
    let cross_capture_src = r#"
effect parent_shift(x: i64, catch k) -> i64
async effect spawn_child() -> i64

fn main() -> i64 {
    (
        (spawn_child()#.await) # {
            async spawn_child() => parent_shift(10)#,
        }
    ) # {
        parent_shift(x, k) => k(x + 1).as(i64),
    }
}
"#;
    let artifact_b = compile_checked(cross_capture_src);
    let mut engine_b = Engine::new_with_workers(EngineConfig::default(), 2);
    let entry_b = driver::install_artifact(engine_b.vm_mut(), artifact_b)
        .unwrap()
        .unwrap();
    let _root_b = engine_b.vm_mut().spawn_root(entry_b);
    let out_b = engine_b.vm_mut().run();
    assert!(
        matches!(
            out_b,
            VmResult::Error(VmError::CrossTaskContinuationCapture)
        ),
        "expected CrossTaskContinuationCapture, got {out_b:?}"
    );
    assert_eq!(engine_b.vm.active_stack_count(), 0);

    // Part C: Capturing a Continuation locally in Task 1 and attempting to resume it on Task 2
    // fails deterministically without corrupting stacks.
    let cross_resume_src = r#"
use std.sync.Channel

effect local_cap(catch k) -> i64
async effect capture_and_send(ch: Channel) -> i64
async effect recv_and_resume(ch: Channel) -> i64

fn main() -> i64 {
    let ch = Channel.buffered(1)
    handles async capture_and_send(c: Channel) => {
        (local_cap()#) # {
            local_cap(k) => {
                c.send(k.clone())! {}
                -- Keep owner task alive briefly while Task 2 attempts illegal cross-task resume
                k(41).as(i64)
            },
        }
    }
    handles async recv_and_resume(c: Channel) => {
        let stolen = c.recv().unwrap().as(Continuation)
        stolen(99).as(i64)
    }
    let f1 = capture_and_send(ch)#
    let f2 = recv_and_resume(ch)#
    f1.await + f2.await
}
"#;
    let artifact_c = compile_checked(cross_resume_src);
    let mut engine_c = Engine::new_with_workers(EngineConfig::default(), 2);
    let entry_c = driver::install_artifact(engine_c.vm_mut(), artifact_c)
        .unwrap()
        .unwrap();
    let _root_c = engine_c.vm_mut().spawn_root(entry_c);
    let out_c = engine_c.vm_mut().run();
    assert!(
        matches!(
            out_c,
            VmResult::Error(VmError::Stack(StackError::InvalidContinuation))
        ),
        "expected Stack(InvalidContinuation) on cross-task continuation resume, got {out_c:?}"
    );
    assert_eq!(engine_c.vm.active_stack_count(), 0);
}

#[test]
fn gate_7_nsbc_archive_roundtrip_disassembly_and_corrupt_rejection() {
    let source = r#"
use std.io.IoError
use std.net.{TcpListener, TcpStream}
use std.sync.{AtomicInt, Channel, Mutex}
use std.task

async effect echo_once(srv: TcpListener, counter: AtomicInt) -> !IoError i64

fn run_gate_7() -> !IoError i64 {
    let counter = AtomicInt.zero()
    let mu = Mutex.new(10)
    let ch = Channel.buffered(2)
    let srv = TcpListener.bind("127.0.0.1", 0)!
    defer {
        srv.close()
        counter.fetch_add(1)
    }
    let port = srv.local_addr()!.port

    handles async echo_once(listener: TcpListener, cnt: AtomicInt) => {
        let conn = listener.accept()!
        defer {
            conn.close()
            cnt.fetch_add(10)
        }
        let line = conn.read_line()! match {
            s? => s,
            null => "",
        }
        conn.write_line("ok:" ++ line)!
        line.len()
    }

    let srv_fut = echo_once(srv, counter)#
    let done_len = task.with_timeout(2000, || -> !IoError i64 {
        let cli = TcpStream.connect("127.0.0.1", port)!
        defer { cli.close() }
        cli.write_line("nessa-nsbc")!
        let reply = cli.read_line()! match {
            s? => s,
            null => "",
        }
        ch.send(reply.len())! {}
        srv_fut.await!
    }) ! {
        _ => return -1,
    }

    let recv_len = ch.recv() ! { _ => 0 }
    let base = mu.with_lock(|v| v.as(i64) + 5).as(i64)
    -- done_len ("nessa-nsbc"=10) + recv_len ("ok:nessa-nsbc"=13) + base (15) + echo defer (10) = 48
    done_len + recv_len.as(i64) + base + counter.load()
}

fn main() -> i64 {
    let out = run_gate_7().unwrap()
    std.runtime.collect_garbage()
    out
}
"#;

    let artifact = compile_checked(source);
    assert_eq!(artifact.builtin_abi_version, runtime::BUILTIN_ABI_VERSION);

    let archive_bytes = nsbc_io::write_artifact(&artifact).expect("serialize .nsbc artifact");

    // 1. Corrupt payload byte must fail SHA-256 verification cleanly without panic.
    let mut flipped = archive_bytes.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 0x5A;
    assert!(
        nsbc_io::read_artifact(&flipped).is_err(),
        "corrupted .nsbc archive must fail checksum verification"
    );

    // 2. Truncated archive bytes must be rejected cleanly without panic.
    assert!(
        nsbc_io::read_artifact(&archive_bytes[..archive_bytes.len() / 2]).is_err(),
        "truncated .nsbc archive must be rejected"
    );

    // 3. Incompatible builtin ABI version must be rejected during artifact installation.
    let mut wrong_abi = nsbc_io::read_artifact(&archive_bytes).expect("deserialize .nsbc");
    wrong_abi.builtin_abi_version = runtime::BUILTIN_ABI_VERSION + 99;
    let mut reject_engine = Engine::with_defaults();
    assert!(
        driver::install_artifact(reject_engine.vm_mut(), wrong_abi).is_err(),
        "mismatched builtin ABI version must be rejected on install"
    );

    // 4. Disassembly must succeed and display async/concurrency opcodes.
    let loaded = nsbc_io::read_artifact(&archive_bytes).expect("deserialize valid .nsbc");
    let disasm = nsbc_io::disassemble_artifact(&loaded);
    assert!(
        disasm.contains("AwaitFuture"),
        "missing AwaitFuture in disasm"
    );
    assert!(
        disasm.contains("AwaitPopHandler"),
        "missing AwaitPopHandler in disasm"
    );
    assert!(disasm.contains("Safepoint"), "missing Safepoint in disasm");

    // 5. Execute standalone deserialized `.nsbc` artifact without source on 4 workers.
    let mut exec_engine = Engine::new_with_workers(EngineConfig::default(), 4);
    let entry = driver::install_artifact(exec_engine.vm_mut(), loaded)
        .expect("install valid .nsbc")
        .expect("main entry");
    let root = exec_engine.vm_mut().spawn_root(entry);
    assert!(matches!(exec_engine.vm_mut().run(), VmResult::Finished));
    assert_eq!(exec_engine.vm.active_stack_count(), 0);
    assert_eq!(exec_engine.vm.active_task_count(), 0);
    assert_eq!(exec_engine.vm.promise_count(), 0);
    assert_eq!(exec_engine.vm_mut().task_result_i64(root).unwrap(), 48);
}
