//! Verify declaration access survives the actual compiler and artifact path.

use type_pool::{MethodAccess, TypeIndex, TypeKind, TypePool};

fn nominal(pool: &TypePool, name: &str) -> TypeIndex {
    pool.snapshot()
        .types
        .iter()
        .enumerate()
        .find_map(|(index, info)| match info.kind {
            TypeKind::Struct { name: declared, .. } if str_interner::get(declared) == name => {
                Some(TypeIndex::from_raw(index as u32))
            }
            _ => None,
        })
        .expect("compiled nominal type")
}

#[test]
fn compiler_and_archive_preserve_method_access_and_scope_identity() {
    let compiled = driver::Driver::new().compile(
        "struct P { value: i64 }\n\
         impl P {\n\
           private fn hidden(self) -> i64 { self.value };\n\
           fn local(self) -> i64 { self.hidden() };\n\
           pub fn exposed(self) -> i64 { self.local() }\n\
         }\n\
         impl P { pub fn sibling(self) -> i64 { self.hidden() } }\n\
         fn main() -> i64 { P { value: 42 }.sibling() }",
    );
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let artifact = compiled.into_artifact().unwrap();
    let ty = nominal(&artifact.type_pool, "P");
    let original = artifact.type_pool.methods_of(ty);
    let access = |name: &str| {
        original
            .iter()
            .find(|method| str_interner::get(method.name) == name)
            .expect("compiled method")
            .access
    };
    let MethodAccess::Private(owner) = access("hidden") else {
        panic!("private declaration lost its access level")
    };
    let MethodAccess::Package(package) = access("local") else {
        panic!("unmodified method must retain package access")
    };
    assert_eq!(access("exposed"), MethodAccess::Public);
    assert_eq!(access("sibling"), MethodAccess::Public);
    let owner_context = artifact.type_pool.scope_context(owner).unwrap();
    assert_eq!(owner_context.package, package);
    assert_eq!(owner_context.assoc_type, Some(ty));
    let hidden = original
        .iter()
        .find(|method| str_interner::get(method.name) == "hidden")
        .unwrap();
    let sibling_scope = artifact
        .type_pool
        .scopes()
        .iter()
        .enumerate()
        .find(|(index, scope)| *index != owner as usize && scope.assoc_type == Some(ty))
        .map(|(index, _)| index as u32)
        .expect("another associated scope");
    assert!(
        artifact
            .type_pool
            .method_accessible(hidden, sibling_scope)
            .unwrap()
    );
    assert!(!artifact.type_pool.method_accessible(hidden, 0).unwrap());
    assert!(
        artifact
            .type_pool
            .snapshot()
            .methods
            .iter()
            .flatten()
            .all(|method| method.access != MethodAccess::LegacyUnknown)
    );

    let saved = nsbc_io::write_artifact(&artifact).unwrap();
    let restored = nsbc_io::read_artifact(&saved).unwrap();
    assert_eq!(restored.type_pool.scopes(), artifact.type_pool.scopes());
    for method in original {
        let restored_method = restored
            .type_pool
            .methods_of(ty)
            .iter()
            .find(|candidate| candidate.name == method.name)
            .unwrap();
        assert_eq!(restored_method.access, method.access);
        assert_eq!(restored_method.visible_scope, method.visible_scope);
        assert_eq!(restored_method.func_id, method.func_id);
    }
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), restored)
        .unwrap()
        .unwrap();
    let task = engine.vm_mut().spawn_root(entry);
    assert!(matches!(
        engine.vm_mut().run(),
        interpreter::VmResult::Finished
    ));
    assert_eq!(
        engine
            .vm_mut()
            .task_result_number(task)
            .unwrap()
            .to_i64_checked(),
        Some(42)
    );
}
