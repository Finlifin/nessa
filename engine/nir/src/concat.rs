//! Concat is a checked source call for static receivers and a dynamic method call for Any.

use ast::NodeIndex;
use resolution::ResolvedAst;

use crate::arguments::snapshot;
use crate::builder::FunctionBuilder;
use crate::expr::lower_expr;
use crate::{BlockId, NirExpr, NirStmt, NirValue};

pub(crate) fn lower(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let children = resolved.ast.fixed_children(node);
    let left = lower_expr(resolved, children[0], builder, block);
    let left = snapshot(left, builder, *block);
    let right = lower_expr(resolved, children[1], builder, block);
    let expression = if let Some(function) = builder.concat_call(resolved, node) {
        let function = builder.func_map[&function];
        NirExpr::Call(function, vec![left, right])
    } else {
        // Resolution only leaves Any receivers for checked runtime dispatch.
        NirExpr::MethodCall(left, str_interner::intern("concat"), vec![right])
    };
    let result = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(result, expression));
    NirValue::Local(result)
}
