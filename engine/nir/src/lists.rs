//! List literal construction without the fixed call-argument window limit.

use ast::NodeIndex;
use resolution::ResolvedAst;

use crate::arguments::snapshot;
use crate::builder::FunctionBuilder;
use crate::expr::lower_expr;
use crate::{BlockId, NirExpr, NirStmt, NirValue};

pub(crate) fn lower_list(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let elements: Vec<_> = resolved
        .ast
        .multi_children(node)
        .iter()
        .map(|&element| {
            let value = lower_expr(resolved, element, builder, block);
            snapshot(value, builder, *block)
        })
        .collect();
    build_list(&elements, builder, *block)
}

pub(crate) fn build_list(
    elements: &[NirValue],
    builder: &mut FunctionBuilder,
    block: BlockId,
) -> NirValue {
    let local = builder.alloc_local();
    // NewList retains its 12-bit initial-length meaning. Larger literals grow
    // through the checked native operation, rather than truncating the length.
    let length = if elements.len() < (1 << 12) {
        elements.len() as u16
    } else {
        0
    };
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(local, NirExpr::NewList(length)));
    let list = NirValue::Local(local);
    for (index, &value) in elements.iter().enumerate() {
        let statement = if length == 0 {
            let result = builder.alloc_local();
            NirStmt::Assign(
                result,
                NirExpr::CallBuiltin(runtime::ids::LIST_PUSH, vec![list, value]),
            )
        } else {
            NirStmt::StoreIndex(list, NirValue::ConstUInt(index as u64), value)
        };
        builder.blocks[block.0 as usize].stmts.push(statement);
    }
    list
}
