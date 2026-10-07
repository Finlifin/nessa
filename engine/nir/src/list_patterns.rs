//! Match stable List slots; referenced element objects retain shallow sharing.

use ast::{NodeIndex, NodeKind};
use resolution::ResolvedAst;

use crate::builder::FunctionBuilder;
use crate::{BinOp, BlockId, NirExpr, NirStmt, NirValue, Terminator};

fn assign(builder: &mut FunctionBuilder, block: BlockId, expression: NirExpr) -> NirValue {
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(local, expression));
    NirValue::Local(local)
}

/// Copy [start, end) through existing checked natives. The cursor lives in the
/// frame rather than a heap iterator, so continuation forks keep their progress.
fn copy_range(
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
    source: NirValue,
    start: NirValue,
    end: NirValue,
) -> NirValue {
    let list = assign(builder, *block, NirExpr::NewList(0));
    let cursor = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(cursor, NirExpr::Use(start)));
    let check = builder.new_block();
    let copy = builder.new_block();
    let done = builder.new_block();
    builder.blocks[block.0 as usize].terminator = Terminator::Goto(check);
    crate::patterns::test(
        builder,
        check,
        NirExpr::BinOp(BinOp::Lt, NirValue::Local(cursor), end),
        copy,
        done,
    );
    let element = assign(
        builder,
        copy,
        NirExpr::IndexAccess(source, NirValue::Local(cursor)),
    );
    assign(
        builder,
        copy,
        NirExpr::CallBuiltin(runtime::ids::LIST_PUSH, vec![list, element]),
    );
    builder.blocks[copy.0 as usize].stmts.push(NirStmt::Assign(
        cursor,
        NirExpr::BinOp(BinOp::Add, NirValue::Local(cursor), NirValue::ConstInt(1)),
    ));
    builder.blocks[copy.0 as usize].terminator = Terminator::Goto(check);
    *block = done;
    list
}

pub(crate) fn branch(
    resolved: &ResolvedAst,
    pattern: NodeIndex,
    value: NirValue,
    builder: &mut FunctionBuilder,
    block: BlockId,
    success: BlockId,
    failure: BlockId,
) {
    let list_type = resolved
        .type_pool
        .list_type()
        .expect("checked List metadata");
    let length_check = builder.new_block();
    crate::patterns::test(
        builder,
        block,
        NirExpr::TypeCheck(value, list_type).in_source_scope(resolved, pattern),
        length_check,
        failure,
    );
    let length = assign(
        builder,
        length_check,
        NirExpr::CallBuiltin(runtime::ids::LIST_LEN, vec![value]),
    );
    let fields = resolved.ast.multi_children(pattern);
    let rest = fields
        .iter()
        .position(|&field| resolved.ast.node(field).kind == NodeKind::PatternRestBind);
    let fixed_count = fields.len() - usize::from(rest.is_some());
    let mut current = builder.new_block();
    crate::patterns::test(
        builder,
        length_check,
        NirExpr::BinOp(
            if rest.is_some() { BinOp::Ge } else { BinOp::Eq },
            length,
            NirValue::ConstUInt(fixed_count as u64),
        ),
        current,
        failure,
    );
    if fields.is_empty() {
        builder.blocks[current.0 as usize].terminator = Terminator::Goto(success);
        return;
    }
    // Freeze all slots before any field guard can shrink, extend, or overwrite
    // the original. Each later arm creates its own snapshot of the then-current
    // source. The alias of the whole input remains the original wrapper.
    let snapshot = copy_range(builder, &mut current, value, NirValue::ConstInt(0), length);
    for (index, &field) in fields.iter().enumerate() {
        if Some(index) == rest {
            let end = assign(
                builder,
                current,
                NirExpr::BinOp(
                    BinOp::Sub,
                    length,
                    NirValue::ConstUInt((fields.len() - index - 1) as u64),
                ),
            );
            let tail = copy_range(
                builder,
                &mut current,
                snapshot,
                NirValue::ConstUInt(index as u64),
                end,
            );
            crate::tuples::bind_pattern(
                resolved,
                resolved.ast.fixed_children(field)[0],
                tail,
                builder,
                current,
            );
        } else {
            let offset = if rest.is_some_and(|rest| index > rest) {
                assign(
                    builder,
                    current,
                    NirExpr::BinOp(
                        BinOp::Sub,
                        length,
                        NirValue::ConstUInt((fields.len() - index) as u64),
                    ),
                )
            } else {
                NirValue::ConstUInt(index as u64)
            };
            let element = assign(builder, current, NirExpr::IndexAccess(snapshot, offset));
            let next = builder.new_block();
            crate::patterns::branch(resolved, field, element, builder, current, next, failure);
            current = next;
        }
    }
    builder.blocks[current.0 as usize].terminator = Terminator::Goto(success);
}
