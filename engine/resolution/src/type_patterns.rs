//! `name: Type` tests the runtime type of a dynamic input and binds it as Type.

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::resolver::Resolver;

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: String) {
    r.diag_ctx
        .error(message)
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}

/// Only Any and trait-typed inputs leave the concrete runtime type open; for
/// any other input the static type already decides whether the test passes.
fn is_dynamic(r: &Resolver<'_>, input: TypeIndex) -> bool {
    r.type_pool.as_intrinsic(input) == Some(Intrinsic::Any)
        || r.type_pool
            .canonical_type(input)
            .is_some_and(|ty| matches!(r.type_pool.get(ty).kind, TypeKind::Trait { .. }))
}

pub(crate) fn resolve(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex, input: TypeIndex) {
    let children = ast.fixed_children(node);
    let (binder, type_expr) = (children[0], children[1]);
    let Some(target) = crate::typing::resolve_type_expr(r, ast, type_expr) else {
        if type_expr.is_null() {
            report(r, ast, node, "typed pattern requires a valid type".into());
        }
        return;
    };
    r.node_types.insert(type_expr, target);
    if !r.annotation_typed_patterns.contains(&node) && !is_dynamic(r, input) {
        let input_name = r
            .type_pool
            .display_name(input)
            .unwrap_or_else(|| "<invalid>".into());
        report(
            r,
            ast,
            node,
            format!(
                "typed patterns test Any or trait-typed inputs; `{input_name}` is already known statically"
            ),
        );
    }
    if crate::associated_types::contains_dynamic_view(r, target, 0) {
        report(
            r,
            ast,
            type_expr,
            "dynamic trait views with associated types are not supported; use a concrete implementation".into(),
        );
    }
    if ast.node(binder).kind == NodeKind::Id && crate::enums::descriptor(r, ast, binder).is_none() {
        r.node_types.insert(binder, target);
        if let Some(&symbol) = r.node_symbols.get(&binder) {
            r.symbols[symbol.0 as usize].type_index = target;
        }
    } else {
        crate::enums::pattern(r, ast, binder, Some(target));
    }
}
