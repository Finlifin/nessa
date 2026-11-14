//! Structural and dynamic equality, and List helpers, run from archives
//! without source.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "nessa-aggregate-equality-{}-{}",
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
fn archives_keep_structural_and_dynamic_equality() {
    roundtrip(
        "struct Q{x:i64};mod inner{extend PartialEq for Q{fn eq(self,other:Self)->bool{self.x==other.x}};pub fn same(a:Any,b:Any)->bool{a==b}};fn main(){let one:Any=1;let t:Any=(1,[2]);println([[1],[2]]==[[1],[2]]);println((1,\"a\")!=(1,\"b\"));println(one==1.0);println(t==(1,[2]));println(one==\"1\");println(inner.same(Q{x:1},Q{x:1}));println(inner.same(Q{x:1},Q{x:2}))}",
        "true\ntrue\ntrue\ntrue\nfalse\ntrue\nfalse\n",
    );
}

#[test]
fn archives_keep_map_equality_and_list_ends() {
    roundtrip(
        "fn main(){let a=Map();let b=Map();a(\"x\")=1;b(\"x\")=1;a(\"y\")=[2];b(\"y\")=[2];println(a==b);println(a.is_empty());println(Map().is_empty());println([1,2].first());println([1,2].last());println([\"a\",\"b\"].join(\"-\"))}",
        "true\nfalse\ntrue\n1\n2\na-b\n",
    );
}

#[test]
fn archives_keep_list_helpers() {
    roundtrip(
        "fn main(){let xs=[3,1,2];println(xs.contains(1));println(xs.index_of(2));println(xs.index_of(9));println(xs.reverse());println(xs.enumerate());println(xs.any(|v:Any|v==2));println(xs.all(|v:Any|v==2));println(xs.is_empty())}",
        "true\n2\nnull\n[2, 1, 3]\n[(0, 3), (1, 1), (2, 2)]\ntrue\nfalse\nfalse\n",
    );
}
