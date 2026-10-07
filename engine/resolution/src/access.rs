//! Actual declaration visibility and artifact-local lexical/package identities.

use type_pool::{MethodAccess, ScopeContext};

use crate::resolver::Resolver;
use crate::{SymbolId, Visibility};

pub(crate) fn method_access(r: &Resolver<'_>, symbol: SymbolId) -> MethodAccess {
    let symbol = &r.symbols[symbol.0 as usize];
    match symbol.visibility {
        Visibility::Public => MethodAccess::Public,
        Visibility::Package => MethodAccess::Package(crate::imports::package_id(r, symbol.scope)),
        Visibility::Private => MethodAccess::Private(symbol.scope.0),
    }
}

pub(crate) fn publish_scopes(r: &mut Resolver<'_>) {
    let scopes = r
        .scopes
        .iter()
        .map(|scope| ScopeContext {
            parent: scope.parent.map(|parent| parent.0),
            package: crate::imports::package_id(r, scope.id),
            assoc_type: scope
                .assoc_type
                .and_then(|ty| r.type_pool.canonical_type(ty)),
        })
        .collect();
    if let Err(error) = r.type_pool.install_scopes(scopes) {
        r.diag_ctx
            .error(format!("invalid resolved access scope metadata: {error}"))
            .emit(r.diag_ctx);
    }
}

#[cfg(test)]
mod tests {
    use ast::NodeKind;
    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};
    use type_pool::MethodAccess;

    use crate::{ResolveOptions, ResolvedAst};

    fn resolve(source: &str, loaded: bool) -> ResolvedAst {
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("access-test".into()), source.into());
        let diagnostics = DiagnosticContext::new(&map);
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty());
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
        let mut options = ResolveOptions::default();
        if loaded {
            // Mark the exact top-level module as a loaded package. An identically
            // named nested user module receives no authority from its name.
            let node = ast
                .multi_children(ast.root)
                .iter()
                .copied()
                .find(|&node| ast.node(node).kind == NodeKind::ModuleDef)
                .unwrap();
            options.package_roots.insert(node);
        }
        let resolved = crate::resolve_with_options(ast, &diagnostics, options);
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
        resolved
    }

    #[test]
    fn ordinary_and_type_body_methods_keep_declaration_visibility() {
        let resolved = resolve(
            "struct P { pub fn direct(self)->i64 {42} }\nimpl P { pub fn exposed(self)->i64 {42}; fn local(self)->i64 {42}; private fn hidden(self)->i64 {42} }\nenum E { one, private fn secret(self)->i64 {42} }",
            false,
        );
        for symbol in resolved.symbols.iter().filter(|symbol| {
            ["direct", "exposed", "local", "hidden", "secret"]
                .contains(&str_interner::get(symbol.name).as_str())
        }) {
            let owner = resolved.scopes[symbol.scope.0 as usize].assoc_type.unwrap();
            let method = resolved.type_pool.find_method(owner, symbol.name).unwrap();
            let expected = match str_interner::get(symbol.name).as_str() {
                "direct" | "exposed" => MethodAccess::Public,
                "local" => MethodAccess::Package(0),
                _ => MethodAccess::Private(symbol.scope.0),
            };
            assert_eq!(method.access, expected);
            assert_eq!(
                resolved
                    .type_pool
                    .scope_context(symbol.scope.0)
                    .unwrap()
                    .assoc_type,
                Some(owner)
            );
        }
        assert_eq!(resolved.type_pool.scopes().len(), resolved.scopes.len());
    }

    #[test]
    fn trait_extend_and_derived_entries_have_explicit_access() {
        let resolved = resolve(
            "struct P {}\ntrait Read { fn read(self)->i64 }\nimpl Read for P { pub fn read(self)->i64 {42} }\nextend P { private fn extension(target:P)->i64 {42} }\nderive Eq for P",
            false,
        );
        let ty = resolved
            .symbols
            .iter()
            .find(|symbol| str_interner::get(symbol.name) == "P")
            .unwrap()
            .type_index;
        let pool = &resolved.type_pool;
        assert_eq!(
            pool.find_method(ty, str_interner::intern("read"))
                .unwrap()
                .access,
            MethodAccess::Public
        );
        let extension = pool
            .find_method(ty, str_interner::intern("extension"))
            .unwrap();
        let symbol = resolved
            .symbols
            .iter()
            .find(|symbol| str_interner::get(symbol.name) == "extension")
            .unwrap();
        assert_eq!(extension.access, MethodAccess::Private(symbol.scope.0));
        assert_eq!(
            extension.visible_scope,
            resolved.scopes[symbol.scope.0 as usize]
                .parent
                .map(|scope| scope.0)
        );
        assert_eq!(
            pool.find_method(ty, str_interner::intern("eq"))
                .unwrap()
                .access,
            MethodAccess::Public
        );
        assert!(
            pool.trait_impls_snapshot()
                .iter()
                .flat_map(|record| &record.methods)
                .all(|method| method.access != MethodAccess::LegacyUnknown)
        );
    }

    #[test]
    fn only_explicit_loaded_roots_create_separate_package_identity() {
        let resolved = resolve(
            "mod std { pub struct Loaded {}\nimpl Loaded { fn local(self)->i64 {42} } }\nmod user { mod std { pub struct Local {}\nimpl Local { fn local(self)->i64 {42} } } }",
            true,
        );
        let loaded = resolved
            .symbols
            .iter()
            .find(|symbol| str_interner::get(symbol.name) == "Loaded")
            .unwrap();
        let local = resolved
            .symbols
            .iter()
            .find(|symbol| str_interner::get(symbol.name) == "Local")
            .unwrap();
        let pool = &resolved.type_pool;
        let loaded_method = pool
            .find_method(loaded.type_index, str_interner::intern("local"))
            .unwrap();
        let local_method = pool
            .find_method(local.type_index, str_interner::intern("local"))
            .unwrap();
        assert!(matches!(loaded_method.access, MethodAccess::Package(package) if package != 0));
        assert_eq!(local_method.access, MethodAccess::Package(0));
        assert_eq!(pool.scope_context(0).unwrap().package, 0);
        for scope in &resolved.scopes {
            assert_eq!(
                pool.scope_context(scope.id.0).unwrap().parent,
                scope.parent.map(|parent| parent.0)
            );
        }
    }
}
