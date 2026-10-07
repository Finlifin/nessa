//! Caller-owned dependency trees must retain package semantics through execution and archives.
use std::collections::BTreeMap;
use std::path::PathBuf;

use driver::{CompileResult, Driver, ManifestDocument, ModuleSources, PackageSources, SourceFile};
use type_pool::{IdentityPathSegment, TypeId, TypeIndex, TypeKind, TypePool};

fn package(
    name: &str,
    version: &str,
    kind: &str,
    deps: &[(&str, &str)],
    source: &str,
) -> PackageSources {
    let (domain, short) = name.split_once('/').unwrap();
    let mut manifest = format!(
        "[package]\nname={short:?}\ndomain={domain:?}\nversion={version:?}\ntype={kind:?}\n[dependencies]\n"
    );
    for (name, version) in deps {
        manifest.push_str(&format!("{name:?}={version:?}\n"));
    }
    let root = PathBuf::from(format!(
        "/nonexistent-nessa-owned-catalog/{domain}/{short}/{version}"
    ));
    PackageSources {
        entry: SourceFile {
            file_path: root.join("src/entry.ns"),
            source: source.into(),
        },
        root,
        manifest: ManifestDocument::parse(&manifest).unwrap(),
        modules: vec![],
    }
}
fn module(package: &mut PackageSources, name: &str, source: &str) {
    package.modules.push(ModuleSources {
        path: vec![name.into()],
        entry: SourceFile {
            file_path: package.root.join(format!("src/{name}.ns")),
            source: source.into(),
        },
        children: vec![],
    });
}
fn checked(root: &PackageSources, catalog: &[PackageSources]) -> CompileResult {
    let result = Driver::new().compile_package_sources_with_dependencies(root, catalog);
    assert!(!result.has_errors, "{:?}", result.diagnostics);
    result.type_pool.validate().unwrap();
    result
}
fn execute(artifact: nsbc::CompiledArtifact) -> i64 {
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
fn value(root: &PackageSources, catalog: &[PackageSources]) -> i64 {
    execute(checked(root, catalog).into_artifact().unwrap())
}
fn reject(root: &PackageSources, catalog: &[PackageSources], expected: &str) {
    let result = Driver::new().compile_package_sources_with_dependencies(root, catalog);
    assert!(
        result.has_errors,
        "expected rejection: {}",
        root.entry.source
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.level == diagnostic::Level::Error
                && d.message.to_lowercase().contains(&expected.to_lowercase())),
        "expected {expected}: {:?}",
        result.diagnostics
    );
    assert!(result.codegen_output.functions.is_empty());
    assert!(result.into_artifact().is_err());
}
fn nominal(pool: &TypePool, name: &str) -> TypeId {
    let found: Vec<_> = (0..pool.len()).map(|n| TypeIndex::from_raw(n as u32)).filter(|&i| matches!(pool.get(i).kind, TypeKind::Struct { name: n, .. } if str_interner::get(n) == name)).collect();
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
    for d in &pool.identity_input().unwrap().declarations {
        if matches!(d.path.first(), Some(IdentityPathSegment::Named(n)) if n == "std") {
            assert!(
                ids.insert(format!("{:?}", d.path), pool.get(d.type_index).type_id)
                    .is_none()
            );
        }
    }
    assert!(ids.len() > 2);
    ids
}

#[test]
fn complete_three_package_dag_selects_versions_independently_of_catalog_order() {
    let leaf1 = package(
        "example.org/leaf",
        "1.0.0",
        "lib",
        &[],
        "pub fn answer()->i64{1}",
    );
    let leaf2 = package(
        "example.org/leaf",
        "2.0.0",
        "lib",
        &[],
        "pub fn answer()->i64{40}",
    );
    let middle = package(
        "example.org/middle",
        "1.0.0",
        "lib",
        &[("example.org/leaf", "2.0.0")],
        "use leaf.answer;pub fn read()->i64{answer()+2}",
    );
    let root = package(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/middle", "1.0.0")],
        "use middle.read;fn main(){read()}",
    );
    let malformed_unused = package(
        "example.org/unused",
        "1.0.0",
        "lib",
        &[],
        "this is not valid source (",
    );
    assert_eq!(
        value(
            &root,
            &[
                leaf1.clone(),
                middle.clone(),
                leaf2.clone(),
                malformed_unused.clone()
            ]
        ),
        42
    );
    assert_eq!(value(&root, &[malformed_unused, leaf2, middle, leaf1]), 42);
}

#[test]
fn divergent_duplicate_missing_conflicting_and_cyclic_catalogs_reject_artifacts() {
    let root = package(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/leaf", "1.0.0")],
        "fn main(){42}",
    );
    let leaf = package("example.org/leaf", "1.0.0", "lib", &[], "pub const n:i64=1");
    let changed = package("example.org/leaf", "1.0.0", "lib", &[], "pub const n:i64=2");
    reject(&root, &[leaf.clone(), changed], "duplicate");
    reject(&root, &[], "leaf");
    let a = package(
        "example.org/a",
        "1.0.0",
        "lib",
        &[("example.org/leaf", "1.0.0")],
        "",
    );
    let b = package(
        "example.org/b",
        "1.0.0",
        "lib",
        &[("example.org/leaf", "2.0.0")],
        "",
    );
    let root = package(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/a", "1.0.0"), ("example.org/b", "1.0.0")],
        "fn main(){42}",
    );
    let leaf2 = package("example.org/leaf", "2.0.0", "lib", &[], "");
    reject(&root, &[a, b, leaf, leaf2], "compatible");
    let a = package(
        "example.org/a",
        "1.0.0",
        "lib",
        &[("example.org/b", "1.0.0")],
        "",
    );
    let b = package(
        "example.org/b",
        "1.0.0",
        "lib",
        &[("example.org/a", "1.0.0")],
        "",
    );
    reject(&root, &[a, b], "cycle");
}

#[test]
fn malformed_selected_source_is_rejected_even_when_not_imported() {
    let root = package(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/leaf", "1.0.0")],
        "fn main(){42}",
    );
    let leaf = package("example.org/leaf", "1.0.0", "lib", &[], "pub fn broken( {");
    reject(&root, &[leaf], "expected");
}

#[test]
fn aggregate_owned_source_bytes_are_bounded_across_selected_packages() {
    // Every file and package is below its individual bound; the complete DAG exceeds 64 MiB.
    let names = [
        "example.org/a",
        "example.org/b",
        "example.org/c",
        "example.org/d",
        "example.org/e",
    ];
    let deps: Vec<_> = names.iter().map(|&name| (name, "1.0.0")).collect();
    let root = package("example.org/app", "1.0.0", "exe", &deps, "fn main(){42}");
    let padding = " ".repeat(13 * 1024 * 1024);
    let catalog: Vec<_> = names
        .iter()
        .map(|name| package(name, "1.0.0", "lib", &[], &padding))
        .collect();
    reject(&root, &catalog, "limit");
}

#[test]
fn imports_preserve_pub_reexports_selective_glob_alias_and_package_private_access() {
    let mut leaf = package(
        "example.org/leaf",
        "1.0.0",
        "lib",
        &[],
        "pub use internal.answer;pub const two:i64=2;fn hidden()->i64{99};private fn secret()->i64{98}",
    );
    module(
        &mut leaf,
        "internal",
        "fn base()->i64{40};fn answer()->i64{base()}",
    );
    for source in [
        "use leaf.{answer as read,two};fn main(){read()+two}",
        "use leaf.*;fn main(){answer()+two}",
        "use leaf as dependency;fn main(){dependency.answer()+dependency.two}",
    ] {
        let root = package(
            "example.org/app",
            "1.0.0",
            "exe",
            &[("example.org/leaf", "1.0.0")],
            source,
        );
        assert_eq!(value(&root, std::slice::from_ref(&leaf)), 42);
    }
    for name in ["hidden", "secret"] {
        let root = package(
            "example.org/app",
            "1.0.0",
            "exe",
            &[("example.org/leaf", "1.0.0")],
            &format!("use leaf.{name};fn main(){{{name}()}}"),
        );
        reject(&root, std::slice::from_ref(&leaf), "not visible");
    }
}

#[test]
fn dependency_fallback_is_direct_only_and_requires_use_before_expression() {
    let leaf = package(
        "example.org/leaf",
        "1.0.0",
        "lib",
        &[],
        "pub fn answer()->i64{42}",
    );
    let middle = package(
        "example.org/middle",
        "1.0.0",
        "lib",
        &[("example.org/leaf", "1.0.0")],
        "pub use leaf.answer",
    );
    for source in [
        "use leaf.answer;fn main(){answer()}",
        "fn main(){middle.answer()}",
    ] {
        let root = package(
            "example.org/app",
            "1.0.0",
            "exe",
            &[("example.org/middle", "1.0.0")],
            source,
        );
        reject(
            &root,
            &[leaf.clone(), middle.clone()],
            if source.starts_with("use") {
                "leaf"
            } else {
                "middle"
            },
        );
    }
    let root = package(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/middle", "1.0.0")],
        "use middle.answer;fn main(){answer()}",
    );
    assert_eq!(value(&root, &[leaf, middle]), 42);
}

#[test]
fn local_module_shadows_dependency_and_duplicate_short_names_are_ambiguous_when_selected() {
    let one = package(
        "one.org/shared",
        "1.0.0",
        "lib",
        &[],
        "pub fn answer()->i64{1}",
    );
    let two = package(
        "two.org/shared",
        "1.0.0",
        "lib",
        &[],
        "pub fn answer()->i64{2}",
    );
    let deps = [("one.org/shared", "1.0.0"), ("two.org/shared", "1.0.0")];
    let unused = package("example.org/app", "1.0.0", "exe", &deps, "fn main(){42}");
    assert_eq!(value(&unused, &[one.clone(), two.clone()]), 42);
    let selected = package(
        "example.org/app",
        "1.0.0",
        "exe",
        &deps,
        "use shared.answer;fn main(){answer()}",
    );
    reject(&selected, &[one.clone(), two.clone()], "ambiguous");
    let shadow = package(
        "example.org/app",
        "1.0.0",
        "exe",
        &deps,
        "mod shared{pub fn answer()->i64{42}};use shared.answer;fn main(){answer()}",
    );
    assert_eq!(value(&shadow, &[one, two]), 42);
}

#[test]
fn dependency_root_paths_and_forward_aliases_cannot_leak_consumer_names() {
    let mut leaf = package(
        "example.org/leaf",
        "1.0.0",
        "lib",
        &[],
        "const seed:i64=40;pub typealias Number=Later;typealias Later=i64;pub fn answer()->Number{nested.read()+2}",
    );
    module(&mut leaf, "nested", "use @seed;pub fn read()->i64{seed}");
    let root = package(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/leaf", "1.0.0")],
        "typealias Later=bool;const seed:i64=0;use leaf.answer;fn main(){answer()}",
    );
    assert_eq!(value(&root, std::slice::from_ref(&leaf)), 42);
    let escaping = package(
        "example.org/leaf",
        "1.0.0",
        "lib",
        &[],
        "use .consumer;pub fn answer()->i64{consumer}",
    );
    let root = package(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/leaf", "1.0.0")],
        "pub const consumer:i64=42;use leaf.answer;fn main(){answer()}",
    );
    reject(&root, &[escaping], "parent");
    let unresolved = package(
        "example.org/leaf",
        "1.0.0",
        "lib",
        &[],
        "pub typealias Number=ConsumerOnly;pub fn answer()->Number{42}",
    );
    let root = package(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/leaf", "1.0.0")],
        "typealias ConsumerOnly=i64;use leaf.answer;fn main(){answer()}",
    );
    reject(&root, &[unresolved], "ConsumerOnly");
}

#[test]
fn diamond_initializers_and_nested_root_hooks_run_once_before_consumer_main() {
    let mut leaf = package(
        "example.org/leaf",
        "1.0.0",
        "lib",
        &[],
        "global count:i64=0;fn __init__(){count=count+1};pub fn read()->i64{count*10+nested.read()}",
    );
    module(
        &mut leaf,
        "nested",
        "global count:i64=0;fn __init__(){count=count+1};pub fn read()->i64{count}",
    );
    let a = package(
        "example.org/a",
        "1.0.0",
        "lib",
        &[("example.org/leaf", "1.0.0")],
        "use leaf.read;pub const observed:i64=read();fn __init__(){if observed!=11{panic(\"leaf not ready\")}}",
    );
    let b = package(
        "example.org/b",
        "1.0.0",
        "lib",
        &[("example.org/leaf", "1.0.0")],
        "use leaf.read;pub const observed:i64=read()",
    );
    let unused = package(
        "example.org/unused",
        "1.0.0",
        "lib",
        &[],
        "fn __init__(){panic(\"unused initialized\")}",
    );
    let root = package(
        "example.org/app",
        "1.0.0",
        "exe",
        &[
            ("example.org/a", "1.0.0"),
            ("example.org/b", "1.0.0"),
            ("example.org/unused", "1.0.0"),
        ],
        "use a;use b;global observed:i64=0;fn __init__(){observed=a.observed+b.observed};fn main(){observed+20}",
    );
    assert_eq!(value(&root, &[b, unused, leaf, a]), 42);
}

#[test]
fn each_package_has_independent_weak_prelude_and_external_names_do_not_grant_privilege() {
    let leaf = package(
        "example.org/leaf",
        "1.0.0",
        "lib",
        &[],
        "pub fn answer()->i64{to_i64(40)+str_len(to_string(12))}",
    );
    let root = package(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/leaf", "1.0.0")],
        "fn to_i64(x:i64)->i64{0};use leaf.answer;fn main(){answer()+to_i64(99)}",
    );
    assert_eq!(value(&root, &[leaf]), 42);
    for name in ["std", "core", "alloc"] {
        let fake = package(
            &format!("example.org/{name}"),
            "1.0.0",
            "lib",
            &[],
            "pub const forged=.to_i64'builtin",
        );
        let root = package(
            "example.org/app",
            "1.0.0",
            "exe",
            &[(&format!("example.org/{name}"), "1.0.0")],
            "fn main(){42}",
        );
        reject(&root, &[fake], "builtin");
    }
}

#[test]
fn full_nominal_and_standard_ids_survive_wrapping_consumer_aliases_and_archive_roundtrip() {
    let leaf = package(
        "example.org/leaf",
        "1.2.3",
        "lib",
        &[],
        "pub struct Payload{pub n:i64};pub typealias Item=Payload;pub fn consume(p:Item)->i64{p.n}",
    );
    let standalone = checked(&leaf, &[]);
    let expected = nominal(&standalone.type_pool, "Payload");
    let expected_std = standard(&standalone.type_pool);
    let mut archives = Vec::new();
    for (name, source) in [
        (
            "first",
            "use leaf.{Item as Imported,consume};typealias Local=Imported;fn main(){consume(Local{n:42})}",
        ),
        (
            "second",
            "use leaf as renamed;fn main(){renamed.consume(renamed.Item{n:42})}",
        ),
    ] {
        let root = package(
            &format!("example.org/{name}"),
            "9.0.0",
            "exe",
            &[("example.org/leaf", "1.2.3")],
            source,
        );
        let result = checked(&root, std::slice::from_ref(&leaf));
        assert_eq!(nominal(&result.type_pool, "Payload"), expected);
        assert_eq!(standard(&result.type_pool), expected_std);
        assert!(!root.root.exists() && !leaf.root.exists());
        let bytes = nsbc_io::write_artifact(&result.into_artifact().unwrap()).unwrap();
        archives.push(bytes);
    }
    // No caller-owned package source survives loading: execution uses archive bytes only.
    drop(leaf);
    for bytes in archives {
        let restored = nsbc_io::read_artifact(&bytes).unwrap();
        assert_eq!(nominal(&restored.type_pool, "Payload"), expected);
        assert_eq!(standard(&restored.type_pool), expected_std);
        assert_eq!(execute(restored), 42);
    }
}

#[test]
fn source_module_named_after_its_package_keeps_original_identity() {
    let leaf = package(
        "example.org/leaf",
        "1.0.0",
        "lib",
        &[],
        "pub mod leaf { pub struct Payload { pub n:i64 } }",
    );
    let standalone = checked(&leaf, &[]);
    let root = package(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/leaf", "1.0.0")],
        "use leaf.leaf.Payload;fn main(){Payload{n:42}.n}",
    );
    let compiled = checked(&root, std::slice::from_ref(&leaf));
    assert_eq!(
        nominal(&compiled.type_pool, "Payload"),
        nominal(&standalone.type_pool, "Payload")
    );
    assert_eq!(execute(compiled.into_artifact().unwrap()), 42);
}

#[test]
fn selective_nested_import_loads_package_root_hook_but_unused_manifest_edge_does_not() {
    let mut leaf = package(
        "example.org/leaf",
        "1.0.0",
        "lib",
        &[],
        "pub mod nested;fn __init__(){panic(\"selected-root-loaded\")}",
    );
    module(&mut leaf, "nested", "pub fn answer()->i64{42}");
    let mut root = package(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/leaf", "1.0.0")],
        "fn main(){42}",
    );
    assert_eq!(value(&root, std::slice::from_ref(&leaf)), 42);
    root.entry.source = "use leaf.nested.answer;fn main(){answer()}".into();
    let artifact = checked(&root, &[leaf]).into_artifact().unwrap();
    match Driver::new().run_artifact(artifact) {
        driver::RunResult::RuntimeError(message) => {
            assert!(message.contains("selected-root-loaded"), "{message}")
        }
        _ => panic!("selected package root initializer did not execute"),
    }
}

#[test]
fn consumer_lexical_extension_cannot_leak_into_dependency() {
    let leaf = package(
        "x.org/leaf",
        "1.0.0",
        "lib",
        &[],
        "pub struct Payload{};pub fn answer()->i64{Payload{}.surprise()}",
    );
    let root = package(
        "x.org/app",
        "1.0.0",
        "exe",
        &[("x.org/leaf", "1.0.0")],
        "use leaf.{Payload,answer};extend Payload{pub fn surprise(self)->i64{42}};fn main(){answer()}",
    );
    reject(&root, &[leaf], "visible");
}

#[test]
fn deepest_valid_nominal_identity_survives_dependency_wrapper() {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            let mut leaf = package("x.org/leaf", "1.0.0", "lib", &[], "");
            let mut next = None;
            for depth in (1..=255).rev() {
                let path: Vec<String> = (0..depth).map(|n| format!("m{n}")).collect();
                let node = ModuleSources {
                    path,
                    entry: SourceFile {
                        file_path: PathBuf::from(format!("/nonexistent/deep/{depth}")),
                        source: if depth == 255 {
                            "pub struct Payload{}".into()
                        } else {
                            String::new()
                        },
                    },
                    children: next.take().into_iter().collect(),
                };
                next = Some(node);
            }
            leaf.modules = next.into_iter().collect();
            let standalone = checked(&leaf, &[]);
            let id = nominal(&standalone.type_pool, "Payload");
            let root = package(
                "x.org/app",
                "1.0.0",
                "exe",
                &[("x.org/leaf", "1.0.0")],
                "fn main(){42}",
            );
            let compiled = checked(&root, &[leaf]);
            assert_eq!(nominal(&compiled.type_pool, "Payload"), id);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn consumer_scoped_trait_evidence_stays_in_consumer_package() {
    let leaf = package(
        "example.org/leaf",
        "1.0.0",
        "lib",
        &[],
        "pub trait Read{fn read(self)->i64};pub struct Payload{};pub fn answer()->i64{let reader:Read=Payload{};reader.read()}",
    );
    let root = package(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/leaf", "1.0.0")],
        "use leaf.{Read,Payload,answer};extend Read for Payload{pub fn read(self)->i64{42}};fn main(){answer()}",
    );
    reject(&root, &[leaf], "does not implement expected trait");
    let leaf = package(
        "example.org/leaf",
        "1.0.0",
        "lib",
        &[],
        "pub struct Payload{};pub fn answer()->i64{2}",
    );
    let root = package(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/leaf", "1.0.0")],
        "use leaf.{Payload,answer};trait Read{fn read(self)->i64};extend Read for Payload{pub fn read(self)->i64{40}};fn main(){let reader:Read=Payload{};reader.read()+answer()}",
    );
    assert_eq!(value(&root, &[leaf]), 42);
}
