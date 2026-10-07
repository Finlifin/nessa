//! Effect contracts execute from archives in a fresh process without source.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "nessa-effect-contract-{}-{}",
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
    assert!(!path.exists());
    let archived = cli("run", &archive);
    assert_success(&archived);
    assert_eq!(String::from_utf8(archived.stdout).unwrap(), expected);
}

#[test]
fn typed_continuations_and_handler_boundaries_survive_source_deleted_archives() {
    for source in [
        r#"effect text(catch k)->String;fn compute(){text()#.len()+34};fn main(){println(compute()#{text(k)=>{let alias=k;alias.clone()("ab"++"cdefgh")}})}"#,
        r#"effect ask()->i64;effect flag()->bool;fn result(){ask()#{ask()=>{let b=flag()#{flag()=>{return true}};if b{42}else{0}}}};fn main(){println(result())}"#,
        r#"effect choose(catch k)->i64;fn compute(){let a=choose()#;a+choose()#};fn main(){let first=compute()#{choose(k)=>k};let second=first(10);println(second(32))}"#,
        r#"effect maybe(catch k)->?String;effect tick(catch k)->Unit;effect small(catch k)->i8;fn main(){let n=maybe()#{maybe(k)=>k(null)};tick()#{tick(k)=>k(())};let v=small()#{small(k)=>k(42)};if n==null and type_of(v)==i8{println(v)}else{println(0)}}"#,
    ] {
        roundtrip(source, "42\n");
    }
}

#[test]
fn known_invalid_resume_and_handler_exits_never_produce_archives() {
    for source in [
        "effect choose(catch k)->i64;fn main(){choose()#{choose(k)=>k(true)}}",
        "effect choose(catch k)->i64;fn main(){choose()#{choose(k)=>{let alias=k;alias.clone()(true)}}}",
        "effect choose(catch k)->i64;fn main(){choose()#{choose(k)=>{let f=||{k(true)};f()}}}",
        "effect ask()->i64;fn main(){ask()#{ask()=>{return true;42}}}",
        "effect ask()->i64;fn main(){ask()#{ask()=>{resume true if true;42}}}",
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("invalid.ns");
        std::fs::write(&path, source).unwrap();
        let output = cli("build", &path);
        assert!(!output.status.success(), "accepted {source}");
        assert!(
            !path.with_extension("nsbc").exists(),
            "created archive for {source}"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("compilation failed"),
            "{source}: {output:?}"
        );
    }
}

#[test]
fn erased_wrong_resumes_and_dynamic_handler_exits_keep_runtime_errors_in_archives() {
    for source in [
        "effect choose(catch k)->i64;fn invoke(k:Continuation){k(true);42};fn main(){let saved=choose()#{choose(k)=>k};invoke(saved)}",
        "effect choose(catch k)->i64;fn identity(k:Continuation)->Continuation{k};fn main(){let saved=choose()#{choose(k)=>k};identity(saved).clone()(true);42}",
        "effect choose(catch k)->i64;struct Box{value:Continuation};fn main(){let saved=choose()#{choose(k)=>k};let box=Box{value:saved};box.value.clone()(true);42}",
        "effect ask()->i64;fn wrong()->Any{true};fn main(){ask()#{ask()=>{return wrong();42}};42}",
        "effect ask()->i64;fn wrong()->Any{true};fn main(){ask()#{ask()=>{resume wrong();42}};42}",
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("dynamic.ns");
        std::fs::write(&path, source).unwrap();
        let source_run = cli("run", &path);
        assert!(!source_run.status.success(), "{source}: {source_run:?}");
        assert!(
            String::from_utf8_lossy(&source_run.stderr).contains("TypeError"),
            "{source}: {source_run:?}"
        );
        assert_success(&cli("build", &path));
        let archive = path.with_extension("nsbc");
        assert!(archive.is_file());
        std::fs::remove_file(&path).unwrap();
        assert!(!path.exists());
        let archived = cli("run", &archive);
        assert!(!archived.status.success(), "{source}: {archived:?}");
        assert!(
            String::from_utf8_lossy(&archived.stderr).contains("TypeError"),
            "{source}: {archived:?}"
        );
    }
}
