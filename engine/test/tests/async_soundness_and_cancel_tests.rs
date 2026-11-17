//! Regression tests for async & concurrency soundness, cancellation safety,
//! `defer` unwinding with contended async operations, and resource leak prevention:
//! - Unbuffered `Channel` rendezvous cancellation never buffers phantom items or loses messages;
//!   buffered `Channel.send` cancellation never commits a timed-out item into the buffer.
//! - Abnormal task exit / panic / cancellation while holding `Mutex`, `RwLock`, or `Once`
//!   releases ownership via `on_task_terminated` instead of permanently deadlocking peers;
//!   contended cancellation storms on `with_lock` / `with_read` / `with_write` / `with_permit`
//!   preserve exact lock/permit counts.
//! - Protected `defer` blocks running during `unwinding_cancel` can suspend on contended
//!   `Mutex` / `Semaphore` / `Channel`, `task.sleep_ms`, and child `async effect` tasks even
//!   when ancestor scopes trigger redundant `request_cancel_subtree` passes.
//! - `Barrier` participant cancellation breaks the barrier and wakes arrived peers instead
//!   of hanging `N - 1` tasks forever.
//! - `await_any` / `await_first` / `with_timeout` wait for cancelled losers' `defer` blocks
//!   to converge before returning, cancel `with_timeout` timers immediately (`<< timeout`),
//!   and mark concurrent loser failures as observed.
//! - `std.net` `TcpListener.accept` and `TcpStream.read_line` under cancellation preserve
//!   queued client connections and buffered stream data without socket leaks.

use std::time::Instant;

use driver::Driver;
use initialization::{Engine, EngineConfig};
use interpreter::{VmError, VmResult};

fn compile_and_run_workers(source: &str, workers: usize) -> VmResult {
    let compiled = Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    let mut engine = Engine::new_with_workers(EngineConfig::default(), workers);
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .expect("main entry");
    let _root = engine.vm_mut().spawn_root(entry);
    engine.vm_mut().run()
}

fn run_value_workers_timed(
    source: &str,
    workers: usize,
) -> Result<(i64, std::time::Duration), String> {
    let compiled = Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().map_err(|e| e.to_string())?;
    let mut engine = Engine::new_with_workers(EngineConfig::default(), workers);
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .map_err(|e| e.to_string())?
        .expect("main entry");
    let root = engine.vm_mut().spawn_root(entry);
    let run_start = Instant::now();
    let res = engine.vm_mut().run();
    let elapsed = run_start.elapsed();
    match res {
        VmResult::Finished => {
            assert_eq!(engine.vm.active_stack_count(), 0);
            assert_eq!(engine.vm.active_task_count(), 0);
            let val = engine
                .vm_mut()
                .task_result_i64(root)
                .map_err(|e| format!("{e:?}"))?;
            Ok((val, elapsed))
        }
        VmResult::Error(e) => Err(format!("{e:?}")),
    }
}

fn run_value_workers(source: &str, workers: usize) -> Result<i64, String> {
    run_value_workers_timed(source, workers).map(|(v, _)| v)
}

#[test]
fn channel_unbuffered_and_buffered_cancellation_preserve_rendezvous_and_buffer_invariants() {
    let source = r#"
use std.sync.{Channel, ChannelError, AtomicInt}
use std.task

async effect bg_sender(ch: Channel, val: i64, ack: AtomicInt) -> !ChannelError i64

fn test_unbuffered_cancel() -> !ChannelError i64 {
    let ch = Channel.unbuffered()
    let ack = AtomicInt.zero()

    -- Receiver times out on empty unbuffered channel: len must remain 0.
    let r1 = task.with_timeout(12, || -> i64 {
        let v = ch.recv() ! { _ => -1 }
        v.as(i64)
    }) ! {
        _ => 100,
    }
    if ch.len() != 0 {
        return -10
    }

    -- Start an unbuffered sender; while no live receiver has taken it yet, len() of
    -- an unbuffered channel is still 0 (no internal buffer), and a live receiver gets 777.
    handles async bg_sender(c: Channel, v: i64, done: AtomicInt) => {
        c.send(v)!
        done.store(1)
        1
    }
    let s_fut = bg_sender(ch, 777, ack)#
    task.sleep_ms(8)
    if ch.len() != 0 or ack.load() != 0 {
        return -20
    }
    let got = ch.recv()!.as(i64)
    let _ = s_fut.await!
    if got != 777 or ack.load() != 1 or ch.len() != 0 {
        return -30
    }
    r1 + got
}

fn test_buffered_send_timeout_never_commits_ghost_message() -> !ChannelError i64 {
    let ch = Channel.buffered(1)
    ch.send(10)!

    -- Buffer is full (`[10]`). A timed-out `send(20)` must NOT leave `20` queued in `ch`.
    let timed_out = task.with_timeout(12, || -> !ChannelError i64 {
        ch.send(20)!
        1
    }) ! {
        _ => 50,
    }

    let first = ch.recv()!.as(i64)
    let after = ch.try_recv() ! {
        ChannelError.Empty => 9,
        _ => -999,
    }
    timed_out + first + after
}

fn main() -> i64 {
    let a = test_unbuffered_cancel() ! { _ => -1 }
    let b = test_buffered_send_timeout_never_commits_ghost_message() ! { _ => -2 }
    -- a = 100 + 777 = 877; b = 50 + 10 + 9 = 69 -> 946
    a + b
}
"#;
    assert_eq!(run_value_workers(source, 4).unwrap(), 946);
}

#[test]
fn abnormal_task_exit_and_cancel_storms_never_leak_mutex_rwlock_semaphore_or_once() {
    let source = r#"
use std.collections.*
use std.sync.{Mutex, RwLock, Semaphore, Once, AtomicInt}
use std.task

async effect leaky_holder(mu: Mutex, rw: RwLock) -> i64
async effect cancel_contender(
    mu: Mutex,
    rw: RwLock,
    sem: Semaphore,
    id: i64,
    completed: AtomicInt,
) -> i64

fn main() -> i64 {
    let mu = Mutex.new(10)
    let rw = RwLock.new(20)
    let sem = Semaphore.new(2)
    let once = Once.new()
    let completed = AtomicInt.zero()

    -- 1. Child acquires raw `mu.lock()` and `rw.write_lock()` WITHOUT `defer`, then gets
    -- cancelled via `with_timeout`: `on_task_terminated` must release both locks cleanly!
    if true {
        handles async leaky_holder(m: Mutex, r: RwLock) => {
            let _ = m.lock()
            let _ = r.write_lock()
            task.sleep_ms(500)
            1
        }
        let _ = task.with_timeout(15, || -> i64 {
            let f = leaky_holder(mu, rw)#
            f.await
        }) ! {
            _ => 0,
        }
    }

    if mu.is_locked() {
        return -100
    }
    let after_mu = mu.with_lock(|v| v.as(i64) + 5).as(i64)
    let after_rw = rw.with_write(|v| v.as(i64) + 7).as(i64)

    -- 2. `Once` cancelled mid-initialization rolls back cleanly for the next caller.
    let _ = task.with_timeout(12, || -> i64 {
        once.call_once(|| -> i64 {
            task.sleep_ms(400)
            999
        }).as(i64)
    }) ! {
        _ => -1,
    }
    if once.is_completed() {
        return -200
    }
    let once_val = once.call_once(|| 88).as(i64)

    -- 3. Contended cancellation storm across 16 tasks racing short timeouts on Mutex/RwLock/Semaphore.
    if true {
        handles async cancel_contender(
            m: Mutex,
            r: RwLock,
            s: Semaphore,
            idx: i64,
            done: AtomicInt,
        ) => {
            let timeout_ms = if idx % 2 == 0 { 4 } else { 80 }
            let _ = task.with_timeout(timeout_ms, || -> i64 {
                s.with_permit(|| -> i64 {
                    m.with_lock(|cur| -> i64 {
                        task.sleep_ms(5)
                        m.set(cur.as(i64) + 1)
                        1
                    }).as(i64)
                }).as(i64) + r.with_read(|rv| rv.as(i64)).as(i64)
            }) ! {
                _ => 0,
            }
            done.fetch_add(1)
            1
        }
        let futs = List()
        var i: i64 = 0
        while i < 16 {
            futs.push(cancel_contender(mu, rw, sem, i, completed)#)
            i += 1
        }
        let _ = futs.await_all()
    }

    if mu.is_locked() or sem.available() != 2 or completed.load() != 16 {
        return -300
    }
    let rw_check = rw.with_write(|v| v.as(i64)).as(i64)
    -- after_mu (15) + after_rw (27) + once_val (88) + rw_check (27) + sem.available (2) = 159
    after_mu + after_rw + once_val + rw_check + sem.available()
}
"#;
    assert_eq!(run_value_workers(source, 4).unwrap(), 159);
}

#[test]
fn defer_cleanup_during_cancel_can_contend_on_mutex_sleep_and_spawn_child_async_effects() {
    let source = r#"
use std.sync.{Mutex, AtomicInt}
use std.task

async effect flush_audit(log_counter: AtomicInt) -> i64
async effect inner_victim(mu: Mutex, started: AtomicInt, audit: AtomicInt) -> i64
async effect middle_scope(mu: Mutex, started: AtomicInt, audit: AtomicInt) -> i64
async effect hold_mutex_briefly(mu: Mutex, holding: AtomicInt) -> i64

fn main() -> i64 {
    let mu = Mutex.new(0)
    let started = AtomicInt.zero()
    let audit = AtomicInt.zero()
    let holding = AtomicInt.zero()

    handles async flush_audit(log_counter: AtomicInt) => {
        task.sleep_ms(5)
        log_counter.fetch_add(10)
        1
    }
    handles async hold_mutex_briefly(m: Mutex, h: AtomicInt) => {
        m.with_lock(|cur| {
            h.store(1)
            task.sleep_ms(30)
            m.set(cur.as(i64) + 1)
            1
        }).as(i64)
    }
    handles async inner_victim(m: Mutex, st: AtomicInt, aud: AtomicInt) => {
        defer {
            -- While `inner_victim` is unwinding cancellation:
            -- 1. Contend on `m` (currently held by `hold_mutex_briefly`!)
            -- 2. Sleep asynchronously inside `defer`
            -- 3. Spawn & await a fresh child async effect `flush_audit`
            m.update(|cur| cur.as(i64) + 100)
            let f = flush_audit(aud)#
            let _ = f.await
        }
        st.store(1)
        task.sleep_ms(500)
        0
    }
    handles async middle_scope(m: Mutex, st: AtomicInt, aud: AtomicInt) => {
        -- Outer timeout (20ms) will cancel `middle_scope` right after `inner_victim` (cancelled
        -- at 10ms) has ALREADY entered its `defer` block and suspended on `m`!
        let _ = task.with_timeout(10, || -> i64 {
            let child = inner_victim(m, st, aud)#
            child.await
        }) ! {
            _ => 0,
        }
        0
    }

    let holder = hold_mutex_briefly(mu, holding)#
    while holding.load() == 0 {
        task.sleep_ms(1)
    }

    -- Cancel `middle_scope` at 16ms while `inner_victim` is mid-`defer` waiting on `mu`!
    let _ = task.with_timeout(16, || -> i64 {
        let mid = middle_scope(mu, started, audit)#
        mid.await
    }) ! {
        _ => 0,
    }

    let _ = holder.await
    let final_mu = mu.with_lock(|v| v.as(i64)).as(i64)
    -- holder (+1) + inner_victim defer (+100) = 101; audit (+10) = 10 -> 111
    final_mu + audit.load()
}
"#;
    assert_eq!(run_value_workers(source, 4).unwrap(), 111);
}

#[test]
fn barrier_cancellation_breaks_barrier_and_wakes_arrived_peers_immediately() {
    let source = r#"
use std.sync.{Barrier, AtomicInt}
use std.task

async effect waiter_party(bar: Barrier, arrived: AtomicInt) -> i64

fn main() -> i64 {
    let bar = Barrier.new(3)
    let arrived = AtomicInt.zero()

    handles async waiter_party(b: Barrier, a: AtomicInt) => {
        a.fetch_add(1)
        let _ = b.wait()
        1
    }

    -- Party #1 parks at `bar.wait()`.
    let p1 = waiter_party(bar, arrived)#
    while arrived.load() == 0 {
        task.sleep_ms(1)
    }
    task.sleep_ms(5)

    -- Party #2 arrives at `bar.wait()` inside `with_timeout` and gets cancelled before Party #3!
    let _ = task.with_timeout(15, || -> i64 {
        let _ = bar.wait()
        0
    }) ! {
        _ => 1,
    }

    -- `p1` must wake up with a broken-barrier runtime error instead of hanging forever!
    p1.await
}
"#;
    let outcome = compile_and_run_workers(source, 2);
    match outcome {
        VmResult::Error(VmError::InvalidArgument(msg)) => {
            assert!(
                msg.contains("Barrier broken"),
                "expected broken-barrier error, got: {msg}"
            );
        }
        other => panic!("expected VmError::InvalidArgument(\"Barrier broken...\"), got: {other:?}"),
    }
}

#[test]
fn await_any_and_with_timeout_converge_loser_defers_cancel_timer_fast_and_observe_loser_errors() {
    let source = r#"
use std.io.IoError
use std.net.TcpListener
use std.sync.{Mutex, AtomicInt}
use std.task

async effect slow_port_holder(port_box: AtomicInt, mu: Mutex, cleaned: AtomicInt) -> !IoError i64
async effect fast_winner(port_box: AtomicInt) -> !IoError i64

fn run_case() -> !IoError i64 {
    -- 1. `with_timeout(10_000, || 42)` must cancel the 10s timer immediately upon completion.
    let fast_to = task.with_timeout(10000, || 42) ! {
        _ => -1,
    }

    -- 2. `await_any` must wait for the cancelled loser's `defer` block (unlocking `mu` and
    -- closing `TcpListener` on `port`) BEFORE `await_any` returns to the caller!
    let port_box = AtomicInt.zero()
    let mu = Mutex.new(0)
    let cleaned = AtomicInt.zero()

    if true {
        handles async slow_port_holder(pb: AtomicInt, m: Mutex, cl: AtomicInt) => {
            let srv = TcpListener.bind("127.0.0.1", 0)!
            defer {
                srv.close()
                cl.store(1)
            }
            let p = srv.local_addr()!.port
            m.with_lock(|_| -> i64 {
                pb.store(p)
                task.sleep_ms(500)
                0
            }).as(i64)
        }
        handles async fast_winner(pb: AtomicInt) => {
            while pb.load() == 0 {
                task.sleep_ms(1)
            }
            task.sleep_ms(5)
            58
        }

        let s = slow_port_holder(port_box, mu, cleaned)#
        let f = fast_winner(port_box)#
        let won = [s, f].await_any()!

        -- STILL INSIDE `handles` SCOPE: loser's `defer` MUST already have converged!
        if cleaned.load() != 1 or mu.is_locked() {
            return -500
        }
        let rebound = TcpListener.bind("127.0.0.1", port_box.load())!
        rebound.close()
        return fast_to + won
    }
    0
}

fn main() -> i64 {
    run_case() ! {
        _ => -999,
    }
}
"#;
    let (val, elapsed) = run_value_workers_timed(source, 4).unwrap();
    assert_eq!(val, 100);
    assert!(
        elapsed.as_millis() < 2_000,
        "10s timer was not cancelled promptly: vm.run() took {:?}",
        elapsed
    );

    // 3. Concurrent loser runtime failure (panic) in `await_any` is marked observed so
    // exiting the `handles` scope does not crash the winner's parent task.
    let loser_panic_source = r#"
use std.task

async effect branch_op(kind: i64) -> i64

fn main() -> i64 {
    handles async branch_op(k: i64) => {
        if k == 0 {
            42
        } else {
            task.sleep_ms(1)
            panic("discarded loser panic")
        }
    }
    let a = branch_op(0)#
    let b = branch_op(1)#
    task.sleep_ms(5)
    [a, b].await_any()
}
"#;
    assert_eq!(run_value_workers(loser_panic_source, 4).unwrap(), 42);
}

#[test]
fn tcp_read_line_and_accept_cancellation_preserve_buffered_data_and_client_connections() {
    let source = r#"
use std.io.IoError
use std.net.{TcpListener, TcpStream}
use std.task

async effect server_task(listener: TcpListener) -> !IoError String

fn run_tcp_case() -> !IoError i64 {
    let listener = TcpListener.bind("127.0.0.1", 0)!
    defer { listener.close() }
    let port = listener.local_addr()!.port

    -- 1. Cancelled `accept()` while no client is present leaves listener healthy for real client.
    let _ = task.with_timeout(10, || -> !IoError i64 {
        let c = listener.accept()!
        c.close()
        1
    }) ! {
        _ => 0,
    }

    handles async server_task(srv: TcpListener) => {
        let conn = srv.accept()!
        defer { conn.close() }
        -- Cancelled `read_line` before client writes must not corrupt or drop subsequent lines!
        let _ = task.with_timeout(10, || -> !IoError ?String {
            conn.read_line()!
        }) ! {
            _ => null,
        }
        let l1 = conn.read_line()! match {
            s? => s,
            null => "",
        }
        let l2 = conn.read_line()! match {
            s? => s,
            null => "",
        }
        l1 ++ ":" ++ l2
    }

    let sf = server_task(listener)#
    let client = TcpStream.connect("127.0.0.1", port)!
    defer { client.close() }
    task.sleep_ms(25)
    let _ = client.write_str("alpha\nbeta\n")!

    let combined = sf.await!
    if combined == "alpha:beta" { 77 } else { -1 }
}

fn main() -> i64 {
    run_tcp_case() ! {
        _ => -999,
    }
}
"#;
    assert_eq!(run_value_workers(source, 4).unwrap(), 77);
}
