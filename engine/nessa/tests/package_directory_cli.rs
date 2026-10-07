//! Directory CLI compilation produces self-contained archives, with file CLI compatibility.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
struct Directory(PathBuf);
impl Directory {
    fn new(kind: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = loop {
            let dir = std::env::temp_dir().join(format!(
                "nessa-package-cli-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&dir) {
                Ok(()) => break Self(dir),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("fixture directory: {e}"),
            }
        };
        dir.write("package.toml",&format!("[package]\nname=\"sample\"\ndomain=\"example.org\"\nversion=\"1.2.3\"\ntype={kind:?}\n"));
        dir
    }
    fn write(&self, name: &str, source: &str) {
        let p = self.0.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, source).unwrap();
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn cli(cmd: &str, path: &Path, dump: bool) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_nessa"));
    c.arg(cmd).arg(path);
    if dump {
        c.arg("--emit-ast-dump");
    }
    c.output().unwrap()
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn run42(path: &Path) {
    let result = cli("run", path, false);
    success(&result);
    assert_eq!(result.stdout, b"42\n");
}
fn rejected(dir: &Directory, needle: &str) {
    for cmd in ["check", "build", "run"] {
        let out = cli(cmd, &dir.0, false);
        assert!(!out.status.success(), "{cmd} accepted invalid input");
        assert!(
            out.stdout.is_empty(),
            "{cmd} executed invalid input: {:?}",
            out.stdout
        );
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .to_lowercase()
                .contains(&needle.to_lowercase()),
            "{cmd}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!dir.0.join("package.nsbc").exists());
    }
}
#[test]
fn directory_run_check_build_and_source_deleted_archive_are_equivalent() {
    let d = Directory::new("exe");
    d.write(
        "src/main.ns",
        "use model.Payload as Item;fn main(){println(数学.answer(Item{n:40}))}",
    );
    d.write("src/model.ns", "pub struct Payload{n:i64}");
    d.write(
        "src/数学/mod.ns",
        "pub fn answer(p:model.Payload)->i64{p.n+child.two()}",
    );
    d.write("src/数学/child.ns", "pub fn two()->i64{2}");
    run42(&d.0);
    success(&cli("check", &d.0, false));
    assert!(!d.0.join("package.nsbc").exists());
    success(&cli("build", &d.0, false));
    let archive = d.0.join("package.nsbc");
    assert!(archive.is_file());
    std::fs::remove_dir_all(d.0.join("src")).unwrap();
    std::fs::remove_file(d.0.join("package.toml")).unwrap();
    run42(&archive);
}
#[test]
fn directory_archives_freeze_cross_file_initialization_and_callbacks() {
    let d = Directory::new("exe");
    d.write("src/main.ns", "fn main(){println(consumer.answer)}");
    d.write(
        "src/consumer.ns",
        "const callback=||provider.read();pub const answer:i64=callback()+2",
    );
    d.write(
        "src/provider.ns",
        "global seed:i64=0;fn __init__(){seed=40};pub fn read()->i64{seed}",
    );
    run42(&d.0);
    success(&cli("build", &d.0, false));
    std::fs::remove_dir_all(d.0.join("src")).unwrap();
    std::fs::remove_file(d.0.join("package.toml")).unwrap();
    run42(&d.0.join("package.nsbc"));
}
#[test]
fn library_and_temporary_startup_execute_initializers_and_skip_main_in_fresh_processes() {
    for kind in ["lib", "tmp"] {
        let d = Directory::new(kind);
        d.write(
            "src/lib.ns",
            "fn __init__(){println(42)};fn main(){panic(\"must not invoke main\")}",
        );
        run42(&d.0);
        success(&cli("build", &d.0, false));
        std::fs::remove_dir_all(d.0.join("src")).unwrap();
        std::fs::remove_file(d.0.join("package.toml")).unwrap();
        run42(&d.0.join("package.nsbc"));
    }
}
#[test]
fn ast_dump_is_written_inside_package_and_keeps_each_file_raw_literal() {
    let d = Directory::new("exe");
    d.write("src/main.ns", "fn main(){println(child.answer())}");
    d.write(
        "src/child.ns",
        "-- 文\r\npub fn answer(){\r\nlet text=\"α\rβ\";42\r\n}",
    );
    let out = cli("check", &d.0, true);
    success(&out);
    let dump = std::fs::read_to_string(d.0.join("package.lisp")).unwrap();
    assert!(dump.contains("ModuleDef"), "{dump}");
    assert!(dump.contains("child") && dump.contains("answer"), "{dump}");
    assert!(dump.contains("\"α\rβ\""), "{dump}");
    assert!(!dump.contains("-- 文"), "{dump}");
    run42(&d.0);
}
#[test]
fn crlf_and_unicode_diagnostics_use_physical_file_and_exact_logical_column() {
    let d = Directory::new("exe");
    d.write("src/main.ns", "fn main(){broken.read()}");
    for newline in ["\n", "\r\n"] {
        d.write(
            "src/broken.ns",
            &format!("-- é{newline}pub fn read(){{{newline}    未定义{newline}}}"),
        );
        let out = cli("check", &d.0, false);
        assert!(!out.status.success());
        let text = String::from_utf8(out.stderr).unwrap();
        assert!(
            text.contains(d.0.join("src/broken.ns").to_str().unwrap()),
            "{text}"
        );
        assert!(text.contains(":3:5"), "{text}");
        assert!(text.contains("未定义"), "{text}");
        assert!(!d.0.join("package.nsbc").exists());
    }
}
#[test]
fn invalid_sources_collisions_private_names_and_missing_entry_have_no_archive() {
    for (entry, module, needle) in [
        (
            "fn main(){42}",
            "pub fn read(){missing_name}",
            "missing_name",
        ),
        (
            "mod api{pub fn value(){40}};fn main(){42}",
            "pub fn read(){2}",
            "api",
        ),
        (
            "fn main(){api.hidden()}",
            "private fn hidden(){42}",
            "not visible",
        ),
        ("mod inner{pub fn main(){42}}", "pub fn read(){2}", "main"),
    ] {
        let d = Directory::new("exe");
        d.write("src/main.ns", entry);
        d.write("src/api.ns", module);
        rejected(&d, needle);
    }
    let d = Directory::new("exe");
    std::fs::create_dir(d.0.join("src")).unwrap();
    rejected(&d, "main.ns");
}
#[test]
fn unresolved_dependencies_are_visible_and_do_not_produce_outputs() {
    let d = Directory::new("exe");
    d.write("src/main.ns", "fn main(){println(42)}");
    d.write("package.toml","[package]\nname=\"sample\"\ndomain=\"example.org\"\nversion=\"1.2.3\"\ntype=\"exe\"\n[dependencies]\n\"example.org/missing\"=\"^1\"\n");
    rejected(&d, "missing");
}
#[test]
fn empty_module_declaration_connects_file_and_preserves_its_visibility() {
    let d = Directory::new("exe");
    d.write("src/main.ns", "mod api{};fn main(){println(api.answer())}");
    d.write("src/api.ns", "fn hidden(){40};pub fn answer(){hidden()+2}");
    run42(&d.0);
    success(&cli("build", &d.0, false));
}
#[test]
fn single_file_cli_keeps_source_archive_and_dump_behavior() {
    let d = Directory::new("exe");
    let source = d.0.join("single.ns");
    d.write("single.ns", "fn main(){println(42)}");
    run42(&source);
    success(&cli("check", &source, true));
    let dump = std::fs::read_to_string(source.with_extension("lisp")).unwrap();
    assert!(
        dump.contains("FunctionDef") && dump.contains("main") && dump.contains("42"),
        "{dump}"
    );
    success(&cli("build", &source, false));
    let archive = source.with_extension("nsbc");
    assert!(archive.is_file());
    std::fs::remove_file(source).unwrap();
    run42(&archive);
}

#[test]
fn executable_nonzero_arity_main_is_rejected_but_library_noop_archive_runs() {
    let d = Directory::new("exe");
    d.write("src/main.ns", "fn main(.x:i64=42){println(x)}");
    rejected(&d, "main");
    for kind in ["lib", "tmp"] {
        let d = Directory::new(kind);
        d.write(
            "src/lib.ns",
            "pub fn answer(){42};fn main(){panic(\"must not call main\")}",
        );
        let output = cli("run", &d.0, false);
        success(&output);
        assert!(output.stdout.is_empty());
        success(&cli("build", &d.0, false));
        std::fs::remove_dir_all(d.0.join("src")).unwrap();
        std::fs::remove_file(d.0.join("package.toml")).unwrap();
        let output = cli("run", &d.0.join("package.nsbc"), false);
        success(&output);
        assert!(output.stdout.is_empty());
    }
}
