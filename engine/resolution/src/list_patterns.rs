//! List elements are Any; a named remainder is a fresh List of those elements.

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{Intrinsic, TypeIndex};

use crate::resolver::Resolver;

pub(crate) fn resolve(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex, expected: TypeIndex) {
    let report = |r: &Resolver<'_>, child: NodeIndex, message: &str| {
        r.diag_ctx
            .error(message.into())
            .with_primary_span(ast.node(child).span)
            .emit(r.diag_ctx);
    };
    let Some(list) = r.type_pool.list_type() else {
        report(r, node, "List patterns require List type metadata");
        return;
    };
    if !crate::lists::is_list(r, expected) && expected != Intrinsic::Any.type_index() {
        report(r, node, "List pattern requires List or Any input");
    }
    let mut has_rest = false;
    for &child in ast.multi_children(node) {
        if ast.node(child).kind == NodeKind::PatternRestBind {
            if has_rest {
                report(r, child, "List pattern permits only one rest binding");
            }
            has_rest = true;
            let binding = ast.fixed_children(child)[0];
            if binding.is_null() || ast.node(binding).kind != NodeKind::Id {
                report(r, child, "List rest requires an identifier binding");
                continue;
            }
            r.node_types.insert(child, list);
            crate::enums::pattern(r, ast, binding, Some(list));
            if r.enum_variants.contains_key(&binding) {
                report(
                    r,
                    binding,
                    "List rest requires an identifier binding, not an enum variant",
                );
            }
        } else {
            crate::enums::pattern(r, ast, child, Some(Intrinsic::Any.type_index()));
        }
    }
}
