//! Integration tests for idiomatic Nessa standard-library APIs:
//! - First-class `Type` parsing (`String.parse`, `String.try_parse`, `conv.parse`) & static rejection
//! - First-class `Type` JSON decoding (`json.decode(T, ...)`) & `json.object { ... }`
//! - Nominal `Path`, `OpenMode`, `File.with_open` (`!Error` callback propagation), `FileKind`, `fs.iter_dir`
//! - Nominal `Command` & `Command.spec { ... }`
//! - Capability effect handlers (`io.capture_output`, `time.with_mock_clock`, `rand.with_seeded_rng`)
//! - Nominal `Weekday`, `DateTime` defaults, `CoroutineStep`, `task.{par_map, par_for_each, race}`, `Barrier.wait()!`

mod common;

#[test]
fn first_class_type_parse_and_json_decode_refine_static_types_and_reject_invalid_targets() {
    let result = common::run_value(
        r#"use std.conv
use std.json

struct Endpoint {
    host: String,
    port: i64 = 8080,
}
derive Eq, PartialEq, Display, Json for Endpoint

fn run_parse_and_json_checks() -> ![ParseError, JsonError] i64 {
    let a: i64 = "-42".parse(i64)!
    let b: u16 = conv.parse(u16, "ff", radix = 16)!
    let c: f64 = "3.5".parse(f64)!
    let d: bool = "true".parse(bool)!
    let ch: char = "λ".parse(char)!
    assert_eq(a, -42)
    assert_eq(b, (255).as(u16))
    assert(c.approx_eq(3.5))
    assert(d)
    assert_eq(ch, 'λ')

    assert_eq("  -42 ".try_parse(i64), null)
    assert_eq("not_an_int".try_parse(i64), null)
    let over_u8 = "300".parse(u8) ! {
        ParseError.Overflow(_) => (9).as(u8),
        _ => (0).as(u8),
    }
    assert_eq(over_u8, (9).as(u8))

    let payload = json.stringify(json.object { host: "127.0.0.1", port: 9000 })!
    let ep: Endpoint = json.decode(Endpoint, payload)!
    assert_eq(ep, Endpoint { host: "127.0.0.1", port: 9000 })
    ep.port + a
}

fn main() -> i64 {
    run_parse_and_json_checks() ! { _ => -1 }
}
"#,
    );
    assert_eq!(result, Ok(8958));

    // Compile-time rejection when passing non-scalar type to `.parse(...)` or non-`Json` struct to `json.decode(...)`.
    let bad = driver::Driver::new().compile(
        r#"use std.json
struct NotJson { x: i64 }
fn main() {
    let _ = "123".try_parse(List)
    let _ = json.decode(NotJson, "{{}}")
}
"#,
    );
    assert!(bad.has_errors);
    assert!(
        bad.diagnostics
            .iter()
            .any(|d| d.message.contains("cannot parse `List`")),
        "expected parse target diagnostic, got: {:?}",
        bad.diagnostics
    );
    assert!(
        bad.diagnostics
            .iter()
            .any(|d| d.message.contains("does not support `json.decode`")),
        "expected json.decode target diagnostic, got: {:?}",
        bad.diagnostics
    );
}

#[test]
fn nominal_path_io_fs_command_and_capability_effects_work_end_to_end() {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temp_dir = loop {
        let candidate = std::env::temp_dir().join(format!(
            "nessa-idiomatic-stdlib-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        match std::fs::create_dir(&candidate) {
            Ok(()) => break candidate,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => panic!("tempdir: {e}"),
        }
    };
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _guard = Cleanup(temp_dir.clone());
    let root = temp_dir.to_string_lossy().replace('\\', "/");

    let source = format!(
        r#"use std.coroutine.{{Coroutine, CoroutineStatus, CoroutineStep}}
use std.fs
use std.fs.FileKind
use std.io
use std.process.Command
use std.rand
use std.time
use std.time.{{DateTime, Duration, Weekday}}

enum AppError {{
    BadHeader,
}}

fn read_checked_header(p: Path) -> ![IoError, AppError] String {{
    File.with_open(p, mode = .read) do |f| {{
        let first = f.read_line()! match {{
            line? => line,
            _ => "",
        }}
        if first != "NESSA_OK" {{
            return error AppError.BadHeader
        }}
        first
    }}!
}}

fn run_io_fs_checks() -> ![IoError, AppError] i64 {{
    let base = Path.new("{root}")
    let sub = base.join("nested")
    fs.create_dir(sub, parents = true)!
    assert_eq(fs.metadata(sub)!.kind, FileKind.directory)

    let data_file = sub.join("sample.txt").with_extension("log")
    assert_eq(data_file.extension(), "log")
    assert_eq(data_file.stem(), "sample")

    File.with_open(data_file, mode = .write) do |f| {{
        let _ = f.write("NESSA_OK\nline2\nline3\n")!
    }}!

    assert_eq(read_checked_header(data_file)!, "NESSA_OK")
    let line_count = File.with_open(data_file) do |f| {{
        f.lines().count()
    }}!
    assert_eq(line_count, (3).as(usize))

    let entries = fs.iter_dir(sub)!.collect()
    assert_eq(entries.len(), 1)
    assert_eq(entries(0).as(fs.DirEntry).kind, FileKind.file)

    -- Capability effect handlers: `io.capture_output`, `time.with_mock_clock`, `rand.with_seeded_rng`
    let (captured, v1): (String, i64) = io.capture_output(|| -> #io.console_println i64 {{
        io.console_println("hello") #
        io.console_println("world") #
        7
    }})
    assert_eq(captured, "hello\nworld\n")
    assert_eq(v1, 7)

    let (end_ms, span): (i64, i64) = time.with_mock_clock(initial_ms = 1000, || -> #[time.clock_now_ms, time.clock_sleep] i64 {{
        let t0 = time.clock_now_ms()#
        time.clock_sleep(Duration.from_millis(250))#
        let t1 = time.clock_now_ms()#
        t1 - t0
    }})
    assert_eq(end_ms, 1250)
    assert_eq(span, 250)

    let r_a: u64 = rand.with_seeded_rng(12345, || -> #rand.random_u64 u64 {{ rand.random_u64()# }})
    let r_b: u64 = rand.with_seeded_rng(12345, || -> #rand.random_u64 u64 {{ rand.random_u64()# }})
    assert_eq(r_a, r_b)

    -- Nominal DateTime defaults + Weekday + CoroutineStep
    let dt = DateTime {{ year: 1970, month: 1, day: 1 }}
    assert_eq(dt.to_unix(utc = true), 0)
    assert_eq(DateTime.from_unix(0, utc = true).weekday(), Weekday.thursday)

    let co = Coroutine.new(|x| {{
        let back = Coroutine.yield_val(value = x.as(i64) + 10)
        back.as(i64) * 2
    }})
    assert_eq(co.status(), CoroutineStatus.suspended)
    let s1 = co.step(arg = 5) ! {{ _ => panic("step1") }}
    assert_eq(s1, CoroutineStep.yielded(15))
    let s2 = co.step(arg = 21) ! {{ _ => panic("step2") }}
    assert_eq(s2, CoroutineStep.finished(42))
    assert_eq(co.status(), CoroutineStatus.dead)

    -- Command builder validation (without depending on external OS binaries)
    let cmd = Command.spec {{ "nessa-bin", "--check", cwd: base }}!
    assert_eq(cmd.program, "nessa-bin")
    assert_eq(cmd.argv, ["--check"])

    fs.remove(sub, recursive = true)!
    assert(not fs.exists(sub))
    77
}}

fn main() -> i64 {{
    run_io_fs_checks() ! {{ _ => -1 }}
}}
"#
    );
    assert_eq!(common::run_value(&source), Ok(77));
}

#[test]
fn structured_task_par_map_race_and_sync_checked_barrier_work() {
    let result = common::run_value(
        r#"use std.sync.{Barrier, Mutex, SyncError}
use std.task

enum WorkErr {
    Boom(at: i64),
}

fn run_task_checks() -> ![WorkErr, SyncError] i64 {
    let squares = task.par_map([1, 2, 3, 4, 5]) do |n| {
        n * n
    }
    assert_eq(squares, [1, 4, 9, 16, 25])

    let failed = task.par_map([2, 4, 6]) do |n| -> !WorkErr i64 {
        if n == 4 {
            return error WorkErr.Boom(n)
        }
        n * 10
    } ! {
        WorkErr.Boom(v) => [v],
    }
    assert_eq(failed, [4])

    let acc = Mutex.new(0)
    task.par_for_each([10, 20, 30]) do |x| {
        let _: i64 = acc.update(|cur| cur.as(i64) + x)
    }
    assert_eq(acc.with_lock(|v| v.as(i64)), 60)

    let winner: String = task.race([
        || -> String {
            task.sleep_ms(100)
            "slow"
        },
        || -> String {
            "fast"
        },
    ])
    assert_eq(winner, "fast")

    let bar = Barrier.new(1)
    assert(bar.wait()!)
    60
}

fn main() -> i64 {
    run_task_checks() ! { _ => -1 }
}
"#,
    );
    assert_eq!(result, Ok(60));
}
