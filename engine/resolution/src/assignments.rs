//! Assignment targets must name storage that lowering can write.

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::TypeIndex;

use crate::resolver::Resolver;

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: impl Into<String>) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}

/// Check a variable or field target after it has been typed. Call targets are
/// checked by their index or update plans instead.
pub(crate) fn check_place(r: &Resolver<'_>, ast: &Ast, target: NodeIndex, context: &str) {
    let kind = ast.node(target).kind;
    if r.node_symbols.contains_key(&target) || r.node_field_indices.contains_key(&target) {
        return;
    }
    if kind == NodeKind::Projection {
        let member = ast.fixed_children(target)[1];
        if r.node_symbols.contains_key(&member) {
            return;
        }
        report(
            r,
            ast,
            target,
            format!(
                "{context} to `{}` requires a statically known struct field",
                str_interner::get(ast.node(member).str_id)
            ),
        );
    } else if kind != NodeKind::Id {
        // Unresolved identifiers already carry a name diagnostic.
        report(
            r,
            ast,
            target,
            format!("{context} target must be a variable, field, or index"),
        );
    }
}

/// Type `receiver(key)` as a compound-assignment target. Only List and Map
/// indexing has a single read/write ABI that can share the evaluated key.
pub(crate) fn compound_index(
    r: &mut Resolver<'_>,
    ast: &Ast,
    call: NodeIndex,
) -> Option<TypeIndex> {
    let receiver = ast.fixed_children(call)[0];
    crate::typing::resolve_types(r, ast, receiver);
    let kind = r
        .node_types
        .get(&receiver)
        .and_then(|&ty| crate::collections::CollectionKind::from_type(&r.type_pool, ty));
    let Some(kind) = kind else {
        for &argument in ast.multi_children(call) {
            crate::typing::resolve_types(r, ast, argument);
        }
        report(
            r,
            ast,
            call,
            "compound assignment through a call target requires a statically known List or Map receiver; write `target(key) = target(key) op value` for other receivers",
        );
        return None;
    };
    let element = crate::collections::index_call(r, ast, call, kind);
    r.node_types.insert(call, element);
    Some(element)
}
