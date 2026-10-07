//! List patterns execute from archives in a fresh process without source.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "nessa-list-pattern-{}-{}",
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
fn archives_preserve_typed_rest_null_elements_and_nested_captures() {
    for source in [
        "fn main(){let value:Any=[null,40,2,null];let answer=value match{[null,...middle,null]=>if middle'type==List and middle.len()==2{middle(0).as(i64)+middle(1).as(i64)}else{0},_=>0};println(answer)}",
        "fn main(){let value:Any=[null,()];println(value match{[null,()]=>42,_=>0})}",
        "enum E{some(text:String)};fn make()->fn()->i64{let xs=[\"abcd\"++\"efgh\",[E.some(\"ijkl\"++\"mnop\"),4],\"qrst\"++\"uvwx\"];xs match{[first,[inside,...inner],last]=>||{inside.as(E) match{E.some(text)=>first.as(String).len()+text.len()+last.as(String).len()+inner(0).as(i64)+14}},_=>||0}};fn main(){let f=make();println(f())}",
    ] {
        roundtrip(source, "42\n");
    }
}

#[test]
fn archives_preserve_independent_rest_slots_and_failed_attempt_captures() {
    for source in [
        "struct Cell{value:i64};fn main(){let a=Cell{value:40};let b=Cell{value:2};let xs=[a,b];let f=xs match{[...tail] as original if if true{let first:Cell=tail(0);first.value=first.value+1;original(0)=Cell{value:0};tail(1)=Cell{value:3};true}else{false}=>||{if a.value==41 and b.value==2 and original(0).as(Cell).value==0 and original(1).as(Cell).value==2{tail(0).as(Cell).value+tail(1).as(Cell).value-2}else{0}},_=>||0};println(f())}",
        "fn main(){let saved:fn()->i64=||0;let xs=[\"prefix\"++\"heap\",\"abcd\"++\"efgh\",\"suffix\"++\"heap\"];let selected=xs match{[first,...rest,last] if if true{saved=||rest(0).as(String).len()+34;xs(0)=40;xs(1)=2;xs.pop();false}else{false}=>0,[40,2]=>42,_=>0};if selected==42{println(saved())}else{println(0)}}",
    ] {
        roundtrip(source, "42\n");
    }
}

#[test]
fn archives_preserve_guard_snapshots_and_shared_heap_iterator_cursor() {
    roundtrip(
        "effect pause(catch k)->i64;global visits:i64=0;global trace:i64=0;global bodies:i64=0;global checked:i64=0;fn compute()->i64{let rows=[[\"abcd\"++\"efgh\",2],[\"ijkl\"++\"mnop\",2]];let total:i64=0;for [head,...rest] as original if if true{visits+=1;trace=trace*10+1;if visits==1{let row:List=original;row.pop();row.pop();let choice=pause()#;if head.as(String).len()==8 and rest.len()==1 and rest(0)==2{checked+=1;choice==0}else{false}}else{true}}else{false} in rows{bodies+=1;total+=head.as(String).len()+rest(0).as(i64)};total};fn main(){let branch=compute()#{pause(k)=>k};let a=branch(0).as(i64);let b=branch(1).as(i64);if a==20 and b==0 and visits==2 and bodies==2 and trace==11 and checked==2{println(a+b+22)}else{println(0)}}",
        "42\n",
    );
}

#[test]
fn archives_keep_default_item_types_frozen_self_guards_and_initializer_dependencies() {
    for source in [
        "struct P{};struct Q{};trait Source{assoc Item:Type=Any;fn next(self)->IterationStep(List);fn fallback(self)->Item;derive fn callback(self)->fn()->Item{self.next() match{IterationStep(List).yielded([value,...rest])=>||value.as(Item),_=>||self.fallback()}}};impl Source for P{assoc Item:Type=i64;pub fn next(self)->IterationStep(List){IterationStep(List).yielded([42])};pub fn fallback(self)->i64{0}};impl Source for Q{assoc Item:Type=String;pub fn next(self)->IterationStep(List){IterationStep(List).yielded([\"abcd\"++\"efgh\"])};pub fn fallback(self)->String{\"\"}};fn main(){let p:fn()->i64=P{}.callback();let q:fn()->String=Q{}.callback();if p'type==fn()->i64 and q'type==fn()->String and q()==\"abcdefgh\"{println(p())}else{println(0)}}",
        "struct P{};trait Read{fn value(self)->i64;derive fn callback(self)->fn()->i64{[42] match{[x] if self.value()==40=>||{x.as(i64)+self.value()-40},_=>||0}}};mod a{extend Read for P{pub fn value(self)->i64{40}};pub fn callback()->fn()->i64{P{}.callback()}};mod b{extend Read for P{pub fn value(self)->i64{2}};pub fn answer()->i64{let f=a.callback();f()+P{}.value()-2}};fn main(){println(b.answer())}",
        "mod api{pub global answer:i64=[40,2] match{[x,...rest] if storage.allow=>x.as(i64)+rest(0).as(i64),_=>0}};mod storage{pub global allow:bool=true};mod unused{pub global value:i64=api.answer};fn main(){println(api.answer)}",
    ] {
        roundtrip(source, "42\n");
    }
}

#[test]
fn malformed_rest_and_future_bindings_reject_builds_without_archives() {
    for (source, expected) in [
        ("fn main(){[42] match{[..._]=>42,_=>0}}", "identifier"),
        ("fn main(){[42] match{[...a,...b]=>42,_=>0}}", "one rest"),
        (
            "fn main(){[42] match{[x if rest.len()==0,...rest]=>42,_=>0}}",
            "undefined",
        ),
        ("fn main(){40 match{[x]=>42,_=>0}}", "requires List or Any"),
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
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostics.contains(expected), "{source}: {output:?}");
    }
}

#[test]
fn archives_restore_list_guard_multishot_with_frame_local_fold_cursor() {
    roundtrip(
        "effect pause(catch k)->i64;global visits:i64=0;global trace:i64=0;global bodies:i64=0;fn compute()->i64{let rows=[[\"abcd\"++\"efgh\",2],[\"ijkl\"++\"mnop\",2]];let total=rows.fold(0,|acc,row|{row match{[head,...rest] as original if if true{visits+=1;trace=trace*10+1;if visits==1{let list:List=original;list.pop();list.pop();pause()#==0}else{true}}else{false}=>{bodies+=1;acc+head.as(String).len()+rest(0).as(i64)},_=>acc}}).as(i64);if total==20{20}else if total==10{22}else{0}};fn main(){let branch=compute()#{pause(k)=>k};let a=branch(0).as(i64);let b=branch(1).as(i64);if a==20 and b==22 and visits==3 and bodies==3 and trace==111{println(a+b)}else{println(0)}}",
        "42\n",
    );
}
