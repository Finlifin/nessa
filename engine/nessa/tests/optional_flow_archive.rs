//! Optional flow results execute from archives in a fresh process without source.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "nessa-optional-flow-{}-{}",
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
fn source_deleted_archives_preserve_propagation_null_exits_patterns_and_unwrap() {
    for source in [
        "fn get()->?i64{42};fn read(){get()?};fn main(){println(read())}",
        "fn read()->?i64{let value:?bool=true;if value?{42}else{0}};fn main(){println(read())}",
        "global trace:i64=0;fn read()->?i64{let value:?i64=null;value?;trace=99;42};fn main(){if read()==null and trace==0{println(42)}else{println(0)}}",
        "fn main(){let value:?(i64,i64)=(40,2);value match{(a,b)?=>println(a+b),null=>println(0)}}",
        "fn main(){let value:?i8=42;println(value.unwrap())}",
        "fn get()->?NoReturn{null};fn read(){get()?};fn main(){if read()==null{println(42)}else{println(0)}}",
        "struct P{};impl P{pub fn unwrap(self)->i64{42}};fn main(){let value:Any=P{};println(value.unwrap())}",
        "struct P{n:i64};fn main(){let p=P{n:40};let value:?P=p;let alias=value.unwrap();alias.n=alias.n+2;println(p.n)}",
        "fn main(){let f=||->?i64{let n:?i64=null;n?;99};if f()==null{println(42)}else{println(0)}}",
        "effect pause(catch k)->?i64;fn work()->?i64{let n=pause()#?;n+20};fn main(){let k=work()#{pause(k)=>k};let missing=k(null);let a=k(0);let b=k(2);if missing==null{println(a+b)}else{println(0)}}",
        "struct P{};trait Read{fn get(self)->?i64;derive fn answer(self)->?i64{let value=self.get()?;value}};mod a{extend Read for P{pub fn get(self)->?i64{40}};pub fn answer()->?i64{P{}.answer()}};mod b{extend Read for P{pub fn get(self)->?i64{2}};pub fn answer()->?i64{P{}.answer()}};fn main(){println(a.answer().unwrap()+b.answer().unwrap())}",
    ] {
        roundtrip(source, "42\n");
    }
}

#[test]
fn invalid_optional_boundaries_and_defaults_never_emit_archives() {
    for source in [
        "fn main(){42?}",
        "fn get()->?i64{42};fn read()->?i64{get()?{return null}};fn main(){42}",
        "fn read()->?i64{let value:?i64=null;(value?{return null})};fn main(){42}",
        "fn main(){let value:?i64=42;value.unwrap(0)}",
        "fn main(){let value:?i64=42;let f=value.unwrap;42}",
        "fn read()->i64{let value:?i64=null;value?};fn main(){42}",
        "fn get()->?i64{42};fn f(.n:i64=get()?){n};fn main(){42}",
        "fn get()->?i64{42};struct P{n:i64=get()?};fn main(){42}",
        "fn get()->?i64{42};global n:?i64=get()?;fn main(){42}",
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("invalid.ns");
        std::fs::write(&path, source).unwrap();
        let output = cli("build", &path);
        assert_eq!(output.status.code(), Some(1), "{source}: {output:?}");
        assert!(!path.with_extension("nsbc").exists());
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("Expected"),
            "parser rejection cannot prove Optional diagnostics: {output:?}"
        );
    }
}

#[test]
fn null_unwrap_and_any_payload_assertions_fail_after_source_deletion() {
    for (source, error) in [
        (
            "fn main(){let value:?i64=null;value.unwrap();println(42)}",
            "panic",
        ),
        (
            "fn get()->Any{true};fn read()->?i64{let n:i64=get()?;n};fn main(){read();println(42)}",
            "TypeError",
        ),
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("dynamic.ns");
        std::fs::write(&path, source).unwrap();
        let output = cli("run", &path);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .to_lowercase()
                .contains(&error.to_lowercase()),
            "{output:?}"
        );
        assert_success(&cli("build", &path));
        let archive = path.with_extension("nsbc");
        assert!(archive.is_file());
        std::fs::remove_file(&path).unwrap();
        let output = cli("run", &archive);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .to_lowercase()
                .contains(&error.to_lowercase()),
            "{output:?}"
        );
    }
}
