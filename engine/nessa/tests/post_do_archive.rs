//! Trailing callbacks execute from archives in a fresh process without source.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "nessa-post-do-{}-{}",
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
fn archives_preserve_trailing_callback_contracts_nested_captures_and_blocks() {
    for source in [
        "fn main(){let bias=1;let result=[1,2].map do |x|{([10,20].map do |y|{x+y+bias}).fold(0,|acc,y|acc+y)};println((result.fold(0) do |acc,x|{acc+x}).as(i64)-28)}",
        "fn keep(f:fn()->i64)->fn()->i64{f};fn make()->fn()->i64{let text=\"ab\"++\"cd\";keep do {text.len()+38}};fn main(){let f=make();println(f())}",
        "global total:i64=0;fn main(){[20,22].foreach do |x|{total+=x.as(i64)};println(total)}",
    ] {
        roundtrip(source, "42\n");
    }
}

#[test]
fn archives_plan_initializers_through_trailing_callback_dependencies() {
    roundtrip(
        "mod api{pub global values:List=[0].map do |x|{storage.value}};mod storage{pub global value:i64=42};mod unused{pub global value:i64=api.values(0).as(i64)};fn main(){println(api.values(0))}",
        "42\n",
    );
}

#[test]
fn archives_preserve_trailing_callback_shallow_snapshots_and_multishot() {
    for source in [
        "struct Cell{value:i64};fn main(){let a=Cell{value:40};let b=Cell{value:2};let xs=[a,b];let mapped=xs.map do |x|{xs.pop();let cell=x.as(Cell);cell.value=cell.value+1;cell};if xs.len()==0 and a.value==41 and b.value==3{println(mapped(0).as(Cell).value+mapped(1).as(Cell).value-2)}else{println(0)}}",
        "effect pause(catch k)->i64;global entries:i64=0;fn compute()->i64{([1,2,3].fold(0) do |acc,x|{entries+=1;if x==1{pause()#}else{acc+x}}).as(i64)};fn main(){let saved=compute()#{pause(k)=>k};let first=saved(10).as(i64);let second=saved(20).as(i64);if first==15 and second==25 and entries==5{println(first+second+2)}else{println(0)}}",
    ] {
        roundtrip(source, "42\n");
    }
}

#[test]
fn malformed_trailing_callbacks_reject_builds_without_archives() {
    for source in [
        "fn main(){[1].filter do |x|{42}}",
        "fn main(){[1].foreach do |x|{true}}",
        "fn main(){[1].fold(0) do |x|{x}}",
        "fn invoke(f:fn()->i64)->i64{f()};fn main(){invoke do {\"bad\"}}",
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
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("compilation failed"),
            "{source}: {output:?}"
        );
    }
}

#[test]
fn erased_trailing_callbacks_keep_runtime_type_errors_after_source_deletion() {
    let directory = TestDirectory::new();
    let path = directory.0.join("invalid-runtime.ns");
    let source = "fn forward(f:Any)->Any{[1].each(f)};fn main(){forward do |x|{[]}}";
    std::fs::write(&path, source).unwrap();
    let source_run = cli("run", &path);
    assert!(!source_run.status.success());
    assert!(
        String::from_utf8_lossy(&source_run.stderr).contains("TypeError"),
        "{source_run:?}"
    );
    assert_success(&cli("build", &path));
    let archive = path.with_extension("nsbc");
    assert!(archive.is_file());
    std::fs::remove_file(&path).unwrap();
    let archived = cli("run", &archive);
    assert!(!archived.status.success());
    assert!(
        String::from_utf8_lossy(&archived.stderr).contains("TypeError"),
        "{archived:?}"
    );
}

#[test]
fn archives_keep_associated_default_callback_types_and_frozen_proofs() {
    for source in [
        "struct P{};trait Read{assoc Item:Type=Any;fn get(self)->Item;derive fn answer(self)->Item{let xs=[0].map do |x|self.get();xs(0).as(Item)}};extend Read for P{assoc Item:Type=i64;pub fn get(self)->i64{42}};fn main(){println(P{}.answer())}",
        "struct P{};trait Read{assoc Item:Type=Any;fn get(self)->Item;derive fn answer(self)->Item{let xs=[0].map do |x|self.get();xs(0).as(Item)}};extend Read for P{assoc Item:Type=String;pub fn get(self)->String{\"ab\"++\"cdefgh\"}};fn main(){println(P{}.answer().len()+34)}",
        "fn invoke(f:fn()->i64)->i64{f()};struct P{};trait Read{fn get(self)->i64;derive fn answer(self)->i64{let saved=self;invoke do {saved.get()}}};mod a{extend Read for P{pub fn get(self)->i64{40}};pub fn answer()->i64{P{}.answer()}};mod b{extend Read for P{pub fn get(self)->i64{2}};pub fn answer()->i64{P{}.answer()}};fn main(){println(a.answer()+b.answer())}",
        "fn apply(f:fn(Any)->Any)->List{[0].map do |x|f(x)};mod api{pub global answer:List=apply() do |x|storage.n};mod storage{pub global n:i64=42;pub fn unused()->List{api.answer}};fn main(){println(api.answer(0))}",
    ] {
        roundtrip(source, "42\n");
    }
}
