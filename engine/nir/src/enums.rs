//! Enum construction preserves source evaluation order and nominal identity.

use ast::{NodeIndex, NodeKind};
use resolution::ResolvedAst;

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
    let plan = builder
        .enum_plan(resolved, node)
        .expect("checked enum construction");
    if plan.argument_type == type_pool::Intrinsic::Unit.type_index() {
        return NirValue::ConstEnum(plan.type_index, plan.variant_tag);
    }
    let values: Vec<_> = if let Some(fields) = &plan.source_fields {
        let explicit: Vec<_> = resolved
            .ast
            .multi_children(node)
            .iter()
            .map(|&argument| {
                let value_node = if resolved.ast.node(argument).kind == NodeKind::NamedArg {
                    resolved.ast.fixed_children(argument)[1]
                } else {
                    argument
                };
                let value = lower_expr(resolved, value_node, builder, block);
                snapshot(value, builder, *block)
            })
            .collect();
        fields.iter().map(|&index| explicit[index]).collect()
    } else {
        crate::arguments::lower_arguments(resolved, node, builder, block)
    };
    let arguments = builder.alloc_local();
    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
        arguments,
        NirExpr::NewObject(plan.argument_type, values),
    ));
    let result = builder.alloc_local();
    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
        result,
        NirExpr::NewEnum(
            plan.type_index,
            plan.variant_tag,
            NirValue::Local(arguments),
        ),
    ));
    NirValue::Local(result)
}
