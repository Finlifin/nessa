//! Optional values share the existing null-or-payload runtime representation.

use ast::{NodeIndex, NodeKind};
use resolution::ResolvedAst;
use type_pool::TypeKind;

use crate::arguments::snapshot;
use crate::builder::FunctionBuilder;
use crate::expr::lower_expr;
use crate::{BinOp, BlockId, NirExpr, NirStmt, NirValue, Terminator};

fn split(value: NirValue, builder: &mut FunctionBuilder, block: BlockId) -> (BlockId, BlockId) {
    let null = builder.new_block();
    let payload = builder.new_block();
    crate::patterns::test(
        builder,
        block,
        NirExpr::BinOp(BinOp::Eq, value, NirValue::Null),
        null,
        payload,
    );
    (null, payload)
}

pub(crate) fn propagate(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let value = lower_expr(
        resolved,
        resolved.ast.fixed_children(node)[0],
        builder,
        block,
    );
    let value = snapshot(value, builder, *block);
    let (null, payload) = split(value, builder, *block);
    builder.blocks[null.0 as usize].terminator = Terminator::Return(NirValue::Null);
    *block = payload;
    value
}

pub(crate) fn unwrap_call(
    resolved: &ResolvedAst,
    call: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> Option<NirValue> {
    let callee = resolved.ast.fixed_children(call)[0];
    if resolved.ast.node(callee).kind != NodeKind::Projection {
        return None;
    }
    let children = resolved.ast.fixed_children(callee);
    if str_interner::get(resolved.ast.node(children[1]).str_id) != "unwrap" {
        return None;
    }
    let receiver = builder.node_type(resolved, children[0])?;
    let receiver = resolved.type_pool.canonical_type(receiver)?;
    if !matches!(
        resolved.type_pool.get(receiver).kind,
        TypeKind::Optional { .. }
    ) {
        return None;
    }
    let value = lower_expr(resolved, children[0], builder, block);
    let value = snapshot(value, builder, *block);
    let (null, payload) = split(value, builder, *block);
    let result = builder.alloc_local();
    builder.blocks[null.0 as usize].stmts.push(NirStmt::Assign(
        result,
        NirExpr::CallBuiltin(
            runtime::ids::PANIC,
            vec![NirValue::ConstStr(str_interner::intern(
                "unwrap called on null",
            ))],
        ),
    ));
    builder.blocks[null.0 as usize].terminator = Terminator::Return(NirValue::Unit);
    *block = payload;
    Some(value)
}

pub(crate) fn some_pattern(
    resolved: &ResolvedAst,
    pattern: NodeIndex,
    value: NirValue,
    builder: &mut FunctionBuilder,
    block: BlockId,
    success: BlockId,
    failure: BlockId,
) {
    let payload = builder.new_block();
    crate::patterns::test(
        builder,
        block,
        NirExpr::BinOp(BinOp::Ne, value, NirValue::Null),
        payload,
        failure,
    );
    crate::patterns::branch(
        resolved,
        resolved.ast.fixed_children(pattern)[0],
        value,
        builder,
        payload,
        success,
        failure,
    );
}
