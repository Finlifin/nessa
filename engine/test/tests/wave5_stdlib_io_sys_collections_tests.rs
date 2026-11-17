//! Comprehensive integration tests for Wave 5 standard library modules:
//! `std.conv`, `std.io`, `std.fs`, `std.path`, `std.env`, `std.time`,
//! `std.process`, `std.collections`, `std.coroutine`, `std.runtime`, and `std.json`.

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
fn io_parse_console_effects_and_streams_cover_bases_floats_and_errors() {
    let src = r#"
        use std.conv
        use std.io

        fn run_checks() -> ![ParseError, IoError] i64 {
            -- Type-directed scalar parsing across radices, widths, floats, bool, and char
            assert_eq("42".parse(i64)!, 42)
            assert_eq("+123".parse(i64)!, 123)
            assert_eq("-3.5".parse(f64)!, -3.5)
            assert_eq("  123  ".trim().parse(i64)!, 123)
            assert_eq("-ff".parse(i64, radix = 16)!, -255)
            assert_eq("1010".parse(i64, radix = 2)!, 10)
            assert_eq("-1011".parse(i64, radix = 2)!, -11)
            assert_eq("77".parse(i64, radix = 8)!, 63)
            assert_eq("z".parse(i64, radix = 36)!, 35)
            assert_eq("3.25".parse(f64)!, 3.25)
            assert_eq("-1.5e2".parse(f64)!, -150.0)
            assert_eq("true".parse(bool)!, true)
            assert_eq("false".parse(bool)!, false)
            assert_eq("λ".parse(char)!, 'λ')

            let narrow_u16: u16 = conv.parse(u16, "ff", radix = 16)!
            assert_eq(narrow_u16, (255).as(u16))
            let narrow_i8: i8 = "-128".parse(i8)!
            assert_eq(narrow_i8, (-128).as(i8))

            -- Optional `try_parse` rejections return null without throwing
            assert_eq("1e9999".try_parse(f64), null)
            assert_eq("".try_parse(i64), null)
            assert_eq("   ".try_parse(i64), null)
            assert_eq("  123  ".try_parse(i64), null)
            assert_eq("12x".try_parse(i64), null)
            assert_eq(conv.try_parse(i64, "zz", radix = 36), 1295)
            assert_eq(conv.try_parse(i64, "zz", radix = 10), null)
            assert_eq("10".try_parse(i64, radix = 1), null)

            -- Checked `ParseError` variants
            let empty_ok = "".parse(i64) match {
                _! => false,
                error ParseError.Empty => true,
                error _ => false,
            }
            assert(empty_ok)

            let bad_digit_ok = "12x".parse(i64) match {
                _! => false,
                error ParseError.InvalidDigit(ch, r) => ch == 'x' and r == 10,
                error _ => false,
            }
            assert(bad_digit_ok)

            let bad_fmt_ok = "yes".parse(bool) match {
                _! => false,
                error ParseError.InvalidFormat(s) => s == "yes",
                error _ => false,
            }
            assert(bad_fmt_ok)

            let bad_radix_lo = "10".parse(i64, radix = 1) match {
                _! => false,
                error ParseError.InvalidRadix(r) => r == 1,
                error _ => false,
            }
            assert(bad_radix_lo)

            let bad_radix_hi = "10".parse(i64, radix = 37) match {
                _! => false,
                error ParseError.InvalidRadix(r) => r == 37,
                error _ => false,
            }
            assert(bad_radix_hi)

            let bad_float_radix = "3.5".parse(f64, radix = 16) match {
                _! => false,
                error ParseError.InvalidRadix(r) => r == 16,
                error _ => false,
            }
            assert(bad_float_radix)

            let overflow_u8 = "300".parse(u8) match {
                _! => false,
                error ParseError.Overflow(ty) => ty == "u8",
                error _ => false,
            }
            assert(overflow_u8)

            let overflow_f64 = "1e9999".parse(f64) match {
                _! => false,
                error ParseError.Overflow(ty) => ty == "f64",
                error _ => false,
            }
            assert(overflow_f64)

            -- Standard streams and console capability handlers
            eprint("")
            eprintln("wave5 stderr check")
            io.flush_stdout()!
            io.flush_stderr()!
            assert(io.stdin().is_open())
            assert(io.stdout().is_open())
            assert(io.stderr().is_open())
            assert_eq(File.stdin(), io.stdin())
            assert_eq(File.stdout(), io.stdout())
            assert_eq(File.stderr(), io.stderr())

            let (captured, out_code): (String, i64) = io.capture_output(|| -> #[io.console_print, io.console_println] i64 {
                io.console_print("part1-") #
                io.console_println("42") #
                100
            })
            assert_eq(captured, "part1-42\n")

            let stdio_ok: i64 = io.with_stdio(|| -> #io.console_print i64 {
                io.console_print("") #
                out_code
            })
            stdio_ok
        }

        fn main() -> i64 {
            run_checks() ! { _ => -999 }
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 100);

    let unsupported_target = r#"
        fn main() -> i64 {
            let _ = "10".try_parse(List)
            0
        }
    "#;
    let compiled = driver::Driver::new().compile(unsupported_target);
    assert!(
        compiled.has_errors,
        "expected compile-time rejection for non-scalar parse target"
    );
}

#[test]
fn io_file_read_write_seek_temp_lines_and_error_variants() {
    let tmp = TempDirGuard::new("io_file");
    let root = tmp.nessa_str();
    let src = r#"
        use std.io.{SeekFrom, read_lines}

        enum ReadGuardError {
            BadHeader(found: String),
        }

        fn verify_file_header(
            path: Path,
            expected: String,
            seen_handles: List,
        ) -> ![IoError, ReadGuardError] String {
            File.with_open(path, mode = .read) do |f| {
                seen_handles.push(f)
                let header = f.read_line()! match {
                    line? => line,
                    _ => "",
                }
                if header != expected {
                    return error ReadGuardError.BadHeader(header)
                }
                header
            }!
        }

        fn run_checks() -> ![IoError, ParseError, ReadGuardError] i64 {
            let base = Path.new("__ROOT__")
            let file_path = base.join("sample.txt")

            -- Missing file returns NotFound
            let missing_ok = File.open(base.join("does_not_exist.txt")) match {
                _! => false,
                error IoError.NotFound(_) => true,
                error _ => false,
            }
            assert(missing_ok)

            -- Create, write, flush, close, and verify closed state
            let f = File.create(file_path)!
            assert(f.is_open())
            assert_eq(f.to_string(), "file (open)")
            f.write("alpha\n")!
            f.write_line("beta")!
            f.write("12.5 -7")!
            f.flush()!
            f.close()!
            assert(not f.is_open())
            assert_eq(f.to_string(), "file (closed)")

            -- Reading from closed handle returns Closed
            let closed_ok = f.read_to_string() match {
                _! => false,
                error IoError.Closed(_) => true,
                error _ => false,
            }
            assert(closed_ok)

            -- Exclusive `OpenMode.create_new` on existing file returns AlreadyExists
            let exists_ok = File.open(file_path, mode = OpenMode.create_new) match {
                _! => false,
                error IoError.AlreadyExists(_) => true,
                error _ => false,
            }
            assert(exists_ok)

            let create_new_ok = File.create_new(file_path) match {
                _! => false,
                error IoError.AlreadyExists(_) => true,
                error _ => false,
            }
            assert(create_new_ok)

            -- Line, parsed numeric tokens, char, and seek operations
            let r = File.open(file_path, mode = OpenMode.read)!
            assert_eq(r.read_line()!, "alpha")
            assert_eq(r.read_line(keep_newline = true)!, "beta\n")
            let num_line = r.read_line()! match {
                s? => s,
                _ => "",
            }
            let tokens = num_line.split_whitespace()
            assert_eq(tokens.get(0).as(String).parse(f64)!, 12.5)
            assert_eq(tokens.get(1).as(String).parse(f64)!, -7.0)
            assert_eq(r.read_line()!, null)

            assert_eq(SeekFrom.start(0).to_string(), "SeekFrom.start(0)")
            assert_eq(r.seek(SeekFrom.start(0))!, 0)
            assert_eq(r.position()!, 0)
            assert_eq(r.read_chars(5)!, "alpha")
            let _ = r.seek(SeekFrom.current(1))!
            assert_eq(r.read_chars(4)!, "beta")
            let end_pos = r.seek(SeekFrom.end(0))!
            assert(end_pos > 10)
            r.close()!

            -- `read_lines` lazy iterator auto-closes on EOF
            let collected = List()
            let iter = read_lines(file_path)!
            for line in iter {
                collected.push(line)
            }
            assert_eq(collected, ["alpha", "beta", "12.5 -7"])

            -- `File.with_open` RAII scoping, `OpenMode.append`, `f.lines()`, and `!Error` cleanup
            let scoped_path = base.join("scoped.txt")
            File.with_open(scoped_path, mode = .write) do |wf| {
                let _ = wf.write("MAGIC_V1\nrow1\n")!
            }!
            File.with_open(scoped_path, mode = OpenMode.append) do |af| {
                let _ = af.write_line("row2")!
            }!
            let scoped_lines = File.with_open(scoped_path) do |rf| {
                rf.lines().collect()
            }!
            assert_eq(scoped_lines, ["MAGIC_V1", "row1", "row2"])

            let seen_ok = List()
            assert_eq(verify_file_header(scoped_path, "MAGIC_V1", seen_ok)!, "MAGIC_V1")
            assert(not seen_ok.get(0).as(File).is_open())

            let seen_err = List()
            let bad_hdr_caught = verify_file_header(scoped_path, "WRONG", seen_err) match {
                _! => false,
                error ReadGuardError.BadHeader(h) => h == "MAGIC_V1",
                error _ => false,
            }
            assert(bad_hdr_caught)
            assert(not seen_err.get(0).as(File).is_open())

            -- Binary read/write, `OpenMode.read_write`, and `File.temp()`
            let bin_path = base.join("raw.bin")
            let bw = File.open(bin_path, mode = OpenMode.write)!
            bw.write_bytes([0, 65, 66, 255])!
            bw.close()!

            let br = File.open(bin_path, mode = OpenMode.read)!
            assert_eq(br.read_bytes(max_bytes = 2)!, [0, 65])
            assert_eq(br.read_bytes()!, [66, 255])
            br.close()!

            let rw = File.open(bin_path, mode = OpenMode.read_write)!
            let _ = rw.seek(SeekFrom.start(1))!
            rw.write_bytes([90])!
            let _ = rw.seek(SeekFrom.start(0))!
            assert_eq(rw.read_bytes()!, [0, 90, 66, 255])
            rw.close()!

            let tf = File.temp()!
            tf.write("scratch")!
            assert_eq(tf.seek(SeekFrom.start(0))!, 0)
            assert_eq(tf.read_to_string()!, "scratch")
            tf.close()!

            42
        }

        fn main() -> i64 {
            run_checks() ! { _ => -999 }
        }
    "#
    .replace("__ROOT__", &root);
    assert_eq!(common::run_value(&src).unwrap(), 42);
}

#[test]
fn fs_operations_cover_files_directories_metadata_copy_rename_and_errors() {
    let tmp = TempDirGuard::new("fs_ops");
    let root = tmp.nessa_str();
    let src = r#"
        use std.fs
        use std.fs.{DirEntry, FileKind}

        fn run_checks() -> !IoError i64 {
            let root_path = Path.new("__ROOT__")
            let nested_root = root_path.join("nested")
            let sub = nested_root.join("dir")
            fs.create_dir(sub, parents = true)!
            assert(fs.exists(sub))
            assert(fs.is_dir(sub))
            assert(not fs.is_file(sub))

            let dir_meta = fs.metadata(sub)!
            assert_eq(dir_meta.kind, FileKind.directory)
            assert(dir_meta.is_dir())
            assert(not dir_meta.is_file())
            assert(not dir_meta.is_symlink())

            let p1 = sub.join("hello.txt")
            fs.write_string(p1, "hello")!
            fs.append_string(p1, " world")!
            assert(fs.exists(p1))
            assert(fs.is_file(p1))
            assert_eq(fs.read_to_string(p1)!, "hello world")

            let meta = fs.metadata(p1)!
            assert_eq(meta.size, 11)
            assert_eq(meta.kind, FileKind.file)
            assert(meta.is_file())
            assert(not meta.is_dir())
            assert(meta.modified_unix_secs > 0)

            -- Copy and rename
            let p2 = sub.join("copied.txt")
            let copied_bytes = fs.copy(p1, p2)!
            assert_eq(copied_bytes, 11)
            let p3 = sub.join("renamed.txt")
            fs.rename(p2, p3)!
            assert(not fs.exists(p2))
            assert_eq(fs.read_to_string(p3)!, "hello world")

            -- Binary helpers (`List` and compact `Bytes`)
            let bin = sub.join("bytes.bin")
            fs.write_bytes(bin, [10, 20, 30, 40])!
            assert_eq(fs.read_bytes(bin)!, [10, 20, 30, 40])

            let raw_bin = sub.join("raw_buf.bin")
            let raw_payload = Bytes.from_list([1, 2, 3, 254])
            fs.write_raw_bytes(raw_bin, raw_payload)!
            let loaded_raw = fs.read_raw_bytes(raw_bin)!
            assert_eq(loaded_raw, raw_payload)
            assert_eq(loaded_raw.to_list(), [1, 2, 3, 254])

            -- Directory listing (`read_dir`, `iter_dir`, `walk_dir`) & canonicalize
            let names = fs.read_dir(sub)!
            names.sort()
            assert_eq(names, ["bytes.bin", "hello.txt", "raw_buf.bin", "renamed.txt"])

            let entry_names = List()
            for entry in (fs.iter_dir(sub)!) {
                let d: DirEntry = entry
                assert_eq(d.kind, FileKind.file)
                assert(d.is_file())
                assert(not d.is_dir())
                assert_eq(d.to_path().file_name(), d.name)
                entry_names.push(d.name)
            }
            entry_names.sort()
            assert_eq(entry_names, ["bytes.bin", "hello.txt", "raw_buf.bin", "renamed.txt"])

            let walked = fs.walk_dir(nested_root)!
            assert_eq(walked.len(), 4)

            let canon = fs.canonicalize(p1)!
            assert(canon.ends_with("hello.txt"))

            -- Type-checked `remove_file` vs `remove_dir`
            let dir_as_file_err = fs.remove_file(sub) match {
                _! => false,
                error IoError.InvalidInput(_) => true,
                error _ => false,
            }
            assert(dir_as_file_err)

            let file_as_dir_err = fs.remove_dir(p1) match {
                _! => false,
                error IoError.InvalidInput(_) => true,
                error _ => false,
            }
            assert(file_as_dir_err)

            fs.remove_file(p1)!
            assert(not fs.exists(p1))
            fs.remove(bin)!
            assert(not fs.exists(bin))
            fs.remove(raw_bin)!
            assert(not fs.exists(raw_bin))

            let gen_tmp = fs.temp_file_path(prefix = "wave5_")!
            assert(gen_tmp.contains("wave5_"))

            fs.remove_dir(nested_root, recursive = true)!
            assert(not fs.exists(nested_root))
            77
        }

        fn main() -> i64 {
            run_checks() ! { _ => -999 }
        }
    "#
    .replace("__ROOT__", &root);
    assert_eq!(common::run_value(&src).unwrap(), 77);
}

#[test]
fn path_manipulation_handles_lexical_normalization_stems_extensions_and_roots() {
    let src = r#"
        use std.env
        use std.json
        use std.path

        fn run_checks() -> !JsonError i64 {
            assert(path.is_absolute("/usr/local"))
            assert(path.is_absolute("C:/Windows/System32"))
            assert(not path.is_absolute("relative/file.txt"))
            assert(not path.is_absolute(""))
            assert(Path.new("/usr/local").is_absolute())
            assert(not Path.new("relative/file.txt").is_absolute())
            assert(Path.new("").is_empty())

            if env.os() == "windows" {
                assert_eq(path.separator(), "\\")
                assert(path.is_absolute("C:\\Windows\\System32"))
                assert_eq(path.join("a\\b", "c.txt"), "a\\b\\c.txt")
                assert_eq(path.join("a\\b\\", "c.txt"), "a\\b\\c.txt")
                assert_eq(path.join("a\\b", "C:\\Windows"), "C:\\Windows")
                assert_eq(
                    path.join_all(["C:\\", "Users", "fin", "nessa.exe"]),
                    "C:\\Users\\fin\\nessa.exe"
                )
                assert_eq(
                    Path.new("C:\\").join_all("Users", "fin", "nessa.exe").as_str(),
                    "C:\\Users\\fin\\nessa.exe"
                )
                assert_eq(path.parent("C:\\Users\\fin"), "C:\\Users")
                assert_eq(path.parent("C:\\Users"), "C:\\")
                assert_eq(path.parent("C:\\"), null)
                assert_eq(Path.new("C:\\Users\\fin").parent(), Path.new("C:\\Users"))
                assert_eq(Path.new("C:\\").parent(), null)
                assert_eq(path.normalize("C:\\a\\b\\..\\c\\.\\d\\\\e\\"), "C:\\a\\c\\d\\e")
                assert_eq(path.components("C:\\a\\b\\..\\c"), ["C:\\", "a", "b", "..", "c"])
            } else {
                assert_eq(path.separator(), "/")
                assert_eq(path.join("a/b", "c.txt"), "a/b/c.txt")
                assert_eq(path.join("a/b/", "c.txt"), "a/b/c.txt")
                assert_eq(path.join("a/b", "/etc/hosts"), "/etc/hosts")
                assert_eq(path.join_all(["usr", "local", "bin", "nessa"]), "usr/local/bin/nessa")
                assert_eq(
                    Path.new("usr").join_all("local", "bin", "nessa").as_str(),
                    "usr/local/bin/nessa"
                )
                assert_eq(path.parent("/usr/local/bin"), "/usr/local")
                assert_eq(path.parent("/usr"), "/")
                assert_eq(path.parent("/"), null)
                assert_eq(Path.new("/usr/local/bin").parent(), Path.new("/usr/local"))
                assert_eq(Path.new("/").parent(), null)
                assert_eq(path.normalize("/a/b/../c/./d//e/"), "/a/c/d/e")
                assert_eq(Path.new("/a/b/../c/./d//e/").normalize(), Path.new("/a/c/d/e"))
                assert_eq(path.normalize("/../../etc"), "/etc")
                assert_eq(path.normalize("a/../../b"), "../b")
                assert_eq(path.components("/a/b/../c"), ["/", "a", "b", "..", "c"])
                assert_eq(Path.new("/a/b/../c").components(), ["/", "a", "b", "..", "c"])
            }

            assert_eq(path.join("", "c.txt"), "c.txt")
            assert_eq(path.join("a/b", ""), "a/b")
            assert_eq(path.parent("single"), null)
            assert_eq(path.parent("a/b/"), "a")

            let archive = Path.new("/usr/local/nessa.tar.gz")
            assert_eq(archive.file_name(), "nessa.tar.gz")
            assert_eq(path.file_name("/usr/local/"), "local")
            assert_eq(path.file_name("/"), null)
            assert_eq(path.file_name(".."), null)

            assert_eq(archive.stem(), "nessa.tar")
            assert_eq(path.stem("/usr/local/nessa.tar.gz"), "nessa.tar")
            assert_eq(archive.extension(), "gz")
            assert_eq(path.extension("/usr/local/nessa.tar.gz"), "gz")
            assert_eq(path.stem(".gitignore"), ".gitignore")
            assert_eq(path.extension(".gitignore"), null)
            assert_eq(path.stem("Makefile"), "Makefile")
            assert_eq(path.extension("Makefile"), null)

            let src_file = Path.new("dir/main.ns")
            assert_eq(src_file.with_extension("nsbc"), Path.new("dir/main.nsbc"))
            assert_eq(src_file.with_extension(".json"), Path.new("dir/main.json"))
            assert_eq(src_file.with_extension(""), Path.new("dir/main"))
            assert_eq(path.with_extension("dir/.gitignore", "bak"), "dir/.gitignore.bak")
            assert_eq(
                src_file.with_file_name("lib.ns"),
                Path.new(path.join("dir", "lib.ns"))
            )
            assert_eq(path.with_file_name("main.ns", "alt.ns"), "alt.ns")

            assert_eq(path.normalize(""), ".")
            assert_eq(path.normalize("./."), ".")
            assert_eq(path.components("x//y/z/"), ["x", "y", "z"])

            -- `Path` implements `Eq`, `Hash`, `Display`, and `Json`
            let path_set = Set.from_list([Path.new("a/b"), Path.from(Path.new("a/b")), Path.new("a/c")])
            assert_eq(path_set.len(), 2)
            assert_eq(Path.new("pkg/mod.ns").to_string(), "pkg/mod.ns")
            let encoded_path = json.encode(Path.new("cfg/app.json"))!
            assert_eq(encoded_path, "\"cfg/app.json\"")
            let decoded_path: Path = json.decode(Path, encoded_path)!
            assert_eq(decoded_path, Path.new("cfg/app.json"))

            55
        }

        fn main() -> i64 {
            run_checks() ! { _ => -999 }
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 55);
}

#[test]
fn env_time_process_and_runtime_modules_work_end_to_end() {
    let src = r#"
        use std.env
        use std.process
        use std.process.Command
        use std.runtime
        use std.time
        use std.time.{DateTime, Duration, Instant, Weekday}

        struct Tracer {}

        impl Tracer {
            pub fn capture(self) -> String {
                runtime.backtrace(message = "trace-marker", skip = 0)
            }
        }

        fn run_checks() -> !IoError i64 {
            -- env
            assert(not env.os().is_empty())
            assert(not env.arch().is_empty())
            let cli_args = env.args()
            assert(cli_args.len() >= 0)
            assert_eq(env.get("__NESSA_NONEXISTENT_ENV_KEY_999__"), null)
            assert_eq(env.get_or("__NESSA_NONEXISTENT_ENV_KEY_999__", "fallback"), "fallback")
            let all_vars = env.vars()
            assert(all_vars.len() >= 0)
            let cwd = env.current_dir()!
            let cwd_path = env.current_dir_path()!
            assert(not cwd.is_empty())
            assert_eq(cwd_path.as_str(), cwd)
            env.set_current_dir(cwd_path)!
            let bad_cwd = env.set_current_dir("/nonexistent_nessa_wave5_dir_xyz") match {
                _! => false,
                error IoError.NotFound(_) => true,
                error _ => false,
            }
            assert(bad_cwd)

            -- time
            assert(time.unix_secs() > 1700000000)
            assert(time.unix_millis() > 1700000000000)
            assert(time.monotonic_nanos() > 0)
            assert(time.cpu_secs() >= 0.0)

            let t0 = Instant.now()
            time.sleep(Duration.from_millis(2))
            let elapsed = t0.elapsed()
            assert(elapsed.as_nanos() >= 0)
            assert(Duration.zero().is_zero())
            assert_eq(Duration.from_secs(3).as_millis(), 3000)
            assert_eq(Duration.from_millis(1500).as_secs_f64(), 1.5)
            assert_eq(Duration.from_mins(2).as_secs(), 120)
            assert_eq(
                Duration.from_secs(10).saturating_sub(Duration.from_secs(4)),
                Duration.from_secs(6)
            )
            assert_eq(
                Duration.from_secs(2).saturating_sub(Duration.from_secs(9)),
                Duration.zero()
            )
            assert(Duration.from_millis(5) < Duration.from_millis(10))

            let epoch = DateTime.from_unix(0, utc = true)
            assert_eq(epoch.year, 1970)
            assert_eq(epoch.month, 1)
            assert_eq(epoch.day, 1)
            assert_eq(epoch.hour, 0)
            assert_eq(epoch.min, 0)
            assert_eq(epoch.sec, 0)
            assert_eq(epoch.wday, 5)
            assert_eq(epoch.yday, 1)
            assert_eq(epoch.to_unix(utc = true), 0)
            assert_eq(epoch.weekday(), Weekday.thursday)
            assert_eq(Weekday.thursday.number_from_monday(), 4)
            assert(not Weekday.thursday.is_weekend())
            assert(Weekday.saturday.is_weekend())
            assert(Weekday.sunday.is_weekend())

            -- `DateTime` struct literal defaults `hour`/`min`/`sec` to 0 (midnight UTC)
            let d_midnight = DateTime { year: 1970, month: 1, day: 1 }
            assert_eq(d_midnight.hour, 0)
            assert_eq(d_midnight.to_unix(utc = true), 0)
            let d_noon = DateTime.new(1970, 1, 1, hour = 12)
            assert_eq(d_noon.to_unix(utc = true), 43200)

            let local_dt = DateTime.from_unix(1700000000, utc = false)
            assert_eq(local_dt.to_unix(utc = false), 1700000000)
            assert_eq(epoch.format(fmt = "%Y-%m-%d %H:%M:%S", utc = true)!, "1970-01-01 00:00:00")
            assert_eq(
                time.format_date(fmt = "%Y-%m-%d %H:%M:%S", timestamp = 0, utc = true)!,
                "1970-01-01 00:00:00"
            )
            let bad_fmt = time.format_date(fmt = "%Q", timestamp = 0, utc = true) match {
                _! => false,
                error IoError.InvalidInput(_) => true,
                error _ => false,
            }
            assert(bad_fmt)

            -- Deterministic `with_mock_clock` and `with_system_clock` capability handlers
            let (final_ms, delta_ms): (i64, i64) = time.with_mock_clock(
                initial_ms = 500,
                || -> #[time.clock_now_ms, time.clock_sleep] i64 {
                    let s0 = time.clock_now_ms()#
                    time.clock_sleep(Duration.from_millis(125))#
                    let s1 = time.clock_now_ms()#
                    s1 - s0
                }
            )
            assert_eq(final_ms, 625)
            assert_eq(delta_ms, 125)

            let sys_now: i64 = time.with_system_clock(|| -> #[time.clock_now_ms, time.clock_sleep] i64 {
                time.clock_sleep(Duration.from_millis(1))#
                time.clock_now_ms()#
            })
            assert(sys_now > 1700000000000)

            -- process (`run`, `shell`, `Command.new`, `Command.spec`)
            assert(process.pid() > 0)
            assert(process.shell_available())

            let empty_spec_err = Command.spec {} match {
                _! => false,
                error IoError.InvalidInput(_) => true,
                error _ => false,
            }
            assert(empty_spec_err)

            let bad_opt_err = Command.spec { "echo", unknown_opt: true } match {
                _! => false,
                error IoError.InvalidInput(_) => true,
                error _ => false,
            }
            assert(bad_opt_err)

            if env.os() == "windows" {
                let sh_out = process.shell("echo hello-shell")!
                assert(sh_out.is_success())
                assert_eq(sh_out.status, 0)
                assert_eq(sh_out.stdout.trim(), "hello-shell")

                let cat_out = process.run(
                    "findstr",
                    args = ["piped"],
                    stdin = "piped-input\r\n",
                    cwd = cwd_path
                )!
                assert(cat_out.is_success())
                assert_eq(cat_out.stdout.trim(), "piped-input")

                let spec_cmd = Command.spec {
                    "findstr",
                    "spec",
                    stdin: "spec-line\r\n",
                    cwd: cwd_path,
                }!
                let spec_out = spec_cmd.run()!
                assert(spec_out.is_success())
                assert_eq(spec_out.stdout.trim(), "spec-line")

                let nonzero = process.shell("exit /b 7")!
                assert(not nonzero.is_success())
                assert_eq(nonzero.status, 7)

                let missing_bin = Command.new("C:\\nonexistent_nessa_binary_xyz_999.exe").run() match {
                    _! => false,
                    error IoError.NotFound(_) => true,
                    error _ => false,
                }
                assert(missing_bin)
            } else {
                let sh_out = process.shell("printf '%s' 'hello-shell'")!
                assert(sh_out.is_success())
                assert_eq(sh_out.status, 0)
                assert_eq(sh_out.stdout, "hello-shell")

                let cat_out = process.run(
                    "sh",
                    args = ["-c", "cat"],
                    stdin = "piped-input",
                    cwd = cwd_path
                )!
                assert(cat_out.is_success())
                assert_eq(cat_out.stdout, "piped-input")

                let builder_out = Command.new("sh")
                    .args("-c", "cat")
                    .stdin("builder-input")
                    .cwd(cwd_path)
                    .run()!
                assert(builder_out.is_success())
                assert_eq(builder_out.stdout, "builder-input")

                let spec_cmd = Command.spec {
                    "sh",
                    "-c",
                    "cat",
                    stdin: "spec-input",
                    cwd: cwd_path,
                }!
                let spec_out = spec_cmd.run()!
                assert(spec_out.is_success())
                assert_eq(spec_out.stdout, "spec-input")

                let nonzero = process.shell("exit 7")!
                assert(not nonzero.is_success())
                assert_eq(nonzero.status, 7)

                let missing_bin = process.run("/nonexistent_nessa_binary_xyz_999") match {
                    _! => false,
                    error IoError.NotFound(_) => true,
                    error _ => false,
                }
                assert(missing_bin)
            }

            -- runtime
            assert_eq(runtime.version(), "Nessa 0.1.0")
            let before_gc = runtime.gc_collections()
            let after_gc = runtime.collect_garbage()
            assert(after_gc > before_gc)
            let _ = runtime.jit_enabled()
            assert(runtime.jit_compiled_functions().as(i64) >= 0)
            assert(runtime.jit_code_bytes().as(i64) >= 0)
            assert(runtime.jit_deopt_count().as(i64) >= 0)
            assert(error_message(IoError.NotFound("missing")).contains("NotFound: missing"))
            let trace = Tracer {}.capture()
            assert(trace.contains("trace-marker"))
            assert(trace.contains("stack traceback:"))
            assert(trace.contains("Tracer.capture"))

            88
        }

        fn main() -> i64 {
            run_checks() ! { _ => -999 }
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

        enum BatchError {
            NegativeItem(value: i64),
        }
        derive Eq, PartialEq for BatchError

        fn run_checks() -> !BatchError i64 {
            let packed = List.pack(3, 1, 4, 1, 5)
            assert_eq(packed, [3, 1, 4, 1, 5])
            assert(not packed.is_sorted())
            packed.sort()
            assert_eq(packed, [1, 1, 3, 4, 5])
            assert(packed.is_sorted())

            assert_eq(packed.binary_search(3), (true, 2))
            assert_eq(packed.binary_search(2), (false, 2))
            assert_eq(packed.binary_search(9), (false, 5))
            assert_eq(
                packed.binary_search_by(|x: Any| -> Ordering {
                    let v = x.as(i64)
                    if v < 4 {
                        Ordering.less
                    } else if v > 4 {
                        Ordering.greater
                    } else {
                        Ordering.equal
                    }
                }),
                (true, 3)
            )

            -- Stability check on `sort_by_key`
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

            -- `slice`, `copy_within`, `copy_to`, `join_range`
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

            -- Higher-order combinators, lazy `.iter()`, and `!Error` propagation
            assert_eq(
                [1, 2, 3].flat_map(|x: Any| -> List { [x, x.as(i64) * 10] }),
                [1, 10, 2, 20, 3, 30]
            )
            assert_eq([3, 8, 11, 14].find(|x: Any| -> bool { x.as(i64) > 9 }), 11)
            assert_eq([3, 8, 11].find(|x: Any| -> bool { x.as(i64) > 99 }), null)
            assert_eq([5, 7, 5, 9].position(|x: Any| -> bool { x == 5 }), 0)
            assert_eq([5, 7, 5, 9].rposition(|x: Any| -> bool { x == 5 }), 2)
            assert_eq([1, 2, 3, 4].reduce(|a: Any, b: Any| -> Any { a.as(i64) + b.as(i64) }), 10)
            assert_eq(List().reduce(|a: Any, b: Any| -> Any { a }), null)

            let lazy_sum = [1, 2, 3, 4, 5]
                .iter()
                .filter(|x| x.as(i64) % 2 == 1)
                .map(|x| x.as(i64) * 10)
                .sum()
            assert_eq(lazy_sum, 90)

            let doubled_ok = [2, 4, 6].map(|x: Any| -> !BatchError i64 {
                let v = x.as(i64)
                if v < 0 { return error BatchError.NegativeItem(v) }
                v * 2
            })!
            assert_eq(doubled_ok, [4, 8, 12])

            let short_circuited = [2, -5, 6].map(|x: Any| -> !BatchError i64 {
                let v = x.as(i64)
                if v < 0 { return error BatchError.NegativeItem(v) }
                v * 2
            }) match {
                _! => false,
                error BatchError.NegativeItem(bad) => bad == -5,
            }
            assert(short_circuited)

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

            -- `Map` operations
            let m = Map()
            m.set("a", 10)
            m.set("b", null)
            m.set("c", 30)
            assert_eq(m.get_or("a", 99), 10)
            assert_eq(m.get_or("b", 99), null)
            assert_eq(m.get_or("missing", 99), 99)

            let filtered = m.filter(|_: String, v: Any| -> bool { v != null })
            assert_eq(filtered.len(), 2)
            let mapped = filtered.map_values(|k: String, v: Any| -> Any {
                k ++ "=" ++ v.as(i64).to_string()
            })
            assert_eq(mapped.get("a"), "a=10")
            assert_eq(mapped.get("c"), "c=30")
            assert_eq(mapped.format_map(), "\x7b\"a\": \"a=10\", \"c\": \"c=30\"\x7d")

            mapped.retain(|k: String, _: Any| -> bool { k == "c" })
            assert_eq(mapped.keys(), ["c"])

            66
        }

        fn main() -> i64 {
            run_checks() ! { _ => -999 }
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
            pub fn hash(self) -> u64 = (7).as(u64)
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

            -- Stress rehash, deterministic insertion order, and lazy `.iter()`
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
            assert_eq(ordered.iter().count(), (149).as(usize))

            let clone_hm = ordered.copy()
            assert_eq(clone_hm, ordered)
            clone_hm.set(999, 1)
            assert(clone_hm != ordered)

            -- `Set` operations & lazy iteration
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
            assert_eq(a.iter().map(|v| v.as(i64) * 2).collect(), [2, 4, 6])

            -- `Deque` wrap-around, growth, equality, and lazy iteration
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
            assert_eq(dq2, Deque.from_list([10, 20, 30]))
            assert_eq(dq2.iter().collect(), [10, 20, 30])

            91
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 91);
}

#[test]
fn coroutines_support_bidirectional_yield_nested_status_close_and_generators() {
    let src = r#"
        use std.coroutine
        use std.coroutine.{Coroutine, CoroutineError, CoroutineStatus, CoroutineStep}

        fn run_checks() -> !CoroutineError i64 {
            assert_eq(coroutine.running(), null)
            assert(not coroutine.is_yieldable())

            -- Bidirectional `step` / `try_resume` / `CoroutineStep` / `DeadCoroutine`
            let co = Coroutine.new(|first: Any| -> Any {
                let second = Coroutine.yield_val(value = first.as(i64) + 1)
                let third = coroutine.yield_val(value = second.as(i64) * 10)
                third.as(i64) + 5
            })
            assert_eq(co.status(), CoroutineStatus.suspended)
            assert(co.is_alive())

            let step1 = coroutine.step(co, arg = 10)!
            assert_eq(step1, CoroutineStep.yielded(11))
            assert(step1.is_yielded())
            assert(not step1.is_finished())
            assert_eq(step1.value(), 11)
            assert_eq(co.status(), CoroutineStatus.suspended)

            assert_eq(co.try_resume(arg = 4)!, 40)
            assert_eq(co.status(), CoroutineStatus.suspended)

            let step3 = co.step(arg = 7)!
            assert_eq(step3, CoroutineStep.finished(12))
            assert(step3.is_finished())
            assert(not step3.is_yielded())
            assert_eq(step3.value(), 12)
            assert_eq(co.status(), CoroutineStatus.dead)
            assert(not co.is_alive())

            let dead_err = co.step(arg = 0) match {
                _! => false,
                error CoroutineError.DeadCoroutine => true,
                error _ => false,
            }
            assert(dead_err)

            -- Nested multi-prompt coroutines: active parent stays `running` & rejects re-entry / close
            let inner = coroutine.create(|_: Any| -> Any {
                let self_co = coroutine.running().as(Coroutine)
                assert_eq(self_co.status(), CoroutineStatus.running)
                assert(coroutine.is_yieldable())
                let _ = coroutine.yield_val(value = "inner-paused")
                "inner-done"
            })
            let outer = coroutine.create(|_: Any| -> Any {
                let me = coroutine.running().as(Coroutine)
                assert_eq(me.status(), CoroutineStatus.running)

                -- Self-resume while running must fail with `AlreadyRunning`
                let self_resume_blocked = me.step() match {
                    _! => false,
                    error CoroutineError.AlreadyRunning => true,
                    error _ => false,
                }
                assert(self_resume_blocked)

                -- While `checker` runs inside `outer`, `outer` remains active (`running`)
                let checker = coroutine.create(|parent_co: Any| -> Any {
                    let p = parent_co.as(Coroutine)
                    assert_eq(p.status(), CoroutineStatus.running)
                    assert(not p.close())
                    let parent_reenter_blocked = p.try_resume() match {
                        _! => false,
                        error CoroutineError.AlreadyRunning => true,
                        error _ => false,
                    }
                    assert(parent_reenter_blocked)
                    123
                })
                let checker_step = checker.step(arg = me) ! { _ => panic("checker step failed") }
                assert_eq(checker_step, CoroutineStep.finished(123))
                assert_eq(me.status(), CoroutineStatus.running)

                let inner_step1 = inner.step() ! { _ => panic("inner step1 failed") }
                assert_eq(inner_step1, CoroutineStep.yielded("inner-paused"))
                assert_eq(inner.status(), CoroutineStatus.suspended)
                let inner_step2 = inner.step() ! { _ => panic("inner step2 failed") }
                assert_eq(inner_step2, CoroutineStep.finished("inner-done"))
                assert_eq(inner.status(), CoroutineStatus.dead)
                999
            })
            assert_eq(outer.step()!, CoroutineStep.finished(999))
            assert_eq(outer.status(), CoroutineStatus.dead)
            assert_eq(coroutine.running(), null)

            -- Closing a suspended coroutine returns `true` and transitions to `dead`
            let closable = coroutine.create(|_: Any| -> Any {
                let _ = coroutine.yield_val(value = 1)
                2
            })
            assert_eq(closable.step()!, CoroutineStep.yielded(1))
            assert_eq(closable.status(), CoroutineStatus.suspended)
            assert(closable.close())
            assert_eq(closable.status(), CoroutineStatus.dead)
            assert(not closable.close())

            -- Infinite & finite `GeneratorIter` streams (including yielding `null` mid-stream)
            let fib_six = Coroutine.generator(|| {
                var a = 1
                var b = 1
                while true {
                    let _ = Coroutine.yield_val(value = a)
                    let nxt = a + b
                    a = b
                    b = nxt
                }
                0
            }).take(6).collect()
            assert_eq(fib_six, [1, 1, 2, 3, 5, 8])

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

        fn main() -> i64 {
            run_checks() ! { _ => -999 }
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 42);
}

#[test]
fn json_rfc8259_parse_stringify_object_decode_derive_and_strict_rejections() {
    let src = r#"
        use std.json
        use std.math

        enum ServiceMode {
            active,
            standby(retry_ms: i64),
        }
        derive Eq, PartialEq, Json for ServiceMode

        struct ServiceConfig {
            host: String,
            port: i64 = 8080,
            mode: ServiceMode = ServiceMode.active,
            tag: ?String,
        }
        derive Eq, PartialEq, Json for ServiceConfig

        fn assert_parse_err(input: String, check: fn(JsonError) -> bool) {
            let matched = json.parse(input) match {
                _! => false,
                error err => check(err),
            }
            assert(matched, message = "expected JSON error for: " ++ input)
        }

        fn run_checks() -> !JsonError i64 {
            let doc = "\x7b\"active\":true,\"nothing\":null,\"count\":42,\"neg\":-9223372036854775808,\"ratio\":3.5,\"huge\":1e30,\"emoji\":\"\\uD83D\\uDE00\",\"escapes\":\"\\\"\\\\\\/\\b\\f\\n\\r\\t\\u001f\",\"items\":[1,false,\"ok\",[]]\x7d"
            let parsed: Map = json.decode(Map, doc)!
            assert_eq(parsed.get("active"), true)
            assert_eq(parsed.get("nothing"), null)
            assert_eq(parsed.get("count"), 42)
            assert_eq(parsed.get("neg"), -9223372036854775808)
            assert_eq(parsed.get("ratio"), 3.5)
            assert_eq(parsed.get("huge"), 1.0e30)
            let neg_zero = json.parse("-0")!
            assert_eq(type_of(neg_zero), f64)
            let emoji = parsed.get("emoji").as(String)
            assert_eq(emoji.len(), 4)
            assert_eq(emoji.char_len(), 1)
            assert_eq(emoji.first().as(char).to_code(), 128512)
            assert_eq(parsed.get("items"), [1, false, "ok", []])

            -- Compact & `indent = 2` pretty stringify + `json.object { ... }` builder
            let compact = json.stringify(parsed)!
            let reparsed: Map = json.decode(Map, compact)!
            assert_eq(reparsed, parsed)

            let small = json.object { a: "x\ny", b: [1, 2] }
            let pretty_text = json.stringify(small, indent = 2)!
            let expected_pretty = "\x7b\n  \"a\": \"x\\ny\",\n  \"b\": [\n    1,\n    2\n  ]\n\x7d"
            assert_eq(pretty_text, expected_pretty)
            assert_eq(json.parse(pretty_text)!, small)

            let mixed_obj = json.object { ("x-trace-id", "abc"), retries: 3 }
            assert_eq(mixed_obj.get("x-trace-id"), "abc")
            assert_eq(mixed_obj.get("retries"), 3)

            -- `derive Json`, `json.encode`, and type-directed `json.decode`
            let cfg = ServiceConfig {
                host: "api.internal",
                port: 9090,
                mode: ServiceMode.standby(250),
                tag: "blue",
            }
            let cfg_raw = json.encode(cfg)!
            let cfg_back: ServiceConfig = json.decode(
                ServiceConfig,
                cfg_raw,
                allow_unknown_fields = false
            )!
            assert_eq(cfg_back, cfg)

            let cfg_pretty = cfg.to_json_pretty(indent = 2)!
            assert(cfg_pretty.contains("\n  \"host\": \"api.internal\""))
            assert_eq(json.decode(ServiceConfig, cfg_pretty)!, cfg)

            let cfg_defaults: ServiceConfig = json.decode(
                ServiceConfig,
                "\x7b\"host\":\"edge\",\"ignored\":123\x7d",
                allow_unknown_fields = true
            )!
            assert_eq(
                cfg_defaults,
                ServiceConfig {
                    host: "edge",
                    port: 8080,
                    mode: ServiceMode.active,
                    tag: null,
                }
            )

            let strict_unknown = json.decode(
                ServiceConfig,
                "\x7b\"host\":\"edge\",\"rogue\":true\x7d",
                allow_unknown_fields = false
            ) match {
                _! => false,
                error JsonError.UnknownField(p, f) => p == "$" and f == "rogue",
                error _ => false,
            }
            assert(strict_unknown)

            let missing_req = json.decode(ServiceConfig, "\x7b\"port\":80\x7d") match {
                _! => false,
                error JsonError.MissingField(p, f) => p == "$" and f == "host",
                error _ => false,
            }
            assert(missing_req)

            let bad_variant = json.decode(ServiceMode, "\"BogusMode\"") match {
                _! => false,
                error JsonError.InvalidVariant(p, ty, v) => {
                    p == "$" and ty == "ServiceMode" and v == "BogusMode"
                },
                error _ => false,
            }
            assert(bad_variant)

            let out_of_range = json.decode(u8, "300") match {
                _! => false,
                error JsonError.IntegerOutOfRange(p, _, ty) => p == "$" and ty == "u8",
                error _ => false,
            }
            assert(out_of_range)

            let decoded_list: List = json.decode(List, "[10, 20, 30]")!
            assert_eq(decoded_list, [10, 20, 30])

            -- Strict RFC 8259 syntax rejections
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

            -- Unsupported `stringify` inputs (`NAN`, `INFINITY`, `NEG_INFINITY`, closures)
            let nan_rejected = json.stringify(math.NAN()) match {
                _! => false,
                error JsonError.UnsupportedValue(_) => true,
                error _ => false,
            }
            assert(nan_rejected)

            let inf_rejected = json.stringify(math.INFINITY()) match {
                _! => false,
                error JsonError.UnsupportedValue(_) => true,
                error _ => false,
            }
            assert(inf_rejected)

            let neg_inf_rejected = json.stringify(math.NEG_INFINITY()) match {
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

        fn main() -> i64 {
            run_checks() ! { _ => -999 }
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 108);

    let non_json_struct = r#"
        use std.json
        struct PlainRecord { id: i64 }
        fn main() -> i64 {
            let _ = json.decode(PlainRecord, "{}")
            0
        }
    "#;
    let compiled = driver::Driver::new().compile(non_json_struct);
    assert!(
        compiled.has_errors,
        "expected compile-time rejection for non-Json decode target"
    );
}
