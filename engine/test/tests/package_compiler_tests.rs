//! Filesystem packages must use the same typed execution and archive pipeline as files.
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use driver::{CompileResult, Driver, RunResult};
use type_pool::{IdentityPathSegment, TypeId, TypeIndex, TypeKind, TypePool};

struct Package(PathBuf);
impl Package {
    fn new(kind: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = loop {
            let path = std::env::temp_dir().join(format!(
                "nessa-package-compiler-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => break path,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("create fixture: {e}"),
            }
        };
        let package = Self(path);
        package.write("package.toml", &format!("[package]\nname=\"sample\"\ndomain=\"example.org\"\nversion=\"1.2.3\"\ntype={kind:?}\n"));
        package
    }
    fn write(&self, relative: &str, contents: &str) {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
}
impl Drop for Package {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn checked(package: &Package) -> CompileResult {
    let result = Driver::new().compile_package(&package.0);
    assert!(!result.has_errors, "{:?}", result.diagnostics);
    result.type_pool.validate().unwrap();
    result
}
fn value(result: CompileResult) -> i64 {
    let artifact = result.into_artifact().unwrap();
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .unwrap();
    let task = engine.vm_mut().spawn_root(entry);
    assert!(matches!(
        engine.vm_mut().run(),
        interpreter::VmResult::Finished
    ));
    assert_eq!(engine.vm.active_stack_count(), 0);
    engine
        .vm_mut()
        .task_result_number(task)
        .unwrap()
        .to_i64_checked()
        .unwrap()
}
fn reject(package: &Package, expected: &str) {
    let result = Driver::new().compile_package(&package.0);
    assert!(
        result.has_errors,
        "unexpected successful package: {}",
        package.0.display()
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.message.to_lowercase().contains(&expected.to_lowercase())),
        "expected {expected}: {:?}",
        result.diagnostics
    );
    assert!(result.codegen_output.functions.is_empty());
    assert!(result.into_artifact().is_err());
    let archive = package.0.join("rejected.nsbc");
    assert!(
        Driver::new()
            .compile_package_to_archive(&package.0, &archive)
            .is_err()
    );
    assert!(!archive.exists());
}
fn named(pool: &TypePool, name: &str) -> TypeId {
    let found:Vec<_>=(0..pool.len()).map(|n|TypeIndex::from_raw(n as u32)).filter(|&i| matches!(pool.get(i).kind,TypeKind::Struct{name:n,..} if str_interner::get(n)==name)).collect();
    assert_eq!(found.len(), 1, "{name}: {found:?}");
    let id = pool.get(found[0]).type_id;
    assert_ne!(id, TypeId::ZERO);
    id
}
fn standard(pool: &TypePool) -> BTreeMap<String, TypeId> {
    let mut ids = BTreeMap::from([
        ("Display".into(), pool.get(pool.well_known.display).type_id),
        ("Eq".into(), pool.get(pool.well_known.eq).type_id),
    ]);
    let mut modules = 0;
    let mut other = 0;
    for declaration in &pool.identity_input().unwrap().declarations {
        if matches!(declaration.path.first(),Some(IdentityPathSegment::Named(n)) if n=="std") {
            if matches!(
                pool.get(declaration.type_index).kind,
                TypeKind::Module { .. }
            ) {
                modules += 1;
            } else {
                other += 1;
            }
            assert!(
                ids.insert(
                    format!("{:?}", declaration.path),
                    pool.get(declaration.type_index).type_id
                )
                .is_none()
            );
        }
    }
    assert!(modules > 0 && other > 0);
    ids
}

#[test]
fn root_sibling_nested_unicode_modules_and_imported_types_execute_42() {
    let p = Package::new("exe");
    p.write(
        "src/main.ns",
        "use model.Payload as Item;fn main(){数学.answer(Item{n:40})}",
    );
    p.write("src/model.ns", "pub struct Payload{n:i64}");
    p.write(
        "src/数学/mod.ns",
        "pub fn answer(p:model.Payload)->i64{p.n+child.two()}",
    );
    p.write("src/数学/child.ns", "pub fn two()->i64{2}");
    assert_eq!(value(checked(&p)), 42);
}
#[test]
fn nested_main_and_lib_are_modules_not_alternate_package_entries() {
    let p = Package::new("exe");
    p.write(
        "src/main.ns",
        "fn main(){nested.main.answer()+nested.lib.answer()}",
    );
    p.write("src/nested/mod.ns", "");
    p.write("src/nested/main.ns", "pub fn answer()->i64{40}");
    p.write("src/nested/lib.ns", "pub fn answer()->i64{2}");
    assert_eq!(value(checked(&p)), 42);
}
#[test]
fn provider_initialization_and_callback_dependencies_cross_files() {
    let p = Package::new("exe");
    p.write("src/main.ns", "fn main(){consumer.answer}");
    p.write(
        "src/consumer.ns",
        "const f=||provider.seed;pub const answer:i64=f()+2",
    );
    p.write("src/provider.ns", "pub const seed:i64=40");
    assert_eq!(value(checked(&p)), 42);
    p.write("src/consumer.ns", "pub const answer:i64=provider.read()+2");
    p.write(
        "src/provider.ns",
        "global seed:i64=0;fn __init__(){seed=40};pub fn read()->i64{seed}",
    );
    assert_eq!(value(checked(&p)), 42);
}
#[test]
fn empty_decl_connects_file_without_exporting_private_bindings() {
    let p = Package::new("exe");
    p.write("src/main.ns", "mod api{};fn main(){api.answer()}");
    p.write(
        "src/api.ns",
        "private fn hidden()->i64{40};pub fn answer()->i64{hidden()+2}",
    );
    assert_eq!(value(checked(&p)), 42);
    p.write("src/main.ns", "mod api{};fn main(){api.hidden()}");
    reject(&p, "not visible");
}
#[test]
fn inline_body_and_nonmodule_collisions_are_errors_without_artifacts() {
    for source in [
        "mod api{pub fn other(){0}};fn main(){42}",
        "const api=42;fn main(){api}",
    ] {
        let p = Package::new("exe");
        p.write("src/main.ns", source);
        p.write("src/api.ns", "pub fn answer(){42}");
        reject(&p, "api");
    }
}
#[test]
fn executable_requires_direct_root_main_not_a_nested_or_value_binding() {
    for source in [
        "mod inner{pub fn main(){42}}",
        "const main=||42",
        "fn other(){42}",
    ] {
        let p = Package::new("exe");
        p.write("src/main.ns", source);
        reject(&p, "main");
    }
}
#[test]
fn lib_and_tmp_initialize_root_but_never_invoke_declared_main() {
    for kind in ["lib", "tmp"] {
        let p = Package::new(kind);
        p.write("src/lib.ns","global count:i64=0;fn __init__(){count=42;if count!=42{panic(\"bad init\")}};fn main(){panic(\"must not invoke library main\")}");
        assert!(matches!(Driver::new().run_package(&p.0), RunResult::Ok));
        p.write("src/lib.ns","fn __init__(){panic(\"library initializer witness\")};fn main(){panic(\"wrong boundary\")}");
        assert!(
            matches!(Driver::new().run_package(&p.0),RunResult::RuntimeError(ref e) if e.contains("library initializer witness") && !e.contains("wrong boundary"))
        );
    }
}
#[test]
fn missing_dependency_is_selected_by_real_resolver_and_rejected() {
    let p = Package::new("exe");
    p.write("src/main.ns", "fn main(){42}");
    p.write("package.toml","[package]\nname=\"sample\"\ndomain=\"example.org\"\nversion=\"1.2.3\"\ntype=\"exe\"\n[dependencies]\n\"example.org/missing\"=\"^1.0\"\n");
    reject(&p, "missing");
}
#[test]
fn full_type_identity_is_path_independent_and_standard_identity_is_preserved() {
    let a = Package::new("exe");
    let b = Package::new("exe");
    for p in [&a, &b] {
        p.write("src/main.ns", "fn main(){model.Payload{n:42}.n}");
        p.write("src/model.ns", "pub struct Payload{n:i64}");
    }
    let first = checked(&a);
    let moved = checked(&b);
    assert_ne!(a.0, b.0);
    assert_eq!(
        named(&first.type_pool, "Payload"),
        named(&moved.type_pool, "Payload")
    );
    let single = Driver::new().compile("fn main(){42}");
    assert!(!single.has_errors);
    assert_eq!(standard(&first.type_pool), standard(&single.type_pool));
    assert_eq!(standard(&moved.type_pool), standard(&single.type_pool));
    b.write("src/model.ns", "pub struct Payload{n:i64,extra:i64}");
    b.write(
        "src/main.ns",
        "fn main(){model.Payload{n:40,extra:2}.n+model.Payload{n:40,extra:2}.extra}",
    );
    let changed = checked(&b);
    assert_ne!(
        named(&first.type_pool, "Payload"),
        named(&changed.type_pool, "Payload")
    );
    assert_eq!(value(changed), 42);
}
#[test]
fn package_archive_executes_after_all_sources_and_manifest_are_deleted() {
    let p = Package::new("exe");
    p.write("src/main.ns", "fn main(){answer.read()}");
    p.write("src/answer.ns", "pub fn read()->i64{42}");
    let output = p.0.join("package.nsbc");
    Driver::new()
        .compile_package_to_archive(&p.0, &output)
        .unwrap();
    std::fs::remove_dir_all(p.0.join("src")).unwrap();
    std::fs::remove_file(p.0.join("package.toml")).unwrap();
    let bytes = std::fs::read(&output).unwrap();
    let artifact = nsbc_io::read_artifact(&bytes).unwrap();
    assert_eq!(value_from_archive(artifact), 42);
}
fn value_from_archive(artifact: nsbc::CompiledArtifact) -> i64 {
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .unwrap();
    let task = engine.vm_mut().spawn_root(entry);
    assert!(matches!(
        engine.vm_mut().run(),
        interpreter::VmResult::Finished
    ));
    assert_eq!(engine.vm.active_stack_count(), 0);
    engine
        .vm_mut()
        .task_result_number(task)
        .unwrap()
        .to_i64_checked()
        .unwrap()
}
#[test]
fn ast_dump_contains_generated_modules_and_exact_raw_literal_payloads() {
    let p = Package::new("exe");
    p.write("src/main.ns", "fn main(){child.read()}");
    p.write("src/child.ns", "-- 文\r\npub fn read(){\r\n\"α\rβ\"\r\n}");
    let path = p.0.join("package.lisp");
    Driver::new().emit_package_ast_dump(&p.0, &path).unwrap();
    let dump = std::fs::read_to_string(path).unwrap();
    assert!(dump.contains("ModuleDef"), "{dump}");
    assert!(dump.contains("child"), "{dump}");
    assert!(dump.contains("read"), "{dump}");
    assert!(dump.contains("\"α\rβ\""), "{dump}");
    assert!(!dump.contains("-- 文"), "{dump}");
}
#[test]
fn invalid_filesystem_entries_and_manifest_do_not_emit_archives() {
    let p = Package::new("exe");
    std::fs::create_dir(p.0.join("src")).unwrap();
    reject(&p, "main.ns");
    p.write("src/main.ns", "fn main(){42}");
    p.write("package.toml", "not a valid manifest = [");
    reject(&p, "manifest");
}

fn owned_package() -> driver::PackageSources {
    let p = Package::new("exe");
    p.write("src/main.ns", "fn main(){alpha.answer()+beta.answer()}");
    p.write("src/alpha.ns", "pub fn answer()->i64{40}");
    p.write("src/beta.ns", "pub fn answer()->i64{2}");
    driver::load_package_sources(&p.0).unwrap()
}
fn reject_owned(sources: &driver::PackageSources, needle: &str) {
    let result = Driver::new().compile_package_sources(sources);
    assert!(result.has_errors, "malformed owned tree was accepted");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.message.to_lowercase().contains(&needle.to_lowercase())),
        "expected {needle}: {:?}",
        result.diagnostics
    );
    assert!(result.codegen_output.functions.is_empty());
    assert!(result.into_artifact().is_err());
}
#[test]
fn owned_sources_survive_file_deletion_and_sibling_input_order_changes() {
    let mut sources = owned_package();
    assert!(!sources.root.exists());
    let first = Driver::new().compile_package_sources(&sources);
    assert!(!first.has_errors, "{:?}", first.diagnostics);
    assert_eq!(value(first), 42);
    sources.modules.reverse();
    let reversed = Driver::new().compile_package_sources(&sources);
    assert!(!reversed.has_errors, "{:?}", reversed.diagnostics);
    assert_eq!(value(reversed), 42);
}
#[test]
fn handbuilt_duplicate_invalid_names_and_wrong_parent_paths_are_rejected() {
    let original = owned_package();
    let mut duplicate = original.clone();
    duplicate.modules.push(duplicate.modules[0].clone());
    reject_owned(&duplicate, "duplicate");
    for name in ["bad-name", "fn", ""] {
        let mut s = original.clone();
        s.modules[0].path = vec![name.into()];
        reject_owned(&s, "module");
    }
    let mut wrong = original.clone();
    let mut child = wrong.modules[1].clone();
    child.path = vec!["elsewhere".into(), "beta".into()];
    wrong.modules[0].children.push(child);
    reject_owned(&wrong, "path");
    let mut empty = original;
    empty.modules[0].path.clear();
    reject_owned(&empty, "path");
}
#[test]
fn handbuilt_source_depth_limit_rejects_instead_of_recursing_into_codegen() {
    let mut sources = owned_package();
    sources.entry.source = "fn main(){42}".into();
    sources.modules.clear();
    let mut node = driver::ModuleSources {
        path: (0..256).map(|i| format!("level{i}")).collect(),
        entry: driver::SourceFile {
            file_path: PathBuf::from("depth.ns"),
            source: String::new(),
        },
        children: vec![],
    };
    for depth in (1..256).rev() {
        node = driver::ModuleSources {
            path: (0..depth).map(|i| format!("level{i}")).collect(),
            entry: driver::SourceFile {
                file_path: PathBuf::from(format!("depth{depth}.ns")),
                source: String::new(),
            },
            children: vec![node],
        };
    }
    sources.modules.push(node);
    reject_owned(&sources, "depth");
}
#[test]
fn handbuilt_file_and_total_source_byte_limits_are_checked_before_parse() {
    let mut sources = owned_package();
    sources.entry.source = " ".repeat(16 * 1024 * 1024 + 1);
    reject_owned(&sources, "byte");
    let mut sources = owned_package();
    sources.entry.source = "fn main(){42}".into();
    sources.modules = (0..5)
        .map(|i| driver::ModuleSources {
            path: vec![format!("large{i}")],
            entry: driver::SourceFile {
                file_path: PathBuf::from(format!("large{i}.ns")),
                source: " ".repeat(14 * 1024 * 1024),
            },
            children: vec![],
        })
        .collect();
    reject_owned(&sources, "byte");
}
#[test]
fn source_errors_from_two_physical_files_have_distinct_exact_token_widths() {
    let p = Package::new("exe");
    p.write("src/main.ns", "fn main(){broken.read()}");
    for newline in ["\n", "\r\n"] {
        p.write(
            "src/broken.ns",
            &format!("-- 文{newline}pub fn read(){{{newline}    缺失{newline}}}"),
        );
        let result = Driver::new().compile_package(&p.0);
        assert!(result.has_errors);
        let matching: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| d.message.contains("缺失"))
            .collect();
        assert_eq!(matching.len(), 1, "{:?}", result.diagnostics);
        let span = matching[0].primary_span.unwrap();
        assert_eq!(span.hi().0 - span.lo().0, "缺失".len() as u32);
        assert!(result.into_artifact().is_err());
    }
}

#[test]
fn handbuilt_exact_file_and_depth_limits_accept_but_one_more_module_rejects() {
    let mut sources = owned_package();
    sources.modules.clear();
    sources.entry.source = "fn main(){42}".into();
    sources
        .entry
        .source
        .push_str(&" ".repeat(16 * 1024 * 1024 - sources.entry.source.len()));
    let result = Driver::new().compile_package_sources(&sources);
    assert!(!result.has_errors, "{:?}", result.diagnostics);
    assert_eq!(value(result), 42);
    sources.entry.source = "fn main(){42}".into();
    let mut node = driver::ModuleSources {
        path: (0..255).map(|i| format!("level{i}")).collect(),
        entry: driver::SourceFile {
            file_path: PathBuf::from("last.ns"),
            source: String::new(),
        },
        children: vec![],
    };
    for depth in (1..255).rev() {
        node = driver::ModuleSources {
            path: (0..depth).map(|i| format!("level{i}")).collect(),
            entry: driver::SourceFile {
                file_path: PathBuf::from(format!("depth{depth}.ns")),
                source: String::new(),
            },
            children: vec![node],
        };
    }
    sources.modules = vec![node];
    let result = Driver::new().compile_package_sources(&sources);
    assert!(!result.has_errors, "{:?}", result.diagnostics);
    assert_eq!(value(result), 42);
    sources.modules = (0..65536)
        .map(|i| driver::ModuleSources {
            path: vec![format!("m{i}")],
            entry: driver::SourceFile {
                file_path: PathBuf::from("module.ns"),
                source: String::new(),
            },
            children: vec![],
        })
        .collect();
    reject_owned(&sources, "module");
}

#[test]
fn declared_package_version_and_namespace_affect_full_ids_but_not_standard_types() {
    let p = Package::new("exe");
    p.write("src/main.ns", "fn main(){model.Payload{n:42}.n}");
    p.write("src/model.ns", "pub struct Payload{n:i64}");
    let first = checked(&p);
    let original = named(&first.type_pool, "Payload");
    p.write(
        "package.toml",
        "[package]\nname=\"sample\"\ndomain=\"example.org\"\nversion=\"2.0.0\"\ntype=\"exe\"\n",
    );
    let changed_version = checked(&p);
    assert_ne!(original, named(&changed_version.type_pool, "Payload"));
    assert_eq!(
        standard(&first.type_pool),
        standard(&changed_version.type_pool)
    );
    p.write("src/main.ns", "fn main(){other.Payload{n:42}.n}");
    std::fs::rename(p.0.join("src/model.ns"), p.0.join("src/other.ns")).unwrap();
    let moved_namespace = checked(&p);
    assert_ne!(
        named(&changed_version.type_pool, "Payload"),
        named(&moved_namespace.type_pool, "Payload")
    );
    assert_eq!(value(moved_namespace), 42);
}

#[test]
fn executable_main_arity_is_checked_and_return_types_are_unrestricted() {
    for source in ["fn main(x:i64){x}", "fn main(.x:i64=42){x}"] {
        let p = Package::new("exe");
        p.write("src/main.ns", source);
        reject(&p, "main");
    }
    for source in ["fn main(){true}", "fn main()->?i64{null}"] {
        let p = Package::new("exe");
        p.write("src/main.ns", source);
        assert!(matches!(Driver::new().run_package(&p.0), RunResult::Ok));
    }
}
#[test]
fn library_without_initializers_can_have_no_startup_entry_and_still_run() {
    for kind in ["lib", "tmp"] {
        let p = Package::new(kind);
        p.write(
            "src/lib.ns",
            "pub fn answer()->i64{42};fn main(){panic(\"uninvoked library main\")}",
        );
        let result = checked(&p);
        let artifact = result.into_artifact().unwrap();
        assert!(matches!(
            Driver::new().run_artifact(artifact),
            RunResult::Ok
        ));
        assert!(matches!(Driver::new().run_package(&p.0), RunResult::Ok));
    }
}

#[test]
fn private_parent_namespace_blocks_child_module_access_but_preserves_local_captures() {
    let p = Package::new("exe");
    p.write("src/api.ns", "pub fn answer()->i64{42}");
    p.write(
        "src/main.ns",
        "private mod api;fn main(){let f=||api.answer();f()}",
    );
    assert_eq!(value(checked(&p)), 42);
    p.write("src/caller.ns", "pub fn read()->i64{api.answer()}");
    p.write("src/main.ns", "private mod api;fn main(){caller.read()}");
    reject(&p, "not visible");
    p.write(
        "src/caller.ns",
        "use api.answer;pub fn read()->i64{answer()}",
    );
    reject(&p, "import");
    p.write(
        "src/caller.ns",
        "use .api.answer;pub fn read()->i64{answer()}",
    );
    reject(&p, "import");
    p.write("src/main.ns", "pub mod api;fn main(){caller.read()}");
    assert_eq!(value(checked(&p)), 42);
}

#[test]
fn inline_private_names_have_the_same_namespace_boundary_as_file_modules() {
    for source in [
        "private mod api{pub fn answer()->i64{42}};mod caller{pub fn read()->i64{api.answer()}};fn main(){caller.read()}",
        "private const secret=42;mod caller{pub fn read()->i64{secret}};fn main(){caller.read()}",
    ] {
        let result = Driver::new().compile(source);
        assert!(result.has_errors);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.message.contains("not visible")),
            "{:?}",
            result.diagnostics
        );
        assert!(result.into_artifact().is_err());
    }
    let result = Driver::new()
        .compile("private const secret=40;fn main(){let f=||{let g=||secret+2;g()};f()}");
    assert!(!result.has_errors, "{:?}", result.diagnostics);
    assert_eq!(value(result), 42);
}
