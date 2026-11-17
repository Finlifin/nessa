//! Full cross-process CLI `.nsbc` archive roundtrip, source-deleted execution, disassembly,
//! and corrupted-archive rejection tests across Waves 1–5 language features and `std` modules.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct TempWorkspace(PathBuf);

impl TempWorkspace {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let dir = std::env::temp_dir().join(format!(
                "nessa-wave6-archive-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&dir) {
                Ok(()) => return Self(dir),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("cannot create temp workspace: {e}"),
            }
        }
    }

    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn nessa(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nessa"))
        .args(args)
        .output()
        .unwrap()
}

fn assert_ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "status={} stdout={} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn language_features_and_async_concurrency_execute_from_standalone_nsbc_without_sources() {
    let ws = TempWorkspace::new();
    let src_path = ws.write(
        "app.ns",
        r##"
use std.collections.{HashMap, Set}
use std.task
use std.task.{left, right}

newtype UserId = i64
derive Eq, PartialEq, Ord, PartialOrd, Display, Hash for UserId

trait Describable(Display) {
    derive fn describe(self) -> String {
        "item:" ++ self.to_string()
    }
}

struct Record {
    id: UserId
    label: String
}
derive Eq, PartialEq, Display, Hash, Describable for Record

effect read_bias() -> i64
async effect compute_chunk(seed: i64) -> i64
async effect slow_spin() -> i64

struct WorkerCap {
    bias: i64
}

impl WorkerCap {
    pub fn get_bias(self) -> i64 handles(read_bias) = self.bias
    pub fn run_chunk(self, seed: i64) -> i64 handles(compute_chunk) {
        task.yield_now()
        seed * 10
    }
}

fn run_pipeline() -> #[read_bias, compute_chunk] i64 {
    let b = read_bias()#
    let f0: Future(i64) = compute_chunk(1)#
    let f1: Future(i64) = compute_chunk(2)#
    let f2: Future(i64) = compute_chunk(3)#
    let v0 = f0.await
    let parts = [f1, f2].await_all()
    b + v0 + parts.get(0).as(i64) + parts.get(1).as(i64)
}

fn race_cancel() -> i64 {
    handles async compute_chunk(seed: i64) -> i64 => seed + 5
    handles async slow_spin() -> i64 => {
        while true {
            task.yield_now()
        }
        0
    }
    let fast = compute_chunk(4)#
    let spin = slow_spin()#
    task.await_first(fast, spin) match {
        left(v) => v.as(i64),
        right(_) => -999,
    }
}

fn main() {
    let uid = UserId(7)
    let rec = Record { id: uid, label: "alpha" }
    let map = HashMap()
    map.set(rec, 100)
    let s = Set()
    s.insert(uid)

    let cap = WorkerCap { bias: 8 }
    let total = run_pipeline().use(cap)
    let won = race_cancel()
    let tag = when {
        total == 68 and won == 9 and s.contains(UserId(7)) => map.get(rec).as(i64)
        else => -1
    }
    println("{rec.describe()}|{total}|{won}|{tag}")
}
"##,
    );
    let archive_path = ws.0.join("app.nsbc");

    assert_ok(&nessa(&["build", src_path.to_str().unwrap()]));
    fs::remove_file(&src_path).unwrap();
    assert!(!src_path.exists());

    let disasm = assert_ok(&nessa(&["disasm", archive_path.to_str().unwrap()]));
    assert!(disasm.contains("AwaitFuture"), "{disasm}");
    assert!(disasm.contains("AwaitPopHandler"), "{disasm}");

    let stdout = assert_ok(&nessa(&["run", archive_path.to_str().unwrap()]));
    assert_eq!(
        stdout.trim(),
        "item:Record { id: UserId(7), label: \"alpha\" }|68|9|100"
    );
}

#[test]
fn full_stdlib_modules_execute_from_standalone_nsbc_with_cli_args() {
    let ws = TempWorkspace::new();
    let data_file = ws.0.join("payload.txt");
    let src_path = ws.write(
        "stdlib_check.ns",
        r#"
use std.bytes
use std.collections.{Deque}
use std.coroutine
use std.env
use std.fmt
use std.fs
use std.io
use std.json
use std.math
use std.path
use std.pattern
use std.rand
use std.runtime
use std.time

fn main() {
    let args = env.args()
    let target_file = args.get(1).as(String)

    -- fmt + pattern + string UTF-8 + bytes pack/unpack
    let formatted = fmt.format("%s:%04d", "ok", 42)
    let matched = pattern.match_once("id=987!", "id=(%d+)")
    let packed = bytes.pack("<i2I2", -12, 500)! { _ => List() }
    let (unpacked_vals, _) = bytes.unpack("<i2I2", packed)! { _ => (List(), 1) }

    -- math + deterministic rand
    let _ = rand.seed(s1 = 12345)
    let r1 = rand.random_int(1, 1000)
    let _ = rand.seed(s1 = 12345)
    let r2 = rand.random_int(1, 1000)
    let trig_ok = (math.sin(0.0) == 0.0) and (r1 == r2)

    -- fs + io + path + json
    let payload_obj = Map()
    payload_obj.set("msg", formatted)
    payload_obj.set("num", unpacked_vals.get(1))
    let encoded = json.stringify(payload_obj)! { _ => "{{}}" }
    fs.write_string(target_file, encoded)! { _ => () }
    let loaded_raw = fs.read_to_string(target_file)! { _ => "" }
    let decoded = json.parse(loaded_raw)! { _ => Map() }
    let base_name = path.file_name(target_file)

    -- coroutine + Deque + time + runtime
    let co = coroutine.create(|x: Any| {
        let mid = coroutine.yield_val(value = x.as(i64) + 10)
        mid.as(i64) * 2
    })
    let first_yield = co.try_resume(arg = 5)! { _ => -1 }
    let second_ret = co.try_resume(arg = first_yield)! { _ => -1 }

    let dq = Deque()
    dq.push_back(second_ret)
    dq.push_front(first_yield)

    let t0 = time.monotonic_nanos()
    let gc_count = runtime.gc_collections()
    let bt = runtime.backtrace(message = "probe")
    let t1 = time.monotonic_nanos()

    let ok = matched == "987"
        and unpacked_vals.get(0).as(i64) == -12
        and trig_ok
        and decoded.as(Map).get("msg") == "ok:0042"
        and decoded.as(Map).get("num") == 500
        and base_name == "payload.txt"
        and dq.pop_front() == 15
        and dq.pop_front() == 30
        and t1 >= t0
        and gc_count >= 0
        and bt.contains("probe")

    println("{ok}")
}
"#,
    );
    let archive_path = ws.0.join("stdlib_check.nsbc");

    assert_ok(&nessa(&["build", src_path.to_str().unwrap()]));
    fs::remove_file(&src_path).unwrap();

    let stdout = assert_ok(&nessa(&[
        "run",
        archive_path.to_str().unwrap(),
        "--",
        data_file.to_str().unwrap(),
    ]));
    assert_eq!(stdout.trim(), "true");
}

#[test]
fn corrupted_future_metadata_archive_is_rejected_cleanly_without_panic() {
    let ws = TempWorkspace::new();
    let src_path = ws.write(
        "fut.ns",
        r#"
async effect ping() -> i64
fn main() {
    let v = (ping()#.await) # { async ping() => 1 }
    println("{v}")
}
"#,
    );
    let archive_path = ws.0.join("fut.nsbc");
    assert_ok(&nessa(&["build", src_path.to_str().unwrap()]));

    // Truncate the archive mid-stream and verify `nessa run` exits non-zero with a clean error, never panicking.
    let mut bytes = fs::read(&archive_path).unwrap();
    bytes.truncate(bytes.len() / 2);
    let broken_path = ws.0.join("broken.nsbc");
    fs::write(&broken_path, &bytes).unwrap();

    let out = nessa(&["run", broken_path.to_str().unwrap()]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("panicked at"),
        "CLI panicked on corrupted archive: {stderr}"
    );
}
