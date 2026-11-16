//! Comprehensive integration tests for Wave 5 standard library modules:
//! `std.io`, `std.fs`, `std.path`, `std.env`, `std.time`, `std.process`,
//! `std.collections`, `std.coroutine`, `std.runtime`, and `std.json`.

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

struct TempDirGuard {
    path: PathBuf,
}

impl TempDirGuard {
    fn new(label: &str) -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("nessa_wave5_{label}_{}_{}", std::process::id(), id));
        std::fs::create_dir_all(&dir).expect("create isolated temp dir");
        Self { path: dir }
    }

    fn nessa_str(&self) -> String {
        self.path.to_string_lossy().replace('\\', "/")
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn io_tonumber_warn_and_streams_cover_bases_floats_and_errors() {
    let src = r#"
        use std.io

        fn main() -> i64 {
            assert_eq(tonumber(42), 42)
            assert_eq(tonumber(-3.5), -3.5)
            assert_eq(tonumber("  123  "), 123)
            assert_eq(tonumber(" -0xff "), -255)
            assert_eq(tonumber("0b1010"), 10)
            assert_eq(tonumber("0o77"), 63)
            assert_eq(tonumber("  3.25 "), 3.25)
            assert_eq(tonumber("-1.5e2"), -150.0)
            assert_eq(tonumber("1e9999"), null)
            assert_eq(tonumber(""), null)
            assert_eq(tonumber("   "), null)
            assert_eq(tonumber("12x"), null)
            assert_eq(tonumber(true), null)

            assert_eq(tonumber("ff", base = 16), 255)
            assert_eq(tonumber("  -1011 ", base = 2), -11)
            assert_eq(tonumber("z", base = 36), 35)
            assert_eq(tonumber("zz", base = 10), null)
            assert_eq(tonumber(42, base = 16), null)

            eprint("")
            eprintln("wave5 stderr check")
            warn("part1-", 42)
            io.flush_stdout().unwrap()
            io.flush_stderr().unwrap()
            assert(io.stdin().is_open())
            assert(io.stdout().is_open())
            assert(io.stderr().is_open())
            100
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 100);

    let invalid_base = r#"
        fn main() -> i64 {
            let _ = tonumber("10", base = 1)
            0
        }
    "#;
    assert!(common::run_value(invalid_base).is_err());
}

#[test]
fn io_file_read_write_seek_temp_lines_and_error_variants() {
    let tmp = TempDirGuard::new("io_file");
    let root = tmp.nessa_str();
    let src = format!(
        r#"
        use std.io.{{File, IoError, SeekFrom, open_file, read_lines}}

        fn main() -> i64 {{
            let file_path = "{root}/sample.txt"

            -- Missing file returns NotFound
            let missing_ok = File.open("{root}/does_not_exist.txt") match {{
                _! => false,
                error IoError.NotFound(_) => true,
                error _ => false,
            }}
            assert(missing_ok)

            -- Create, write, flush, seek, and read back
            let f = File.create(file_path).unwrap()
            assert(f.is_open())
            assert_eq(f.to_string(), "file (open)")
            f.write("alpha\n").unwrap()
            f.write_line("beta").unwrap()
            f.write("12.5 -7").unwrap()
            f.flush().unwrap()
            f.close().unwrap()
            assert(not f.is_open())
            assert_eq(f.to_string(), "file (closed)")

            -- Reading from closed handle returns Closed
            let closed_ok = f.read_to_string() match {{
                _! => false,
                error IoError.Closed(_) => true,
                error _ => false,
            }}
            assert(closed_ok)

            -- Exclusive wx on existing file returns AlreadyExists
            let exists_ok = File.open(file_path, mode = "wx") match {{
                _! => false,
                error IoError.AlreadyExists(_) => true,
                error _ => false,
            }}
            assert(exists_ok)

            -- Line, char, number, and seek operations
            let r = open_file(file_path, mode = "r").unwrap()
            assert_eq(r.read_line().unwrap(), "alpha")
            assert_eq(r.read_line(keep_newline = true).unwrap(), "beta\n")
            assert_eq(r.read_number().unwrap(), 12.5)
            assert_eq(r.read_number().unwrap(), -7.0)
            assert_eq(r.read_line().unwrap(), null)

            assert_eq(r.seek(SeekFrom.start(0)).unwrap(), 0)
            assert_eq(r.position().unwrap(), 0)
            assert_eq(r.read_chars(5).unwrap(), "alpha")
            let _ = r.seek(SeekFrom.current(1)).unwrap()
            assert_eq(r.read_chars(4).unwrap(), "beta")
            let end_pos = r.seek(SeekFrom.end(0)).unwrap()
            assert(end_pos > 10)
            r.close().unwrap()

            -- read_lines auto-closes on EOF
            let collected = List()
            let iter = read_lines(file_path).unwrap()
            for line in iter {{
                collected.push(line)
            }}
            assert_eq(collected, ["alpha", "beta", "12.5 -7"])

            -- Binary read/write and File.temp()
            let bin_path = "{root}/raw.bin"
            let bw = File.open(bin_path, mode = "wb").unwrap()
            bw.write_bytes([0, 65, 66, 255]).unwrap()
            bw.close().unwrap()

            let br = File.open(bin_path, mode = "rb").unwrap()
            assert_eq(br.read_bytes(max_bytes = 2).unwrap(), [0, 65])
            assert_eq(br.read_bytes().unwrap(), [66, 255])
            br.close().unwrap()

            let tf = File.temp().unwrap()
            tf.write("scratch").unwrap()
            assert_eq(tf.seek(SeekFrom.start(0)).unwrap(), 0)
            assert_eq(tf.read_to_string().unwrap(), "scratch")
            tf.close().unwrap()

            42
        }}
        "#
    );
    assert_eq!(common::run_value(&src).unwrap(), 42);
}

#[test]
fn fs_operations_cover_files_directories_metadata_copy_rename_and_errors() {
    let tmp = TempDirGuard::new("fs_ops");
    let root = tmp.nessa_str();
    let src = format!(
        r#"
        use std.fs
        use std.io.IoError

        fn main() -> i64 {{
            let sub = "{root}/nested/dir"
            fs.create_dir_all(sub).unwrap()
            assert(fs.exists(sub))
            assert(fs.is_dir(sub))
            assert(not fs.is_file(sub))

            let p1 = sub ++ "/hello.txt"
            fs.write_string(p1, "hello").unwrap()
            fs.append_string(p1, " world").unwrap()
            assert(fs.exists(p1))
            assert(fs.is_file(p1))
            assert_eq(fs.read_to_string(p1).unwrap(), "hello world")

            let meta = fs.metadata(p1).unwrap()
            assert_eq(meta.size, 11)
            assert(meta.is_file)
            assert(not meta.is_dir)
            assert(meta.modified_unix_secs > 0)

            -- Copy and rename
            let p2 = sub ++ "/copied.txt"
            let copied_bytes = fs.copy(p1, p2).unwrap()
            assert_eq(copied_bytes, 11)
            let p3 = sub ++ "/renamed.txt"
            fs.rename(p2, p3).unwrap()
            assert(not fs.exists(p2))
            assert_eq(fs.read_to_string(p3).unwrap(), "hello world")

            -- Binary helpers
            let bin = sub ++ "/bytes.bin"
            fs.write_bytes(bin, [10, 20, 30, 40]).unwrap()
            assert_eq(fs.read_bytes(bin).unwrap(), [10, 20, 30, 40])

            -- Directory listing & canonicalize
            let names = fs.read_dir(sub).unwrap()
            names.sort()
            assert_eq(names, ["bytes.bin", "hello.txt", "renamed.txt"])
            let canon = fs.canonicalize(p1).unwrap()
            assert(canon.ends_with("hello.txt"))

            -- Type-checked remove_file vs remove_dir
            let dir_as_file_err = fs.remove_file(sub) match {{
                _! => false,
                error IoError.InvalidInput(_) => true,
                error _ => false,
            }}
            assert(dir_as_file_err)

            let file_as_dir_err = fs.remove_dir(p1) match {{
                _! => false,
                error IoError.InvalidInput(_) => true,
                error _ => false,
            }}
            assert(file_as_dir_err)

            fs.remove_file(p1).unwrap()
            assert(not fs.exists(p1))
            fs.remove(bin).unwrap()
            assert(not fs.exists(bin))

            let gen_tmp = fs.temp_file_path(prefix = "wave5_").unwrap()
            assert(gen_tmp.contains("wave5_"))

            fs.remove_dir_all("{root}/nested").unwrap()
            assert(not fs.exists("{root}/nested"))
            77
        }}
        "#
    );
    assert_eq!(common::run_value(&src).unwrap(), 77);
}

#[test]
fn path_manipulation_handles_lexical_normalization_stems_extensions_and_roots() {
    let src = r#"
        use std.path

        fn main() -> i64 {
            assert_eq(path.SEPARATOR, "/")
            assert(path.is_absolute("/usr/local"))
            assert(path.is_absolute("C:/Windows/System32"))
            assert(not path.is_absolute("relative/file.txt"))
            assert(not path.is_absolute(""))

            assert_eq(path.join("a/b", "c.txt"), "a/b/c.txt")
            assert_eq(path.join("a/b/", "c.txt"), "a/b/c.txt")
            assert_eq(path.join("a/b", "/etc/hosts"), "/etc/hosts")
            assert_eq(path.join("", "c.txt"), "c.txt")
            assert_eq(path.join("a/b", ""), "a/b")
            assert_eq(path.join_all(["usr", "local", "bin", "nessa"]), "usr/local/bin/nessa")

            assert_eq(path.parent("/usr/local/bin"), "/usr/local")
            assert_eq(path.parent("/usr"), "/")
            assert_eq(path.parent("/"), null)
            assert_eq(path.parent("single"), null)
            assert_eq(path.parent("a/b/"), "a")

            assert_eq(path.file_name("/usr/local/nessa.tar.gz"), "nessa.tar.gz")
            assert_eq(path.file_name("/usr/local/"), "local")
            assert_eq(path.file_name("/"), null)
            assert_eq(path.file_name(".."), null)

            assert_eq(path.file_stem("/usr/local/nessa.tar.gz"), "nessa.tar")
            assert_eq(path.extension("/usr/local/nessa.tar.gz"), "gz")
            assert_eq(path.file_stem(".gitignore"), ".gitignore")
            assert_eq(path.extension(".gitignore"), null)
            assert_eq(path.file_stem("Makefile"), "Makefile")
            assert_eq(path.extension("Makefile"), null)

            assert_eq(path.with_extension("dir/main.ns", "nsbc"), "dir/main.nsbc")
            assert_eq(path.with_extension("dir/main.ns", ".json"), "dir/main.json")
            assert_eq(path.with_extension("dir/main.ns", ""), "dir/main")
            assert_eq(path.with_extension("dir/.gitignore", "bak"), "dir/.gitignore.bak")

            assert_eq(path.normalize("/a/b/../c/./d//e/"), "/a/c/d/e")
            assert_eq(path.normalize("/../../etc"), "/etc")
            assert_eq(path.normalize("a/../../b"), "../b")
            assert_eq(path.normalize(""), ".")
            assert_eq(path.normalize("./."), ".")

            assert_eq(path.components("/a/b/../c"), ["/", "a", "b", "..", "c"])
            assert_eq(path.components("x//y/z/"), ["x", "y", "z"])
            55
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 55);
}

#[test]
fn env_time_process_and_runtime_modules_work_end_to_end() {
    let src = r#"
        use std.env
        use std.io.IoError
        use std.process
        use std.runtime
        use std.time
        use std.time.{DateTime, Duration, Instant}

        struct Tracer {}

        impl Tracer {
            pub fn capture(self) -> String {
                runtime.backtrace(message = "trace-marker", skip = 0)
            }
        }

        fn main() -> i64 {
            -- env
            assert(not env.os().is_empty())
            assert(not env.arch().is_empty())
            let cli_args = env.args()
            assert(cli_args.len() >= 0)
            assert_eq(env.get("__NESSA_NONEXISTENT_ENV_KEY_999__"), null)
            assert_eq(env.get_or("__NESSA_NONEXISTENT_ENV_KEY_999__", "fallback"), "fallback")
            let all_vars = env.vars()
            assert(all_vars.len() >= 0)
            let cwd = env.current_dir().unwrap()
            assert(not cwd.is_empty())
            env.set_current_dir(cwd).unwrap()
            let bad_cwd = env.set_current_dir("/nonexistent_nessa_wave5_dir_xyz") match {
                _! => false,
                error IoError.NotFound(_) => true,
                error _ => false,
            }
            assert(bad_cwd)

            -- time
            assert(time.unix_secs() > 1700000000)
            assert(time.unix_millis() > 1700000000000)
            assert_eq(time.difftime(100, 40), 60.0)
            assert(time.cpu_secs() >= 0.0)

            let t0 = Instant.now()
            time.sleep_ms(2)
            let elapsed = t0.elapsed()
            assert(elapsed.as_nanos() >= 0)
            assert_eq(Duration.from_secs(3).as_millis(), 3000)
            assert_eq(Duration.from_millis(1500).as_secs_f64(), 1.5)
            assert(Duration.from_millis(5) < Duration.from_millis(10))

            let epoch = DateTime.from_unix_utc(0)
            assert_eq(epoch.year, 1970)
            assert_eq(epoch.month, 1)
            assert_eq(epoch.day, 1)
            assert_eq(epoch.hour, 0)
            assert_eq(epoch.min, 0)
            assert_eq(epoch.sec, 0)
            assert_eq(epoch.wday, 5)
            assert_eq(epoch.yday, 1)
            assert_eq(epoch.to_unix_utc(), 0)

            -- Default hour = 12 (noon) matches Lua os.time table convention
            let d_noon = DateTime { year: 1970, month: 1, day: 1 }
            assert_eq(d_noon.hour, 12)
            assert_eq(d_noon.to_unix_utc(), 43200)

            let local_dt = DateTime.from_unix_local(1700000000)
            assert_eq(local_dt.to_unix_local(), 1700000000)
            assert_eq(
                time.format_date(fmt = "!%Y-%m-%d %H:%M:%S", timestamp = 0).unwrap(),
                "1970-01-01 00:00:00"
            )
            let bad_fmt = time.format_date(fmt = "%Q", timestamp = 0) match {
                _! => false,
                error IoError.InvalidInput(_) => true,
                error _ => false,
            }
            assert(bad_fmt)

            -- process
            assert(process.pid() > 0)
            assert_eq(process.current_pid(), process.pid())
            assert(process.shell_available())

            let sh_out = process.shell("printf '%s' 'hello-shell'").unwrap()
            assert(sh_out.success())
            assert_eq(sh_out.status, 0)
            assert_eq(sh_out.stdout, "hello-shell")

            let cat_out = process.exec("sh", args = ["-c", "cat"], stdin_data = "piped-input").unwrap()
            assert(cat_out.success())
            assert_eq(cat_out.stdout, "piped-input")

            let nonzero = process.shell("exit 7").unwrap()
            assert(not nonzero.success())
            assert_eq(nonzero.status, 7)

            let missing_bin = process.exec("/nonexistent_nessa_binary_xyz_999") match {
                _! => false,
                error IoError.NotFound(_) => true,
                error _ => false,
            }
            assert(missing_bin)

            -- runtime
            assert_eq(runtime.VERSION, "Nessa 0.1.0")
            let before_gc = runtime.gc_collections()
            let after_gc = runtime.collect_garbage()
            assert(after_gc > before_gc)
            let trace = Tracer {}.capture()
            assert(trace.contains("trace-marker"))
            assert(trace.contains("stack traceback:"))
            assert(trace.contains("Tracer.capture"))

            88
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 88);
}

#[test]
fn collections_preserve_no_display_on_list_and_map_and_support_all_list_map_ops() {
    // Critical invariant: List and Map must NOT implement Display.
    for rejected in [
        "struct Bag { items: List }\nderive Display for Bag\nfn main() -> i64 { 0 }",
        "struct Dict { entries: Map }\nderive Display for Dict\nfn main() -> i64 { 0 }",
    ] {
        let compiled = driver::Driver::new().compile(rejected);
        assert!(
            compiled.has_errors,
            "Display must not be implemented for List/Map: {rejected}"
        );
    }

    let src = r#"
        struct Pair {
            key: i64,
            tag: String,
        }
        derive Eq, PartialEq for Pair

        fn main() -> i64 {
            let packed = List.pack(3, 1, 4, 1, 5)
            assert_eq(packed, [3, 1, 4, 1, 5])
            assert(not packed.is_sorted())
            packed.sort()
            assert_eq(packed, [1, 1, 3, 4, 5])
            assert(packed.is_sorted())

            assert_eq(packed.binary_search(3), (true, 2))
            assert_eq(packed.binary_search(2), (false, 2))
            assert_eq(packed.binary_search(9), (false, 5))

            -- Stability check on sort_by_key
            let pairs = [
                Pair { key: 2, tag: "first-2" },
                Pair { key: 1, tag: "first-1" },
                Pair { key: 2, tag: "second-2" },
                Pair { key: 1, tag: "second-1" },
            ]
            pairs.sort_by_key(|p: Any| -> Any { p.as(Pair).key })
            assert_eq(pairs.get(0).as(Pair).tag, "first-1")
            assert_eq(pairs.get(1).as(Pair).tag, "second-1")
            assert_eq(pairs.get(2).as(Pair).tag, "first-2")
            assert_eq(pairs.get(3).as(Pair).tag, "second-2")

            let desc = [1, 4, 2, 3]
            desc.sort_by_lt(|a: Any, b: Any| -> bool { a > b })
            assert_eq(desc, [4, 3, 2, 1])
            assert(desc.is_sorted_by_lt(|a: Any, b: Any| -> bool { a > b }))

            -- slice, copy_within, copy_to, join_range
            let words = ["a", "b", "c", "d"]
            assert_eq(words.slice(1, 3), ["b", "c"])
            assert_eq(words.join_range("-", 1, 4), "b-c-d")

            let overlap = [10, 20, 30, 40, 50]
            let _ = overlap.copy_within(0, 3, 1)
            assert_eq(overlap, [10, 10, 20, 30, 50])
            let _ = overlap.copy_within(2, 5, 0)
            assert_eq(overlap, [20, 30, 50, 30, 50])
            let _ = overlap.copy_within(3, 5, 4)
            assert_eq(overlap, [20, 30, 50, 30, 30, 50])

            let dst = [99]
            let _ = [1, 2, 3].copy_to(1, 3, dst, 1)
            assert_eq(dst, [99, 2, 3])

            -- flat_map, find, position, rposition, reduce, partition, retain, dedup, reverse_in_place
            assert_eq([1, 2, 3].flat_map(|x: Any| -> List { [x, x.as(i64) * 10] }), [1, 10, 2, 20, 3, 30])
            assert_eq([3, 8, 11, 14].find(|x: Any| -> bool { x.as(i64) > 9 }), 11)
            assert_eq([3, 8, 11].find(|x: Any| -> bool { x.as(i64) > 99 }), null)
            assert_eq([5, 7, 5, 9].position(|x: Any| -> bool { x == 5 }), 0)
            assert_eq([5, 7, 5, 9].rposition(|x: Any| -> bool { x == 5 }), 2)
            assert_eq([1, 2, 3, 4].reduce(|a: Any, b: Any| -> Any { a.as(i64) + b.as(i64) }), 10)
            assert_eq(List().reduce(|a: Any, b: Any| -> Any { a }), null)

            let (evens, odds) = [1, 2, 3, 4, 5].partition(|x: Any| -> bool { x.as(i64) % 2 == 0 })
            assert_eq(evens, [2, 4])
            assert_eq(odds, [1, 3, 5])

            let nums = [1, 1, 2, 2, 2, 3, 1, 1]
            nums.dedup()
            assert_eq(nums, [1, 2, 3, 1])
            nums.retain(|x: Any| -> bool { x.as(i64) >= 2 })
            assert_eq(nums, [2, 3])
            nums.reverse_in_place()
            assert_eq(nums, [3, 2])
            assert_eq([1, "hi", [true]].format_list(), "[1, \"hi\", [true]]")

            -- Map additions
            let m = Map()
            m.set("a", 10)
            m.set("b", null)
            m.set("c", 30)
            assert_eq(m.get_or("a", 99), 10)
            assert_eq(m.get_or("b", 99), null)
            assert_eq(m.get_or("missing", 99), 99)

            let filtered = m.filter(|_: String, v: Any| -> bool { v != null })
            assert_eq(filtered.len(), 2)
            let mapped = filtered.map_values(|k: String, v: Any| -> Any { k ++ "=" ++ v.as(i64).to_string() })
            assert_eq(mapped.get("a"), "a=10")
            assert_eq(mapped.get("c"), "c=30")
            assert_eq(mapped.format_map(), "\x7b\"a\": \"a=10\", \"c\": \"c=30\"\x7d")

            mapped.retain(|k: String, _: Any| -> bool { k == "c" })
            assert_eq(mapped.keys(), ["c"])

            66
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 66);
}

#[test]
fn hashmap_set_and_deque_handle_collisions_rehashing_set_algebra_and_wraparound() {
    let src = r#"
        use std.collections.{Deque, HashMap, Set}

        -- Deliberately colliding hash implementation to stress bucket chains.
        struct CollideKey {
            id: i64,
        }
        derive Eq, PartialEq for CollideKey

        impl Hash for CollideKey {
            pub fn hash(self) -> u64 { (7).as(u64) }
        }

        fn main() -> i64 {
            let hm = HashMap.new()
            assert(hm.is_empty())

            -- Heterogeneous keys: int, String, tuple, null, and colliding custom struct
            hm.set(1, "one")
            hm.set("two", 2)
            hm.set((3, "three"), 33)
            hm.set(null, null)
            assert_eq(hm.len(), 4)
            assert(hm.contains(null))
            assert_eq(hm.get_or(null, "fallback"), null)
            assert_eq(hm.get_or("absent", "fallback"), "fallback")
            assert_eq(hm.get((3, "three")), 33)

            -- Colliding keys + overwrite + removal
            var c = 0
            while c < 20 {
                hm.set(CollideKey { id: c }, c * 10)
                c += 1
            }
            assert_eq(hm.get(CollideKey { id: 15 }), 150)
            hm.set(CollideKey { id: 15 }, 999)
            assert_eq(hm.get(CollideKey { id: 15 }), 999)
            assert_eq(hm.remove(CollideKey { id: 15 }), 999)
            assert(not hm.contains(CollideKey { id: 15 }))

            -- Stress rehash and deterministic insertion order
            let ordered = HashMap.new()
            var i = 0
            while i < 150 {
                ordered.set(i, i * 2)
                i += 1
            }
            let _ = ordered.remove(50)
            let seen_keys = List()
            for pair in ordered {
                seen_keys.push(pair.0)
            }
            assert_eq(seen_keys.len(), 149)
            assert_eq(seen_keys.first(), 0)
            assert_eq(seen_keys.last(), 149)
            assert(not seen_keys.contains(50))

            let clone_hm = ordered.copy()
            assert_eq(clone_hm, ordered)
            clone_hm.set(999, 1)
            assert(clone_hm != ordered)

            -- Set operations
            let a = Set.from_list([1, 2, 3, 2, 1])
            let b = Set.from_list([3, 4, 5])
            assert_eq(a.len(), 3)
            assert(not a.insert(2))
            assert(a.insert(6))
            assert(a.remove(6))
            assert_eq(a.union(b), Set.from_list([1, 2, 3, 4, 5]))
            assert_eq(a.intersection(b), Set.from_list([3]))
            assert_eq(a.difference(b), Set.from_list([1, 2]))
            assert_eq(a.symmetric_difference(b), Set.from_list([1, 2, 4, 5]))
            assert(Set.from_list([1, 3]).is_subset(a))
            assert(a.is_superset(Set.from_list([1, 2])))

            -- Deque wrap-around and growth
            let dq = Deque.new()
            var k = 0
            while k < 40 {
                if k % 2 == 0 {
                    dq.push_back(k)
                } else {
                    dq.push_front(k)
                }
                k += 1
            }
            assert_eq(dq.len(), 40)
            assert_eq(dq.front(), 39)
            assert_eq(dq.back(), 38)
            assert_eq(dq.pop_front(), 39)
            assert_eq(dq.pop_back(), 38)
            assert_eq(dq.len(), 38)
            dq.set(0, 777)
            assert_eq(dq.get(0), 777)

            let dq2 = Deque.from_list([10, 20, 30])
            let iter_list = List()
            for item in dq2 {
                iter_list.push(item)
            }
            assert_eq(iter_list, [10, 20, 30])

            91
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 91);
}

#[test]
fn coroutines_support_bidirectional_yield_nested_status_close_wrap_and_generators() {
    let src = r#"
        use std.coroutine
        use std.coroutine.{CoroutineError, CoroutineStatus}

        fn main() -> i64 {
            assert_eq(coroutine.running(), null)
            assert(not coroutine.is_yieldable())

            -- Bidirectional resume / yield / finish / dead error
            let co = coroutine.create(|first: Any| -> Any {
                let second = coroutine.yield_val(value = first.as(i64) + 1)
                let third = coroutine.yield_val(value = second.as(i64) * 10)
                third.as(i64) + 5
            })
            assert_eq(co.status(), CoroutineStatus.suspended)
            assert_eq(coroutine.`resume`(co, arg = 10).unwrap(), 11)
            assert_eq(co.status(), CoroutineStatus.suspended)
            assert_eq(co.try_resume(arg = 4).unwrap(), 40)
            assert_eq(co.status(), CoroutineStatus.suspended)
            assert_eq(co.resume_with(arg = 7).unwrap(), 12)
            assert_eq(co.status(), CoroutineStatus.dead)

            let dead_err = co.try_resume(arg = 0) match {
                _! => false,
                error CoroutineError.DeadCoroutine => true,
                error _ => false,
            }
            assert(dead_err)

            -- Nested coroutines: normal vs running status & AlreadyRunning protection
            let inner = coroutine.create(|_: Any| -> Any {
                let self_co = coroutine.running().as(coroutine.Coroutine)
                assert_eq(self_co.status(), CoroutineStatus.running)
                assert(coroutine.is_yieldable())
                let _ = coroutine.yield_val(value = "inner-paused")
                "inner-done"
            })
            let outer = coroutine.create(|_: Any| -> Any {
                let me = coroutine.running().as(coroutine.Coroutine)
                assert_eq(me.status(), CoroutineStatus.running)

                -- Self-resume while running must fail with AlreadyRunning
                let self_resume_blocked = me.try_resume() match {
                    _! => false,
                    error CoroutineError.AlreadyRunning => true,
                    error _ => false,
                }
                assert(self_resume_blocked)

                -- Check outer becomes `normal` while inner is running
                let checker = coroutine.create(|parent_co: Any| -> Any {
                    let p = parent_co.as(coroutine.Coroutine)
                    assert_eq(p.status(), CoroutineStatus.normal)
                    let close_blocked = p.close() match {
                        _! => false,
                        error CoroutineError.AlreadyRunning => true,
                        error _ => false,
                    }
                    assert(close_blocked)
                    123
                })
                assert_eq(checker.step(arg = me), 123)
                assert_eq(me.status(), CoroutineStatus.running)

                assert_eq(inner.step(), "inner-paused")
                assert_eq(inner.status(), CoroutineStatus.suspended)
                assert_eq(inner.step(), "inner-done")
                assert_eq(inner.status(), CoroutineStatus.dead)
                999
            })
            assert_eq(outer.step(), 999)
            assert_eq(outer.status(), CoroutineStatus.dead)
            assert_eq(coroutine.running(), null)

            -- Closing a suspended coroutine
            let closable = coroutine.create(|_: Any| -> Any {
                let _ = coroutine.yield_val(value = 1)
                2
            })
            assert_eq(closable.step(), 1)
            assert_eq(closable.status(), CoroutineStatus.suspended)
            closable.close().unwrap()
            assert_eq(closable.status(), CoroutineStatus.dead)

            -- wrap & Generator (including yielding null mid-stream)
            let next_fib = coroutine.wrap(|_: Any| -> Any {
                var a = 1
                var b = 1
                while true {
                    let _ = coroutine.yield_val(value = a)
                    let nxt = a + b
                    a = b
                    b = nxt
                }
                0
            })
            assert_eq([next_fib(null), next_fib(null), next_fib(null), next_fib(null)], [1, 1, 2, 3])

            let gen_items = List()
            for item in coroutine.generator(|| {
                let _ = coroutine.yield_val(value = 10)
                let _ = coroutine.yield_val(value = null)
                let _ = coroutine.yield_val(value = 20)
            }) {
                gen_items.push(item)
            }
            assert_eq(gen_items, [10, null, 20])

            42
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 42);
}

#[test]
fn json_rfc8259_parse_stringify_surrogate_pairs_pretty_print_and_strict_rejections() {
    let src = r#"
        use std.json
        use std.json.JsonError
        use std.math

        fn assert_parse_err(input: String, check: fn(JsonError) -> bool) {
            let matched = json.parse(input) match {
                _! => false,
                error err => check(err),
            }
            assert(matched, message = "expected JSON error for: " ++ input)
        }

        fn main() -> i64 {
            let doc = "\x7b\"active\":true,\"nothing\":null,\"count\":42,\"neg\":-9223372036854775808,\"ratio\":3.5,\"huge\":1e30,\"emoji\":\"\\uD83D\\uDE00\",\"escapes\":\"\\\"\\\\\\/\\b\\f\\n\\r\\t\\u001f\",\"items\":[1,false,\"ok\",[]]\x7d"
            let parsed = json.parse(doc).unwrap().as(Map)
            assert_eq(parsed.get("active"), true)
            assert_eq(parsed.get("nothing"), null)
            assert_eq(parsed.get("count"), 42)
            assert_eq(parsed.get("neg"), -9223372036854775808)
            assert_eq(parsed.get("ratio"), 3.5)
            assert_eq(parsed.get("huge"), 1.0e30)
            let neg_zero = json.parse("-0").unwrap()
            assert_eq(type_of(neg_zero), f64)
            let emoji = parsed.get("emoji").as(String)
            assert_eq(emoji.len(), 4)
            assert_eq(emoji.char_len(), 1)
            assert_eq(emoji.first().as(char).to_code(), 128512)
            assert_eq(parsed.get("items"), [1, false, "ok", []])

            -- Compact & pretty stringify roundtrip with deterministic sorted keys
            let compact = json.stringify(parsed).unwrap()
            let reparsed = json.parse(compact).unwrap().as(Map)
            assert_eq(reparsed, parsed)

            let small = Map()
            small.set("b", [1, 2])
            small.set("a", "x\ny")
            let pretty_text = json.stringify(small, pretty = true).unwrap()
            let expected_pretty = "\x7b\n  \"a\": \"x\\ny\",\n  \"b\": [\n    1,\n    2\n  ]\n\x7d"
            assert_eq(pretty_text, expected_pretty)
            assert_eq(json.parse(pretty_text).unwrap(), small)

            -- Strict RFC 8259 rejections
            assert_parse_err("", |e: JsonError| -> bool { e matches JsonError.UnexpectedEof(_) })
            assert_parse_err("   ", |e: JsonError| -> bool { e matches JsonError.UnexpectedEof(_) })
            assert_parse_err("[1, 2", |e: JsonError| -> bool { e matches JsonError.UnexpectedEof(_) })
            assert_parse_err("\"unterminated", |e: JsonError| -> bool { e matches JsonError.UnexpectedEof(_) })
            assert_parse_err("true false", |e: JsonError| -> bool { e matches JsonError.TrailingCharacters(_) })
            assert_parse_err("01", |e: JsonError| -> bool { e matches JsonError.InvalidNumber(_, _) })
            assert_parse_err("-01", |e: JsonError| -> bool { e matches JsonError.InvalidNumber(_, _) })
            assert_parse_err("+1", |e: JsonError| -> bool { e matches JsonError.InvalidNumber(_, _) })
            assert_parse_err("-", |e: JsonError| -> bool { e matches JsonError.InvalidNumber(_, _) })
            assert_parse_err("1.", |e: JsonError| -> bool { e matches JsonError.InvalidNumber(_, _) })
            assert_parse_err("1e+", |e: JsonError| -> bool { e matches JsonError.InvalidNumber(_, _) })
            assert_parse_err("1e9999", |e: JsonError| -> bool { e matches JsonError.InvalidNumber(_, _) })
            assert_parse_err("\"raw\nnewline\"", |e: JsonError| -> bool { e matches JsonError.InvalidString(_, _) })
            assert_parse_err("\"\\x41\"", |e: JsonError| -> bool { e matches JsonError.InvalidEscape(_, _) })
            assert_parse_err("\"\\uD83D\"", |e: JsonError| -> bool { e matches JsonError.InvalidEscape(_, _) })
            assert_parse_err("\"\\uD83D\\u0041\"", |e: JsonError| -> bool { e matches JsonError.InvalidEscape(_, _) })
            assert_parse_err("\"\\uDE00\"", |e: JsonError| -> bool { e matches JsonError.InvalidEscape(_, _) })
            assert_parse_err("[1,]", |e: JsonError| -> bool { e matches JsonError.UnexpectedToken(_, _) })
            assert_parse_err("\x7b\"a\":1,\x7d", |e: JsonError| -> bool { e matches JsonError.UnexpectedToken(_, _) })
            assert_parse_err("\x7ba:1\x7d", |e: JsonError| -> bool { e matches JsonError.UnexpectedToken(_, _) })

            -- Depth limit > 128
            let deep = "[".repeat(130) ++ "]".repeat(130)
            assert_parse_err(deep, |e: JsonError| -> bool { e == JsonError.DepthLimitExceeded })

            -- Unsupported stringify inputs (NaN, Infinity, -Infinity, closures)
            let nan_rejected = json.stringify(math.nan()) match {
                _! => false,
                error JsonError.UnsupportedValue(_) => true,
                error _ => false,
            }
            assert(nan_rejected)

            let inf_rejected = json.stringify(math.infinity()) match {
                _! => false,
                error JsonError.UnsupportedValue(_) => true,
                error _ => false,
            }
            assert(inf_rejected)

            let neg_inf_rejected = json.stringify(math.neg_infinity()) match {
                _! => false,
                error JsonError.UnsupportedValue(_) => true,
                error _ => false,
            }
            assert(neg_inf_rejected)

            let fn_rejected = json.stringify(|x: i64| -> i64 { x }) match {
                _! => false,
                error JsonError.UnsupportedValue(_) => true,
                error _ => false,
            }
            assert(fn_rejected)

            108
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 108);
}
