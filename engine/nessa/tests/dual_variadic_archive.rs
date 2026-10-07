//! Dual variadic results execute from archives in a fresh process without source.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "nessa-dual-variadic-{}-{}",
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
fn source_deleted_archives_keep_functions_methods_effects_and_scoped_defaults() {
    for source in [
        "fn gather(...xs:List,...ps:Map)->i64{if xs.len()==0 and ps.len()==0{42}else{0}};fn main(){println(gather{})}",
        "use api.gather as chosen;mod api{pub fn gather(...xs:List,...ps:Map)->i64{xs(0).as(i64)+ps(\"tail\").as(i64)}};fn main(){println(chosen{40,tail:2})}",
        "fn main(){println((|...xs:List,...ps:Map|->i64{xs(0).as(i64)+ps(\"tail\").as(i64)}){40,tail:2})}",
        "struct P{n:i64};impl P{pub fn gather(self,...xs:List,...ps:Map)->i64{self.n+ps(\"tail\").as(i64)}};fn create(...xs:List,...ps:Map){P{n:40}};fn factory(){create{}};fn read(){factory().gather{tail:2}};fn main(){println(read())}",
        "effect gather(...xs:List,catch k,...ps:Map)->i64;fn main(){println(gather{40,tail:2}#{gather(xs,k,ps)=>k(xs(0).as(i64)+ps(\"tail\").as(i64))})}",
        "global trace:i64=0;fn value(n:i64)->i64{trace=trace*10+n;n};fn gather(...xs:List,...ps:Map)->i64{if trace==123 and ps.len()==1{ps(\"same\").as(i64)+39}else{0}};fn main(){println(gather{same:value(1),same:value(2),same:value(3)})}",
        "struct P{x:i64};trait Source{assoc Item:Type=Self;fn next(self)->Item;derive fn gather(self,...xs:List,...ps:Map)->Item{let copy:Self=self;let value:Item=copy.next();let f=ps(\"callback\").as(fn(Item)->Item);f(value)}};impl Source for P{pub fn next(self)->P{self}};fn identity(value:P)->P{value};fn main(){println(P{x:42}.gather{callback:identity}.x)}",
        "struct P{};trait Source{fn next(self)->i64;derive fn gather(self,...xs:List,...ps:Map)->i64{let copy=self;copy.next()}};mod a{extend Source for P{pub fn next(self)->i64{40}};pub fn read()->i64{P{}.gather{}}};mod b{extend Source for P{pub fn next(self)->i64{2}};pub fn read()->i64{P{}.gather{}}};fn main(){println(a.read()+b.read())}",
        "fn invoke(...xs:List,...ps:Map)->i64{let f=ps(\"callback\").as(fn()->i64);f()};mod consumer{pub global answer:i64=invoke{callback:provider.read}};mod provider{pub global value:i64=42;pub fn read()->i64{value}};fn main(){println(consumer.answer)}",
        "fn invoke(...xs:List,...ps:Map)->i64{let f=ps(\"callback\").as(fn()->i64);f()};mod consumer{pub global answer:i64=invoke{callback:unused.read,callback:provider.read}};mod unused{pub global value:i64=consumer.answer;pub fn read()->i64{value}};mod provider{pub global value:i64=42;pub fn read()->i64{value}};fn main(){println(consumer.answer)}",
        "fn gather(xs:List,ps:Map)->i64{xs(0).as(i64)+ps(\"tail\").as(i64)};fn main(){let f:fn(List,Map)->i64=gather;let ps=Map();ps(\"tail\")=2;println(f([40],ps))}",
        "fn gather(...xs:List,...ps:Map)->i64{xs(0).as(i64)+ps(\"tail\").as(i64)};fn main(){let f:fn(List,Map)->i64=gather;let ps=Map();ps(\"tail\")=2;println(f([40],ps))}",
        "fn gather(...xs:List,...ps:Map)->i64{let f=ps(\"callback\").as(fn()->i64);if xs(0)==null and xs(1)==(){f()}else{0}};fn main(){let n:i8=42;println(gather{null,(),callback:||n.as(i64)})}",
    ] {
        roundtrip(source, "42\n");
    }
}

#[test]
fn large_children_and_long_inferred_extended_dependency_chains_survive_archives() {
    let entries = (0..4100)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    roundtrip(
        &format!(
            "fn gather(...xs:List,...ps:Map){{if xs.len()==4100 and xs(4099)==4099{{xs(40).as(i64)+ps(\"tail\").as(i64)}}else{{0}}}};fn main(){{println(gather{{{entries},tail:2}})}}"
        ),
        "42\n",
    );
    let mut definitions = vec!["fn leaf(...xs:List,...ps:Map){42};".to_owned()];
    for index in 0..512 {
        let body = if index == 0 {
            "leaf{answer:42}".to_owned()
        } else {
            format!("chain_{}{{}}", index - 1)
        };
        definitions.push(format!("fn chain_{index}(...xs:List,...ps:Map){{{body}}};"));
    }
    definitions.reverse();
    roundtrip(
        &format!(
            "{}fn main(){{println(chain_511{{}})}}",
            definitions.join("")
        ),
        "42\n",
    );
    roundtrip(
        r#"
        effect pause(catch k)->i64;global entries:i64=0;
        fn gather(...xs:List,...ps:Map)->i64{xs(0).as(String).len()+xs(1).as(i64)+ps("base").as(i64)};
        fn work()->i64{entries+=1;gather{"abcd"++"efgh",pause()#,base:12}};
        fn main(){let saved=work()#{pause(k)=>k};let a=saved(0);let b=saved(2);
            if a==20 and b==22 and entries==1{println(a+b)}else{println(0)}}
    "#,
        "42\n",
    );
}

#[test]
fn invalid_layouts_calls_and_stale_return_types_never_emit_archives() {
    for source in [
        "fn bad(...xs:Map,...ps:List)->i64{42};fn main(){println(42)}",
        "fn bad(n:i64,...xs:List,...ps:Map)->i64{42};fn main(){println(42)}",
        "fn bad(...xs:List,...ps:Map,.n:i64=42)->i64{42};fn main(){println(42)}",
        "enum E{bad(...xs:List,...ps:Map)};fn main(){println(42)}",
        "fn gather(...xs:List,...ps:Map)->i64{42};fn main(){println(gather([],Map()))}",
        "fn gather(...xs:List,...ps:Map)->i64{42};fn main(){let f=gather;println(f{40,tail:2})}",
        "fn wrapper(){leaf{}};fn leaf(...xs:List,...ps:Map){true};effect unused(.n:i64=wrapper())->i64;fn main(){println(42)}",
        "trait T{fn gather(self,...xs:List,...ps:Map)->i64};struct P{};impl T for P{pub fn gather(self,...xs:List,...ps:List)->i64{42}};fn main(){println(42)}",
    ] {
        let directory = TestDirectory::new();
        let path = directory.0.join("invalid.ns");
        std::fs::write(&path, source).unwrap();
        let output = cli("build", &path);
        assert_eq!(output.status.code(), Some(1), "{source}: {output:?}");
        assert!(!path.with_extension("nsbc").exists());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("Expected"),
            "generic parse rejection: {source}: {stderr}"
        );
        assert!(!stderr.is_empty(), "missing diagnostic: {source}");
        assert!(
            ["variadic", "extended application", "type mismatch"]
                .iter()
                .any(|category| stderr.contains(category)),
            "missing semantic diagnostic: {source}: {stderr}"
        );
    }
}

#[test]
fn erased_packed_payloads_keep_runtime_trait_guards_after_source_deletion() {
    for (entry, body) in [
        ("erased", "let value=xs(0);consume(value)"),
        ("value:erased", "let value=ps(\"value\");consume(value)"),
    ] {
        let source = format!(
            "struct P{{}};trait Read{{fn value(self)->i64}};mod a{{extend Read for P{{pub fn value(self)->i64{{42}}}};pub fn answer()->i64{{outside.take(P{{}})}}}};mod outside{{fn consume(x:Read)->i64{{x.value()}};fn pack(...xs:List,...ps:Map)->i64{{{body}}};pub fn take(x:Read)->i64{{let erased:Any=x;pack{{{entry}}}}}}};fn main(){{println(a.answer())}}"
        );
        let directory = TestDirectory::new();
        let path = directory.0.join("guard.ns");
        std::fs::write(&path, source).unwrap();
        let original = cli("run", &path);
        assert_eq!(original.status.code(), Some(1), "{original:?}");
        assert!(
            String::from_utf8_lossy(&original.stderr).contains("TypeError"),
            "{original:?}"
        );
        assert!(original.stdout.is_empty());
        assert_success(&cli("build", &path));
        let archive = path.with_extension("nsbc");
        assert!(archive.is_file());
        std::fs::remove_file(&path).unwrap();
        assert!(!path.exists());
        let archived = cli("run", &archive);
        assert_eq!(archived.status.code(), Some(1), "{archived:?}");
        assert!(
            String::from_utf8_lossy(&archived.stderr).contains("TypeError"),
            "{archived:?}"
        );
        assert!(archived.stdout.is_empty());
    }
}
