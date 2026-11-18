use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

struct TempCliDir(PathBuf);

impl TempCliDir {
    fn new() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        loop {
            let dir = std::env::temp_dir().join(format!(
                "nessa-fmt-lsp-cli-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir_all(&dir) {
                Ok(()) => return Self(dir),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("failed to create temp cli dir: {e}"),
            }
        }
    }
}

impl Drop for TempCliDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn nessa_fmt_and_nessa_lsp_subcommands_execute_end_to_end() {
    let tmp = TempCliDir::new();
    let sample_path = tmp.0.join("sample.ns");
    std::fs::write(
        &sample_path,
        "\
enum Color { red, green }

--% .skip
fn keep_raw( x : i64 ) -> i64 = x+1

fn compute_total(a:i64,b:i64)->i64{
let c=Color.red
let _=c
a+b
}

fn main() {
    println(to_string(compute_total(1, 2) + keep_raw(3)))
}
",
    )
    .unwrap();

    let nessa = env!("CARGO_BIN_EXE_nessa");

    // 1. `nessa fmt --version` and `nessa lsp --version`
    let fmt_ver = Command::new(nessa)
        .args(["fmt", "--version"])
        .output()
        .expect("nessa fmt --version");
    assert!(fmt_ver.status.success());
    assert!(String::from_utf8_lossy(&fmt_ver.stdout).starts_with("nessa-fmt "));

    let lsp_ver = Command::new(nessa)
        .args(["lsp", "--version"])
        .output()
        .expect("nessa lsp --version");
    assert!(lsp_ver.status.success());
    assert!(String::from_utf8_lossy(&lsp_ver.stdout).starts_with("nessa-lsp "));

    // 2. `nessa fmt --check` detects diff with exit code 1
    let fmt_check_before = Command::new(nessa)
        .args(["fmt", "--check", sample_path.to_str().unwrap()])
        .output()
        .expect("nessa fmt --check before");
    assert_eq!(fmt_check_before.status.code(), Some(1));

    // 3. `nessa fmt` formats in place (preserving `--% .skip`) and second `--check` exits 0
    let fmt_apply = Command::new(nessa)
        .args(["fmt", sample_path.to_str().unwrap()])
        .output()
        .expect("nessa fmt in-place");
    assert!(fmt_apply.status.success());

    let formatted = std::fs::read_to_string(&sample_path).unwrap();
    assert!(formatted.contains("--% .skip\nfn keep_raw( x : i64 ) -> i64 = x+1"));
    assert!(formatted.contains("fn compute_total(a: i64, b: i64) -> i64 {"));

    let fmt_check_after = Command::new(nessa)
        .args(["fmt", "--check", sample_path.to_str().unwrap()])
        .output()
        .expect("nessa fmt --check after");
    assert_eq!(fmt_check_after.status.code(), Some(0));

    // 4. `nessa lsp --check` validates the formatted file cleanly (exit code 0)
    let lsp_check = Command::new(nessa)
        .args(["lsp", "--check", sample_path.to_str().unwrap()])
        .output()
        .expect("nessa lsp --check");
    assert_eq!(
        lsp_check.status.code(),
        Some(0),
        "nessa lsp --check stderr: {}",
        String::from_utf8_lossy(&lsp_check.stderr)
    );
}
