//! Pattern tests branch before extraction and evaluate the scrutinee once.

use ast::{NodeIndex, NodeKind};
use resolution::ResolvedAst;

use crate::arguments::snapshot;
use crate::builder::FunctionBuilder;
use crate::expr::lower_expr;
use crate::{BinOp, BlockId, NirExpr, NirStmt, NirValue, Terminator};

pub(crate) fn test(
    builder: &mut FunctionBuilder,
    block: BlockId,
    expression: NirExpr,
    success: BlockId,
    failure: BlockId,
) {
    let result = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(result, expression));
    builder.blocks[block.0 as usize].terminator =
        Terminator::Branch(NirValue::Local(result), success, failure);
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
    if crate::errors::pattern(resolved, pattern, value, builder, block, success, failure) {
        return;
    }
    branch_payload(resolved, pattern, value, builder, block, success, failure);
}

pub(crate) fn branch_payload(
    resolved: &ResolvedAst,
    pattern: NodeIndex,
    value: NirValue,
    builder: &mut FunctionBuilder,
    block: BlockId,
    success: BlockId,
    failure: BlockId,
) {
    let ast = &resolved.ast;
    if matches!(
        ast.node(pattern).kind,
        NodeKind::PatternError | NodeKind::PatternErrorOk
    ) && crate::errors::pattern(resolved, pattern, value, builder, block, success, failure)
    {
        return;
    }
    if let Some(variant) = builder.enum_variant(resolved, pattern) {
        let payload = builder.new_block();
        test(
            builder,
            block,
            NirExpr::EnumIs(value, variant.type_index, variant.variant_tag),
            payload,
            failure,
        );
        let patterns = if ast.node(pattern).kind == NodeKind::PatternCall {
            ast.multi_children(pattern)
        } else {
            &[]
        };
        let mut current = payload;
        for (index, &field) in patterns.iter().enumerate() {
            let local = builder.alloc_local();
            builder.blocks[current.0 as usize]
                .stmts
                .push(NirStmt::Assign(
                    local,
                    NirExpr::EnumField(value, index as u32).in_source_scope(resolved, pattern),
                ));
            let next = builder.new_block();
            branch_payload(
                resolved,
                field,
                NirValue::Local(local),
                builder,
                current,
                next,
                failure,
            );
            current = next;
        }
        builder.blocks[current.0 as usize].terminator = Terminator::Goto(success);
        return;
    }
    match ast.node(pattern).kind {
        NodeKind::PatternTypeFamily => {
            test(
                builder,
                block,
                NirExpr::TypeCheck(
                    value,
                    builder
                        .node_type(resolved, pattern)
                        .expect("checked family pattern"),
                )
                .in_source_scope(resolved, pattern),
                success,
                failure,
            );
        }
        NodeKind::Underscore => {
            builder.blocks[block.0 as usize].terminator = Terminator::Goto(success);
        }
        NodeKind::Id => {
            crate::tuples::bind_pattern(resolved, pattern, value, builder, block);
            builder.blocks[block.0 as usize].terminator = Terminator::Goto(success);
        }
        NodeKind::PatternOptionSome => crate::optional::some_pattern(
            resolved, pattern, value, builder, block, success, failure,
        ),
        NodeKind::PatternList => {
            crate::list_patterns::branch(
                resolved, pattern, value, builder, block, success, failure,
            );
        }
        NodeKind::PatternTuple => {
            let payload = builder.new_block();
            test(
                builder,
                block,
                NirExpr::TypeCheck(
                    value,
                    builder
                        .node_type(resolved, pattern)
                        .expect("checked pattern type"),
                )
                .in_source_scope(resolved, pattern),
                payload,
                failure,
            );
            let mut current = payload;
            for (index, &field) in ast.multi_children(pattern).iter().enumerate() {
                let local = builder.alloc_local();
                builder.blocks[current.0 as usize]
                    .stmts
                    .push(NirStmt::Assign(
                        local,
                        NirExpr::FieldAccess(value, index as u32)
                            .in_source_scope(resolved, pattern),
                    ));
                let next = builder.new_block();
                branch_payload(
                    resolved,
                    field,
                    NirValue::Local(local),
                    builder,
                    current,
                    next,
                    failure,
                );
                current = next;
            }
            builder.blocks[current.0 as usize].terminator = Terminator::Goto(success);
        }
        NodeKind::PatternAndIs => {
            let children = ast.fixed_children(pattern);
            let mut computed = builder.new_block();
            branch_payload(
                resolved,
                children[0],
                value,
                builder,
                block,
                computed,
                failure,
            );
            let expression = lower_expr(resolved, children[1], builder, &mut computed);
            let expression = snapshot(expression, builder, computed);
            branch_payload(
                resolved,
                children[2],
                expression,
                builder,
                computed,
                success,
                failure,
            );
        }
        NodeKind::PatternNot => {
            branch_payload(
                resolved,
                ast.fixed_children(pattern)[0],
                value,
                builder,
                block,
                failure,
                success,
            );
        }
        NodeKind::PatternOr => {
            let children = ast.fixed_children(pattern);
            let alternative = builder.new_block();
            branch_payload(
                resolved,
                children[0],
                value,
                builder,
                block,
                success,
                alternative,
            );
            branch_payload(
                resolved,
                children[1],
                value,
                builder,
                alternative,
                success,
                failure,
            );
        }
        NodeKind::PatternAsBind => {
            let children = ast.fixed_children(pattern);
            let binding = builder.new_block();
            branch_payload(
                resolved,
                children[0],
                value,
                builder,
                block,
                binding,
                failure,
            );
            crate::tuples::bind_pattern(resolved, children[1], value, builder, binding);
            builder.blocks[binding.0 as usize].terminator = Terminator::Goto(success);
        }
        NodeKind::PatternIfGuard => {
            let children = ast.fixed_children(pattern);
            let mut guard = builder.new_block();
            branch_payload(resolved, children[0], value, builder, block, guard, failure);
            let condition = lower_expr(resolved, children[1], builder, &mut guard);
            builder.blocks[guard.0 as usize].terminator =
                Terminator::Branch(condition, success, failure);
        }
        _ => {
            let mut current = block;
            let literal =
                crate::expr::lower_pattern_value(resolved, ast, pattern, builder, &mut current);
            test(
                builder,
                current,
                NirExpr::BinOp(BinOp::Eq, value, literal),
                success,
                failure,
            );
        }
    }
}

pub(crate) fn lower_match(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let ast = &resolved.ast;
    let scrutinee = lower_expr(resolved, ast.fixed_children(node)[0], builder, block);
    let scrutinee = snapshot(scrutinee, builder, *block);
    let result = builder.alloc_local();
    let merge = builder.new_block();
    let mut current = *block;
    for &arm in ast.multi_children(node) {
        let children = ast.fixed_children(arm);
        let mut body = builder.new_block();
        let next = builder.new_block();
        branch(
            resolved,
            children[0],
            scrutinee,
            builder,
            current,
            body,
            next,
        );
        let value = lower_expr(resolved, children[1], builder, &mut body);
        if matches!(
            builder.blocks[body.0 as usize].terminator,
            Terminator::Unreachable
        ) {
            builder.blocks[body.0 as usize]
                .stmts
                .push(NirStmt::Assign(result, NirExpr::Use(value)));
            builder.blocks[body.0 as usize].terminator = Terminator::Goto(merge);
        }
        current = next;
    }
    builder.blocks[current.0 as usize].terminator = Terminator::MatchFail;
    *block = merge;
    NirValue::Local(result)
}

pub(crate) fn lower_matches(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let children = resolved.ast.fixed_children(node);
    let value = lower_expr(resolved, children[0], builder, block);
    let value = snapshot(value, builder, *block);
    let success = builder.new_block();
    let failure = builder.new_block();
    let merge = builder.new_block();
    let result = builder.alloc_local();
    branch(
        resolved,
        children[1],
        value,
        builder,
        *block,
        success,
        failure,
    );
    for (branch, value) in [(success, true), (failure, false)] {
        builder.blocks[branch.0 as usize]
            .stmts
            .push(NirStmt::Assign(
                result,
                NirExpr::Use(NirValue::ConstBool(value)),
            ));
        builder.blocks[branch.0 as usize].terminator = Terminator::Goto(merge);
    }
    *block = merge;
    NirValue::Local(result)
}
