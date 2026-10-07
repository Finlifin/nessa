//! Static associated type declarations and exact implementation bindings.

use std::collections::HashSet;

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::resolver::Resolver;

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: impl Into<String>) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}

fn default_expression(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    active: &mut HashSet<NodeIndex>,
    depth: usize,
) -> Result<Option<type_pool::AssociatedTypeExpr>, String> {
    use type_pool::AssociatedTypeExpr as Expr;
    if depth >= 256 || !active.insert(node) {
        return Err("associated default alias/type nesting is cyclic or exceeds 256".into());
    }
    let result = (|| {
        Ok(Some(match ast.node(node).kind {
            NodeKind::Call
                if crate::type_factories::identity(r, ast.fixed_children(node)[0]).is_some() =>
            {
                let [item] = ast.multi_children(node) else {
                    return Err(
                        "IterationStep requires exactly one positional type argument".into(),
                    );
                };
                if ast.node(*item).kind == NodeKind::NamedArg {
                    return Err("IterationStep requires a positional type argument".into());
                }
                let Some(item) = default_expression(r, ast, *item, active, depth + 1)? else {
                    return Ok(None);
                };
                Expr::IterationStep {
                    item: Box::new(item),
                }
            }
            NodeKind::SelfUpper => {
                let Some(owner) = r
                    .node_symbols
                    .get(&node)
                    .map(|symbol| r.symbols[symbol.0 as usize].type_index)
                else {
                    return Ok(None);
                };
                Expr::SelfType { trait_owner: owner }
            }
            NodeKind::Id | NodeKind::Projection => {
                if let Some(&symbol) = r.node_symbols.get(&node) {
                    let source = r.symbols[symbol.0 as usize].clone();
                    let declaration = source.def_node;
                    if !declaration.is_null()
                        && ast.node(declaration).kind == NodeKind::AssocBinding
                    {
                        let owner = r.scopes[source.scope.0 as usize].assoc_type;
                        if let Some(owner) = owner
                            && matches!(r.type_pool.get(owner).kind, TypeKind::Trait { .. })
                        {
                            return Ok(Some(Expr::Binding {
                                trait_owner: owner,
                                name: source.name,
                            }));
                        }
                    }
                    if !declaration.is_null() && ast.node(declaration).kind == NodeKind::Typealias {
                        return default_expression(
                            r,
                            ast,
                            ast.fixed_children(declaration)[1],
                            active,
                            depth + 1,
                        );
                    }
                }
                let Some(ty) = crate::typing::resolve_type_expr_inner(r, ast, node) else {
                    return Ok(None);
                };
                Expr::Concrete(ty)
            }
            NodeKind::OptionalType => {
                let Some(inner) =
                    default_expression(r, ast, ast.fixed_children(node)[0], active, depth + 1)?
                else {
                    return Ok(None);
                };
                Expr::Optional {
                    inner: Box::new(inner),
                }
            }
            NodeKind::Tuple if !ast.multi_children(node).is_empty() => {
                let mut elements = Vec::new();
                for &element in ast.multi_children(node) {
                    let Some(element) = default_expression(r, ast, element, active, depth + 1)?
                    else {
                        return Ok(None);
                    };
                    elements.push(element);
                }
                Expr::Tuple { elements }
            }
            NodeKind::Arrow if ast.node(ast.fixed_children(node)[0]).kind == NodeKind::FnType => {
                let children = ast.fixed_children(node);
                let mut parameters = Vec::new();
                for &parameter in ast.multi_children(children[0]) {
                    let Some(parameter) = default_expression(r, ast, parameter, active, depth + 1)?
                    else {
                        return Ok(None);
                    };
                    parameters.push(parameter);
                }
                let Some(return_type) = default_expression(r, ast, children[1], active, depth + 1)?
                else {
                    return Ok(None);
                };
                Expr::Function {
                    parameters,
                    return_type: Box::new(return_type),
                }
            }
            NodeKind::FnType => {
                let mut parameters = Vec::new();
                for &parameter in ast.multi_children(node) {
                    let Some(parameter) = default_expression(r, ast, parameter, active, depth + 1)?
                    else {
                        return Ok(None);
                    };
                    parameters.push(parameter);
                }
                Expr::Function {
                    parameters,
                    return_type: Box::new(Expr::Concrete(Intrinsic::Unit.type_index())),
                }
            }
            _ => {
                let Some(ty) = crate::typing::resolve_type_expr_inner(r, ast, node) else {
                    return Ok(None);
                };
                Expr::Concrete(ty)
            }
        }))
    })();
    active.remove(&node);
    result
}

pub(crate) fn prepare(r: &mut Resolver<'_>, ast: &Ast) {
    let nodes: Vec<_> = (1..ast.nodes.len())
        .map(|i| NodeIndex(i as u32))
        .filter(|&node| ast.node(node).kind == NodeKind::AssocBinding)
        .collect();
    // Give every declaration its own binder before aliases or defaults are read.
    for &node in &nodes {
        let Some(&scope) = r.node_scopes.get(&node) else {
            continue;
        };
        let owner_node = r.scopes[scope.0 as usize].node;
        if ast.node(owner_node).kind != NodeKind::TraitDef {
            continue;
        }
        let Some(owner) = r.scopes[scope.0 as usize].assoc_type else {
            continue;
        };
        let children = ast.fixed_children(node);
        let name = ast.node(children[0]).str_id;
        let marker = r.register_type(TypeKind::AssociatedType {
            trait_owner: owner,
            name,
        });
        if let Some(&symbol) = r.node_symbols.get(&children[0]) {
            r.symbols[symbol.0 as usize].type_index = marker;
        }
        r.node_type_values.insert(node, marker);
        if let TypeKind::Trait { assoc_types, .. } = &mut r.type_pool.get_mut(owner).kind {
            assoc_types.push((name, marker));
        }
    }
    let mut pending = nodes;
    loop {
        let before = pending.len();
        let before_types = r.type_pool.len();
        crate::typing::prepare_type_aliases_staged(r, ast, false);
        pending
            .retain(|&node| matches!(prepare_binding(r, ast, node), BindingPreparation::Pending));
        if pending.is_empty() || (before == pending.len() && before_types == r.type_pool.len()) {
            break;
        }
    }
    for node in pending {
        report(
            r,
            ast,
            node,
            "associated binding value must be a statically known, non-cyclic type",
        );
    }
}

enum BindingPreparation {
    Pending,
    Complete,
}

fn prepare_binding(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) -> BindingPreparation {
    let Some(&scope) = r.node_scopes.get(&node) else {
        return BindingPreparation::Complete;
    };
    let owner_node = r.scopes[scope.0 as usize].node;
    if !matches!(
        ast.node(owner_node).kind,
        NodeKind::TraitDef | NodeKind::ImplTraitDef | NodeKind::ExtendTraitDef
    ) {
        report(
            r,
            ast,
            node,
            "associated type bindings require a trait declaration or trait implementation",
        );
        return BindingPreparation::Complete;
    }
    let children = ast.fixed_children(node);
    let Some(annotation) = crate::typing::resolve_type_expr_inner(r, ast, children[1]) else {
        return BindingPreparation::Pending;
    };
    if r.type_pool.as_intrinsic(annotation) != Some(Intrinsic::Type) {
        report(
            r,
            ast,
            node,
            "associated declarations currently require the Type annotation; associated constants are not supported",
        );
        return BindingPreparation::Complete;
    }
    if ast.node(owner_node).kind == NodeKind::TraitDef {
        let expression = match default_expression(r, ast, children[2], &mut HashSet::new(), 0) {
            Ok(Some(expression)) => expression,
            Ok(None) => return BindingPreparation::Pending,
            Err(error) => {
                report(r, ast, node, error);
                return BindingPreparation::Complete;
            }
        };
        let Some(owner) = r.scopes[scope.0 as usize].assoc_type else {
            report(
                r,
                ast,
                node,
                "associated declaration is missing its trait owner",
            );
            return BindingPreparation::Complete;
        };
        let default = type_pool::AssociatedTypeDefault {
            trait_owner: owner,
            name: ast.node(children[0]).str_id,
            expression,
        };
        if let Err(error) = r.type_pool.register_associated_default(default) {
            report(r, ast, node, error.to_string());
        }
    } else {
        let Some(value) = crate::typing::resolve_type_expr_inner(r, ast, children[2]) else {
            return BindingPreparation::Pending;
        };
        if !r.type_pool.is_static_associated_type(value) {
            report(
                r,
                ast,
                node,
                "associated types containing a trait view require a trait return or storage proof contract, which is not supported yet",
            );
            return BindingPreparation::Complete;
        }
        if let Some(&symbol) = r.node_symbols.get(&children[0]) {
            r.symbols[symbol.0 as usize].type_index = value;
        }
        r.node_type_values.insert(node, value);
    }
    BindingPreparation::Complete
}

fn declarations(
    r: &Resolver<'_>,
    owner: TypeIndex,
) -> Vec<(TypeIndex, str_interner::StrId, TypeIndex)> {
    let mut pending = vec![owner];
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    while let Some(owner) = pending.pop() {
        if !seen.insert(owner) {
            continue;
        }
        if let TypeKind::Trait {
            parents,
            assoc_types,
            ..
        } = &r.type_pool.get(owner).kind
        {
            result.extend(
                assoc_types
                    .iter()
                    .map(|&(name, value)| (owner, name, value)),
            );
            pending.extend(parents);
        }
    }
    result
}

pub(crate) fn install(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    implementor: TypeIndex,
    trait_type: TypeIndex,
    scope: Option<u32>,
) {
    if r.type_pool
        .associated_bindings_snapshot()
        .iter()
        .any(|binding| {
            binding.implementor == implementor
                && binding.trait_type == trait_type
                && binding.visible_scope == scope
        })
    {
        return;
    }
    let declared = declarations(r, trait_type);
    let mut names = HashSet::new();
    if declared.iter().any(|(_, name, _)| !names.insert(*name)) {
        report(
            r,
            ast,
            node,
            "ambiguous inherited associated type declarations",
        );
        return;
    }
    let mut values = Vec::new();
    for &member in ast.multi_children(node) {
        let member = if matches!(
            ast.node(member).kind,
            NodeKind::PubDef | NodeKind::PrivateDef
        ) {
            ast.fixed_children(member)[0]
        } else {
            member
        };
        if ast.node(member).kind != NodeKind::AssocDecl {
            continue;
        }
        let inner = ast.fixed_children(member)[0];
        if ast.node(inner).kind != NodeKind::AssocBinding {
            continue;
        }
        let name = ast.node(ast.fixed_children(inner)[0]).str_id;
        let matches: Vec<_> = declared
            .iter()
            .filter(|(_, candidate, _)| *candidate == name)
            .collect();
        if matches.len() != 1 {
            report(
                r,
                ast,
                inner,
                "associated binding does not identify one declaration of the implemented trait",
            );
            continue;
        }
        if let Some(&value) = r.node_type_values.get(&inner) {
            values.push((matches[0].0, name, value));
        }
    }
    let bindings =
        match r
            .type_pool
            .resolve_associated_defaults(implementor, trait_type, scope, &values)
        {
            Ok(bindings) => bindings,
            Err(error) => {
                report(r, ast, node, error.to_string());
                return;
            }
        };
    for binding in bindings {
        if let Err(error) = r.type_pool.register_associated_binding(binding) {
            report(r, ast, node, error.to_string());
        }
    }
}

pub(crate) fn has_declarations(r: &Resolver<'_>, ty: TypeIndex) -> bool {
    !declarations(r, ty).is_empty()
}

pub(crate) fn contains_dynamic_view(r: &Resolver<'_>, ty: TypeIndex, depth: usize) -> bool {
    if depth >= 256 {
        return true;
    }
    let Some(ty) = r.type_pool.canonical_type(ty) else {
        return false;
    };
    match &r.type_pool.get(ty).kind {
        TypeKind::Trait { .. } => has_declarations(r, ty),
        TypeKind::IterationStepTemplate { item: inner }
        | TypeKind::Optional { inner }
        | TypeKind::ErrorQualified { inner, .. }
        | TypeKind::EffectQualified { inner, .. } => contains_dynamic_view(r, *inner, depth + 1),
        TypeKind::Tuple { elements } => elements
            .iter()
            .any(|&ty| contains_dynamic_view(r, ty, depth + 1)),
        TypeKind::Function { params, ret } => params
            .iter()
            .chain(std::iter::once(ret))
            .any(|&ty| contains_dynamic_view(r, ty, depth + 1)),
        _ => false,
    }
}

pub(crate) fn validate_dynamic_functions(r: &Resolver<'_>, ast: &Ast) {
    for symbol in &r.symbols {
        if symbol.def_node.is_null() || ast.node(symbol.def_node).kind != NodeKind::FunctionDef {
            continue;
        }
        if contains_dynamic_view(r, symbol.type_index, 0) {
            report(
                r,
                ast,
                symbol.def_node,
                "dynamic trait function signatures with associated types require an associated return proof contract, which is not supported yet",
            );
        }
    }
}

/// Stage associated implementation identities before body typing needs exact
/// concrete return contracts. Executable methods are filled in the trait pass.
pub(crate) fn stage_implementations(r: &mut Resolver<'_>, ast: &Ast) {
    let mut records = Vec::new();
    for scope in &r.scopes {
        let node = scope.node;
        if node.is_null()
            || !matches!(
                ast.node(node).kind,
                NodeKind::ImplTraitDef | NodeKind::ExtendTraitDef
            )
        {
            continue;
        }
        let Some(implementor) = scope.assoc_type else {
            continue;
        };
        let trait_node = ast.fixed_children(node)[0];
        let Some(trait_type) = r.node_symbols.get(&trait_node).and_then(|symbol| {
            r.type_pool
                .canonical_type(r.symbols[symbol.0 as usize].type_index)
        }) else {
            continue;
        };
        if !has_declarations(r, trait_type) {
            continue;
        }
        let visible_scope = r
            .imports
            .extension_scopes
            .get(&scope.id)
            .map(|scope| scope.0);
        records.push(type_pool::TraitImplRecord {
            implementor,
            trait_type,
            visible_scope,
            methods: Vec::new(),
        });
    }
    for record in records {
        if !r.type_pool.trait_impls_snapshot().iter().any(|existing| {
            existing.implementor == record.implementor
                && existing.trait_type == record.trait_type
                && existing.visible_scope == record.visible_scope
        }) {
            r.type_pool.add_trait_impl(record);
        }
    }
    install_all(r, ast);
}

pub(crate) fn install_all(r: &mut Resolver<'_>, ast: &Ast) {
    let mut rows = Vec::new();
    for scope in &r.scopes {
        let node = scope.node;
        if node.is_null()
            || !matches!(
                ast.node(node).kind,
                NodeKind::ImplTraitDef | NodeKind::ExtendTraitDef
            )
        {
            continue;
        }
        let children = ast.fixed_children(node);
        let owner = r.node_symbols.get(&children[0]).and_then(|id| {
            r.type_pool
                .canonical_type(r.symbols[id.0 as usize].type_index)
        });
        let implementor = r.node_symbols.get(&children[1]).and_then(|id| {
            r.type_pool
                .canonical_type(r.symbols[id.0 as usize].type_index)
        });
        let (Some(owner), Some(implementor)) = (owner, implementor) else {
            continue;
        };
        let visible = r.imports.extension_scopes.get(&scope.id).map(|id| id.0);
        rows.push((node, implementor, owner, visible));
    }
    // Parent binding rows are staged before child rows, independent of source order.
    rows.sort_by_key(|row| {
        let mut pending = vec![row.2];
        let mut seen = HashSet::new();
        while let Some(owner) = pending.pop() {
            if !seen.insert(owner) {
                continue;
            }
            if let TypeKind::Trait { parents, .. } = &r.type_pool.get(owner).kind {
                pending.extend(parents);
            }
        }
        seen.len()
    });
    for (node, implementor, owner, visible) in rows {
        install(r, ast, node, implementor, owner, visible);
    }
}

/// Abstract declaration binders may be reflected only after an exact static
/// implementation has replaced them. They are not runtime Type values.
pub(crate) fn validate_abstract_values(r: &Resolver<'_>, ast: &Ast) {
    let mut pending = vec![ast.root];
    while let Some(node) = pending.pop() {
        if node.is_null() {
            continue;
        }
        if matches!(
            ast.node(node).kind,
            NodeKind::TraitDef | NodeKind::AssocDecl | NodeKind::Typealias
        ) {
            continue;
        }
        if r.node_types.get(&node).copied() == Some(Intrinsic::Type.type_index())
            && r.node_type_values
                .get(&node)
                .is_some_and(|&ty| r.type_pool.contains_associated_type(ty))
        {
            report(
                r,
                ast,
                node,
                "abstract associated declarations cannot be used as runtime Type values",
            );
        }
        pending.extend(ast.fixed_children(node));
        pending.extend(ast.multi_children(node));
    }
    for symbol in &r.symbols {
        if symbol.def_node.is_null() || symbol.type_index == TypeIndex::INVALID {
            continue;
        }
        if matches!(
            ast.node(symbol.def_node).kind,
            NodeKind::FunctionDef | NodeKind::StructDef | NodeKind::EnumDef | NodeKind::Newtype
        ) && r.type_pool.contains_associated_type(symbol.type_index)
        {
            report(
                r,
                ast,
                symbol.def_node,
                "abstract associated types cannot appear in executable signatures or object fields",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};

    fn resolve(source: &str) -> (crate::ResolvedAst, Vec<String>) {
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty());
        let map = SourceMap::new(FilePathMapping::empty());
        let file =
            map.new_source_file(FileName::Custom("associated-type.ns".into()), source.into());
        let diagnostics = DiagnosticContext::new(&map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve(ast, &diagnostics);
        let errors = diagnostics
            .diagnostics()
            .iter()
            .map(|diag| diag.message.clone())
            .collect();
        (resolved, errors)
    }

    #[test]
    fn aliases_preserve_associated_source_paths_without_replacing_explicit_any() {
        let (resolved, errors) = resolve(
            "trait Stream{assoc Item:Type=Any;assoc typealias Output=?Item;fn next(self)->(Output,Any)};struct P{};impl Stream for P{assoc Item:Type=String;fn next(self)->(?String,Any){(null,42)}};fn main(){42}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let signature = resolved
            .type_pool
            .trait_schemas_snapshot()
            .iter()
            .filter(|schema| matches!(resolved.type_pool.get(schema.trait_type).kind, TypeKind::Trait { name, .. } if str_interner::get(name) == "Stream"))
            .flat_map(|schema| &schema.slots)
            .find(|key| key.name == str_interner::intern("next") && key.signature.is_some())
            .unwrap()
            .signature
            .as_ref()
            .unwrap();
        assert_eq!(signature.associated_paths.len(), 1);
        assert_eq!(
            signature.associated_paths[0].path,
            vec![
                type_pool::TraitTypeStep::Return,
                type_pool::TraitTypeStep::TupleElement(0),
                type_pool::TraitTypeStep::OptionalInner
            ]
        );
    }

    #[test]
    fn inherited_associated_values_follow_the_exact_parent_implementation() {
        let (resolved, errors) = resolve(
            "trait Base{assoc Item:Type=Any;fn next(self)->Item};trait Child(Base){fn count(self)->i64};struct P{};impl Child for P{fn count(self)->i64{42}};impl Base for P{assoc Item:Type=String;fn next(self)->String{\"text\"}};fn main(){42}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let bindings = resolved.type_pool.associated_bindings_snapshot();
        assert_eq!(bindings.len(), 2);
        assert!(
            bindings
                .iter()
                .all(|binding| resolved.type_pool.as_intrinsic(binding.value)
                    == Some(Intrinsic::Str))
        );
    }

    #[test]
    fn unrelated_inherited_names_and_invalid_default_bodies_are_rejected() {
        for (source, message) in [
            (
                "trait A{assoc Item:Type=Any};trait B{assoc Item:Type=Any};trait Both(A,B){};struct P{};impl A for P{};impl B for P{};impl Both for P{};fn main(){42}",
                "ambiguous inherited associated",
            ),
            (
                "trait S{assoc Item:Type=Any;derive fn read(self)->Item{42}};struct P{};impl S for P{assoc Item:Type=String};fn main(){42}",
                "type mismatch",
            ),
            (
                "trait S{assoc Item:Type=Item;fn read(self)->Item};struct P{};impl S for P{fn read(self)->i64{42}};fn main(){42}",
                "cyclic associated type default",
            ),
        ] {
            let (_, errors) = resolve(source);
            assert!(
                errors.iter().any(|error| error.contains(message)),
                "{source}: {errors:?}"
            );
        }
    }

    #[test]
    fn concrete_function_bindings_and_unused_dependent_defaults_preserve_their_contracts() {
        let (_, errors) = resolve(
            "trait S{assoc Item:Type=fn(i64)->i64;fn next(self)->Item};struct P{};impl S for P{fn next(self)->fn(i64)->i64{|x|x}};fn main(){42}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        for source in [
            "trait S{assoc Item:Type=Self;fn next(self)->Item};fn main(){42}",
            "trait S{assoc First:Type=Any;assoc Second:Type=First};fn main(){42}",
        ] {
            let (resolved, errors) = resolve(source);
            assert!(errors.is_empty(), "{errors:?}");
            assert!(!resolved.type_pool.associated_defaults_snapshot().is_empty());
        }
        for source in [
            "trait Read{fn value(self)->i64};trait S{assoc Item:Type=Read;fn next(self)->Item};fn main(){42}",
            "trait Read{fn value(self)->i64};trait S{assoc Item:Type=Any;fn next(self)->Item};struct P{};impl S for P{assoc Item:Type=?Read;fn next(self)->?Read{null}};fn main(){42}",
        ] {
            let (_, errors) = resolve(source);
            assert!(
                errors.iter().any(|error| error.contains("proof contract")
                    || error.contains("associated default concrete type")),
                "{source}: {errors:?}"
            );
        }
    }
}
