//! Typed patterns and Display-aware printing execute from archives without source.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "nessa-typed-pattern-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create test directory: {error}"),
            }
        }
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn cli(command: &str, path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nessa"))
        .arg(command)
        .arg(path)
        .output()
        .unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "status: {}; stdout: {}; stderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn roundtrip(source: &str, expected: &str) {
    let directory = TestDirectory::new();
    let path = directory.0.join("program.ns");
    std::fs::write(&path, source).unwrap();
    let source_run = cli("run", &path);
    assert_success(&source_run);
    assert_eq!(String::from_utf8(source_run.stdout).unwrap(), expected);
    assert_success(&cli("build", &path));
    let archive = path.with_extension("nsbc");
    assert!(archive.is_file());
    std::fs::remove_file(&path).unwrap();
    let archived = cli("run", &archive);
    assert_success(&archived);
    assert_eq!(String::from_utf8(archived.stdout).unwrap(), expected);
}

#[test]
fn archives_keep_typed_pattern_tests_and_trait_bindings() {
    roundtrip(
        "struct D{a:i64};derive Display for D;struct P{a:i64};fn describe(v:Any)->String{v match{p:P=>\"P{p.a}\",n:i64=>\"int {n}\",shown:Display=>\"shown {shown}\",_=>\"other\"}};fn main(){println(describe(P{a:7}));println(describe(5));println(describe(D{a:1}));println(describe([1]))}",
        "P7\nint 5\nshown D { a: 1 }\nother\n",
    );
}

#[test]
fn print_uses_display_when_available_and_native_output_otherwise() {
    roundtrip(
        "struct N{a:i64};impl Display for N{pub fn to_string(self)->String{\"named\"}};struct D{a:i64};derive Display for D;fn main(){let f=println;f(N{a:1});println(D{a:2});print(\"x\");print(N{a:3});println(\"\");println([1,2]);println((1,\"a\"));println(null);println(2.5)}",
        "named\nD { a: 2 }\nxnamed\n[1, 2]\n(1, \"a\")\nnull\n2.5\n",
    );
}
