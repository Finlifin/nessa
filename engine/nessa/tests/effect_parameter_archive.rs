//! Effect caller binding survives source deletion and a fresh process/interner.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "nessa-effect-parameter-archive-{}-{}",
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

fn success(output: &Output, source: &str) {
    assert!(
        output.status.success(),
        "{source}: {}; stdout: {}; stderr: {}",
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
    success(&source_run, source);
    assert_eq!(
        String::from_utf8(source_run.stdout).unwrap(),
        expected,
        "{source}"
    );
    success(&cli("build", &path), source);
    let archive = path.with_extension("nsbc");
    assert!(archive.is_file());
    std::fs::remove_file(path).unwrap();
    let archive_run = cli("run", &archive);
    success(&archive_run, source);
    assert_eq!(
        String::from_utf8(archive_run.stdout).unwrap(),
        expected,
        "{source}"
    );
}

fn runtime_error_roundtrip(source: &str) {
    let directory = TestDirectory::new();
    let path = directory.0.join("program.ns");
    std::fs::write(&path, source).unwrap();
    success(&cli("build", &path), source);
    let source_run = cli("run", &path);
    assert!(!source_run.status.success(), "accepted {source}");
    assert!(
        String::from_utf8_lossy(&source_run.stderr).contains("TypeError"),
        "{source}: {source_run:?}"
    );
    let archive = path.with_extension("nsbc");
    assert!(archive.is_file());
    std::fs::remove_file(path).unwrap();
    let archive_run = cli("run", &archive);
    assert!(!archive_run.status.success(), "accepted archived {source}");
    assert!(
        String::from_utf8_lossy(&archive_run.stderr).contains("TypeError"),
        "{source}: {archive_run:?}"
    );
}

fn rejected_without_archive(source: &str) {
    let compiled = driver::Driver::new().compile(source);
    assert!(compiled.has_errors, "accepted {source}");
    assert!(compiled.codegen_output.functions.is_empty(), "{source}");
    assert!(!compiled.diagnostics.is_empty(), "{source}");
    assert!(compiled.into_artifact().is_err(), "{source}");
    let directory = TestDirectory::new();
    let path = directory.0.join("invalid.ns");
    std::fs::write(&path, source).unwrap();
    let output = cli("build", &path);
    assert!(!output.status.success(), "accepted {source}");
    assert!(
        !path.with_extension("nsbc").exists(),
        "archive created for {source}"
    );
}

#[test]
fn catch_at_every_position_preserves_named_and_default_caller_slots() {
    for (parameters, bindings) in [
        ("catch k,a:i64,.b:i64=a+2", "k,a,b"),
        ("a:i64,catch k,.b:i64=a+2", "a,k,b"),
        ("a:i64,.b:i64=a+2,catch k", "a,b,k"),
    ] {
        roundtrip(
            &format!(
                "effect pick({parameters})->i64;fn compute(){{pick(a=40)#}};fn main(){{print(compute()#{{pick({bindings})=>k(b)}})}}"
            ),
            "42",
        );
    }
}

#[test]
fn source_argument_order_precedes_selected_default_order() {
    roundtrip(
        "global trace:i64=0;fn step(n:i64)->i64{trace=trace*10+n;n};effect read(a:i64,.b:i64=step(3),.c:i64=step(4))->i64;fn main(){let answer=read(c=step(2),a=step(1))#{read(a,b,c)=>a+b+c+36};println(trace);println(answer)}",
        "213\n42\n",
    );
}

#[test]
fn large_variadics_pack_before_inserting_catch_at_every_position() {
    let arguments = (0..40).map(|n| n.to_string()).collect::<Vec<_>>().join(",");
    for (parameters, bindings) in [
        ("catch k,...xs:List,.n:i64=xs.len()", "k,xs,n"),
        ("...xs:List,catch k,.n:i64=xs.len()", "xs,k,n"),
        ("...xs:List,.n:i64=xs.len(),catch k", "xs,n,k"),
    ] {
        roundtrip(
            &format!(
                "effect count({parameters})->i64;fn main(){{print(count({arguments})#{{count({bindings})=>k(if xs.len()==40 and xs(0)==0 and xs(39)==39 and n==40{{n+2}}else{{0}})}})}}"
            ),
            "42",
        );
    }
}

#[test]
fn later_function_and_effect_defaults_have_complete_signatures() {
    roundtrip(
        "effect outer(.a:i64=later(),.b:i64=supply()#{supply()=>2})->i64;effect supply()->i64;fn later()->i64{40};fn main(){print(outer()#{outer(a,b)=>a+b})}",
        "42",
    );
}

#[test]
fn declaration_global_defaults_order_module_initialization() {
    roundtrip(
        "effect read(.x:i64=storage.n)->i64;mod api{pub global answer:i64=read()#{read(x)=>x}};mod storage{pub global n:i64=42};fn main(){let n:i64=0;print(api.answer)}",
        "42",
    );
}

#[test]
fn cloned_multishot_resumes_do_not_repeat_parameter_defaults() {
    roundtrip(
        "global calls:i64=0;fn make_text()->String{calls+=1;\"ab\"++\"cdefgh\"};effect choose(catch k,...xs:List,.text:String=make_text(),.n:i64=xs.len())->i64;fn compute(){choose(1,2,null,())#+2};fn main(){let answer=compute()#{choose(k,xs,text,n)=>{let first=k(10).as(i64);let copy=k.clone();let second=copy(20).as(i64);if calls==1 and text==\"abcdefgh\" and n==4 and xs(0)==1 and xs(1)==2 and xs(2)==null and xs(3)==(){first+second+8}else{0}}};println(answer);println(calls)}",
        "42\n1\n",
    );
}

#[test]
fn gradual_named_and_default_arguments_fail_before_ignored_handler_use() {
    for call in ["ask()", "ask(x=wrong())"] {
        runtime_error_roundtrip(&format!(
            "fn wrong()->Any{{true}};effect ask(.x:i64=wrong())->i64;fn main(){{{call}#{{ask(x)=>42}};42}}"
        ));
    }
    roundtrip(
        "fn wrong()->Any{true};effect ask(.x:i64=wrong())->i64;fn main(){print(ask(x=42)#{ask(x)=>x})}",
        "42",
    );
}

#[test]
fn invalid_effect_parameters_and_calls_never_create_archives() {
    for source in [
        "effect ask(.x:i64=true)->i64;fn main(){42}",
        "effect ask(a:i64)->i64;fn main(){ask(a=1,a=2)#{ask(a)=>a}}",
        "effect ask(catch k)->i64;fn main(){ask(k=42)#{ask(k)=>k(42)}}",
        "effect ask(catch k,.x:i64=k(42))->i64;fn main(){42}",
        "effect ask(.x:i64=y,.y:i64=42)->i64;fn main(){42}",
        "effect ask(...xs:List,fixed:i64)->i64;fn main(){42}",
        "effect ask(.x:i64=ask()#)->i64;fn main(){42}",
    ] {
        rejected_without_archive(source);
    }
}

#[test]
fn inferred_function_defaults_are_checked_after_body_inference_in_both_orders() {
    for selected in [false, true] {
        let main = if selected {
            "fn main(){println(unused()#{unused(x)=>42})}"
        } else {
            "fn main(){println(42)}"
        };
        for declarations in [
            "effect unused(.x:i64=later())->i64;fn later(){true};",
            "fn later(){true};effect unused(.x:i64=later())->i64;",
        ] {
            rejected_without_archive(&format!("{declarations}{main}"));
        }
    }
    for declarations in [
        "effect read(.x:i64=later())->i64;fn later(){42};",
        "fn later(){42};effect read(.x:i64=later())->i64;",
    ] {
        roundtrip(
            &format!("{declarations}fn main(){{println(read()#{{read(x)=>x}})}}"),
            "42\n",
        );
    }
}
