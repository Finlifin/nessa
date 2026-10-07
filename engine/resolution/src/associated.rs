//! Plain impl blocks share a type namespace while retaining declaration scopes.

use std::collections::HashMap;

use ast::{Ast, NodeIndex, NodeKind};
use str_interner::StrId;
use type_pool::{TypeIndex, TypeKind};

use crate::resolver::Resolver;
use crate::{ScopeId, SymbolId, SymbolKind, Visibility};

/// Keep associated bindings distinct from ordinary namespace constants until
/// implementation records can retain and substitute their exact bindings.
pub(crate) fn resolve_declaration(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) {
    let scope = &r.scopes[r.current_scope.0 as usize];
    let allowed = !scope.node.is_null()
        && matches!(
            ast.node(scope.node).kind,
            NodeKind::TraitDef
                | NodeKind::ImplDef
                | NodeKind::ImplTraitDef
                | NodeKind::ExtendDef
                | NodeKind::ExtendTraitDef
        );
    if !allowed {
        r.diag_ctx
            .error("`assoc` is only allowed directly in trait, impl or extend bodies".into())
            .with_primary_span(ast.node(node).span)
            .emit(r.diag_ctx);
        return;
    }
    let declaration = ast.fixed_children(node)[0];
    if declaration.is_null() {
        return;
    }
    if ast.node(declaration).kind == NodeKind::AssocBinding {
        for &child in &ast.fixed_children(declaration)[1..] {
            crate::name::resolve_names(r, ast, child);
        }
        r.node_scopes.insert(declaration, r.current_scope);
        return;
    }
    // Retain the existing legacy `assoc <definition>` namespace behavior.
    crate::name::resolve_names(r, ast, declaration);
}

pub(crate) fn enclosing_type(r: &Resolver<'_>, mut scope: ScopeId) -> Option<TypeIndex> {
    loop {
        let entry = &r.scopes[scope.0 as usize];
        if let Some(ty) = entry.assoc_type {
            return r.type_pool.canonical_type(ty);
        }
        scope = entry.parent?;
    }
}

pub(crate) fn resolve_self_type(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) {
    if let Some(ty) = enclosing_type(r, r.current_scope) {
        let symbol = r.alloc_unbound_symbol(
            str_interner::intern("Self"),
            SymbolKind::Type,
            NodeIndex::NULL,
        );
        r.symbols[symbol.0 as usize].type_index = ty;
        r.node_symbols.insert(node, symbol);
    } else {
        r.diag_ctx
            .error("`Self` is only available in a type scope".into())
            .with_primary_span(ast.node(node).span)
            .emit(r.diag_ctx);
    }
}

fn expression_symbol(r: &Resolver<'_>, ast: &Ast, node: NodeIndex) -> Option<SymbolId> {
    if node.is_null() {
        return None;
    }
    match ast.node(node).kind {
        NodeKind::Id => r.lookup(ast.node(node).str_id),
        NodeKind::Projection => {
            let children = ast.fixed_children(node);
            let owner = expression_symbol(r, ast, children[0])?;
            crate::imports::member(r, owner, ast.node(children[1]).str_id).ok()
        }
        _ => r.node_symbols.get(&node).copied(),
    }
}

fn target_type(
    r: &mut Resolver<'_>,
    ast: &Ast,
    symbol: SymbolId,
    depth: usize,
) -> Option<TypeIndex> {
    if depth >= r.symbols.len() {
        return None;
    }
    let entry = r.symbols[symbol.0 as usize].clone();
    if !matches!(entry.kind, SymbolKind::Type | SymbolKind::Module) {
        return None;
    }
    if let Some(ty) = r.type_pool.canonical_type(entry.type_index) {
        return Some(ty);
    }
    // Forward aliases have not completed their TypePool target yet. Follow the
    // bound declaration in its lexical scope without mutating its identity.
    if !entry.def_node.is_null() && ast.node(entry.def_node).kind == NodeKind::Typealias {
        let value = ast.fixed_children(entry.def_node)[1];
        if !value.is_null() && matches!(ast.node(value).kind, NodeKind::Tuple | NodeKind::Call) {
            bind_forward_type_names(r, ast, value, entry.scope);
            return crate::typing::resolve_type_expr_inner(r, ast, value);
        }
        if !value.is_null() && ast.node(value).kind == NodeKind::View {
            let children = ast.fixed_children(value);
            let object = children[0];
            if (r.options.builtin_access || r.options.privileged_nodes.contains(&value))
                && !object.is_null()
                && ast.node(object).kind == NodeKind::Symbol
                && str_interner::get(ast.node(children[1]).str_id) == "builtin"
            {
                let name = ast.fixed_children(object)[0];
                let name = str_interner::get(ast.node(name).str_id);
                if let Some(symbol) = r.resolve_builtin_view(value, &name) {
                    r.node_symbols.insert(value, symbol);
                    if let Some(factory) = crate::type_factories::identity(r, value) {
                        r.symbols[entry.id.0 as usize].kind = SymbolKind::TypeFactory(factory);
                        return None;
                    }
                    return r
                        .type_pool
                        .canonical_type(r.symbols[symbol.0 as usize].type_index);
                }
            }
        }
        let target = expression_symbol_in_scope(r, ast, value, entry.scope)?;
        let result = target_type(r, ast, target, depth + 1);
        if let SymbolKind::TypeFactory(factory) = r.symbols[target.0 as usize].kind {
            r.symbols[entry.id.0 as usize].kind = SymbolKind::TypeFactory(factory);
        }
        return result;
    }
    None
}

fn bind_forward_type_names(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex, scope: ScopeId) {
    if node.is_null() {
        return;
    }
    if matches!(ast.node(node).kind, NodeKind::Id | NodeKind::Projection) {
        if let Some(symbol) = expression_symbol_in_scope(r, ast, node, scope) {
            r.node_symbols.insert(node, symbol);
        }
        return;
    }
    for &child in ast
        .fixed_children(node)
        .iter()
        .chain(ast.multi_children(node))
    {
        bind_forward_type_names(r, ast, child, scope);
    }
}

fn expression_symbol_in_scope(
    r: &Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    mut scope: ScopeId,
) -> Option<SymbolId> {
    if ast.node(node).kind == NodeKind::Projection {
        let children = ast.fixed_children(node);
        let owner = expression_symbol_in_scope(r, ast, children[0], scope)?;
        return crate::imports::member(r, owner, ast.node(children[1]).str_id).ok();
    }
    if ast.node(node).kind != NodeKind::Id {
        return r.node_symbols.get(&node).copied();
    }
    loop {
        let entry = &r.scopes[scope.0 as usize];
        if let Some(&symbol) = entry.bindings.get(&ast.node(node).str_id) {
            return Some(symbol);
        }
        if r.options.detached_package_roots.contains(&entry.node) {
            return None;
        }
        scope = entry.parent?;
    }
}

/// Attach every resolvable prepared plain impl. Called during import fixpoint:
/// an impl target can itself be imported, while imports can name impl members.
pub(crate) fn attach(r: &mut Resolver<'_>, ast: &Ast) -> bool {
    let original = r.current_scope;
    let aliases: Vec<_> = r
        .symbols
        .iter()
        .filter_map(|symbol| {
            (!symbol.def_node.is_null() && ast.node(symbol.def_node).kind == NodeKind::Typealias)
                .then_some(symbol.id)
        })
        .collect();
    let mut changed = false;
    for alias in aliases {
        let previous_kind = r.symbols[alias.0 as usize].kind;
        let index = r.symbols[alias.0 as usize].type_index;
        if r.type_pool.canonical_type(index).is_none()
            && let Some(target) = target_type(r, ast, alias, 0)
        {
            if index == TypeIndex::INVALID {
                let name = r.symbols[alias.0 as usize].name;
                let index = r.register_type(TypeKind::Typealias { name, target });
                r.symbols[alias.0 as usize].type_index = index;
            } else if let TypeKind::Typealias {
                target: destination,
                ..
            } = &mut r.type_pool.get_mut(index).kind
            {
                *destination = target;
            }
            changed = true;
        }
        changed |= previous_kind != r.symbols[alias.0 as usize].kind;
    }
    let pending: Vec<_> = r
        .scopes
        .iter()
        .filter_map(|scope| {
            (!scope.node.is_null()
                && matches!(
                    ast.node(scope.node).kind,
                    NodeKind::ImplDef
                        | NodeKind::ImplTraitDef
                        | NodeKind::ExtendDef
                        | NodeKind::ExtendTraitDef
                )
                && scope.assoc_type.is_none())
            .then_some((scope.id, scope.node, scope.parent))
        })
        .collect();
    for (scope, node, parent) in pending {
        r.current_scope = parent.unwrap_or(ScopeId::ROOT);
        let target = ast.fixed_children(node)[usize::from(matches!(
            ast.node(node).kind,
            NodeKind::ImplTraitDef | NodeKind::ExtendTraitDef
        ))];
        let ty = if ast.node(target).kind == NodeKind::Call {
            bind_forward_type_names(r, ast, target, r.current_scope);
            crate::typing::resolve_type_expr_inner(r, ast, target)
        } else {
            expression_symbol(r, ast, target).and_then(|symbol| target_type(r, ast, symbol, 0))
        };
        if let Some(ty) = ty {
            if ast.node(target).kind == NodeKind::Call && !r.node_symbols.contains_key(&target) {
                // Publish the applied type identity for existing impl/trait consumers.
                // The callee retains its distinct compile-time factory binding.
                let Some(name) = r.type_pool.display_name(ty) else {
                    r.diag_ctx
                        .error("impl type exceeds supported type nesting".into())
                        .with_primary_span(ast.node(target).span)
                        .emit(r.diag_ctx);
                    continue;
                };
                let name = str_interner::intern(&name);
                let symbol = r.alloc_unbound_symbol(name, SymbolKind::Type, NodeIndex::NULL);
                r.symbols[symbol.0 as usize].type_index = ty;
                r.node_symbols.insert(target, symbol);
                r.node_type_values.insert(target, ty);
            }
            r.scopes[scope.0 as usize].assoc_type = Some(ty);
            changed = true;
        }
    }
    r.current_scope = original;
    changed
}

fn within(r: &Resolver<'_>, ancestor: ScopeId, mut scope: ScopeId) -> bool {
    if !crate::imports::same_package(r, ancestor, scope) {
        return false;
    }
    loop {
        if scope == ancestor {
            return true;
        }
        let Some(parent) = r.scopes[scope.0 as usize].parent else {
            return false;
        };
        scope = parent;
    }
}

fn private_accessible(r: &Resolver<'_>, owner: ScopeId, from: ScopeId) -> bool {
    if crate::imports::lexical_private_accessible(r, owner, from) {
        return true;
    }
    let Some(ty) = r.scopes[owner.0 as usize].assoc_type else {
        return false;
    };
    if matches!(r.type_pool.get(ty).kind, TypeKind::Module { .. }) {
        return false;
    }
    let mut scope = from;
    loop {
        if r.scopes[scope.0 as usize].assoc_type == Some(ty) {
            return crate::imports::same_package(r, owner, scope);
        }
        if r.imports.namespace_scopes.contains(&scope) {
            return false;
        }
        let Some(parent) = r.scopes[scope.0 as usize].parent else {
            return false;
        };
        scope = parent;
    }
}

pub(crate) fn member(r: &Resolver<'_>, ty: TypeIndex, name: StrId) -> Result<SymbolId, String> {
    member_from(r, ty, name, r.current_scope)
}

pub(crate) fn member_from(
    r: &Resolver<'_>,
    ty: TypeIndex,
    name: StrId,
    from: ScopeId,
) -> Result<SymbolId, String> {
    let ty = r.type_pool.canonical_type(ty).unwrap_or(ty);
    let candidates: Vec<_> = r
        .scopes
        .iter()
        .filter(|scope| scope.assoc_type == Some(ty))
        .filter_map(|scope| scope.bindings.get(&name).map(|&symbol| (scope.id, symbol)))
        .collect();
    let mut visible = candidates.iter().copied().filter(|&(owner, target)| {
        if let Some(&extension) = r.imports.extension_scopes.get(&owner)
            && !within(r, extension, from)
        {
            return false;
        }
        match crate::imports::binding_visibility(r, owner, name, target) {
            Visibility::Private => private_accessible(r, owner, from),
            Visibility::Package => crate::imports::same_package(r, owner, from),
            Visibility::Public => true,
        }
    });
    let Some((_, target)) = visible.next() else {
        if candidates.is_empty() {
            return Err(format!("unknown member `{}`", str_interner::get(name)));
        }
        return Err(format!(
            "`{}` is not visible from this module",
            str_interner::get(name)
        ));
    };
    if visible.any(|(_, symbol)| symbol != target) {
        return Err(format!(
            "ambiguous associated member `{}`",
            str_interner::get(name)
        ));
    }
    Ok(target)
}

/// Check instance access without binding the projection to a static function
/// that would discard the receiver during lowering.
pub(crate) fn validate_instance_member(
    r: &Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    ty: TypeIndex,
) {
    let name = ast.node(ast.fixed_children(node)[1]).str_id;
    let Some(ty) = r.type_pool.canonical_type(ty) else {
        return;
    };
    if !r
        .scopes
        .iter()
        .any(|scope| scope.assoc_type == Some(ty) && scope.bindings.contains_key(&name))
    {
        return;
    }
    let Some(&from) = r.node_scopes.get(&node) else {
        return;
    };
    if let Err(error) = member_from(r, ty, name, from) {
        r.diag_ctx
            .error(error)
            .with_primary_span(ast.node(node).span)
            .emit(r.diag_ctx);
    }
}

pub(crate) fn validate(r: &Resolver<'_>, ast: &Ast) {
    let mut members = HashMap::new();
    for scope in &r.scopes {
        let Some(ty) = scope
            .assoc_type
            .and_then(|ty| r.type_pool.canonical_type(ty))
        else {
            continue;
        };
        if matches!(r.type_pool.get(ty).kind, TypeKind::Trait { .. }) {
            continue;
        }
        // Trait implementations have independent method namespaces. Their
        // slots are selected by trait identity, not merged into plain impls.
        if !scope.node.is_null()
            && matches!(
                ast.node(scope.node).kind,
                NodeKind::ImplTraitDef | NodeKind::ExtendTraitDef
            )
        {
            continue;
        }
        for (&name, &symbol) in &scope.bindings {
            let extension = r.imports.extension_scopes.get(&scope.id).copied();
            if let Some(previous) = members.insert((ty, name, extension), symbol)
                && previous != symbol
            {
                r.diag_ctx
                    .error(format!(
                        "duplicate associated definition `{}`",
                        str_interner::get(name)
                    ))
                    .with_primary_span(ast.node(r.symbols[symbol.0 as usize].def_node).span)
                    .emit(r.diag_ctx);
            }
        }
    }
}

/// Rewrite a static type call to its checked constructor declaration. Dynamic
/// Type values retain their value semantics and are rejected by type checking.
pub(crate) fn resolve_constructor(r: &mut Resolver<'_>, ast: &Ast, call: NodeIndex) {
    let callee = ast.fixed_children(call)[0];
    let Some(&symbol) = r.node_symbols.get(&callee) else {
        return;
    };
    if r.symbols[symbol.0 as usize].kind != SymbolKind::Type {
        return;
    }
    let ty = r.symbols[symbol.0 as usize].type_index;
    match member(r, ty, str_interner::intern("new")) {
        Ok(constructor) => {
            r.node_symbols.insert(callee, constructor);
            r.constructor_types.insert(call, symbol);
        }
        Err(error) => {
            r.diag_ctx
                .error(format!("cannot construct type: {error}"))
                .with_primary_span(ast.node(callee).span)
                .emit(r.diag_ctx);
        }
    }
}

pub(crate) fn validate_constructors(r: &Resolver<'_>, ast: &Ast) {
    for &call in r.constructor_types.keys() {
        let callee = ast.fixed_children(call)[0];
        let Some(&symbol) = r.node_symbols.get(&callee) else {
            continue;
        };
        let ty = r.symbols[symbol.0 as usize].type_index;
        let callable = r
            .type_pool
            .canonical_type(ty)
            .is_some_and(|ty| matches!(r.type_pool.get(ty).kind, TypeKind::Function { .. }));
        if !callable {
            r.diag_ctx
                .error("type constructor `new` must have a function type".into())
                .with_primary_span(ast.node(callee).span)
                .emit(r.diag_ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use diagnostic::DiagnosticContext;
    use rustc_span::source_map::FilePathMapping;
    use rustc_span::{FileName, SourceMap};

    use super::*;

    fn resolve(source: &str) -> (crate::ResolvedAst, Vec<String>) {
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("associated.ns".into()), source.to_owned());
        let diagnostics = DiagnosticContext::new(&map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve(ast, &diagnostics);
        let errors = diagnostics
            .diagnostics()
            .iter()
            .map(|error| error.message.clone())
            .collect();
        (resolved, errors)
    }

    #[test]
    fn multiple_plain_impls_share_canonical_namespace_and_forward_members() {
        let (resolved, errors) = resolve(
            "struct Point { x: i64 }\n\
             fn main() -> i64 { Alias.answer() }\n\
             impl Alias { fn answer() -> i64 { helper() } }\n\
             impl Point { fn helper() -> i64 { 42 } }\n\
             typealias Alias = Point",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let nominal = resolved
            .symbols
            .iter()
            .find(|symbol| str_interner::get(symbol.name) == "Point")
            .unwrap()
            .type_index;
        let impl_scopes: Vec<_> = resolved
            .scopes
            .iter()
            .filter(|scope| {
                !scope.node.is_null() && resolved.ast.node(scope.node).kind == NodeKind::ImplDef
            })
            .collect();
        assert_eq!(impl_scopes.len(), 2);
        assert!(
            impl_scopes
                .iter()
                .all(|scope| scope.assoc_type == Some(nominal))
        );
        let helper = resolved
            .symbols
            .iter()
            .find(|symbol| str_interner::get(symbol.name) == "helper")
            .unwrap();
        assert!(resolved.node_symbols.iter().any(|(&node, &symbol)| {
            symbol == helper.id && node != resolved.ast.fixed_children(helper.def_node)[0]
        }));
    }

    #[test]
    fn static_type_calls_bind_the_checked_new_signature() {
        let (resolved, errors) = resolve(
            "fn main() -> Point { Point(7) }\n\
             struct Point { x: i64 }\n\
             impl Point { pub fn new(x: i64) -> Point { Point { x: x } } }",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let call = resolved
            .ast
            .nodes
            .iter()
            .enumerate()
            .find(|(_, node)| node.kind == NodeKind::Call)
            .map(|(index, _)| NodeIndex(index as u32))
            .unwrap();
        let callee = resolved.ast.fixed_children(call)[0];
        let symbol = resolved.node_symbols[&callee];
        assert_eq!(
            str_interner::get(resolved.symbols[symbol.0 as usize].name),
            "new"
        );
        assert_eq!(
            resolved.call_arguments[&call].declaration,
            resolved.symbols[symbol.0 as usize].def_node
        );
    }

    #[test]
    fn duplicate_members_and_non_type_impl_targets_are_rejected() {
        for (source, expected) in [
            (
                "struct P {}\nimpl P { fn f() {} }\nimpl P { fn f() {} }",
                "duplicate associated definition",
            ),
            (
                "struct P { fn f() {} }\nimpl P { fn f() {} }",
                "duplicate associated definition",
            ),
            (
                "const value = 1\nimpl value { fn f() {} }",
                "impl requires a statically known type",
            ),
            ("struct P {}\nfn main() { P() }", "cannot construct type"),
            (
                "struct P {}\nimpl P { const new = true }\nfn main() { P() }",
                "constructor `new` must have a function type",
            ),
        ] {
            let (_, errors) = resolve(source);
            assert!(
                errors.iter().any(|error| error.contains(expected)),
                "{source}: {errors:?}"
            );
        }
    }

    #[test]
    fn imports_can_select_members_from_impls_whose_target_is_imported() {
        let (_, errors) = resolve(
            "mod types { pub struct Point {} }\n\
             use types.Point\n\
             use Point.answer\n\
             impl Point { pub fn answer() -> i64 { 42 } }\n\
             fn main() -> i64 { answer() }",
        );
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn private_members_are_shared_inside_the_type_but_hidden_outside() {
        let (_, errors) = resolve(
            "struct P { private fn helper() -> i64 { 42 } }\n\
             impl P { fn answer() -> i64 { P.helper() } }\n\
             fn main() -> i64 { P.answer() }",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let (_, errors) = resolve(
            "struct P {}\nimpl P { private fn hidden() -> i64 { 42 } }\n\
             fn main() -> i64 { P.hidden() }",
        );
        assert!(
            errors.iter().any(|error| error.contains("not visible")),
            "{errors:?}"
        );
    }
}
