//! Execute callback archives in fresh processes after deleting their source.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "nessa-list-callback-{}-{}",
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
        String::from_utf8_lossy(&output.stderr),
    );
}

fn source_deleted_roundtrip(source: &str, expected: &str) {
    let directory = TestDirectory::new();
    let path = directory.0.join("program.ns");
    std::fs::write(&path, source).unwrap();
    let original = cli("run", &path);
    assert_success(&original);
    assert_eq!(String::from_utf8(original.stdout).unwrap(), expected);
    assert_success(&cli("build", &path));
    let archive = path.with_extension("nsbc");
    assert!(archive.is_file());
    std::fs::remove_file(&path).unwrap();
    assert!(!path.exists());
    let restored = cli("run", &archive);
    assert_success(&restored);
    assert_eq!(String::from_utf8(restored.stdout).unwrap(), expected);
}

#[test]
fn list_callback_apis_null_unit_and_source_snapshots_survive_source_deletion() {
    source_deleted_roundtrip(
        "global total:i64=0;fn main(){let xs=[null,(),40,2];let mapped=xs.map(|x|{xs.pop();xs.push(99);x});let kept=mapped.filter(|x|x!=null and x!=());println(kept.fold(0,|acc,x|acc+x));kept.each(|x|{total+=x.as(i64)});kept.foreach(|x|{total+=x.as(i64)});println(total);println(mapped.len()==4 and mapped(0)==null and mapped(1)==());println(List().fold(null,|acc,x|42)==null)}",
        "42\n84\ntrue\ntrue\n",
    );
}

#[test]
fn returned_capturing_closures_and_typed_any_callbacks_survive_source_deletion() {
    source_deleted_roundtrip(
        "fn make()->fn(Any)->Any{let xs=[40];let f=|x|xs(0)+x;f};fn positive(x:i64)->bool{x>0};fn main(){let mapper:Any=make();let predicate:Any=positive;let xs=[-1,0,1,2].filter(predicate).map(mapper);println(xs(0));println(xs(1));println(xs.fold(0,|acc,x|acc+x))}",
        "41\n42\n83\n",
    );
}

#[test]
fn multishot_fold_cursor_and_accumulators_survive_source_deletion() {
    source_deleted_roundtrip(
        "effect pause(catch k)->i64;global entries:i64=0;fn compute()->i64{[1,2,3].fold(0,|acc,x|{entries+=1;if x==1{pause()#}else{acc+x}}).as(i64)};fn main(){let saved=compute()#{pause(k)=>k};let first=saved(10).as(i64);let second=saved(20).as(i64);println(if first==15 and second==25 and entries==5{first+second+2}else{0})}",
        "42\n",
    );
}

#[test]
fn erased_callback_argument_and_result_errors_survive_source_deletion() {
    for source in [
        "fn read(x:i64)->i64{x};fn main(){let f:Any=read;[null].map(f)}",
        "fn main(){let f:Any=|x|42;[1].filter(f)}",
        "fn main(){let f:Any=|x|null;[1].each(f)}",
        "fn main(){let f:Any=|x|true;[1].foreach(f)}",
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("invalid-callback.ns");
        std::fs::write(&path, source).unwrap();
        let original = cli("run", &path);
        assert!(!original.status.success());
        assert!(original.stdout.is_empty());
        assert!(String::from_utf8_lossy(&original.stderr).contains("TypeError"));
        assert_success(&cli("build", &path));
        let archive = path.with_extension("nsbc");
        assert!(archive.is_file());
        std::fs::remove_file(&path).unwrap();
        assert!(!path.exists());
        let restored = cli("run", &archive);
        assert!(!restored.status.success());
        assert!(restored.stdout.is_empty());
        assert!(String::from_utf8_lossy(&restored.stderr).contains("TypeError"));
    }
}

#[test]
fn invoked_callback_initialization_edges_survive_source_deletion() {
    source_deleted_roundtrip(
        "fn apply(f:fn(Any)->Any)->List{let copy=f;[0].map(|x|copy(x))};mod api{pub global answer:List=apply(|x|storage.n)};mod storage{pub global n:i64=42;pub fn unused()->List{api.answer}};fn main(){println(api.answer(0))}",
        "42\n",
    );
}
