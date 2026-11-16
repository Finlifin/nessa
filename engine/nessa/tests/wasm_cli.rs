use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

struct TempCliDir(PathBuf);

impl TempCliDir {
    fn new() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        loop {
            let dir = std::env::temp_dir().join(format!(
                "nessa-wasm-cli-{}-{}",
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
fn cli_builds_disassembles_and_runs_standalone_wasm32_wasi_binaries() {
    let tmp = TempCliDir::new();
    let src_path = tmp.0.join("hello_wasm.ns");
    let wasm_path = tmp.0.join("hello_wasm.wasm");

    std::fs::write(
        &src_path,
        r#"
use std.env
fn main() {
    let args = env.args()
    println("wasm_cli:" ++ to_string(args.len()))
}
"#,
    )
    .unwrap();

    let nessa = env!("CARGO_BIN_EXE_nessa");

    // 1. Auto-detect `wasm32-wasi` when `-o *.wasm` is passed!
    let build = Command::new(nessa)
        .args([
            "build",
            src_path.to_str().unwrap(),
            "-O2",
            "-o",
            wasm_path.to_str().unwrap(),
        ])
        .output()
        .expect("nessa build -o hello_wasm.wasm");
    assert!(
        build.status.success(),
        "nessa build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    assert!(wasm_path.is_file());

    // 2. `nessa disasm hello_wasm.wasm`
    let disasm = Command::new(nessa)
        .args(["disasm", wasm_path.to_str().unwrap()])
        .output()
        .expect("nessa disasm hello_wasm.wasm");
    assert!(disasm.status.success());
    let disasm_out = String::from_utf8_lossy(&disasm.stdout);
    assert!(disasm_out.contains("_start"), "{disasm_out}");

    // 3. `nessa run hello_wasm.wasm -- alpha beta`
    let run = Command::new(nessa)
        .args(["run", wasm_path.to_str().unwrap(), "--", "alpha", "beta"])
        .output()
        .expect("nessa run hello_wasm.wasm");
    assert!(
        run.status.success(),
        "nessa run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "wasm_cli:3\n");
}
