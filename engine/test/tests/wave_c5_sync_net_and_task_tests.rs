//! Wave C5 verification tests:
//! - `std.sync`: `Channel` (unbuffered rendezvous, buffered MPMC, `try_send`/`try_recv` `Full`/`Empty`/`Closed`, moving GC, cancellation),
//!   `Mutex`, `RwLock`, `Semaphore`, `Once`, `Lazy`, `Barrier`, `AtomicInt`, and `AtomicBool`.
//! - `std.net`: `TcpListener`, `TcpStream`, `UdpSocket`, `UdpDatagram`, `SocketAddr`, `resolve_host`,
//!   multi-client concurrent echo server, EOF/closed errors, and task cancellation while parked on sockets.
//! - `std.task`: `current_id`, `tree_snapshot`, `sleep_ms`, `sleep(Duration)`, `with_timeout` (success, deadline expiry with `defer` cleanup, nested error flattening),
//!   static combinator typing (`await_all`, `await_any`, `await_first`) without `.as(...)` casts, and compile-time rejection of non-`Future` elements.

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
fn std_sync_channels_unbuffered_buffered_mpmc_cancel_and_gc_safety() {
    let source = r#"
use std.collections.*
use std.runtime
use std.sync.{Channel, ChannelError, AtomicInt}
use std.task

async effect send_unbuf(ch: Channel, v: i64) -> !ChannelError i64
async effect produce_batch(ch: Channel, id: i64, count: i64) -> !ChannelError i64
async effect consume_all(ch: Channel, acc: AtomicInt) -> i64
async effect blocked_recv(ch: Channel, cleaned: AtomicInt) -> i64

fn run_rendezvous(ch: Channel) -> #send_unbuf !ChannelError i64 {
    let sender = send_unbuf(ch, 42)#
    let got = ch.recv()!.as(i64)
    let sent_ack = sender.await!
    ch.close()
    let closed_send = ch.send(1) ! {
        ChannelError.Closed => 30,
        _ => 0,
    }
    let closed_recv = ch.recv() ! {
        ChannelError.Closed => 40,
        _ => 0,
    }
    got + sent_ack + closed_send + closed_recv
}

fn test_unbuffered() -> !ChannelError i64 {
    let ch = Channel.unbuffered()
    let empty_err = ch.try_recv() ! {
        ChannelError.Empty => 10,
        _ => 0,
    }
    let full_err = ch.try_send(99) ! {
        ChannelError.Full => 20,
        _ => 0,
    }
    let rendezvous = run_rendezvous(ch) # {
        async send_unbuf(c, v) => {
            c.send(v)!
            8
        },
    }
    empty_err + full_err + rendezvous!
}

fn run_mpmc(ch: Channel, total_sum: AtomicInt) -> #[produce_batch, consume_all] !ChannelError i64 {
    let p0 = produce_batch(ch, 0, 25)#
    let p1 = produce_batch(ch, 1, 25)#
    let p2 = produce_batch(ch, 2, 25)#
    let p3 = produce_batch(ch, 3, 25)#
    let c0 = consume_all(ch, total_sum)#
    let c1 = consume_all(ch, total_sum)#
    let c2 = consume_all(ch, total_sum)#
    let c3 = consume_all(ch, total_sum)#

    let prod_res = [p0, p1, p2, p3].await_all()
    prod_res(0)!
    prod_res(1)!
    prod_res(2)!
    prod_res(3)!
    ch.close()

    let cons_counts = [c0, c1, c2, c3].await_all()
    cons_counts(0) + cons_counts(1) + cons_counts(2) + cons_counts(3)
}

fn test_buffered_mpmc_with_gc() -> !ChannelError i64 {
    let ch = Channel.buffered(4)
    let total_sum = AtomicInt.zero()
    let out = run_mpmc(ch, total_sum) # {
        async produce_batch(c, pid, count) => {
            var i: i64 = 1
            while i <= count {
                let tag = "item-" ++ (pid * 100 + i).to_string()
                c.send((i, tag))!
                if i % 7 == 0 {
                    runtime.collect_garbage()
                }
                i += 1
            }
            1
        },
        async consume_all(c, acc) => {
            var handled: i64 = 0
            while true {
                let next = c.recv() ! {
                    ChannelError.Closed => break,
                    _ => break,
                }
                let (num, label) = next.as((i64, String))
                if label.len() > 5 {
                    acc.fetch_add(num)
                    handled += 1
                }
            }
            handled
        },
    }
    let count = out!
    if count == 100 and total_sum.load() == 1300 {
        1000
    } else {
        -1
    }
}

fn test_channel_cancel_cleanup() -> i64 {
    let ch = Channel.unbuffered()
    let cleaned = AtomicInt.zero()
    let winner = task.with_timeout(15, || {
        defer { cleaned.fetch_add(1) }
        let v = ch.recv() ! { _ => -1 }
        v.as(i64)
    }) ! {
        _ => 77,
    }
    let _ = ch.try_send(5) ! { _ => () }
    if winner == 77 and cleaned.load() == 1 {
        500
    } else {
        -100
    }
}

fn main() -> i64 {
    let a = test_unbuffered() ! { _ => -10 }
    let b = test_buffered_mpmc_with_gc() ! { _ => -20 }
    let c = test_channel_cancel_cleanup()
    -- a = 10 + 20 + (42 + 8 + 30 + 40) = 150
    -- b = 1000
    -- c = 500
    a + b + c
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 1650);
}

#[test]
fn std_sync_mutex_rwlock_semaphore_once_lazy_barrier_and_atomics() {
    let source = r#"
use std.collections.*
use std.sync.{Mutex, RwLock, Semaphore, Once, Lazy, Barrier, AtomicInt, AtomicBool}
use std.task

enum InitErr {
    Transient,
}

async effect worker_inc(mu: Mutex, n: i64) -> i64
async effect rw_worker(rw: RwLock, is_writer: bool, delta: i64) -> i64
async effect sem_worker(sem: Semaphore, active: AtomicInt, peak: AtomicInt) -> i64
async effect once_worker(once: Once, lazy_val: Lazy) -> i64
async effect barrier_party(bar: Barrier, phase_counter: AtomicInt, leaders: AtomicInt) -> i64
async effect atomic_worker(ai: AtomicInt, ab: AtomicBool, flips: AtomicInt) -> i64

fn run_mu_workers(mu: Mutex) -> #worker_inc i64 {
    let futs = List()
    var i: i64 = 0
    while i < 8 {
        futs.push(worker_inc(mu, 25)#)
        i += 1
    }
    let _ = futs.await_all()
    mu.with_lock(|v| v).as(i64)
}

fn run_rw_workers(rw: RwLock) -> #rw_worker i64 {
    let r0 = rw_worker(rw, false, 0)#
    let w0 = rw_worker(rw, true, 15)#
    let r1 = rw_worker(rw, false, 0)#
    let w1 = rw_worker(rw, true, 25)#
    let r2 = rw_worker(rw, false, 0)#
    let _ = [r0, w0, r1, w1, r2].await_all()
    rw.with_read(|v| v.as(i64)).as(i64)
}

fn run_sem_workers(sem: Semaphore, active: AtomicInt, peak: AtomicInt) -> #sem_worker i64 {
    let fs = List()
    var s: i64 = 0
    while s < 6 {
        fs.push(sem_worker(sem, active, peak)#)
        s += 1
    }
    let res = fs.await_all()
    res(0) + res(1) + res(2) + res(3) + res(4) + res(5)
}

fn run_once_workers(once: Once, lazy_val: Lazy) -> #once_worker i64 {
    let fs = List()
    var j: i64 = 0
    while j < 8 {
        fs.push(once_worker(once, lazy_val)#)
        j += 1
    }
    let rs = fs.await_all()
    rs(0) + rs(7)
}

fn run_barrier_workers(bar: Barrier, phase_counter: AtomicInt, leaders: AtomicInt) -> #barrier_party i64 {
    let b0 = barrier_party(bar, phase_counter, leaders)#
    let b1 = barrier_party(bar, phase_counter, leaders)#
    let b2 = barrier_party(bar, phase_counter, leaders)#
    let b3 = barrier_party(bar, phase_counter, leaders)#
    let [x0, x1, x2, x3] = [b0, b1, b2, b3].await_all() else { panic("barrier len") }
    x0 + x1 + x2 + x3
}

fn run_atomic_workers(ai: AtomicInt, ab: AtomicBool, flips: AtomicInt) -> #atomic_worker List {
    let fs = List()
    var w: i64 = 0
    while w < 8 {
        fs.push(atomic_worker(ai, ab, flips)#)
        w += 1
    }
    fs.await_all()
}

fn main() -> i64 {
    -- 1. Mutex contention + defer unlock on error
    let mu = Mutex.new(0)
    let err_recovered = mu.with_lock(|cur| -> !InitErr i64 {
        mu.set(cur.as(i64) + 5)
        error InitErr.Transient
    }).as(!InitErr i64) ! {
        InitErr.Transient => if not mu.is_locked() { mu.with_lock(|v| v).as(i64) } else { -999 },
    }

    let mu_sum = run_mu_workers(mu) # {
        async worker_inc(m, count) => {
            var k: i64 = 0
            while k < count {
                m.update(|v| v.as(i64) + 1)
                k += 1
            }
            count
        },
    }

    -- 2. RwLock concurrent readers + writers
    let rw = RwLock.new(10)
    let rw_final = run_rw_workers(rw) # {
        async rw_worker(lock, writer, delta) => {
            if writer {
                lock.with_write(|v| v.as(i64) + delta).as(i64)
            } else {
                var acc: i64 = 0
                var r: i64 = 0
                while r < 10 {
                    acc += lock.with_read(|v| v.as(i64)).as(i64)
                    task.yield_now()
                    r += 1
                }
                acc
            }
        },
    }

    -- 3. Semaphore concurrency bound (max 2 concurrent holders across 6 tasks)
    let sem = Semaphore.new(2)
    let active = AtomicInt.zero()
    let peak = AtomicInt.zero()
    let sem_done = run_sem_workers(sem, active, peak) # {
        async sem_worker(sm, act, pk) => {
            sm.with_permit(|| {
                let now = act.fetch_add(1) + 1
                while true {
                    let prev_pk = pk.load()
                    if now <= prev_pk { break }
                    let (ok, _) = pk.compare_exchange(prev_pk, now)
                    if ok { break }
                }
                task.sleep_ms(4)
                act.fetch_sub(1)
                1
            }).as(i64)
        },
    }

    -- 4. Once (including abort-and-retry on error) & Lazy under 8-task contention
    let once = Once.new()
    let init_calls = AtomicInt.zero()
    let first_attempt = once.call_once(|| -> !InitErr i64 {
        init_calls.fetch_add(1)
        error InitErr.Transient
    }).as(!InitErr i64) ! {
        InitErr.Transient => -1,
    }
    let lazy_calls = AtomicInt.zero()
    let lazy_val = Lazy.new(|| {
        lazy_calls.fetch_add(1)
        task.sleep_ms(3)
        300
    })
    let once_sum = run_once_workers(once, lazy_val) # {
        async once_worker(o, lz) => {
            let a = o.call_once(|| {
                init_calls.fetch_add(1)
                task.sleep_ms(3)
                100
            }).as(i64)
            let b = lz.force().as(i64)
            a + b
        },
    }

    -- 5. Barrier multi-cycle synchronization (4 parties x 3 cycles = 3 leaders)
    let bar = Barrier.new(4)
    let phase_counter = AtomicInt.zero()
    let leaders = AtomicInt.zero()
    let bar_ok = run_barrier_workers(bar, phase_counter, leaders) # {
        async barrier_party(b, pc, ldr) => {
            var cycle: i64 = 0
            while cycle < 3 {
                pc.fetch_add(1)
                if b.wait() {
                    ldr.fetch_add(1)
                }
                cycle += 1
            }
            1
        },
    }

    -- 6. AtomicInt & AtomicBool CAS contention
    let ai = AtomicInt.new(10)
    let ab = AtomicBool.new(false)
    let flips = AtomicInt.zero()
    let _ = run_atomic_workers(ai, ab, flips) # {
        async atomic_worker(aint, abool, fl) => {
            var n: i64 = 0
            while n < 20 {
                aint.fetch_add(2)
                aint.fetch_sub(1)
                let (won, _) = abool.compare_exchange(false, true)
                if won {
                    fl.fetch_add(1)
                    abool.store(false)
                }
                n += 1
            }
            1
        },
    }

    if err_recovered == 5 and
        mu_sum == 205 and
        rw_final == 50 and
        sem_done == 6 and
        peak.load() <= 2 and
        peak.load() >= 1 and
        sem.available() == 2 and
        first_attempt == -1 and
        init_calls.load() == 2 and
        lazy_calls.load() == 1 and
        once_sum == 800 and
        bar_ok == 4 and
        phase_counter.load() == 12 and
        leaders.load() == 3 and
        ai.load() == 170 and
        flips.load() >= 1 {
        4242
    } else {
        -1
    }
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 4242);
}

#[test]
fn std_net_tcp_udp_dns_and_cancellation() {
    let source = r#"
use std.collections.*
use std.io.IoError
use std.net.{TcpListener, TcpStream, UdpSocket, resolve_host}
use std.sync.AtomicInt
use std.task

async effect handle_client(stream: TcpStream) -> !IoError i64
async effect run_server(listener: TcpListener, clients: i64) -> !IoError i64
async effect run_client(port: i64, idx: i64) -> !IoError i64
async effect parked_accept(listener: TcpListener, closed_flag: AtomicInt) -> !IoError i64

fn test_dns() -> !IoError i64 {
    let loopbacks = resolve_host("127.0.0.1")!
    let locals = resolve_host("localhost")!
    if loopbacks.len() >= 1 and loopbacks(0).as(String) == "127.0.0.1" and locals.len() >= 1 {
        10
    } else {
        -10
    }
}

fn run_tcp_cluster(listener: TcpListener, port: i64) -> #[run_server, run_client] !IoError i64 {
    let srv = run_server(listener, 4)#
    let c0 = run_client(port, 1)#
    let c1 = run_client(port, 2)#
    let c2 = run_client(port, 3)#
    let c3 = run_client(port, 4)#
    let client_res = [c0, c1, c2, c3].await_all()
    let sum = client_res(0)! + client_res(1)! + client_res(2)! + client_res(3)!
    let served = srv.await!
    sum + served
}

fn test_tcp_echo() -> !IoError i64 {
    let listener = TcpListener.bind("127.0.0.1", 0)!
    defer { listener.close() }
    let addr = listener.local_addr()!
    let port = addr.port

    let total = run_tcp_cluster(listener, port) # {
        async run_server(lst, n) => {
            let handlers = List()
            var i: i64 = 0
            while i < n {
                let conn = lst.accept()!
                handlers.push(handle_client(conn)#)
                i += 1
            }
            let done = handlers.await_all()
            done(0)! + done(1)! + done(2)! + done(3)!
        },
        async handle_client(conn) => {
            defer { conn.close() }
            conn.set_nodelay(true)!
            let peer = conn.peer_addr()!
            if peer.port <= 0 {
                return 0
            }
            let line_opt = conn.read_line()!
            let line = line_opt match {
                s? => s,
                null => "",
            }
            conn.write_line("echo:" ++ line)!
            1
        },
        async run_client(p, id) => {
            let stream = TcpStream.connect("127.0.0.1", p)!
            defer { stream.close() }
            stream.set_nodelay(true)!
            let msg = "ping-" ++ id.to_string()
            stream.write_line(msg)!
            let reply_opt = stream.read_line()!
            let reply = reply_opt match {
                s? => s,
                null => "",
            }
            let eof_line = stream.read_line()!
            if reply == ("echo:" ++ msg) and eof_line == null {
                id * 10
            } else {
                -999
            }
        },
    }
    -- sum = 10 + 20 + 30 + 40 = 100, served = 4 => 104
    total!
}

fn test_udp_roundtrip() -> !IoError i64 {
    let a = UdpSocket.bind("127.0.0.1", 0)!
    defer { a.close() }
    let b = UdpSocket.bind("127.0.0.1", 0)!
    defer { b.close() }

    let a_addr = a.local_addr()!
    let b_addr = b.local_addr()!

    a.send_to_str("127.0.0.1", b_addr.port, "hello-udp")!
    let pkt = b.recv_from()!
    if pkt.text() != "hello-udp" or pkt.addr.port != a_addr.port {
        return -50
    }
    b.send_to_bytes("127.0.0.1", a_addr.port, [65, 66, 67])!
    let reply = a.recv_from()!
    if reply.text() == "ABC" and reply.data.len() == 3 {
        200
    } else {
        -60
    }
}

fn test_socket_accept_cancellation() -> !IoError i64 {
    let listener = TcpListener.bind("127.0.0.1", 0)!
    let closed_flag = AtomicInt.zero()
    let outcome = task.with_timeout(15, || -> !IoError i64 {
        defer {
            listener.close()
            closed_flag.fetch_add(1)
        }
        let _ = listener.accept()!
        0
    }) ! {
        _ => 300,
    }
    if outcome == 300 and closed_flag.load() == 1 {
        300
    } else {
        -300
    }
}

fn main() -> i64 {
    let d = test_dns() ! { _ => -1 }
    let t = test_tcp_echo() ! { _ => -2 }
    let u = test_udp_roundtrip() ! { _ => -3 }
    let c = test_socket_accept_cancellation() ! { _ => -4 }
    -- 10 + 104 + 200 + 300 = 614
    d + t + u + c
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 614);
}

#[test]
fn std_task_sleep_timeout_introspection_and_static_combinator_typing() {
    let source = r#"
use std.collections.*
use std.sync.AtomicInt
use std.task
use std.task.{Left, Right, TimeoutError}
use std.time.Duration

enum BizErr {
    Rejected,
}

async effect compute_int(ms: i64, val: i64) -> i64
async effect compute_str(ms: i64, text: String) -> String

fn fallible_work(should_fail: bool, ms: i64, cleaned: AtomicInt) -> !BizErr i64 {
    defer { cleaned.fetch_add(1) }
    if ms > 0 {
        task.sleep_ms(ms)
    }
    if should_fail {
        error BizErr.Rejected
    } else {
        55
    }
}

fn run_typed_combinators() -> #[compute_int, compute_str] i64 {
    -- 1. await_all literal + indexing + .get() + list destructuring WITHOUT .as(...)
    let f1 = compute_int(2, 10)#
    let f2 = compute_int(1, 20)#
    let f3 = compute_int(3, 30)#
    let all_res = [f1, f2, f3].await_all()
    let indexed_sum: i64 = all_res(0) + all_res.get(1) + all_res(2)

    let [d0, d1, d2] = [compute_int(1, 4)#, compute_int(1, 5)#, compute_int(1, 6)#].await_all() else {
        panic("expected 3 items")
    }
    let destructured_sum: i64 = d0 + d1 + d2

    -- 2. Tracked List() + .push(fut) + task.await_all(xs) WITHOUT .as(...)
    let dyn_futs = List()
    dyn_futs.push(compute_int(1, 7)#)
    dyn_futs.push(compute_int(1, 8)#)
    let pushed_res = task.await_all(dyn_futs)
    let pushed_sum: i64 = pushed_res(0) + pushed_res.get(1)

    -- 3. await_any WITHOUT .as(...)
    let any_a: i64 = [compute_int(40, 999)#, compute_int(1, 11)#].await_any()
    let any_b: i64 = task.await_any([compute_int(40, 888)#, compute_int(1, 12)#])

    -- 4. await_first pattern matching AND unwrap_left/unwrap_right WITHOUT .as(...)
    let race1 = task.await_first(compute_int(1, 100)#, compute_str(40, "slow")#)
    let matched_val: i64 = race1 match {
        Left(num) => num + 5,
        Right(txt) => txt.len(),
    }
    let race2 = task.await_first(compute_int(40, 0)#, compute_str(1, "hello")#)
    let unwrapped_len: i64 = race2.unwrap_right().len()

    indexed_sum + destructured_sum + pushed_sum + any_a + any_b + matched_val + unwrapped_len
}

fn main() -> i64 {
    let root_id = task.current_id()
    task.sleep(Duration.from_millis(2))
    let snap = task.tree_snapshot()
    let snap_ok = snap.len() > 0 and root_id >= 0

    let cleaned = AtomicInt.zero()

    -- Fast timeout succeeds, statically typed as ![TimeoutError, BizErr] i64!
    let fast_res: ![TimeoutError, BizErr] i64 = task.with_timeout(200, || fallible_work(false, 1, cleaned))
    let fast_val = fast_res ! {
        TimeoutError { millis } => -millis,
        BizErr.Rejected => -2,
    }

    -- Business error propagates cleanly as BizErr before deadline!
    let err_val = task.with_timeout(200, || fallible_work(true, 1, cleaned)) ! {
        TimeoutError { millis } => -millis,
        BizErr.Rejected => 22,
    }

    -- Slow work times out and runs defer cleanup on cancelled child task!
    let timeout_val = task.with_timeout(12, || fallible_work(false, 250, cleaned)) ! {
        TimeoutError { millis } => millis,
        BizErr.Rejected => -3,
    }

    let child_ids = AtomicInt.zero()
    let comb_total = run_typed_combinators() # {
        async compute_int(ms, val) => {
            if task.current_id() != root_id {
                child_ids.fetch_add(1)
            }
            task.sleep_ms(ms)
            val
        },
        async compute_str(ms, text) => {
            task.sleep_ms(ms)
            text
        },
    }

    -- indexed_sum=60, destructured_sum=15, pushed_sum=15, any_a=11, any_b=12, matched_val=105, unwrapped_len=5 => 223
    -- fast_val=55, err_val=22, timeout_val=12, cleaned=3 => 223 + 55 + 22 + 12 = 312
    if snap_ok and cleaned.load() == 3 and child_ids.load() >= 1 {
        comb_total + fast_val + err_val + timeout_val
    } else {
        -1
    }
}
"#;
    assert_eq!(common::run_value(source).unwrap(), 312);
}

#[test]
fn std_task_combinators_reject_non_future_elements_at_compile_time() {
    assert_rejected_msg(
        r#"
use std.collections.*
fn main() -> List {
    [123].await_all()
}
"#,
        "`await_all` requires `Future` elements, found `i64`",
    );

    assert_rejected_msg(
        r#"
use std.task
fn main() -> Any {
    task.await_any(["not-a-future"])
}
"#,
        "`await_any` requires `Future` elements, found `String`",
    );

    assert_rejected_msg(
        r#"
use std.collections.*
use std.task
fn main() -> List {
    let xs = List()
    xs.push(true)
    task.await_all(xs)
}
"#,
        "`await_all` requires `Future` elements, found `bool`",
    );

    assert_rejected_msg(
        r#"
use std.task
fn main() -> Any {
    task.await_first(1, 2)
}
"#,
        "Future",
    );

    assert_rejected_msg(
        r#"
use std.task
fn main() -> Any {
    task.with_timeout(10, |x: i64| x)
}
"#,
        "with_timeout",
    );
}
