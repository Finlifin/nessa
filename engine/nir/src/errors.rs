//! Checked Error facts lower to ordinary blocks and authenticated envelope operations.

use ast::NodeIndex;
use resolution::{ErrorConversionPlan, ErrorPatternBranch, ResolvedAst};

use crate::arguments::snapshot;
use crate::builder::FunctionBuilder;
use crate::expr::lower_expr;
use crate::{BlockId, NirExpr, NirStmt, NirValue, Terminator};

fn emit(builder: &mut FunctionBuilder, block: BlockId, expression: NirExpr) -> NirValue {
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(local, expression));
    NirValue::Local(local)
}

pub(crate) fn convert(
    resolved: &ResolvedAst,
    node: NodeIndex,
    value: NirValue,
    plan: &ErrorConversionPlan,
    builder: &mut FunctionBuilder,
    block: BlockId,
) -> NirValue {
    let expression = match plan.kind {
        resolution::ErrorConversionKind::LiftOk
            if resolved
                .type_pool
                .error_shape(plan.target)
                .expect("resolved Error conversion target has a checked shape")
                .is_some() =>
        {
            NirExpr::ErrorOk(value, plan.target)
        }
        resolution::ErrorConversionKind::CheckQualified => NirExpr::TypeAssert(value, plan.target),
        _ => NirExpr::TypeCast(value, plan.target),
    };
    emit(builder, block, expression.in_source_scope(resolved, node))
}

pub(crate) fn conversions(
    resolved: &ResolvedAst,
    node: NodeIndex,
    mut value: NirValue,
    builder: &mut FunctionBuilder,
    block: BlockId,
) -> NirValue {
    for plan in builder.error_conversions(resolved, node) {
        value = convert(resolved, node, value, &plan, builder, block);
    }
    value
}

pub(crate) fn construct(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let plan = builder
        .error_construction(resolved, node)
        .expect("checked Error constructor");
    let value = lower_expr(resolved, plan.operand, builder, block);
    let value = snapshot(value, builder, *block);
    emit(
        builder,
        *block,
        NirExpr::ErrorErr(value, plan.result).in_source_scope(resolved, node),
    )
}

pub(crate) fn propagate(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let plan = builder
        .error_propagation(resolved, node)
        .expect("checked Error propagation");
    let value = lower_expr(resolved, plan.operand, builder, block);
    let value = snapshot(value, builder, *block);
    let success = builder.new_block();
    let error = builder.new_block();
    crate::patterns::test(
        builder,
        *block,
        NirExpr::ErrorIsOk(value).in_source_scope(resolved, node),
        success,
        error,
    );
    let returned = convert(
        resolved,
        node,
        value,
        &plan.error_conversion,
        builder,
        error,
    );
    builder.blocks[error.0 as usize].terminator = Terminator::Return(returned);
    *block = success;
    emit(
        builder,
        success,
        NirExpr::ErrorPayload(value).in_source_scope(resolved, node),
    )
}

pub(crate) fn pattern(
    resolved: &ResolvedAst,
    node: NodeIndex,
    value: NirValue,
    builder: &mut FunctionBuilder,
    block: BlockId,
    success: BlockId,
    failure: BlockId,
) -> bool {
    let Some(plan) = builder.error_pattern(resolved, node) else {
        return false;
    };
    let payload_block = builder.new_block();
    let (yes, no) = if plan.branch == ErrorPatternBranch::Ok {
        (payload_block, failure)
    } else {
        (failure, payload_block)
    };
    crate::patterns::test(
        builder,
        block,
        NirExpr::ErrorIsOk(value).in_source_scope(resolved, node),
        yes,
        no,
    );
    let payload = emit(
        builder,
        payload_block,
        NirExpr::ErrorPayload(value).in_source_scope(resolved, node),
    );
    let mut start = payload_block;
    if let Some(ty) = plan.type_test {
        let checked = builder.new_block();
        crate::patterns::test(
            builder,
            start,
            NirExpr::TypeCheck(payload, ty).in_source_scope(resolved, node),
            checked,
            failure,
        );
        start = checked;
    }
    let child = match resolved.ast.node(node).kind {
        ast::NodeKind::PatternError | ast::NodeKind::PatternErrorOk => {
            resolved.ast.fixed_children(node)[0]
        }
        _ => node,
    };
    crate::patterns::branch_payload(resolved, child, payload, builder, start, success, failure);
    true
}

pub(crate) fn eliminate(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let plan = builder
        .error_elimination(resolved, node)
        .expect("checked Error elimination");
    let value = lower_expr(resolved, plan.operand, builder, block);
    let value = snapshot(value, builder, *block);
    let result = builder.alloc_local();
    let merge = builder.new_block();
    let mut current = *block;
    for arm in plan.arms {
        let children = resolved.ast.fixed_children(arm);
        let mut body = builder.new_block();
        let next = builder.new_block();
        crate::patterns::branch(resolved, children[0], value, builder, current, body, next);
        let output = lower_expr(resolved, children[1], builder, &mut body);
        if matches!(
            builder.blocks[body.0 as usize].terminator,
            Terminator::Unreachable
        ) {
            builder.blocks[body.0 as usize]
                .stmts
                .push(NirStmt::Assign(result, NirExpr::Use(output)));
            builder.blocks[body.0 as usize].terminator = Terminator::Goto(merge);
        }
        current = next;
    }
    let success = builder.new_block();
    let residual = builder.new_block();
    crate::patterns::test(
        builder,
        current,
        NirExpr::ErrorIsOk(value).in_source_scope(resolved, node),
        success,
        residual,
    );
    for (start, conversion, is_success) in [
        (success, plan.implicit_success_conversion, true),
        (residual, plan.residual_conversion, false),
    ] {
        if let Some(conversion) = conversion {
            let input = if is_success {
                emit(
                    builder,
                    start,
                    NirExpr::ErrorPayload(value).in_source_scope(resolved, node),
                )
            } else {
                value
            };
            let output = convert(resolved, node, input, &conversion, builder, start);
            builder.blocks[start.0 as usize]
                .stmts
                .push(NirStmt::Assign(result, NirExpr::Use(output)));
            builder.blocks[start.0 as usize].terminator = Terminator::Goto(merge);
        } else {
            builder.blocks[start.0 as usize].terminator = Terminator::MatchFail;
        }
    }
    *block = merge;
    NirValue::Local(result)
}
