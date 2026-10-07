//! Tuple values and recursive bindings share the checked object field layout.

use ast::{NodeIndex, NodeKind};
use resolution::ResolvedAst;

use crate::arguments::snapshot;
use crate::builder::FunctionBuilder;
use crate::expr::lower_expr;
use crate::{BlockId, NirExpr, NirStmt, NirValue};

pub(crate) fn lower_tuple(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let elements = resolved.ast.multi_children(node);
    if elements.is_empty() {
        return NirValue::Unit;
    }
    let fields = elements
        .iter()
        .map(|&element| {
            let value = lower_expr(resolved, element, builder, block);
            snapshot(value, builder, *block)
        })
        .collect();
    let ty = builder
        .node_type(resolved, node)
        .expect("checked tuple type");
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(local, NirExpr::NewObject(ty, fields)));
    NirValue::Local(local)
}

/// The resolver checks tuple shape and annotates every leaf before lowering.
/// Values are already evaluated; projections never evaluate the source again.
pub(crate) fn bind_pattern(
    resolved: &ResolvedAst,
    pattern: NodeIndex,
    value: NirValue,
    builder: &mut FunctionBuilder,
    block: BlockId,
) {
    match resolved.ast.node(pattern).kind {
        NodeKind::PatternTuple => {
            let value = snapshot(value, builder, block);
            for (index, &element) in resolved.ast.multi_children(pattern).iter().enumerate() {
                let local = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    local,
                    NirExpr::FieldAccess(value, index as u32).in_source_scope(resolved, pattern),
                ));
                bind_pattern(resolved, element, NirValue::Local(local), builder, block);
            }
        }
        NodeKind::Id => {
            if let Some(symbol) = builder.node_symbol(resolved, pattern) {
                let local = builder.local_for_symbol(symbol);
                crate::trait_parameters::bind_local_type(
                    resolved,
                    local,
                    builder.symbol_type(resolved, symbol),
                    builder,
                );
                crate::trait_parameters::copy_proof(local, value, builder, block);
                builder.blocks[block.0 as usize]
                    .stmts
                    .push(NirStmt::Assign(local, NirExpr::Use(value)));
            }
        }
        NodeKind::Underscore => {}
        _ => unreachable!("resolver admits only irrefutable tuple bindings"),
    }
}
