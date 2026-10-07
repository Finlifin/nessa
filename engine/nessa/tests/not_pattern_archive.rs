//! Negated patterns execute from archives in a fresh process without source.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "nessa-not-pattern-{}-{}",
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
fn archives_keep_private_guard_captures_and_outer_tuple_aliases() {
    roundtrip(
        "enum E{some(text:String),none};fn main(){let saved:fn()->i64=||0;let selected=E.some(\"abcd\"++\"efgh\") match{not(E.some(text) if if true{saved=||text.len()+34;true}else{false})=>1,_=>2};if selected==2{println(saved())}else{println(0)}}",
        "42\n",
    );
    roundtrip(
        "fn main(){let f=(\"abcd\"++\"efgh\",42) match{not((text,n) if n==0) as whole=>||whole.0.len()+34,_=>||0};println(f())}",
        "42\n",
    );
}

#[test]
fn archives_restore_negated_guard_continuation_branches_and_trace() {
    roundtrip(
        "effect pause(catch k)->i64;global trace:i64=0;enum E{some(text:String),none};fn compute()->i64{let capture:fn()->i64=||0;E.some(\"abcd\"++\"efgh\") match{not(E.some(text) if if true{trace=trace*10+1;capture=||text.len();pause()#==0}else{false}) as whole=>{trace=trace*10+2;let f=||{whole match{E.some(text)=>text.len()+13,_=>0}};f()},_=>{trace=trace*10+3;capture()+13}}};fn main(){let branch=compute()#{pause(k)=>k};let a=branch(0).as(i64);let b=branch(1).as(i64);if a==21 and b==21 and trace==132{println(a+b)}else{println(0)}}",
        "42\n",
    );
}

#[test]
fn archives_preserve_negated_self_alias_frozen_proofs_and_initializer_guards() {
    roundtrip(
        "struct P{};trait Read{fn value(self)->i64;derive fn callback(self)->fn()->i64{self match{not(_ if false) as saved=>||saved.value(),_=>||0}}};mod a{extend Read for P{pub fn value(self)->i64{40}};pub fn callback()->fn()->i64{P{}.callback()}};mod b{extend Read for P{pub fn value(self)->i64{2}};pub fn answer()->i64{let f=a.callback();f()+P{}.value()}};fn main(){println(b.answer())}",
        "42\n",
    );
    roundtrip(
        "mod api{pub global answer:i64=42 match{not(x if storage.allow) as whole=>whole,_=>0}};mod storage{pub global allow:bool=false};mod unused{pub global value:i64=api.answer};fn main(){println(api.answer)}",
        "42\n",
    );
}

#[test]
fn negated_child_bindings_do_not_escape_into_case_bodies_or_outer_guards() {
    for source in [
        "enum E{some(text:String),none};fn main(){E.none match{not E.some(text)=>text.len(),_=>0}}",
        "enum E{some(text:String),none};fn main(){E.none match{not E.some(text) if text.len()>0=>42,_=>0}}",
        "enum E{some(text:String),none};fn main(){E.none matches not E.some(text);text}",
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("invalid.ns");
        std::fs::write(&path, source).unwrap();
        let output = cli("build", &path);
        assert!(!output.status.success(), "accepted {source}");
        assert!(
            !path.with_extension("nsbc").exists(),
            "created artifact for {source}"
        );
        let diagnostics = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(diagnostics.contains("undefined"), "{source}: {diagnostics}");
    }
}
