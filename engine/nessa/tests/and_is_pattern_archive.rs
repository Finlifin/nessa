//! Computed pattern constraints execute from archives in a fresh process without source.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "nessa-and-is-pattern-{}-{}",
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
fn archives_preserve_computed_heap_bindings_aliases_and_failed_guard_captures() {
    for source in [
        "enum E{some(text:String),none};fn main(){let f=E.some(\"abcd\"++\"efgh\") match{E.some(left) as original and E.some(left++\"ij\") is E.some(right) as computed=>||{if original'type==E and computed'type==E and left.len()==8{right.len()+32}else{0}},_=>||0};println(f())}",
        "enum E{some(text:String),none};fn main(){let saved:fn()->i64=||0;let result=(\"abcd\"++\"efgh\") match{left and E.some(left) is E.some(right) if if true{saved=||right.len()+34;false}else{false}=>0,_=>42};if result==42{println(saved())}else{println(0)}}",
    ] {
        roundtrip(source, "42\n");
    }
}

#[test]
fn archives_restore_rhs_expression_continuation_branches() {
    roundtrip(
        "effect pause(catch k)->i64;global trace:i64=0;enum E{some(text:String),none};fn compute()->i64{E.some(\"abcd\"++\"efgh\") match{E.some(left) and if true{trace=trace*10+1;let n=pause()#;E.some(left++if n==0{\"ab\"}else{\"cd\"})}else{E.none} is E.some(right)=>{if right==\"abcdefghab\"{trace=trace*10+2;20}else{trace=trace*10+3;22}},_=>0}};fn main(){let branch=compute()#{pause(k)=>k};let a=branch(0).as(i64);let b=branch(1).as(i64);if a==20 and b==22 and trace==123{println(a+b)}else{println(0)}}",
        "42\n",
    );
}

#[test]
fn archives_restore_computed_guard_branches_with_escaped_rhs_capture() {
    roundtrip(
        "effect pause(catch k)->i64;global trace:i64=0;enum E{some(text:String),none};fn compute()->i64{let keep:fn()->i64=||0;E.some(\"abcd\"++\"efgh\") match{E.some(left) as original and if true{trace=trace*10+1;E.some(left++\"ij\")}else{E.none} is (E.some(right) as computed if if true{trace=trace*10+2;keep=||right.len();pause()#==0}else{false})=>{trace=trace*10+3;let f=||{if original'type==E and computed'type==E{right.len()+10}else{0}};f()},_=>{trace=trace*10+4;keep()+12}}};fn main(){let branch=compute()#{pause(k)=>k};let a=branch(0).as(i64);let b=branch(1).as(i64);if a==20 and b==22 and trace==1234{println(a+b)}else{println(0)}}",
        "42\n",
    );
}

#[test]
fn archives_preserve_computed_self_proofs_item_specialization_and_initializer_order() {
    for source in [
        "struct P{};trait Read{fn value(self)->i64;derive fn callback(self)->fn()->i64{self match{_ as source and source is _ as saved=>||saved.value()}}};mod a{extend Read for P{pub fn value(self)->i64{40}};pub fn callback()->fn()->i64{P{}.callback()}};mod b{extend Read for P{pub fn value(self)->i64{2}};pub fn answer()->i64{let f=a.callback();f()+P{}.value()}};fn main(){println(b.answer())}",
        "struct P{};struct Q{};trait Source{assoc Item:Type=Any;fn next(self)->Item;derive fn callback(self)->fn()->Item{self.next() match{x and x is y=>||y}}};impl Source for P{assoc Item:Type=i64;pub fn next(self)->i64{42}};impl Source for Q{assoc Item:Type=String;pub fn next(self)->String{\"abcd\"++\"efgh\"}};fn main(){let p:fn()->i64=P{}.callback();let q:fn()->String=Q{}.callback();if p'type==fn()->i64 and q'type==fn()->String and q()==\"abcdefgh\"{println(p())}else{println(0)}}",
        "mod api{pub global answer:i64=40 match{left and left+storage.value is 42=>42,_=>0}};mod storage{pub global value:i64=2};mod unused{pub global value:i64=api.answer};fn main(){println(api.answer)}",
    ] {
        roundtrip(source, "42\n");
    }
}

#[test]
fn malformed_duplicate_and_future_computed_bindings_reject_archives() {
    for (source, expected) in [
        ("fn main(){40 match{x and y is y=>42,_=>0}}", "undefined"),
        ("fn main(){40 match{x and x is x=>42,_=>0}}", "same name"),
        ("fn main(){40 match{x and 42=>42,_=>0}}", "is"),
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
        let diagnostics = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(diagnostics.contains(expected), "{source}: {diagnostics}");
    }
}
