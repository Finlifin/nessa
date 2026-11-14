//! Materialize checked struct fields in their declared runtime layout.

use ast::{NodeIndex, NodeKind};
use resolution::{ResolvedAst, StructFieldValue};

use crate::arguments::snapshot;
use crate::builder::FunctionBuilder;
use crate::expr::lower_expr;
use crate::{BlockId, NirExpr, NirStmt, NirValue};

pub(crate) fn lower_construction(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    // Driver stops on resolution errors; every admitted construction has a
    // complete field binding plan, including selected declaration defaults.
    let plan = builder
        .struct_plan(resolved, node)
        .expect("resolved struct construction has a field binding plan");
    let explicit: Vec<_> = explicit_sources(resolved, node)
        .into_iter()
        .map(|expression| {
            let value = lower_expr(resolved, expression, builder, block);
            snapshot(value, builder, *block)
        })
        .collect();
    let fields = plan
        .fields
        .iter()
        .map(|field| match field.value {
            StructFieldValue::Explicit { source_index } => explicit[source_index],
            StructFieldValue::Default { expression } => {
                let value = lower_expr(resolved, expression, builder, block);
                snapshot(value, builder, *block)
            }
        })
        .collect();
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
        local,
        NirExpr::NewObject(plan.type_index, fields),
    ));
    NirValue::Local(local)
}

/// Explicit field values in source order. Range operators carry their bounds
/// as fixed operands; `Type { ... }` carries properties or shorthand names.
fn explicit_sources(resolved: &ResolvedAst, node: NodeIndex) -> Vec<NodeIndex> {
    let ast = &resolved.ast;
    if ast.node(node).kind != NodeKind::ExtendedCall {
        return ast.fixed_children(node).to_vec();
    }
    ast.multi_children(node)
        .iter()
        .map(|&argument| {
            if ast.node(argument).kind == NodeKind::Property {
                ast.fixed_children(argument)[1]
            } else {
                argument
            }
        })
        .collect()
}
