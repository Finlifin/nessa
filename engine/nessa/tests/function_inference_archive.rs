//! Inferred function results execute from archives in a fresh process without source.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "nessa-function-inference-{}-{}",
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

fn chain(leaf: &str, main: &str) -> String {
    let mut source = String::new();
    for index in 0..513 {
        let body = if index == 512 {
            leaf.to_owned()
        } else {
            format!("chain_{}()", index + 1)
        };
        source.push_str(&format!("fn chain_{index}(){{{body}}};"));
    }
    source.push_str(main);
    source
}

#[test]
fn source_deleted_archives_keep_long_chain_alias_factory_and_projection_results() {
    roundtrip(&chain("42", "fn main(){println(chain_0())}"), "42\n");
    roundtrip(
        &chain(
            "true",
            "fn main(){if chain_0(){println(42)}else{println(0)}}",
        ),
        "42\n",
    );
    for source in [
        "const alias=leaf;fn factory(){||alias()};fn leaf(){42};fn main(){println(factory()())}",
        "mod api{pub fn wrapper(){leaf()};fn leaf(){42}};fn main(){println(api.wrapper())}",
        "fn outer(){let x=40;let inner=||{x+2};inner()};fn main(){println(outer())}",
        "fn leaf(){true};fn main(){let result=leaf();fn leaf(){42};println(result)}",
        "fn outer(){fn wrapper(){leaf()};fn leaf(){42};wrapper()};fn main(){println(outer())}",
        "fn first(n:i64)->i64{if n==0{42}else{second(n-1)}};fn second(n:i64){first(n)};fn main(){println(second(3))}",
        "struct P{};trait Read{assoc Item:Type=Any;fn get(self)->Item;derive fn answer(self)->Item{self.get()}};impl Read for P{assoc Item:Type=i64;pub fn get(self)->i64{42}};fn main(){println(P{}.answer())}",
    ] {
        roundtrip(source, "42\n");
    }
}

#[test]
fn known_bad_inferred_chains_never_create_archives_even_with_unused_defaults() {
    let sources=[
        chain("true","fn main(){let n:i64=chain_0();42}"),
        "effect ask(.n:i64=wrapper())->i64;fn wrapper(){middle()};fn middle(){leaf()};fn leaf(){true};fn main(){42}".into(),
        "effect ask(.n:i64=wrapper())->i64;fn wrapper(){middle()};fn middle(){leaf()};fn leaf(){true};fn main(){ask()#{ask(n)=>n}}".into(),
        "fn factory(){||leaf()};fn leaf(){true};fn main(){let n:i64=factory()();42}".into(),
    ];
    for source in sources {
        let directory = TestDirectory::new();
        let path = directory.0.join("invalid.ns");
        std::fs::write(&path, &source).unwrap();
        let output = cli("build", &path);
        assert!(!output.status.success(), "accepted {source}");
        assert!(!path.with_extension("nsbc").exists());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("type mismatch"),
            "{output:?}"
        );
    }
}

#[test]
fn genuine_any_return_boundaries_still_fail_after_source_deletion() {
    for source in [
        "fn wrapper(){leaf()};fn leaf()->Any{true};fn main(){let n:i64=wrapper();println(42)}",
        "fn wrapper(){leaf(true)};fn leaf(b:bool){if b{true}else{42}};fn main(){let n:i64=wrapper();println(42)}",
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("dynamic.ns");
        std::fs::write(&path, source).unwrap();
        let source_run = cli("run", &path);
        assert!(!source_run.status.success());
        assert!(source_run.stdout.is_empty(), "{source_run:?}");
        assert!(
            String::from_utf8_lossy(&source_run.stderr).contains("TypeError"),
            "{source_run:?}"
        );
        assert_success(&cli("build", &path));
        let archive = path.with_extension("nsbc");
        assert!(archive.is_file());
        std::fs::remove_file(&path).unwrap();
        assert!(!path.exists());
        let output = cli("run", &archive);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("TypeError"),
            "{output:?}"
        );
    }
}

fn factory_method_chain(leaf: &str, prefix: &str, main: &str) -> String {
    let mut source = format!(
        "{prefix}fn wrapper(){{factory().m0()}};fn factory(){{P{{}}}};struct P{{}};impl P{{"
    );
    for index in 0..513 {
        let body = if index == 512 {
            leaf.to_owned()
        } else {
            format!("self.m{}()", index + 1)
        };
        source.push_str(&format!("pub fn m{index}(self){{{body}}};"));
    }
    source.push_str("};");
    source.push_str(main);
    source
}

#[test]
fn factory_receiver_long_method_chains_reject_known_bool_and_archive_i64() {
    for (prefix, main) in [
        ("effect unused(.x:i64=wrapper())->i64;", "fn main(){42}"),
        (
            "effect selected(.x:i64=wrapper())->i64;",
            "fn main(){selected()#{selected(x)=>x}}",
        ),
        ("", "fn main(){let result:i64=wrapper();result}"),
    ] {
        let source = factory_method_chain("true", prefix, main);
        let directory = TestDirectory::new();
        let path = directory.0.join("invalid-method-chain.ns");
        std::fs::write(&path, source).unwrap();
        let output = cli("build", &path);
        assert_eq!(
            output.status.code(),
            Some(1),
            "compiler must reject without aborting: {output:?}"
        );
        assert!(!path.with_extension("nsbc").exists());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("type mismatch"),
            "{output:?}"
        );
    }
    roundtrip(
        &factory_method_chain("42", "", "fn main(){println(wrapper())}"),
        "42\n",
    );
}

#[test]
fn source_deleted_named_default_callbacks_specialize_p_and_q_preserving_any() {
    roundtrip(
        "struct P{value:i64};struct Q{value:i64};trait Read{assoc Item:Type=Self;derive fn named(self)->fn(Item)->Item{fn identity(other:Item)->Item{other};identity};derive fn explicit(self,value:Any)->Any{value}};impl Read for P{};impl Read for Q{};fn main(){let p=P{value:40};let q=Q{value:2};let pn=p.named();let qn=q.named();println(type_of(pn)==(fn(P)->P));println(type_of(qn)==(fn(Q)->Q));println(pn(p).value+qn(q).value);println(type_of(p.explicit(q))==Q)}",
        "true\ntrue\n42\ntrue\n",
    );
}
