//! `==`/`!=` planned for runtime-typed operands. The trusted std function
//! decides native, tuple and cross-type comparisons; when it returns `null`
//! both operands share a runtime type, and the Eq or PartialEq implementation
//! visible at the comparison site decides. A missing implementation fails the
//! runtime trait proof with a type error.

use ast::{NodeIndex, NodeKind};
use resolution::{ResolvedAst, SymbolId};
use type_pool::Intrinsic;

use crate::builder::FunctionBuilder;
use crate::trait_dispatch::TraitMethodTarget;
use crate::{BlockId, NirExpr, NirLocal, NirStmt, NirValue, Terminator, UnaryOp};

fn assign(builder: &mut FunctionBuilder, block: BlockId, local: NirLocal, expression: NirExpr) {
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(local, expression));
}

pub(crate) fn lower(
    resolved: &ResolvedAst,
    node: NodeIndex,
    function: SymbolId,
    lhs: NirValue,
    rhs: NirValue,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let scope = resolved.node_scopes[&node].0;
    let well_known = &resolved.type_pool.well_known;
    let (total, partial) = (well_known.eq, well_known.partial_eq);
    let lhs = crate::arguments::snapshot(lhs, builder, *block);
    let rhs = crate::arguments::snapshot(rhs, builder, *block);

    let decided = builder.alloc_local();
    let function = builder.func_map[&function];
    assign(
        builder,
        *block,
        decided,
        NirExpr::Call(function, vec![lhs, rhs]),
    );
    let is_decided = builder.alloc_local();
    assign(
        builder,
        *block,
        is_decided,
        NirExpr::TypeCheck(NirValue::Local(decided), Intrinsic::Bool.type_index()).in_scope(scope),
    );

    let equal = builder.alloc_local();
    let known = builder.new_block();
    let by_trait = builder.new_block();
    let by_total = builder.new_block();
    let by_partial = builder.new_block();
    let merge = builder.new_block();
    builder.blocks[block.0 as usize].terminator =
        Terminator::Branch(NirValue::Local(is_decided), known, by_trait);

    assign(
        builder,
        known,
        equal,
        NirExpr::TypeAssert(NirValue::Local(decided), Intrinsic::Bool.type_index()).in_scope(scope),
    );
    builder.blocks[known.0 as usize].terminator = Terminator::Goto(merge);

    let is_total = builder.alloc_local();
    assign(
        builder,
        by_trait,
        is_total,
        NirExpr::TypeCheck(lhs, total).in_scope(scope),
    );
    builder.blocks[by_trait.0 as usize].terminator =
        Terminator::Branch(NirValue::Local(is_total), by_total, by_partial);

    for (target, view) in [(by_total, total), (by_partial, partial)] {
        let expression = crate::trait_dispatch::call(
            resolved,
            node,
            TraitMethodTarget {
                owner: view,
                view,
                name: "eq",
            },
            lhs,
            vec![rhs],
            builder,
            target,
        );
        assign(builder, target, equal, expression);
        builder.blocks[target.0 as usize].terminator = Terminator::Goto(merge);
    }

    *block = merge;
    if resolved.ast.node(node).kind != NodeKind::BoolNotEq {
        return NirValue::Local(equal);
    }
    let unequal = builder.alloc_local();
    assign(
        builder,
        merge,
        unequal,
        NirExpr::UnaryOp(UnaryOp::Not, NirValue::Local(equal)),
    );
    NirValue::Local(unequal)
}
