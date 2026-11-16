//! Range operators construct the trusted `std.range` structs with i64 bounds.
//!
//! A range expression is checked like a struct construction whose explicit
//! field values are the operator's operands, so later stages reuse the
//! ordinary construction plan and object layout.

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{Intrinsic, TypeIndex};

use crate::resolver::Resolver;
use crate::structs::{StructConstructionPlan, StructFieldBinding, StructFieldValue};

/// The `std.range` struct built by `kind` and the field receiving each
/// operand, in source order.
fn layout(kind: NodeKind) -> Option<(&'static str, &'static [&'static str])> {
    Some(match kind {
        NodeKind::RangeFromTo => ("Range", &["start", "end"]),
        NodeKind::RangeFromToInclusive => ("RangeInclusive", &["start", "end"]),
        NodeKind::RangeFrom => ("RangeFrom", &["start"]),
        NodeKind::RangeTo => ("RangeTo", &["end"]),
        NodeKind::RangeToInclusive => ("RangeToInclusive", &["end"]),
        NodeKind::RangeFull => ("RangeFull", &[]),
        _ => return None,
    })
}

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: String) {
    r.diag_ctx
        .error(message)
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}

pub(crate) fn literal(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) -> Option<TypeIndex> {
    let (name, operand_fields) = layout(ast.node(node).kind)?;
    let operands = ast.fixed_children(node).to_vec();
    let bound = Intrinsic::I64.type_index();
    for &operand in &operands {
        crate::typing::resolve_types_expected(r, ast, operand, Some(bound));
        if let Some(&actual) = r.node_types.get(&operand)
            && r.type_pool.as_intrinsic(actual) == Some(Intrinsic::Usize)
        {
            r.node_coercions.insert(
                operand,
                crate::Coercion {
                    target: bound,
                    kind: crate::CoercionKind::Convert,
                },
            );
        } else {
            crate::typing::check_expected_type(r, ast, operand, bound, "range bound");
        }
    }
    let Some((ty, declaration)) = crate::trusted::std_type(r, "range", name)
        .map(|(ty, symbol)| (ty, symbol.def_node))
        .filter(|&(_, declaration)| ast.node(declaration).kind == NodeKind::StructDef)
    else {
        report(
            r,
            ast,
            node,
            format!("range operators require the standard-library struct `std.range.{name}`"),
        );
        return None;
    };
    let declared = crate::structs::fields(ast, declaration);
    let fields: Option<Vec<_>> = declared
        .iter()
        .map(|&field| {
            let field_name = str_interner::get(ast.node(ast.fixed_children(field)[0]).str_id);
            let source_index = operand_fields.iter().position(|&f| f == field_name)?;
            Some(StructFieldBinding {
                field,
                value: StructFieldValue::Explicit { source_index },
            })
        })
        .collect();
    match fields {
        Some(fields) if fields.len() == operand_fields.len() => {
            r.struct_constructions.insert(
                node,
                StructConstructionPlan {
                    type_index: ty,
                    declaration,
                    fields,
                },
            );
            Some(ty)
        }
        _ => {
            report(
                r,
                ast,
                node,
                format!(
                    "standard-library struct `std.range.{name}` must declare exactly the fields {operand_fields:?}"
                ),
            );
            None
        }
    }
}
