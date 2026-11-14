//! `==`/`!=` whose operand types leave equality to runtime values: Any
//! operands and tuples compare through the trusted `std` equality function.

use ast::{Ast, NodeIndex};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::resolver::Resolver;

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: String) {
    r.diag_ctx
        .error(message)
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}

fn is_any(r: &Resolver<'_>, ty: TypeIndex) -> bool {
    r.type_pool.as_intrinsic(ty) == Some(Intrinsic::Any)
}

fn tuple_elements(r: &Resolver<'_>, ty: TypeIndex) -> Option<Vec<TypeIndex>> {
    let ty = r.type_pool.canonical_type(ty)?;
    match &r.type_pool.get(ty).kind {
        TypeKind::Tuple { elements } => Some(elements.clone()),
        _ => None,
    }
}

fn numeric(r: &Resolver<'_>, ty: TypeIndex) -> bool {
    r.type_pool
        .as_intrinsic(ty)
        .is_some_and(Intrinsic::is_numeric)
}

/// Tuple elements are checked pairwise by their static types; Any elements
/// and numeric kinds compare at runtime.
fn comparable(r: &Resolver<'_>, left: TypeIndex, right: TypeIndex, depth: usize) -> bool {
    if depth >= 256 {
        return false;
    }
    if is_any(r, left) || is_any(r, right) || (numeric(r, left) && numeric(r, right)) {
        return true;
    }
    match (tuple_elements(r, left), tuple_elements(r, right)) {
        (Some(left), Some(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(&right)
                    .all(|(&left, &right)| comparable(r, left, right, depth + 1))
        }
        (None, None) => {
            r.type_pool.is_gradually_consistent(left, right)
                || r.type_pool.is_subtype(left, right)
                || r.type_pool.is_subtype(right, left)
        }
        _ => false,
    }
}

pub(crate) fn plan(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) {
    let children = ast.fixed_children(node);
    let (Some(&left), Some(&right)) = (
        r.node_types.get(&children[0]),
        r.node_types.get(&children[1]),
    ) else {
        return;
    };
    let tuples = tuple_elements(r, left).is_some() && tuple_elements(r, right).is_some();
    if !(is_any(r, left) || is_any(r, right) || tuples) {
        return;
    }
    if tuples && !comparable(r, left, right, 0) {
        let name = |ty| {
            r.type_pool
                .display_name(ty)
                .unwrap_or_else(|| "<invalid>".into())
        };
        report(
            r,
            ast,
            node,
            format!(
                "tuples of types `{}` and `{}` cannot be compared: arity or element types differ",
                name(left),
                name(right)
            ),
        );
        return;
    }
    match crate::trusted::std_function(r, "collections", "dynamic_equal") {
        Some(function) => {
            r.equality_calls.insert(node, function);
        }
        None => report(
            r,
            ast,
            node,
            "equality of Any or tuple values requires `std.collections`".into(),
        ),
    }
}
