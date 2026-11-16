//! End-to-end CLI tests for local path dependencies, `package.lock`,
//! `.nsbc` disassembly (`nessa disasm` / `--emit-bc-dump`), and `nessa run` script arguments.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct WorkspaceDir(PathBuf);

impl WorkspaceDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let dir = std::env::temp_dir().join(format!(
                "nessa-cli-deps-{}-{}",
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

    fn pkg(
        &self,
        rel_dir: &str,
        id: &str,
        kind: &str,
        deps_toml: &str,
        entry_source: &str,
    ) -> PathBuf {
        let (qualified, version) = id.split_once('@').unwrap();
        let (domain, name) = qualified.split_once('/').unwrap();
        let manifest = format!(
            "[package]\nname = {name:?}\ndomain = {domain:?}\nversion = {version:?}\ntype = {kind:?}\n\n{deps_toml}\n"
        );
        self.write(&format!("{rel_dir}/package.toml"), &manifest);
        let entry = match kind {
            "exe" => "src/main.ns",
            "lib" => "src/lib.ns",
            "tmp" => "src/tmp.ns",
            other => panic!("unexpected package type {other}"),
        };
        self.write(&format!("{rel_dir}/{entry}"), entry_source);
        self.0.join(rel_dir)
    }
}

impl Drop for WorkspaceDir {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(entries) = fs::read_dir(&self.0) {
                for entry in entries.flatten() {
                    let _ = fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o755));
                }
            }
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn nessa(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nessa"))
        .args(args)
        .output()
        .unwrap()
}

fn assert_ok(out: &Output) {
    assert!(
        out.status.success(),
        "status={} stdout={} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn assert_all_commands_reject(pkg_dir: &Path, expected_stderr_substring: &str) {
    let dir_str = pkg_dir.to_str().unwrap();
    for cmd in ["check", "build", "run"] {
        let out = nessa(&[cmd, dir_str]);
        assert!(
            !out.status.success(),
            "`nessa {cmd}` unexpectedly succeeded for {}",
            pkg_dir.display()
        );
        assert!(
            out.stdout.is_empty(),
            "`nessa {cmd}` produced stdout on failure: {:?}",
            String::from_utf8_lossy(&out.stdout)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr
                .to_lowercase()
                .contains(&expected_stderr_substring.to_lowercase()),
            "`nessa {cmd}` stderr missing {expected_stderr_substring:?}: {stderr}"
        );
        assert!(
            !pkg_dir.join("package.nsbc").exists(),
            "`nessa {cmd}` created package.nsbc despite error"
        );
    }
}

#[test]
fn multi_package_diamond_dependencies_compile_run_build_lock_and_execute_standalone() {
    let ws = WorkspaceDir::new();
    ws.pkg(
        "shared",
        "org.demo/shared@0.2.0",
        "lib",
        "",
        r#"
pub mod extra {}

global init_seed: i64 = 0

fn __init__() {
    println("shared-init")
    init_seed = 10
}

pub fn base() -> i64 {
    init_seed
}
"#,
    );
    ws.write(
        "shared/src/extra.ns",
        "pub fn scale(x: i64) -> i64 { x * 3 }\n",
    );

    ws.pkg(
        "left",
        "org.demo/left@0.1.0",
        "lib",
        "[dependencies]\nshared = { path = \"../shared\", version = \"^0.2.0\" }",
        r#"
use @shared.base
use @shared.extra.scale

pub fn compute_left() -> i64 {
    scale(base()) + 2
}
"#,
    );

    ws.pkg(
        "nested/right",
        "org.demo/right@0.1.0",
        "lib",
        "[dependencies]\nshared = { path = \"../../shared\" }",
        r#"
use shared.base

pub fn compute_right() -> i64 {
    base()
}
"#,
    );

    let app_dir = ws.pkg(
        "app",
        "org.demo/app@0.1.0",
        "exe",
        "[dependencies]\nleft = { path = \"../left\" }\nright = { path = \"../nested/right\", version = \"0.1.0\" }",
        r#"
use @left.compute_left
use right.compute_right

fn main() {
    println(compute_left() + compute_right())
}
"#,
    );
    let app_str = app_dir.to_str().unwrap();

    // 1. `nessa check` succeeds and does NOT create `package.lock` (even on read-only directory).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&app_dir, fs::Permissions::from_mode(0o555)).unwrap();
        let check_ro = nessa(&["check", app_str]);
        let run_ro = nessa(&["run", app_str]);
        fs::set_permissions(&app_dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert_ok(&check_ro);
        assert_eq!(check_ro.stdout, b"ok\n");
        assert_ok(&run_ro);
        assert_eq!(run_ro.stdout, b"shared-init\n42\n");
        assert!(!app_dir.join("package.lock").exists());
    }
    #[cfg(not(unix))]
    {
        let check_out = nessa(&["check", app_str]);
        assert_ok(&check_out);
        assert_eq!(check_out.stdout, b"ok\n");
        let run_out = nessa(&["run", app_str]);
        assert_ok(&run_out);
        assert_eq!(run_out.stdout, b"shared-init\n42\n");
        assert!(!app_dir.join("package.lock").exists());
    }

    // 2. `nessa build` writes both `package.nsbc` and deterministic `package.lock`.
    let build_out = nessa(&["build", app_str]);
    assert_ok(&build_out);
    let lock_path = app_dir.join("package.lock");
    let archive_path = app_dir.join("package.nsbc");
    assert!(lock_path.is_file());
    assert!(archive_path.is_file());

    let lock_contents = fs::read_to_string(&lock_path).unwrap();
    assert!(
        lock_contents.starts_with("version = 1\n"),
        "{lock_contents}"
    );
    assert!(lock_contents.contains("name = \"left\""), "{lock_contents}");
    assert!(
        lock_contents.contains("name = \"right\""),
        "{lock_contents}"
    );
    assert!(
        lock_contents.contains("name = \"shared\""),
        "{lock_contents}"
    );
    assert!(
        lock_contents.contains("path = \"../nested/right\""),
        "{lock_contents}"
    );

    // Building a second time produces byte-for-byte identical `package.lock`.
    let build_again = nessa(&["build", app_str]);
    assert_ok(&build_again);
    assert_eq!(fs::read_to_string(&lock_path).unwrap(), lock_contents);

    // 3. Updating `package.toml` dependencies updates `package.lock` deterministically on next `nessa build`.
    ws.write(
        "app/package.toml",
        "[package]\nname = \"app\"\ndomain = \"org.demo\"\nversion = \"0.1.0\"\ntype = \"exe\"\n\n[dependencies]\nleft = { path = \"../left\" }\n",
    );
    ws.write(
        "app/src/main.ns",
        "use @left.compute_left\nfn main() { println(compute_left() + 10) }\n",
    );
    let rebuild = nessa(&["build", app_str]);
    assert_ok(&rebuild);
    let updated_lock = fs::read_to_string(&lock_path).unwrap();
    assert!(!updated_lock.contains("name = \"right\""), "{updated_lock}");
    assert!(updated_lock.contains("name = \"left\""), "{updated_lock}");
    assert!(updated_lock.contains("name = \"shared\""), "{updated_lock}");

    // 4. Built `.nsbc` archive is completely self-contained after deleting all `.ns` sources.
    let saved_archive = ws.0.join("standalone.nsbc");
    fs::copy(&archive_path, &saved_archive).unwrap();
    fs::remove_dir_all(ws.0.join("shared")).unwrap();
    fs::remove_dir_all(ws.0.join("left")).unwrap();
    fs::remove_dir_all(&app_dir).unwrap();

    let run_archive = nessa(&["run", saved_archive.to_str().unwrap()]);
    assert_ok(&run_archive);
    assert_eq!(run_archive.stdout, b"shared-init\n42\n");
}

#[test]
fn cli_rejects_cycles_missing_paths_version_mismatches_conflicts_and_corrupt_lockfile() {
    // Missing dependency directory
    let ws1 = WorkspaceDir::new();
    let missing_app = ws1.pkg(
        "app",
        "org.demo/app@0.1.0",
        "exe",
        "[dependencies]\nmissing_dep = { path = \"../missing_dep\" }",
        "fn main() {}\n",
    );
    assert_all_commands_reject(&missing_app, "missing_dep");

    // Cycle (`a -> b -> a`)
    let ws2 = WorkspaceDir::new();
    let cyc_a = ws2.pkg(
        "a",
        "org.demo/a@0.1.0",
        "exe",
        "[dependencies]\nb = { path = \"../b\" }",
        "fn main() {}\n",
    );
    ws2.pkg(
        "b",
        "org.demo/b@0.1.0",
        "lib",
        "[dependencies]\na = { path = \"../a\" }",
        "",
    );
    assert_all_commands_reject(&cyc_a, "cycle");

    // Version mismatch
    let ws3 = WorkspaceDir::new();
    ws3.pkg("dep", "org.demo/dep@0.3.0", "lib", "", "");
    let ver_app = ws3.pkg(
        "app",
        "org.demo/app@0.1.0",
        "exe",
        "[dependencies]\ndep = { path = \"../dep\", version = \"^1.0.0\" }",
        "fn main() {}\n",
    );
    assert_all_commands_reject(&ver_app, "version");

    // Conflicting paths for same dependency name
    let ws4 = WorkspaceDir::new();
    ws4.pkg("one/util", "org.demo/util@0.1.0", "lib", "", "");
    ws4.pkg("two/util", "org.demo/util@0.1.0", "lib", "", "");
    ws4.pkg(
        "helper",
        "org.demo/helper@0.1.0",
        "lib",
        "[dependencies]\nutil = { path = \"../two/util\" }",
        "",
    );
    let conflict_app = ws4.pkg(
        "app",
        "org.demo/app@0.1.0",
        "exe",
        "[dependencies]\nutil = { path = \"../one/util\" }\nhelper = { path = \"../helper\" }",
        "fn main() {}\n",
    );
    assert_all_commands_reject(&conflict_app, "conflicting");

    // Corrupt `package.lock`
    let ws5 = WorkspaceDir::new();
    ws5.pkg("dep", "org.demo/dep@0.1.0", "lib", "", "");
    let corrupt_lock_app = ws5.pkg(
        "app",
        "org.demo/app@0.1.0",
        "exe",
        "[dependencies]\ndep = { path = \"../dep\" }",
        "fn main() {}\n",
    );
    ws5.write("app/package.lock", "version = \"corrupt-not-an-int\"\n");
    assert_all_commands_reject(&corrupt_lock_app, "package.lock");
}

#[test]
fn disasm_subcommand_and_emit_bc_dump_flag_work_across_source_package_and_archive() {
    let ws = WorkspaceDir::new();
    let single_file = ws.write(
        "hello.ns",
        r#"
fn twice(n: i64) -> i64 {
    n + n
}

fn main() {
    println(twice(21))
}
"#,
    );
    let single_str = single_file.to_str().unwrap();

    // 1. `nessa disasm <file.ns>` prints deterministic disassembly to stdout.
    let disasm_src = nessa(&["disasm", single_str]);
    assert_ok(&disasm_src);
    let disasm_text = String::from_utf8(disasm_src.stdout).unwrap();
    assert!(
        disasm_text.contains("; NSBC Disassembly\n"),
        "{disasm_text}"
    );
    assert!(
        disasm_text.contains("builtin_abi_version:"),
        "{disasm_text}"
    );
    assert!(disasm_text.contains("twice"), "{disasm_text}");
    assert!(disasm_text.contains("0000:"), "{disasm_text}");

    // 2. `nessa build <file.ns> --emit-bc-dump` writes both `.nsbc` and `.bc`.
    let build_out = nessa(&["build", single_str, "--emit-bc-dump"]);
    assert_ok(&build_out);
    let nsbc_file = ws.0.join("hello.nsbc");
    let bc_file = ws.0.join("hello.bc");
    assert!(nsbc_file.is_file());
    assert!(bc_file.is_file());
    let dumped_bc = fs::read_to_string(&bc_file).unwrap();
    assert!(dumped_bc.contains("twice"), "{dumped_bc}");

    // 3. `nessa disasm <file.nsbc>` matches `.bc` dump emitted from the archive.
    let disasm_nsbc = nessa(&["disasm", nsbc_file.to_str().unwrap()]);
    assert_ok(&disasm_nsbc);
    assert_eq!(String::from_utf8(disasm_nsbc.stdout).unwrap(), dumped_bc);

    // 4. `nessa check <pkg_dir> --emit-bc-dump` writes `package.bc` inside package directory.
    let pkg_dir = ws.pkg(
        "pkg",
        "org.demo/pkg@0.1.0",
        "exe",
        "",
        "fn main() { println(99) }\n",
    );
    let pkg_str = pkg_dir.to_str().unwrap();
    let check_dump = nessa(&["check", pkg_str, "--emit-bc-dump"]);
    assert_ok(&check_dump);
    let pkg_bc = pkg_dir.join("package.bc");
    assert!(pkg_bc.is_file());
    let pkg_disasm = nessa(&["disasm", pkg_str]);
    assert_ok(&pkg_disasm);
    assert_eq!(
        String::from_utf8(pkg_disasm.stdout).unwrap(),
        fs::read_to_string(&pkg_bc).unwrap()
    );

    // 5. Malformed `.nsbc` archive is cleanly rejected by `nessa disasm` and `nessa run --emit-bc-dump`.
    let bad_nsbc = ws.write("broken.nsbc", "not a valid nsbc binary");
    let bad_str = bad_nsbc.to_str().unwrap();
    let bad_disasm = nessa(&["disasm", bad_str]);
    assert!(!bad_disasm.status.success());
    assert!(bad_disasm.stdout.is_empty());
    assert!(
        String::from_utf8_lossy(&bad_disasm.stderr)
            .to_lowercase()
            .contains("disassembly failed")
    );
    let bad_run = nessa(&["run", bad_str, "--emit-bc-dump"]);
    assert!(!bad_run.status.success());
}

#[test]
fn run_subcommand_accepts_double_dash_flags_and_positional_args_across_targets() {
    let ws = WorkspaceDir::new();
    let script = ws.write("script.ns", "fn main() { println(42) }\n");
    let script_str = script.to_str().unwrap();

    // `nessa run file.ns -- --some-flag foo` passes `--some-flag` verbatim without rejecting it.
    let run_file = nessa(&["run", script_str, "--", "--some-flag", "foo", "bar"]);
    assert_ok(&run_file);
    assert_eq!(run_file.stdout, b"42\n");

    // Trailing positional args without `--` also work cleanly.
    let run_pos = nessa(&["run", script_str, "arg1", "arg2"]);
    assert_ok(&run_pos);
    assert_eq!(run_pos.stdout, b"42\n");

    // Unknown flag BEFORE `--` is still rejected as an unknown `nessa` CLI option.
    let bad_flag = nessa(&["run", script_str, "--some-flag", "foo"]);
    assert!(!bad_flag.status.success());
    assert!(
        String::from_utf8_lossy(&bad_flag.stderr).contains("unknown option: --some-flag"),
        "{}",
        String::from_utf8_lossy(&bad_flag.stderr)
    );

    // Works identically for `.nsbc` archives and package directories!
    assert_ok(&nessa(&["build", script_str]));
    let nsbc_path = ws.0.join("script.nsbc");
    let run_nsbc = nessa(&["run", nsbc_path.to_str().unwrap(), "--", "--flag", "x"]);
    assert_ok(&run_nsbc);
    assert_eq!(run_nsbc.stdout, b"42\n");

    let pkg_dir = ws.pkg(
        "cli_pkg",
        "org.demo/cli_pkg@0.1.0",
        "exe",
        "",
        "fn main() { println(42) }\n",
    );
    let run_pkg = nessa(&[
        "run",
        pkg_dir.to_str().unwrap(),
        "--",
        "--verbose",
        "input.txt",
    ]);
    assert_ok(&run_pkg);
    assert_eq!(run_pkg.stdout, b"42\n");
}

#[test]
fn cli_enforces_lockfile_integrity_same_short_name_domains_and_newtype_module_conflicts() {
    let ws = WorkspaceDir::new();
    ws.pkg(
        "alpha_utils",
        "org.alpha/utils@0.1.0",
        "lib",
        "",
        "pub fn val() -> i64 { 19 }\n",
    );
    ws.pkg(
        "beta_utils",
        "org.beta/utils@0.2.0",
        "lib",
        "",
        "pub fn val() -> i64 { 23 }\n",
    );
    ws.pkg(
        "left",
        "org.demo/left@0.1.0",
        "lib",
        "[dependencies]\n\"org.alpha/utils\" = { path = \"../alpha_utils\" }",
        "use @utils.val\npub fn left_v() -> i64 { val() }\n",
    );
    ws.pkg(
        "right",
        "org.demo/right@0.1.0",
        "lib",
        "[dependencies]\n\"org.beta/utils\" = { path = \"../beta_utils\" }",
        "use @utils.val\npub fn right_v() -> i64 { val() }\n",
    );
    let app_dir = ws.pkg(
        "app",
        "org.demo/app@0.1.0",
        "exe",
        "[dependencies]\nleft = { path = \"../left\" }\nright = { path = \"../right\" }",
        r#"
use @left.left_v
use @right.right_v

fn main() {
    println(left_v() + right_v())
}
"#,
    );
    let app_str = app_dir.to_str().unwrap();

    // `--locked` before `package.lock` exists must fail cleanly.
    let missing_locked = nessa(&["build", "--locked", app_str]);
    assert!(!missing_locked.status.success());
    assert!(
        String::from_utf8_lossy(&missing_locked.stderr).contains("missing package.lock"),
        "{}",
        String::from_utf8_lossy(&missing_locked.stderr)
    );

    // Initial `nessa build` creates valid `package.lock` with both `org.alpha/utils` and `org.beta/utils`.
    assert_ok(&nessa(&["build", app_str]));
    assert_ok(&nessa(&["build", "--locked", app_str]));
    let run_ok = nessa(&["run", "--locked", app_str]);
    assert_ok(&run_ok);
    assert_eq!(run_ok.stdout, b"42\n");

    // Mutating dependency source invalidates `package.lock`: `check`, `run`, `disasm`, and `build --locked` MUST all fail!
    let lock_path = app_dir.join("package.lock");
    let orig_lock = fs::read_to_string(&lock_path).unwrap();
    ws.write("beta_utils/src/lib.ns", "pub fn val() -> i64 { 100 }\n");

    for args in [
        &["check", app_str][..],
        &["run", app_str][..],
        &["disasm", app_str][..],
        &["build", "--locked", app_str][..],
    ] {
        let out = nessa(args);
        assert!(
            !out.status.success(),
            "`nessa {:?}` unexpectedly accepted stale package.lock",
            args
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("sha256 mismatch"),
            "`nessa {:?}` stderr missing sha256 mismatch: {stderr}",
            args
        );
    }
    // Confirm `--locked` did NOT overwrite `package.lock`.
    assert_eq!(fs::read_to_string(&lock_path).unwrap(), orig_lock);

    // Ordinary `nessa build` updates `package.lock` cleanly so subsequent `run` succeeds with new value.
    assert_ok(&nessa(&["build", app_str]));
    let run_updated = nessa(&["run", app_str]);
    assert_ok(&run_updated);
    assert_eq!(run_updated.stdout, b"119\n");

    // `newtype` colliding with a filesystem module (`src/token.ns`) must be rejected cleanly.
    let conflict_pkg = ws.pkg(
        "newtype_conflict",
        "org.demo/newtype_conflict@0.1.0",
        "exe",
        "",
        "pub newtype token = i64\nfn main() {}\n",
    );
    ws.write("newtype_conflict/src/token.ns", "pub fn f() -> i64 { 1 }\n");
    let conflict_check = nessa(&["check", conflict_pkg.to_str().unwrap()]);
    assert!(!conflict_check.status.success());
    assert!(
        String::from_utf8_lossy(&conflict_check.stderr)
            .contains("filesystem module `token` conflicts with a source binding"),
        "{}",
        String::from_utf8_lossy(&conflict_check.stderr)
    );
}
