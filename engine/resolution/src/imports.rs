//! Import bindings share symbol identities; visibility belongs to the binding.

use std::collections::{HashMap, HashSet};

use ast::{Ast, NodeIndex, NodeKind};
use str_interner::StrId;

use crate::resolver::Resolver;
use crate::{ScopeId, SymbolId, Visibility};

#[derive(Default)]
pub(crate) struct ImportState {
    pub(crate) module_scopes: HashMap<NodeIndex, ScopeId>,
    /// File and module namespaces bound private lexical access.
    pub(crate) namespace_scopes: HashSet<ScopeId>,
    pub(crate) extension_scopes: HashMap<ScopeId, ScopeId>,
    pub(crate) prepared_scopes: HashSet<NodeIndex>,
    pub(crate) binding_visibility: HashMap<(ScopeId, StrId), Visibility>,
    pub(crate) uses: Vec<(ScopeId, NodeIndex, Visibility)>,
    pub(crate) processed_uses: HashSet<NodeIndex>,
    weak_bindings: HashSet<(ScopeId, StrId)>,
}

pub(crate) fn package_scope(r: &Resolver<'_>, mut scope: ScopeId) -> ScopeId {
    loop {
        let entry = &r.scopes[scope.0 as usize];
        if r.options.package_roots.contains(&entry.node) {
            return scope;
        }
        let Some(parent) = entry.parent else {
            return scope;
        };
        // The synthetic bootstrap ROOT precedes the user's real FileScope.
        // Package-relative paths start in that file's declaration namespace.
        if parent == ScopeId::ROOT {
            return scope;
        }
        scope = parent;
    }
}

pub(crate) fn same_package(r: &Resolver<'_>, left: ScopeId, right: ScopeId) -> bool {
    package_id(r, left) == package_id(r, right)
}

pub(crate) fn package_id(r: &Resolver<'_>, scope: ScopeId) -> u32 {
    let root = package_scope(r, scope);
    if r.options
        .package_roots
        .contains(&r.scopes[root.0 as usize].node)
    {
        root.0
    } else {
        // Bootstrap ROOT, user FileScope and ordinary modules share the one
        // user package. Only explicit loaded roots create another identity.
        ScopeId::ROOT.0
    }
}

pub(crate) fn binding_accessible(
    r: &Resolver<'_>,
    scope: ScopeId,
    name: StrId,
    symbol: SymbolId,
) -> bool {
    let visibility = binding_visibility(r, scope, name, symbol);
    match visibility {
        Visibility::Public => true,
        Visibility::Package => same_package(r, scope, r.current_scope),
        Visibility::Private => lexical_private_accessible(r, scope, r.current_scope),
    }
}

/// Lexical functions, blocks and closures retain private access, but a child
/// module is a separate namespace even when its scope has the owner as ancestor.
pub(crate) fn lexical_private_accessible(
    r: &Resolver<'_>,
    owner: ScopeId,
    mut from: ScopeId,
) -> bool {
    loop {
        if from == owner {
            return true;
        }
        if r.imports.namespace_scopes.contains(&from) {
            return false;
        }
        let Some(parent) = r.scopes[from.0 as usize].parent else {
            return false;
        };
        from = parent;
    }
}

pub(crate) fn binding_visibility(
    r: &Resolver<'_>,
    scope: ScopeId,
    name: StrId,
    symbol: SymbolId,
) -> Visibility {
    r.imports
        .binding_visibility
        .get(&(scope, name))
        .copied()
        .unwrap_or(r.symbols[symbol.0 as usize].visibility)
}

pub(crate) fn member(r: &Resolver<'_>, symbol: SymbolId, name: StrId) -> Result<SymbolId, String> {
    let raw_type = r.symbols[symbol.0 as usize].type_index;
    let ty = r.type_pool.canonical_type(raw_type).unwrap_or(raw_type);
    crate::associated::member(r, ty, name)
}

fn lookup_from(
    r: &Resolver<'_>,
    mut scope: ScopeId,
    name: StrId,
) -> Result<Option<SymbolId>, String> {
    let package = package_scope(r, scope);
    loop {
        let entry = &r.scopes[scope.0 as usize];
        if let Some(&symbol) = entry.bindings.get(&name) {
            return if binding_accessible(r, scope, name, symbol) {
                Ok(Some(symbol))
            } else {
                Err(format!(
                    "import `{}` is not visible from this module",
                    str_interner::get(name)
                ))
            };
        }
        if r.options.detached_package_roots.contains(&entry.node) {
            break;
        }
        let Some(parent) = entry.parent else { break };
        scope = parent;
    }
    let node = r.scopes[package.0 as usize].node;
    let Some(candidates) = r
        .options
        .package_dependencies
        .get(&node)
        .and_then(|imports| imports.get(&name))
    else {
        return Ok(None);
    };
    if candidates.len() != 1 {
        return Err(format!(
            "ambiguous dependency name `{}`",
            str_interner::get(name)
        ));
    }
    r.node_symbols
        .get(&candidates[0])
        .copied()
        .map(Some)
        .ok_or_else(|| "dependency package has no prepared module symbol".into())
}

fn parent_scope(r: &Resolver<'_>, scope: ScopeId) -> Result<ScopeId, String> {
    if r.options
        .detached_package_roots
        .contains(&r.scopes[scope.0 as usize].node)
    {
        return Err("import parent path cannot cross package root".into());
    }
    r.scopes[scope.0 as usize]
        .parent
        .ok_or_else(|| "import path has no parent scope".into())
}

fn resolve_path(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    start: ScopeId,
) -> Result<SymbolId, String> {
    let children = ast.fixed_children(node);
    match ast.node(node).kind {
        NodeKind::Id => {
            let symbol = lookup_from(r, start, ast.node(node).str_id)?.ok_or_else(|| {
                format!(
                    "unknown import `{}`",
                    str_interner::get(ast.node(node).str_id)
                )
            })?;
            // Keep the selected namespace as a load fact. A selective import
            // from a nested module must also load its actual package root hook.
            r.node_symbols.insert(node, symbol);
            Ok(symbol)
        }
        NodeKind::SuperPath => {
            let parent = parent_scope(r, start)?;
            resolve_path(r, ast, children[0], parent)
        }
        NodeKind::PackagePath => resolve_path(r, ast, children[0], package_scope(r, start)),
        NodeKind::PathProjection | NodeKind::Projection => {
            let base = resolve_path(r, ast, children[0], start)?;
            member(r, base, ast.node(children[1]).str_id)
        }
        NodeKind::PathAsBind => resolve_path(r, ast, children[0], start),
        _ => Err("unsupported import path".to_string()),
    }
}

fn collect_bindings(
    r: &mut Resolver<'_>,
    ast: &Ast,
    path: NodeIndex,
    start: ScopeId,
) -> Result<Vec<(StrId, SymbolId)>, String> {
    let children = ast.fixed_children(path);
    match ast.node(path).kind {
        NodeKind::SuperPath => {
            let parent = parent_scope(r, start)?;
            collect_bindings(r, ast, children[0], parent)
        }
        NodeKind::PackagePath => collect_bindings(r, ast, children[0], package_scope(r, start)),
        NodeKind::PathProjectionAll => {
            let base = resolve_path(r, ast, children[0], start)?;
            exported_bindings(r, base)
        }
        NodeKind::PathProjectionMulti => {
            let base = resolve_path(r, ast, children[0], start)?;
            let mut bindings = Vec::new();
            for &item in ast.multi_children(path) {
                bindings.extend(collect_selected(r, ast, item, base)?);
            }
            Ok(bindings)
        }
        NodeKind::PathAsBind => Ok(vec![(
            ast.node(children[1]).str_id,
            resolve_path(r, ast, children[0], start)?,
        )]),
        _ => {
            let symbol = resolve_path(r, ast, path, start)?;
            // The exported binding may rename a shared symbol. Preserve the
            // path's member name rather than reverting to its original name.
            let name = if matches!(
                ast.node(path).kind,
                NodeKind::PathProjection | NodeKind::Projection
            ) {
                ast.node(children[1]).str_id
            } else {
                ast.node(path).str_id
            };
            Ok(vec![(name, symbol)])
        }
    }
}

fn exported_bindings(r: &Resolver<'_>, symbol: SymbolId) -> Result<Vec<(StrId, SymbolId)>, String> {
    let raw_type = r.symbols[symbol.0 as usize].type_index;
    let ty = r.type_pool.canonical_type(raw_type).unwrap_or(raw_type);
    let scope = r
        .scopes
        .iter()
        .find(|scope| scope.assoc_type == Some(ty))
        .ok_or_else(|| "glob import requires an associated scope".to_string())?;
    let mut bindings: Vec<_> = scope
        .bindings
        .iter()
        .filter(|&(name, symbol)| {
            binding_visibility(r, scope.id, *name, *symbol) == Visibility::Public
        })
        .map(|(&name, &symbol)| (name, symbol))
        .collect();
    bindings.sort_by_key(|(name, _)| str_interner::get(*name));
    Ok(bindings)
}

/// Selection items are relative to their selected namespace, never to a
/// lexical fallback with the same spelling.
fn selected_path(
    r: &Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    base: SymbolId,
) -> Result<SymbolId, String> {
    let children = ast.fixed_children(node);
    match ast.node(node).kind {
        NodeKind::Id => member(r, base, ast.node(node).str_id),
        NodeKind::PathProjection => member(
            r,
            selected_path(r, ast, children[0], base)?,
            ast.node(children[1]).str_id,
        ),
        NodeKind::PathAsBind => selected_path(r, ast, children[0], base),
        _ => Err("unsupported selected import path".into()),
    }
}

fn collect_selected(
    r: &Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    base: SymbolId,
) -> Result<Vec<(StrId, SymbolId)>, String> {
    let children = ast.fixed_children(node);
    match ast.node(node).kind {
        NodeKind::PathProjectionMulti => {
            let nested = selected_path(r, ast, children[0], base)?;
            let mut bindings = Vec::new();
            for &item in ast.multi_children(node) {
                bindings.extend(collect_selected(r, ast, item, nested)?)
            }
            Ok(bindings)
        }
        NodeKind::PathProjectionAll => {
            let nested = selected_path(r, ast, children[0], base)?;
            exported_bindings(r, nested)
        }
        NodeKind::PathAsBind => Ok(vec![(
            ast.node(children[1]).str_id,
            selected_path(r, ast, children[0], base)?,
        )]),
        _ => {
            let symbol = selected_path(r, ast, node, base)?;
            Ok(vec![(r.symbols[symbol.0 as usize].name, symbol)])
        }
    }
}

fn apply_use(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    visibility: Visibility,
) -> Result<bool, String> {
    let path = ast.fixed_children(node)[0];
    let bindings = collect_bindings(r, ast, path, r.current_scope)?;
    let implicit = r.options.implicit_imports.contains(&node);
    let scope = r.current_scope;
    // Validate before inserting anything: a failed selective import is atomic.
    let mut selected = HashMap::new();
    for &(name, target) in &bindings {
        if selected
            .insert(name, target)
            .is_some_and(|previous| previous != target)
        {
            return Err(format!(
                "conflicting selected import `{}`",
                str_interner::get(name)
            ));
        }
        if let Some(&existing) = r.scopes[scope.0 as usize].bindings.get(&name)
            && existing != target
            && !implicit
            && !r.imports.weak_bindings.contains(&(scope, name))
        {
            return Err(format!(
                "import conflicts with existing binding `{}`",
                str_interner::get(name)
            ));
        }
    }
    let mut changed = false;
    for (name, target) in bindings {
        let existing = r.scopes[scope.0 as usize].bindings.get(&name).copied();
        if implicit && existing.is_some_and(|existing| existing != target) {
            continue;
        }
        if existing.is_none()
            || (!implicit
                && existing != Some(target)
                && r.imports.weak_bindings.contains(&(scope, name)))
        {
            r.scopes[scope.0 as usize].bindings.insert(name, target);
            r.imports
                .binding_visibility
                .insert((scope, name), visibility);
            changed = true;
        } else if visibility == Visibility::Public
            && r.imports.binding_visibility.get(&(scope, name)) != Some(&Visibility::Public)
        {
            r.imports
                .binding_visibility
                .insert((scope, name), visibility);
            changed = true;
        }
        if implicit && existing.is_none() {
            r.imports.weak_bindings.insert((scope, name));
        } else if !implicit {
            r.imports.weak_bindings.remove(&(scope, name));
        }
    }
    Ok(changed)
}

pub(crate) fn resolve_all(r: &mut Resolver<'_>, ast: &Ast) {
    let uses = r.imports.uses.clone();
    let original = r.current_scope;
    // Each successful iteration adds a binding or upgrades its visibility.
    // Cyclic reexports without a concrete definition therefore reach a fixed point.
    loop {
        let mut changed = crate::associated::attach(r, ast);
        for &(scope, node, visibility) in &uses {
            r.current_scope = scope;
            if let Ok(added) = apply_use(r, ast, node, visibility) {
                changed |= added
            }
        }
        if !changed {
            break;
        }
    }
    for (scope, node, visibility) in uses {
        r.current_scope = scope;
        if let Err(error) = apply_use(r, ast, node, visibility) {
            r.diag_ctx
                .error(error)
                .with_primary_span(ast.node(node).span)
                .emit(r.diag_ctx);
        }
        r.imports.processed_uses.insert(node);
    }
    r.current_scope = original;
}

pub(crate) fn resolve_local_use(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    visibility: Visibility,
) {
    if r.imports.processed_uses.contains(&node) {
        return;
    }
    if let Err(error) = apply_use(r, ast, node, visibility) {
        r.diag_ctx
            .error(error)
            .with_primary_span(ast.node(node).span)
            .emit(r.diag_ctx);
    }
    r.imports.processed_uses.insert(node);
}

#[cfg(test)]
mod tests {
    use super::*;

    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};

    use crate::{ResolveOptions, ResolvedAst, SymbolKind};

    fn resolve(source: &str, package: Option<&str>) -> (ResolvedAst, Vec<String>) {
        resolve_configured(source, package, false, false)
    }

    fn resolve_configured(
        source: &str,
        package: Option<&str>,
        implicit: bool,
        privileged: bool,
    ) -> (ResolvedAst, Vec<String>) {
        let source_map = SourceMap::new(FilePathMapping::empty());
        let file =
            source_map.new_source_file(FileName::Custom("imports-test".into()), source.into());
        let diagnostics = DiagnosticContext::new(&source_map);
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty());
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
        let mut options = ResolveOptions::default();
        if implicit {
            let index = ast
                .nodes
                .iter()
                .position(|node| node.kind == NodeKind::UseStatement)
                .unwrap();
            options.implicit_imports.insert(NodeIndex(index as u32));
        }
        if privileged {
            options.expose_root_builtins = false;
            options
                .privileged_nodes
                .extend((1..ast.nodes.len()).map(|index| NodeIndex(index as u32)));
        }
        if let Some(package) = package {
            let node = ast
                .nodes
                .iter()
                .enumerate()
                .find_map(|(index, node)| {
                    (node.kind == NodeKind::ModuleDef
                        && str_interner::get(ast.node(node.children[0]).str_id) == package)
                        .then_some(NodeIndex(index as u32))
                })
                .unwrap();
            options.package_roots.insert(node);
        }
        let resolved = crate::resolve_with_options(ast, &diagnostics, options);
        let messages = diagnostics
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect();
        (resolved, messages)
    }

    #[test]
    fn nested_modules_forward_declare_and_bind_full_projections() {
        let (resolved, errors) = resolve(
            "mod outer { mod inner { pub fn answer() -> i64 { later() }; fn later() -> i64 { 42 } } }; fn main() -> i64 { outer.inner.answer() }",
            None,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let answer = resolved
            .symbols
            .iter()
            .find(|symbol| str_interner::get(symbol.name) == "answer")
            .unwrap();
        assert_eq!(answer.visibility, Visibility::Public);
        assert!(
            resolved
                .ast
                .nodes
                .iter()
                .enumerate()
                .any(|(index, node)| node.kind == NodeKind::Projection
                    && resolved.node_symbols.get(&NodeIndex(index as u32)) == Some(&answer.id))
        );
    }

    #[test]
    fn selective_alias_and_reexport_share_the_original_symbol() {
        let (resolved, errors) = resolve(
            "use api.{answer as renamed}\nmod api { pub use implementation.answer }\nmod implementation { fn answer() -> i64 { 42 } }\nfn main() -> i64 { renamed() }",
            None,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let answer = resolved
            .symbols
            .iter()
            .find(|symbol| str_interner::get(symbol.name) == "answer")
            .unwrap();
        assert_eq!(answer.visibility, Visibility::Package);
        assert!(
            resolved
                .scopes
                .iter()
                .any(|scope| scope.bindings.get(&str_interner::intern("renamed"))
                    == Some(&answer.id))
        );
    }

    #[test]
    fn glob_reexports_reach_a_fixed_point_without_exposing_package_members() {
        let (resolved, errors) = resolve(
            "use api.*;\nmod api { pub use implementation.*; }\nmod implementation { pub fn answer() -> i64 { 42 }; fn hidden() -> i64 { 0 } }\nfn main() -> i64 { answer() }",
            None,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let file = resolved
            .scopes
            .iter()
            .find(|scope| scope.node == resolved.ast.root)
            .unwrap();
        assert!(file.bindings.contains_key(&str_interner::intern("answer")));
        assert!(!file.bindings.contains_key(&str_interner::intern("hidden")));
    }

    #[test]
    fn external_package_visibility_and_unknown_members_are_diagnosed() {
        let (_, errors) = resolve(
            "use dependency.hidden\nmod dependency { fn hidden() -> i64 { 42 } }\nfn main() { dependency.missing() }",
            Some("dependency"),
        );
        assert!(
            errors.iter().any(|error| error.contains("not visible")),
            "{errors:?}"
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("unknown member `missing`")),
            "{errors:?}"
        );
    }

    #[test]
    fn conflicting_import_is_rejected_and_does_not_override_the_definition() {
        let (resolved, errors) = resolve(
            "use dependency.answer\nmod dependency { pub fn answer() -> i64 { 1 } }\nfn answer() -> i64 { 42 }\nfn main() -> i64 { answer() }",
            None,
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("import conflicts")),
            "{errors:?}"
        );
        let file = resolved
            .scopes
            .iter()
            .find(|scope| scope.node == resolved.ast.root)
            .unwrap();
        let symbol = file.bindings[&str_interner::intern("answer")];
        assert_eq!(resolved.symbols[symbol.0 as usize].scope, file.id);
    }

    #[test]
    fn public_function_does_not_make_its_parameter_public() {
        let (resolved, errors) = resolve("pub fn identity(value: i64) -> i64 { value }", None);
        assert!(errors.is_empty(), "{errors:?}");
        let function = resolved
            .symbols
            .iter()
            .find(|symbol| symbol.kind == SymbolKind::Function)
            .unwrap();
        let parameter = resolved
            .symbols
            .iter()
            .find(|symbol| str_interner::get(symbol.name) == "value")
            .unwrap();
        assert_eq!(function.visibility, Visibility::Public);
        assert_eq!(parameter.visibility, Visibility::Private);
    }

    #[test]
    fn duplicate_definitions_and_named_std_modules_do_not_bypass_checks() {
        for source in [
            "fn repeated() {}\nfn repeated() {}",
            "struct repeated {}\nstruct repeated {}",
            "fn repeated() {}\nmod repeated {}",
        ] {
            let (_, errors) = resolve(source, None);
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains("duplicate definition")),
                "{errors:?}"
            );
        }
        let (_, errors) = resolve("mod std { const output = .println'builtin }", None);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("only allowed in privileged packages")),
            "{errors:?}"
        );
    }

    #[test]
    fn module_values_and_globals_are_forward_declared_with_shared_identity() {
        let (resolved, errors) = resolve(
            "use values.{first, second, third}\nfn read() { first + second + third }\nmod values { pub let first: i64 = 1\npub var second: i64 = 2\npub global third: i64 = 3 }",
            None,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let file = resolved
            .scopes
            .iter()
            .find(|scope| scope.node == resolved.ast.root)
            .unwrap();
        for name in ["first", "second", "third"] {
            let symbol = file.bindings[&str_interner::intern(name)];
            assert_eq!(
                resolved.symbols[symbol.0 as usize].kind,
                SymbolKind::Variable
            );
            let declaration = resolved.symbols[symbol.0 as usize].def_node;
            assert_eq!(resolved.node_symbols[&declaration], symbol);
            assert_eq!(
                resolved.node_symbols[&resolved.ast.fixed_children(declaration)[0]],
                symbol
            );
        }
        let global = resolved
            .ast
            .nodes
            .iter()
            .enumerate()
            .find(|(_, node)| node.kind == NodeKind::GlobalDecl)
            .unwrap()
            .0;
        assert_eq!(
            resolved.node_symbols[&NodeIndex(global as u32)],
            file.bindings[&str_interner::intern("third")]
        );
    }

    #[test]
    fn local_shadowing_does_not_reuse_a_module_value_symbol() {
        let (resolved, errors) = resolve(
            "let value: i64 = 1\nfn read() { let value: i64 = 42; value = 43; value }",
            None,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let values: Vec<_> = resolved
            .symbols
            .iter()
            .filter(|symbol| str_interner::get(symbol.name) == "value")
            .collect();
        assert_eq!(values.len(), 2);
        assert_ne!(values[0].id, values[1].id);
        assert_ne!(values[0].scope, values[1].scope);
    }

    #[test]
    fn unsupported_module_value_destructuring_is_rejected() {
        let (_, errors) = resolve("let (first, second) = (1, 2)", None);
        assert!(
            errors
                .iter()
                .any(|error| error.contains("module value destructuring")),
            "{errors:?}"
        );
    }

    #[test]
    fn associated_type_values_share_predeclared_scopes_and_symbols() {
        let (resolved, errors) = resolve(
            "use Holder.value as selected\nfn read() { selected }\nstruct Holder { pub global value: i64 = 42\nlet other: i64 = value\nfn __init__() { value = other; } }\nenum Status { ready, pub const default_value: i64 = 42; fn __init__() {} }",
            None,
        );
        assert!(errors.is_empty(), "{errors:?}");
        for name in ["Holder", "Status"] {
            let symbol = resolved
                .symbols
                .iter()
                .find(|symbol| {
                    str_interner::get(symbol.name) == name && symbol.kind == SymbolKind::Type
                })
                .unwrap();
            let associated: Vec<_> = resolved
                .scopes
                .iter()
                .filter(|scope| scope.assoc_type == Some(symbol.type_index))
                .collect();
            assert_eq!(associated.len(), 1);
            assert_eq!(associated[0].node, symbol.def_node);
            let hook = associated[0].bindings[&str_interner::intern("__init__")];
            assert_eq!(resolved.symbols[hook.0 as usize].scope, associated[0].id);
        }
        let file = resolved
            .scopes
            .iter()
            .find(|scope| scope.node == resolved.ast.root)
            .unwrap();
        let selected = file.bindings[&str_interner::intern("selected")];
        assert_eq!(
            resolved.symbols[selected.0 as usize].visibility,
            Visibility::Public
        );
        assert_eq!(
            resolved.symbols[selected.0 as usize].kind,
            SymbolKind::Variable
        );
    }

    #[test]
    fn control_statements_require_a_real_function_or_loop_context() {
        for source in [
            "return 42",
            "resume 42",
            "break",
            "continue",
            "if true { return 42 }",
            "while true { return 42 }",
            "struct Holder { return 42 }",
            "fn main() { break }",
            "fn main() { let target = 42; while true { break target } }",
            "while true { fn inner() { break } }",
            "fn main() { let f = || { continue }; }",
            "mod values { if true { resume 42 } }",
        ] {
            let (_, errors) = resolve(source, None);
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains("is only allowed inside")),
                "accepted {source}: {errors:?}"
            );
        }
        let (_, errors) = resolve(
            "while true { break }\nfn main() { while true { continue }; return 42 }",
            None,
        );
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn global_placement_init_signatures_and_const_writes_are_checked() {
        for source in [
            "fn main() { global value: i64 = 42 }",
            "fn __init__(value: i64) {}",
            "mod values { fn __init__() -> i64 { 42 } }",
            "fn __init__() { 42 }",
            "const value = 42\nfn main() { value = 43 }",
            "fn main() { const value = 42; value += 1 }",
            "mod values { pub const value = 42 }\nfn main() { values.value = 43 }",
        ] {
            let (_, errors) = resolve(source, None);
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains("global declarations")
                        || error.contains("__init__")
                        || error.contains("cannot assign to constant")),
                "accepted {source}: {errors:?}"
            );
        }
        let (_, errors) = resolve(
            "fn __init__() -> Unit {}\nmod values { fn __init__() {} }\nlet mutable: i64 = 0\nfn main() { mutable = 42; var local = 0; local += 1 }",
            None,
        );
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn implicit_imports_yield_to_local_definitions_and_explicit_imports() {
        let (resolved, errors) = resolve_configured(
            "use baseline.*;\nuse special.selected\nmod baseline { pub fn selected() -> i64 { 0 }; pub fn local() -> i64 { 0 } }\nmod special { pub fn selected() -> i64 { 42 } }\nfn local() -> i64 { 42 }\nfn main() -> i64 { selected() + local() }",
            None,
            true,
            false,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let file = resolved
            .scopes
            .iter()
            .find(|scope| scope.node == resolved.ast.root)
            .unwrap();
        let local = file.bindings[&str_interner::intern("local")];
        assert_eq!(resolved.symbols[local.0 as usize].scope, file.id);
        let selected = file.bindings[&str_interner::intern("selected")];
        let origin = &resolved.scopes[resolved.symbols[selected.0 as usize].scope.0 as usize];
        assert_eq!(
            str_interner::get(
                resolved
                    .ast
                    .node(resolved.ast.fixed_children(origin.node)[0])
                    .str_id
            ),
            "special"
        );
    }

    #[test]
    fn imported_native_constant_keeps_identity_after_builtin_promotion() {
        let (resolved, errors) = resolve_configured(
            "use api.output\nmod api { pub const output: fn(Any) -> Unit = .println'builtin }\nfn main() { output(42) }",
            None,
            false,
            true,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let file = resolved
            .scopes
            .iter()
            .find(|scope| scope.node == resolved.ast.root)
            .unwrap();
        let output = file.bindings[&str_interner::intern("output")];
        assert_eq!(
            resolved.symbols[output.0 as usize].kind,
            SymbolKind::BuiltinFunction(runtime::builtin::ids::PRINTLN)
        );
        assert_eq!(
            resolved.builtin_fns[&output],
            runtime::builtin::ids::PRINTLN
        );
    }

    #[test]
    fn parent_and_package_prefixes_wrap_globs_and_selective_paths() {
        let (resolved, errors) = resolve(
            "mod library { mod types { pub typealias Integer = i64 }\nmod api { use .types.*\nuse @library.types.{Integer as RootInteger}\nfn answer() -> Integer { const value: RootInteger = 42; value } } }",
            None,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let api = resolved
            .scopes
            .iter()
            .find(|scope| {
                !scope.node.is_null()
                    && resolved.ast.node(scope.node).kind == NodeKind::ModuleDef
                    && str_interner::get(
                        resolved
                            .ast
                            .node(resolved.ast.fixed_children(scope.node)[0])
                            .str_id,
                    ) == "api"
            })
            .unwrap();
        assert_eq!(
            api.bindings[&str_interner::intern("Integer")],
            api.bindings[&str_interner::intern("RootInteger")]
        );
    }

    #[test]
    fn nested_selection_paths_and_globs_bind_each_export() {
        let (resolved, errors) = resolve(
            "use api.{inner.answer as renamed, inner.{answer as selected}, inner.*}\nmod api { mod inner { pub fn answer() -> i64 { 42 } } }\nfn main() -> i64 { renamed() + selected() + answer() }",
            None,
        );
        assert!(errors.is_empty(), "{errors:?}");
        let file = resolved
            .scopes
            .iter()
            .find(|scope| scope.node == resolved.ast.root)
            .unwrap();
        let answer = file.bindings[&str_interner::intern("answer")];
        assert_eq!(file.bindings[&str_interner::intern("renamed")], answer);
        assert_eq!(file.bindings[&str_interner::intern("selected")], answer);
    }

    #[test]
    fn conflicting_selection_is_atomic_and_unresolved_cycles_are_diagnosed() {
        let (resolved, errors) = resolve(
            "use api.{left as repeated, right as repeated}\nmod api { pub fn left() -> i64 { 1 }; pub fn right() -> i64 { 2 } }",
            None,
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("conflicting selected import")),
            "{errors:?}"
        );
        let file = resolved
            .scopes
            .iter()
            .find(|scope| scope.node == resolved.ast.root)
            .unwrap();
        assert!(
            !file
                .bindings
                .contains_key(&str_interner::intern("repeated"))
        );
        let (_, errors) = resolve(
            "mod left { pub use right.answer }\nmod right { pub use left.answer }",
            None,
        );
        assert_eq!(
            errors
                .iter()
                .filter(|error| error.contains("unknown member `answer`"))
                .count(),
            2
        );
    }

    #[test]
    fn package_reexport_exposes_its_binding_without_mutating_origin_visibility() {
        let (resolved, errors) = resolve(
            "use dependency.answer\nmod dependency { pub use implementation.answer\nmod implementation { fn answer() -> i64 { 42 } } }\nfn main() -> i64 { answer() }",
            Some("dependency"),
        );
        assert!(errors.is_empty(), "{errors:?}");
        let answer = resolved
            .symbols
            .iter()
            .find(|symbol| str_interner::get(symbol.name) == "answer")
            .unwrap();
        assert_eq!(answer.visibility, Visibility::Package);
        let (_, errors) = resolve(
            "use dependency.implementation.answer\nmod dependency { pub use implementation.answer\npub mod implementation { fn answer() -> i64 { 42 } } }",
            Some("dependency"),
        );
        assert!(
            errors.iter().any(|error| error.contains("not visible")),
            "{errors:?}"
        );
    }
}
