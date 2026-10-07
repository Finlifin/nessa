//! Compile-time type constructors identified by their resolved binding.

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::TypeIndex;

use crate::{SymbolKind, resolver::Resolver, typing};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeFactory {
    IterationStep,
}

pub(crate) fn identity(r: &Resolver<'_>, node: NodeIndex) -> Option<TypeFactory> {
    match r.symbols.get(r.node_symbols.get(&node)?.0 as usize)?.kind {
        SymbolKind::TypeFactory(factory) => Some(factory),
        _ => None,
    }
}

/// None means an ordinary call; errors belong to a recognized factory application.
pub(crate) fn application(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
) -> Option<Result<TypeIndex, String>> {
    if !matches!(ast.node(node).kind, NodeKind::Call | NodeKind::PatternCall) {
        return None;
    }
    let factory = identity(r, ast.fixed_children(node)[0])?;
    Some((|| {
        let [item] = ast.multi_children(node) else {
            return Err("IterationStep requires exactly one positional type argument".into());
        };
        if ast.node(*item).kind == NodeKind::NamedArg {
            return Err("IterationStep requires exactly one positional type argument".into());
        }
        let item = typing::resolve_type_expr_inner(r, ast, *item).ok_or_else(|| {
            "IterationStep requires a statically known concrete Item type".to_string()
        })?;
        match factory {
            TypeFactory::IterationStep => {
                let source_self = r
                    .node_scopes
                    .get(&node)
                    .and_then(|&scope| crate::associated::enclosing_type(r, scope))
                    .filter(|&owner| {
                        matches!(
                            r.type_pool.get(owner).kind,
                            type_pool::TypeKind::Trait { .. }
                        )
                    })
                    .is_some_and(|owner| {
                        crate::trait_signatures::annotation_self_paths(
                            r,
                            ast,
                            ast.multi_children(node)[0],
                            owner,
                        )
                        .is_ok_and(|paths| !paths.is_empty())
                    });
                if r.type_pool.contains_associated_type(item)
                    || source_self && !r.type_pool.is_static_associated_type(item)
                {
                    r.type_pool.intern_iteration_step_template(item)
                } else {
                    r.type_pool.intern_iteration_step(item)
                }
                .map_err(|error| error.to_string())
            }
        }
    })())
}

/// Name binding may precede associated binder registration. Only the known
/// factory's two variants are deferred; typing must later produce a concrete plan.
pub(crate) fn deferred_variant(
    r: &Resolver<'_>,
    ast: &Ast,
    mut owner: NodeIndex,
    member: NodeIndex,
) -> bool {
    if !matches!(
        str_interner::get(ast.node(member).str_id).as_str(),
        "done" | "yielded"
    ) {
        return false;
    }
    for _ in 0..r.symbols.len() {
        if matches!(ast.node(owner).kind, NodeKind::Call | NodeKind::PatternCall) {
            return identity(r, ast.fixed_children(owner)[0]).is_some();
        }
        let Some(symbol) = r
            .node_symbols
            .get(&owner)
            .map(|symbol| &r.symbols[symbol.0 as usize])
        else {
            return false;
        };
        if symbol.def_node.is_null() || ast.node(symbol.def_node).kind != NodeKind::Typealias {
            return false;
        }
        owner = ast.fixed_children(symbol.def_node)[1];
    }
    false
}

pub(crate) fn bare_value(r: &Resolver<'_>, ast: &Ast, node: NodeIndex) -> bool {
    if identity(r, node).is_none() {
        return false;
    }
    r.diag_ctx
        .error(
            "a type factory requires a type argument and cannot be used as a runtime value".into(),
        )
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
    true
}
