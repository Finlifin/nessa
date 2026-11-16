//! Statement lowering — converts AST statement nodes into NIR statements
//! within basic blocks.

use ast::{Ast, NodeIndex, NodeKind};
use resolution::{ForLoopPlan, ResolvedAst};

use crate::builder::{CleanupAction, FunctionBuilder, LoopTargets};
use crate::expr::lower_expr;
use crate::{BlockId, NirExpr, NirStmt, NirValue, Terminator};

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Lower the body of a function/block into a given basic block.
///
/// If the body is a `Block`, walks each child statement.  Otherwise treats it
/// as a single expression whose value is returned.
pub(crate) fn lower_body_into(
    resolved: &ResolvedAst,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) {
    if node_idx.is_null() {
        return;
    }
    let ast = &resolved.ast;
    let node = ast.node(node_idx);
    match node.kind {
        NodeKind::ExprStatement | NodeKind::ReturnStatement | NodeKind::ResumeStatement => {
            lower_stmt(resolved, node_idx, builder, block, true);
        }
        NodeKind::Block => {
            crate::capability::lower_block_statements(
                resolved,
                ast.multi_children(node_idx),
                crate::capability::BlockExecMode::TailReturn,
                builder,
                block,
            );
        }
        _ => {
            // Single expression body — return its value.
            let val = lower_expr(resolved, node_idx, builder, block);
            if matches!(
                builder.blocks[block.0 as usize].terminator,
                Terminator::Unreachable
            ) {
                emit_normal_return(resolved, node_idx, val, builder, block);
            }
        }
    }
}

pub(crate) fn run_defer_node(
    resolved: &ResolvedAst,
    defer_body: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) {
    let saved_scopes = std::mem::replace(&mut builder.defer_scopes, vec![Vec::new()]);
    let saved_loops = std::mem::take(&mut builder.loop_targets);
    lower_non_tail_body(resolved, defer_body, builder, block);
    let inner_top = builder.defer_scopes.pop().unwrap_or_default();
    if matches!(
        builder.blocks[block.0 as usize].terminator,
        Terminator::Unreachable
    ) {
        emit_scope_defers(resolved, &inner_top, builder, block);
    }
    builder.defer_scopes = saved_scopes;
    builder.loop_targets = saved_loops;
}

pub(crate) fn emit_scope_defers_mode(
    resolved: &ResolvedAst,
    defers: &[CleanupAction],
    await_handlers: bool,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) {
    for &action in defers.iter().rev() {
        if !matches!(
            builder.blocks[block.0 as usize].terminator,
            Terminator::Unreachable
        ) {
            break;
        }
        match action {
            CleanupAction::Defer(defer_body) => {
                let pop_tmp = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    pop_tmp,
                    NirExpr::CallBuiltin(runtime::ids::DEFER_POP, Vec::new()),
                ));
                run_defer_node(resolved, defer_body, builder, block);
            }
            CleanupAction::PopHandler => {
                builder.blocks[block.0 as usize]
                    .stmts
                    .push(if await_handlers {
                        NirStmt::AwaitPopHandler
                    } else {
                        NirStmt::PopHandler
                    });
            }
        }
    }
}

pub(crate) fn emit_scope_defers(
    resolved: &ResolvedAst,
    defers: &[CleanupAction],
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) {
    emit_scope_defers_mode(resolved, defers, true, builder, block);
}

pub(crate) fn emit_scope_defers_for_value(
    resolved: &ResolvedAst,
    scope_node: NodeIndex,
    value: NirValue,
    value_type: Option<type_pool::TypeIndex>,
    defers: &[CleanupAction],
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    if !matches!(
        builder.blocks[block.0 as usize].terminator,
        Terminator::Unreachable
    ) || defers.is_empty()
    {
        return value;
    }
    let value = crate::arguments::snapshot(value, builder, *block);
    let has_handlers = defers
        .iter()
        .any(|action| matches!(action, CleanupAction::PopHandler));
    let is_error_qualified = value_type.is_some_and(|ty| {
        resolved
            .type_pool
            .error_shape(ty)
            .is_ok_and(|shape| shape.is_some())
    });
    if has_handlers && is_error_qualified {
        let scope = resolved
            .node_scopes
            .get(&scope_node)
            .map_or(builder.entry_scope.unwrap_or(0), |s| s.0);
        let ok_block = builder.new_block();
        let err_block = builder.new_block();
        let merge_block = builder.new_block();
        crate::patterns::test(
            builder,
            *block,
            NirExpr::ErrorIsOk(value).in_scope(scope),
            ok_block,
            err_block,
        );
        let mut ok_cur = ok_block;
        emit_scope_defers_mode(resolved, defers, true, builder, &mut ok_cur);
        if matches!(
            builder.blocks[ok_cur.0 as usize].terminator,
            Terminator::Unreachable
        ) {
            builder.blocks[ok_cur.0 as usize].terminator = Terminator::Goto(merge_block);
        }
        let mut err_cur = err_block;
        emit_scope_defers_mode(resolved, defers, false, builder, &mut err_cur);
        if matches!(
            builder.blocks[err_cur.0 as usize].terminator,
            Terminator::Unreachable
        ) {
            builder.blocks[err_cur.0 as usize].terminator = Terminator::Goto(merge_block);
        }
        *block = merge_block;
    } else {
        emit_scope_defers_mode(resolved, defers, true, builder, block);
    }
    value
}

pub(crate) struct HandlerScopeSpec {
    pub scope_node: NodeIndex,
    pub expr_node: NodeIndex,
    pub count: usize,
    pub captures: bool,
    pub answer_type: Option<type_pool::TypeIndex>,
}

pub(crate) fn finish_handler_scope_expr(
    resolved: &ResolvedAst,
    spec: HandlerScopeSpec,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    if spec.captures {
        let (body, _) = crate::expr::lower_closure(
            resolved,
            spec.expr_node,
            &[],
            None,
            spec.answer_type,
            builder,
            block,
        );
        let local = builder.alloc_local();
        builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
            local,
            NirExpr::DelimitedCall {
                body,
                handler_count: spec.count as u8,
            },
        ));
        let pop_actions = vec![CleanupAction::PopHandler; spec.count];
        emit_scope_defers_for_value(
            resolved,
            spec.scope_node,
            NirValue::Local(local),
            builder.node_type(resolved, spec.scope_node),
            &pop_actions,
            builder,
            block,
        )
    } else {
        builder
            .defer_scopes
            .push(vec![CleanupAction::PopHandler; spec.count]);
        let result = lower_expr(resolved, spec.expr_node, builder, block);
        let pop_actions = builder.defer_scopes.pop().unwrap_or_default();
        emit_scope_defers_for_value(
            resolved,
            spec.scope_node,
            result,
            builder.node_type(resolved, spec.scope_node),
            &pop_actions,
            builder,
            block,
        )
    }
}

pub(crate) fn emit_defers_from_mode(
    resolved: &ResolvedAst,
    from_depth: usize,
    await_handlers: bool,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) {
    if from_depth >= builder.defer_scopes.len() {
        return;
    }
    let scopes: Vec<Vec<CleanupAction>> = builder.defer_scopes[from_depth..].to_vec();
    for scope_defers in scopes.iter().rev() {
        emit_scope_defers_mode(resolved, scope_defers, await_handlers, builder, block);
    }
}

pub(crate) fn emit_defers_from(
    resolved: &ResolvedAst,
    from_depth: usize,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) {
    emit_defers_from_mode(resolved, from_depth, false, builder, block);
}

pub(crate) fn emit_return(
    resolved: &ResolvedAst,
    val: NirValue,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) {
    let has_defers = builder.defer_scopes.iter().any(|scope| !scope.is_empty());
    let val = if has_defers {
        crate::arguments::snapshot(val, builder, *block)
    } else {
        val
    };
    emit_defers_from(resolved, 0, builder, block);
    if matches!(
        builder.blocks[block.0 as usize].terminator,
        Terminator::Unreachable
    ) {
        builder.blocks[block.0 as usize].terminator = Terminator::Return(val);
    }
}

pub(crate) fn emit_normal_return(
    resolved: &ResolvedAst,
    scope_node: NodeIndex,
    val: NirValue,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) {
    let has_defers = builder.defer_scopes.iter().any(|scope| !scope.is_empty());
    let val = if has_defers {
        crate::arguments::snapshot(val, builder, *block)
    } else {
        val
    };
    let has_handlers = builder
        .defer_scopes
        .iter()
        .flatten()
        .any(|action| matches!(action, CleanupAction::PopHandler));
    let is_error_qualified = resolved
        .type_pool
        .error_shape(builder.return_type)
        .is_ok_and(|shape| shape.is_some());
    if has_handlers && is_error_qualified {
        let scope = resolved
            .node_scopes
            .get(&scope_node)
            .map_or(builder.entry_scope.unwrap_or(0), |s| s.0);
        let ok_block = builder.new_block();
        let err_block = builder.new_block();
        let merge_block = builder.new_block();
        crate::patterns::test(
            builder,
            *block,
            NirExpr::ErrorIsOk(val).in_scope(scope),
            ok_block,
            err_block,
        );
        let mut ok_cur = ok_block;
        emit_defers_from_mode(resolved, 0, true, builder, &mut ok_cur);
        if matches!(
            builder.blocks[ok_cur.0 as usize].terminator,
            Terminator::Unreachable
        ) {
            builder.blocks[ok_cur.0 as usize].terminator = Terminator::Goto(merge_block);
        }
        let mut err_cur = err_block;
        emit_defers_from_mode(resolved, 0, false, builder, &mut err_cur);
        if matches!(
            builder.blocks[err_cur.0 as usize].terminator,
            Terminator::Unreachable
        ) {
            builder.blocks[err_cur.0 as usize].terminator = Terminator::Goto(merge_block);
        }
        *block = merge_block;
    } else {
        emit_defers_from_mode(resolved, 0, true, builder, block);
    }
    if matches!(
        builder.blocks[block.0 as usize].terminator,
        Terminator::Unreachable
    ) {
        builder.blocks[block.0 as usize].terminator = Terminator::Return(val);
    }
}

/// Lower a single AST statement into the given basic block.
///
/// `is_last` is `true` for the last statement in a block, which can influence
/// whether its value is returned.
pub(crate) fn lower_stmt(
    resolved: &ResolvedAst,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
    is_last: bool,
) {
    if node_idx.is_null() {
        return;
    }
    let ast = &resolved.ast;
    let node = ast.node(node_idx);
    match node.kind {
        NodeKind::PubDef | NodeKind::PrivateDef | NodeKind::GlobalDecl => {
            lower_stmt(
                resolved,
                ast.fixed_children(node_idx)[0],
                builder,
                block,
                is_last,
            );
        }
        // ── Return ─────────────────────────────────────────────────
        NodeKind::ReturnStatement | NodeKind::ResumeStatement => {
            let children = ast.fixed_children(node_idx);
            if !children[1].is_null() {
                // Evaluate the guard before the value: a false guard must not
                // execute effects in the return/resume expression.
                let condition = lower_expr(resolved, children[1], builder, block);
                let return_block = builder.new_block();
                let continue_block = builder.new_block();
                builder.blocks[block.0 as usize].terminator =
                    Terminator::Branch(condition, return_block, continue_block);
                *block = return_block;
                let value = if children[0].is_null() {
                    NirValue::Unit
                } else {
                    lower_expr(resolved, children[0], builder, block)
                };
                if matches!(
                    builder.blocks[block.0 as usize].terminator,
                    Terminator::Unreachable
                ) {
                    emit_return(resolved, value, builder, block);
                }
                *block = continue_block;
                return;
            }
            let val = if !children[0].is_null() {
                lower_expr(resolved, children[0], builder, block)
            } else {
                NirValue::Unit
            };
            if matches!(
                builder.blocks[block.0 as usize].terminator,
                Terminator::Unreachable
            ) {
                emit_return(resolved, val, builder, block);
            }
        }

        NodeKind::BreakStatement | NodeKind::ContinueStatement => {
            let children = ast.fixed_children(node_idx);
            let label = (!children[0].is_null()).then(|| ast.node(children[0]).str_id);
            let targets = builder
                .loop_targets
                .iter()
                .rev()
                .find(|target| label.is_none() || target.label == label)
                .expect("resolution validates loop control targets");
            let target = if node.kind == NodeKind::BreakStatement {
                targets.break_to
            } else {
                targets.continue_to
            };
            let defer_depth = targets.defer_depth;
            if children[1].is_null() {
                emit_defers_from(resolved, defer_depth, builder, block);
                if matches!(
                    builder.blocks[block.0 as usize].terminator,
                    Terminator::Unreachable
                ) {
                    builder.blocks[block.0 as usize].terminator = Terminator::Goto(target);
                }
            } else {
                let condition = lower_expr(resolved, children[1], builder, block);
                let mut jump_block = builder.new_block();
                let remainder = builder.new_block();
                builder.blocks[block.0 as usize].terminator =
                    Terminator::Branch(condition, jump_block, remainder);
                emit_defers_from(resolved, defer_depth, builder, &mut jump_block);
                if matches!(
                    builder.blocks[jump_block.0 as usize].terminator,
                    Terminator::Unreachable
                ) {
                    builder.blocks[jump_block.0 as usize].terminator = Terminator::Goto(target);
                }
                *block = remainder;
            }
        }

        // ── Variable declarations ──────────────────────────────────
        NodeKind::LetDecl | NodeKind::ConstDecl => {
            lower_var_decl(resolved, ast, node_idx, builder, block);
        }
        NodeKind::VarDecl => {
            lower_var_decl(resolved, ast, node_idx, builder, block);
        }

        // ── Expression statement ───────────────────────────────────
        NodeKind::ExprStatement => {
            let children = ast.fixed_children(node_idx);
            if is_last {
                let val = lower_expr(resolved, children[0], builder, block);
                if matches!(
                    builder.blocks[block.0 as usize].terminator,
                    Terminator::Unreachable
                ) {
                    emit_normal_return(resolved, children[0], val, builder, block);
                }
            } else {
                lower_expr(resolved, children[0], builder, block);
            }
        }

        // ── Post-match expression used as statement ───────────────
        NodeKind::PostMatch => {
            lower_post_match_stmt(resolved, ast, node_idx, builder, block, is_last);
        }

        // ── If statement ───────────────────────────────────────────
        NodeKind::IfStatement => {
            lower_if(resolved, ast, node_idx, builder, block, is_last);
        }

        // ── When statement ─────────────────────────────────────────
        NodeKind::WhenStatement => {
            lower_when_stmt(resolved, ast, node_idx, builder, block, is_last);
        }

        // ── For loop ───────────────────────────────────────────────
        NodeKind::ForLoop => {
            lower_for_loop(resolved, ast, node_idx, builder, block);
            if is_last
                && matches!(
                    builder.blocks[block.0 as usize].terminator,
                    Terminator::Unreachable
                )
            {
                emit_normal_return(resolved, node_idx, NirValue::Unit, builder, block);
            }
        }

        // ── While loop ─────────────────────────────────────────────
        NodeKind::WhileLoop => {
            lower_while_loop(resolved, ast, node_idx, builder, block);
            if is_last
                && matches!(
                    builder.blocks[block.0 as usize].terminator,
                    Terminator::Unreachable
                )
            {
                emit_normal_return(resolved, node_idx, NirValue::Unit, builder, block);
            }
        }

        // ── Defer statement ────────────────────────────────────────
        NodeKind::DeferStatement => {
            let defer_body = ast.fixed_children(node_idx)[0];
            if !defer_body.is_null() {
                let is_unwinding = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    is_unwinding,
                    NirExpr::CallBuiltin(runtime::ids::DEFER_PUSH, Vec::new()),
                ));
                let mut unwind_block = builder.new_block();
                let continue_block = builder.new_block();
                builder.blocks[block.0 as usize].terminator =
                    Terminator::Branch(NirValue::Local(is_unwinding), unwind_block, continue_block);

                run_defer_node(resolved, defer_body, builder, &mut unwind_block);
                if matches!(
                    builder.blocks[unwind_block.0 as usize].terminator,
                    Terminator::Unreachable
                ) {
                    let resume_tmp = builder.alloc_local();
                    builder.blocks[unwind_block.0 as usize]
                        .stmts
                        .push(NirStmt::Assign(
                            resume_tmp,
                            NirExpr::CallBuiltin(runtime::ids::DEFER_RESUME_UNWIND, Vec::new()),
                        ));
                }

                *block = continue_block;
                if let Some(scope) = builder.defer_scopes.last_mut() {
                    scope.push(CleanupAction::Defer(defer_body));
                }
            }
            if is_last
                && matches!(
                    builder.blocks[block.0 as usize].terminator,
                    Terminator::Unreachable
                )
            {
                emit_normal_return(resolved, node_idx, NirValue::Unit, builder, block);
            }
        }

        NodeKind::HandlesStatement | NodeKind::LetHandler => {
            crate::capability::lower_block_statements(
                resolved,
                &[node_idx],
                if is_last {
                    crate::capability::BlockExecMode::TailReturn
                } else {
                    crate::capability::BlockExecMode::NonTail
                },
                builder,
                block,
            );
        }

        // ── Non-runtime declarations in statement position ─────────
        NodeKind::FunctionDef
        | NodeKind::EffectDef
        | NodeKind::AsyncEffectDef
        | NodeKind::StructDef
        | NodeKind::StructField
        | NodeKind::EnumDef
        | NodeKind::EnumVariant
        | NodeKind::TraitDef
        | NodeKind::TraitDefFn
        | NodeKind::TraitDeriveFn
        | NodeKind::ImplDef
        | NodeKind::ImplTraitDef
        | NodeKind::ExtendDef
        | NodeKind::ExtendTraitDef
        | NodeKind::DeriveDef
        | NodeKind::Typealias
        | NodeKind::Newtype
        | NodeKind::ModuleDef
        | NodeKind::UseStatement
        | NodeKind::AssocDecl
        | NodeKind::AssocBinding => {
            if is_last
                && matches!(
                    builder.blocks[block.0 as usize].terminator,
                    Terminator::Unreachable
                )
            {
                emit_normal_return(resolved, node_idx, NirValue::Unit, builder, block);
            }
        }

        // ── Expressions in statement position ──────────────────────
        NodeKind::Int
        | NodeKind::Real
        | NodeKind::Str
        | NodeKind::FStringConcat
        | NodeKind::Char
        | NodeKind::Bool
        | NodeKind::Id
        | NodeKind::SelfUpper
        | NodeKind::SelfLower
        | NodeKind::Null
        | NodeKind::Underscore
        | NodeKind::Unit
        | NodeKind::Symbol
        | NodeKind::ListOf
        | NodeKind::Tuple
        | NodeKind::Object
        | NodeKind::Lambda
        | NodeKind::CaseMap
        | NodeKind::CaseAlternative
        | NodeKind::Negative
        | NodeKind::BoolNot
        | NodeKind::ErrorConstruction
        | NodeKind::OptionalType
        | NodeKind::EffectQualifiedType
        | NodeKind::ErrorQualifiedType
        | NodeKind::RangeFull
        | NodeKind::RangeFrom
        | NodeKind::RangeTo
        | NodeKind::RangeFromTo
        | NodeKind::RangeToInclusive
        | NodeKind::RangeFromToInclusive
        | NodeKind::Add
        | NodeKind::Sub
        | NodeKind::Mul
        | NodeKind::Div
        | NodeKind::Mod
        | NodeKind::Concat
        | NodeKind::BoolEq
        | NodeKind::BoolNotEq
        | NodeKind::BoolGt
        | NodeKind::BoolGtEq
        | NodeKind::BoolLt
        | NodeKind::BoolLtEq
        | NodeKind::BoolAnd
        | NodeKind::BoolOr
        | NodeKind::BoolMatches
        | NodeKind::Projection
        | NodeKind::View
        | NodeKind::Arrow
        | NodeKind::Pipeline
        | NodeKind::InfixFnCall
        | NodeKind::EffectPropagation
        | NodeKind::EffectElimination
        | NodeKind::ErrorPropagation
        | NodeKind::ErrorElimination
        | NodeKind::OptionPropagation
        | NodeKind::HandlerApplication
        | NodeKind::Call
        | NodeKind::ExtendedCall
        | NodeKind::PostDo
        | NodeKind::TypeCast
        | NodeKind::Await
        | NodeKind::SpreadArg
        | NodeKind::ResetExpr
        | NodeKind::ShiftExpr
        | NodeKind::FnType
        | NodeKind::EffectType
        | NodeKind::AsyncEffectType
        | NodeKind::Assign
        | NodeKind::AddAssign
        | NodeKind::SubAssign
        | NodeKind::MulAssign
        | NodeKind::DivAssign
        | NodeKind::ModAssign
        | NodeKind::Block
        | NodeKind::PropertyAssignment
        | NodeKind::NamedArg => {
            let val = lower_expr(resolved, node_idx, builder, block);
            if is_last
                && matches!(
                    builder.blocks[block.0 as usize].terminator,
                    Terminator::Unreachable
                )
            {
                emit_normal_return(resolved, node_idx, val, builder, block);
            }
        }

        // ── Structural / pattern / invalid nodes ───────────────────
        NodeKind::Invalid
        | NodeKind::ConditionArm
        | NodeKind::ElseConditionArm
        | NodeKind::CaseArm
        | NodeKind::CatchArm
        | NodeKind::PatternOr
        | NodeKind::PatternNot
        | NodeKind::PatternError
        | NodeKind::PatternEffect
        | NodeKind::PatternTypeFamily
        | NodeKind::PatternCall
        | NodeKind::PatternExtendedCall
        | NodeKind::PatternAsBind
        | NodeKind::PatternTyped
        | NodeKind::PatternIfGuard
        | NodeKind::PatternAndIs
        | NodeKind::PatternOptionSome
        | NodeKind::PatternErrorOk
        | NodeKind::PatternList
        | NodeKind::PatternTuple
        | NodeKind::PatternRecord
        | NodeKind::PatternRestBind
        | NodeKind::PatternAsync
        | NodeKind::PatternFromExpr
        | NodeKind::PropertyPattern
        | NodeKind::SuperPath
        | NodeKind::PackagePath
        | NodeKind::PathProjection
        | NodeKind::PathProjectionAll
        | NodeKind::PathProjectionMulti
        | NodeKind::PathAsBind
        | NodeKind::ParamSelf
        | NodeKind::ParamVarargs
        | NodeKind::ParamOptional
        | NodeKind::ParamLambda
        | NodeKind::ParamTyped
        | NodeKind::ParamCatch
        | NodeKind::Property
        | NodeKind::FileScope => {
            if is_last
                && matches!(
                    builder.blocks[block.0 as usize].terminator,
                    Terminator::Unreachable
                )
            {
                emit_normal_return(resolved, node_idx, NirValue::Unit, builder, block);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Variable declaration
// ---------------------------------------------------------------------------

fn lower_var_decl(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) {
    let children = ast.fixed_children(node_idx);
    // [0] pattern  [1] type  [2] value  [3] else
    let pattern_node = children[0];
    if children.len() > 3 && !children[3].is_null() {
        let value = lower_expr(resolved, children[2], builder, block);
        let value = crate::arguments::snapshot(value, builder, *block);
        let ok_block = builder.new_block();
        let mut else_block = builder.new_block();
        crate::patterns::branch(
            resolved,
            pattern_node,
            value,
            builder,
            *block,
            ok_block,
            else_block,
        );
        lower_non_tail_body(resolved, children[3], builder, &mut else_block);
        if matches!(
            builder.blocks[else_block.0 as usize].terminator,
            Terminator::Unreachable
        ) {
            builder.blocks[else_block.0 as usize].terminator = Terminator::Return(NirValue::Unit);
        }
        *block = ok_block;
        return;
    }
    if matches!(
        ast.node(pattern_node).kind,
        NodeKind::PatternTuple
            | NodeKind::PatternExtendedCall
            | NodeKind::PatternTyped
            | NodeKind::PatternCall
    ) {
        let value = lower_expr(resolved, children[2], builder, block);
        crate::tuples::bind_pattern(resolved, pattern_node, value, builder, *block);
        return;
    }
    if let Some(global) = resolved
        .node_symbols
        .get(&pattern_node)
        .and_then(|symbol| builder.global_map.get(symbol))
        .copied()
    {
        let value = lower_expr(resolved, children[2], builder, block);
        builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::StoreGlobal(global, value).in_source_scope(resolved, node_idx));
        return;
    }
    let local = if let Some(&sym_id) = resolved.node_symbols.get(&pattern_node) {
        let local = builder.local_for_symbol(sym_id);
        crate::trait_parameters::bind_local_type(
            resolved,
            local,
            builder.symbol_type(resolved, sym_id),
            builder,
        );
        local
    } else {
        builder.alloc_local()
    };
    if children.len() > 2 && !children[2].is_null() {
        let val = lower_expr(resolved, children[2], builder, block);
        crate::trait_parameters::copy_proof(local, val, builder, *block);
        builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::Assign(local, NirExpr::Use(val)));
    }
}

// ---------------------------------------------------------------------------
// If statement
// ---------------------------------------------------------------------------

fn lower_if(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
    is_last: bool,
) {
    let children = ast.fixed_children(node_idx);
    // [0] condition  [1] then  [2] else
    let cond = lower_expr(resolved, children[0], builder, block);
    let mut then_block = builder.new_block();
    let mut else_block = builder.new_block();
    let merge_block = builder.new_block();

    builder.blocks[block.0 as usize].terminator = Terminator::Branch(cond, then_block, else_block);

    if is_last {
        lower_body_into(resolved, children[1], builder, &mut then_block);
    } else {
        lower_non_tail_body(resolved, children[1], builder, &mut then_block);
        if matches!(
            builder.blocks[then_block.0 as usize].terminator,
            Terminator::Unreachable
        ) {
            builder.blocks[then_block.0 as usize].terminator = Terminator::Goto(merge_block);
        }
    }

    if children.len() > 2 && !children[2].is_null() {
        if is_last {
            lower_body_into(resolved, children[2], builder, &mut else_block);
        } else {
            lower_non_tail_body(resolved, children[2], builder, &mut else_block);
            if matches!(
                builder.blocks[else_block.0 as usize].terminator,
                Terminator::Unreachable
            ) {
                builder.blocks[else_block.0 as usize].terminator = Terminator::Goto(merge_block);
            }
        }
    } else {
        if is_last {
            emit_normal_return(resolved, node_idx, NirValue::Unit, builder, &mut else_block);
        } else {
            builder.blocks[else_block.0 as usize].terminator = Terminator::Goto(merge_block);
        }
    }

    *block = merge_block;
}

fn lower_when_stmt(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
    is_last: bool,
) {
    let arms = ast.multi_children(node_idx);
    let has_else = arms
        .iter()
        .any(|&arm| !arm.is_null() && ast.node(arm).kind == NodeKind::ElseConditionArm);
    let merge_block = builder.new_block();

    for &arm in arms {
        if arm.is_null() {
            continue;
        }
        match ast.node(arm).kind {
            NodeKind::ConditionArm => {
                let children = ast.fixed_children(arm);
                let cond = lower_expr(resolved, children[0], builder, block);
                let mut arm_block = builder.new_block();
                let next_block = builder.new_block();
                builder.blocks[block.0 as usize].terminator =
                    Terminator::Branch(cond, arm_block, next_block);
                if is_last && has_else {
                    lower_body_into(resolved, children[1], builder, &mut arm_block);
                } else {
                    lower_non_tail_body(resolved, children[1], builder, &mut arm_block);
                    if matches!(
                        builder.blocks[arm_block.0 as usize].terminator,
                        Terminator::Unreachable
                    ) {
                        builder.blocks[arm_block.0 as usize].terminator =
                            Terminator::Goto(merge_block);
                    }
                }
                *block = next_block;
            }
            NodeKind::ElseConditionArm => {
                let body = ast.fixed_children(arm)[0];
                if is_last {
                    lower_body_into(resolved, body, builder, block);
                } else {
                    lower_non_tail_body(resolved, body, builder, block);
                    if matches!(
                        builder.blocks[block.0 as usize].terminator,
                        Terminator::Unreachable
                    ) {
                        builder.blocks[block.0 as usize].terminator = Terminator::Goto(merge_block);
                    }
                }
                *block = merge_block;
                return;
            }
            _ => {}
        }
    }

    if matches!(
        builder.blocks[block.0 as usize].terminator,
        Terminator::Unreachable
    ) {
        builder.blocks[block.0 as usize].terminator = Terminator::Goto(merge_block);
    }
    *block = merge_block;
    if is_last
        && matches!(
            builder.blocks[block.0 as usize].terminator,
            Terminator::Unreachable
        )
    {
        emit_normal_return(resolved, node_idx, NirValue::Unit, builder, block);
    }
}

/// Discard ordinary expression values while preserving explicit control flow.
pub(crate) fn lower_non_tail_body(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) {
    if node.is_null() {
        return;
    }
    if resolved.ast.node(node).kind == NodeKind::Block {
        crate::capability::lower_block_statements(
            resolved,
            resolved.ast.multi_children(node),
            crate::capability::BlockExecMode::NonTail,
            builder,
            block,
        );
    } else {
        builder.defer_scopes.push(Vec::new());
        lower_stmt(resolved, node, builder, block, false);
        let scope_defers = builder.defer_scopes.pop().unwrap_or_default();
        if matches!(
            builder.blocks[block.0 as usize].terminator,
            Terminator::Unreachable
        ) {
            emit_scope_defers(resolved, &scope_defers, builder, block);
        }
    }
}

// ---------------------------------------------------------------------------
// For loop → header/body/exit basic blocks
// ---------------------------------------------------------------------------

fn lower_for_loop(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) {
    let children = ast.fixed_children(node_idx);
    // ForLoop: [0] label  [1] pattern  [2] iterable  [3] body

    let plan = builder
        .for_loop(resolved, node_idx)
        .expect("resolution froze the iterator protocol")
        .clone();
    let iterable = lower_expr(resolved, children[2], builder, block);
    let iter_local = builder.alloc_local();
    let iter_val = NirValue::Local(iter_local);
    // A static plan knows the exact IterationStep type; a dynamic one tests
    // whichever concrete step the selected implementation returns.
    let (next_expression, step_type) = match &plan {
        ForLoopPlan::Static(plan) => {
            let iter_expression = if let Some(call) = &plan.into_iter {
                let function = builder.func_map[&call.function.expect("checked into_iter target")];
                NirExpr::Call(function, vec![iterable])
            } else {
                NirExpr::Use(iterable)
            };
            builder.blocks[block.0 as usize]
                .stmts
                .push(NirStmt::Assign(iter_local, iter_expression));
            let function = builder.func_map[&plan.next.function.expect("checked next target")];
            (
                NirExpr::Call(function, vec![iter_val]),
                Some(plan.step_type),
            )
        }
        ForLoopPlan::Dynamic { query_scope } => {
            let next = crate::dynamic_loops::prepare(
                resolved,
                iterable,
                iter_local,
                query_scope.0,
                builder,
                block,
            );
            (next, None)
        }
    };
    let header_block = builder.new_block();
    let payload_block = builder.new_block();
    let body_block = builder.new_block();
    let exit_block = builder.new_block();
    builder.blocks[block.0 as usize].terminator = Terminator::Goto(header_block);

    // Each visit consumes exactly one next result. Only the explicit done tag
    // terminates; yielded null/Unit is still an element.
    let step_local = builder.alloc_local();
    builder.blocks[header_block.0 as usize]
        .stmts
        .push(NirStmt::Assign(step_local, next_expression));
    let step = NirValue::Local(step_local);
    let done_expression = match step_type {
        Some(step_type) => NirExpr::EnumIs(step, step_type, type_pool::ITERATION_DONE_TAG),
        None => NirExpr::CallBuiltin(runtime::ids::ITERATION_STEP_DONE, vec![step]),
    };
    let done = builder.alloc_local();
    builder.blocks[header_block.0 as usize]
        .stmts
        .push(NirStmt::Assign(done, done_expression));
    builder.blocks[header_block.0 as usize].terminator =
        Terminator::Branch(NirValue::Local(done), exit_block, payload_block);
    let item = builder.alloc_local();
    builder.blocks[payload_block.0 as usize]
        .stmts
        .push(NirStmt::Assign(
            item,
            NirExpr::EnumField(NirValue::Local(step_local), 0).in_source_scope(resolved, node_idx),
        ));
    crate::patterns::branch(
        resolved,
        children[1],
        NirValue::Local(item),
        builder,
        payload_block,
        body_block,
        header_block,
    );

    // Lower body into body block.
    let mut body_block_cur = body_block;
    let defer_depth = builder.defer_scopes.len();
    builder.loop_targets.push(LoopTargets {
        label: (!children[0].is_null()).then(|| ast.node(children[0]).str_id),
        break_to: exit_block,
        continue_to: header_block,
        defer_depth,
    });
    lower_non_tail_body(resolved, children[3], builder, &mut body_block_cur);
    builder.loop_targets.pop();
    // After body, jump back to header (loop back edge).
    if matches!(
        builder.blocks[body_block_cur.0 as usize].terminator,
        Terminator::Unreachable
    ) {
        builder.blocks[body_block_cur.0 as usize].terminator = Terminator::Goto(header_block);
    }

    *block = exit_block;
}

// ---------------------------------------------------------------------------
// While loop → header/body/exit basic blocks
// ---------------------------------------------------------------------------

fn lower_while_loop(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) {
    let children = ast.fixed_children(node_idx);
    // WhileLoop: [0] label  [1] condition  [2] body

    let header_block = builder.new_block();
    let body_block = builder.new_block();
    let exit_block = builder.new_block();

    // Jump from current block to header.
    builder.blocks[block.0 as usize].terminator = Terminator::Goto(header_block);

    // Header: evaluate condition.
    let mut header_block_cur = header_block;
    let cond = lower_expr(resolved, children[1], builder, &mut header_block_cur);
    builder.blocks[header_block_cur.0 as usize].terminator =
        Terminator::Branch(cond, body_block, exit_block);

    // Body.
    let mut body_block_cur = body_block;
    let defer_depth = builder.defer_scopes.len();
    builder.loop_targets.push(LoopTargets {
        label: (!children[0].is_null()).then(|| ast.node(children[0]).str_id),
        break_to: exit_block,
        continue_to: header_block,
        defer_depth,
    });
    lower_non_tail_body(resolved, children[2], builder, &mut body_block_cur);
    builder.loop_targets.pop();
    // After body, jump back to header.
    if matches!(
        builder.blocks[body_block_cur.0 as usize].terminator,
        Terminator::Unreachable
    ) {
        builder.blocks[body_block_cur.0 as usize].terminator = Terminator::Goto(header_block);
    }

    *block = exit_block;
}

// ---------------------------------------------------------------------------
// Post-match statement lowering
// ---------------------------------------------------------------------------

fn lower_post_match_stmt(
    resolved: &ResolvedAst,
    _ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
    is_last: bool,
) {
    let value = crate::patterns::lower_match(resolved, node_idx, builder, block);
    if is_last
        && matches!(
            builder.blocks[block.0 as usize].terminator,
            Terminator::Unreachable
        )
    {
        emit_normal_return(resolved, node_idx, value, builder, block);
    }
}

#[cfg(test)]
fn lower_case_pattern_value(
    resolved: &ResolvedAst,
    ast: &Ast,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    crate::expr::lower_pattern_value(resolved, ast, node, builder, block)
}

#[cfg(test)]
mod integer_pattern_tests {
    use super::*;

    #[test]
    fn statement_patterns_keep_radix_and_signed_minimum() {
        for (text, negative, ty, expected) in [
            ("0o52", false, "i64", NirValue::ConstInt(42)),
            (
                "170141183460469231731687303715884105728",
                true,
                "i128",
                NirValue::ConstI128(i128::MIN),
            ),
            (
                "0xffffffffffffffffffffffffffffffff",
                false,
                "u128",
                NirValue::ConstU128(u128::MAX),
            ),
        ] {
            let (resolved, pattern) = crate::literal::tests::resolved_integer(text, negative, ty);
            let mut builder =
                FunctionBuilder::new(nsbc::FuncId(0), str_interner::intern("pattern"));
            let mut block = builder.new_block();
            assert_eq!(
                lower_case_pattern_value(
                    &resolved,
                    &resolved.ast,
                    pattern,
                    &mut builder,
                    &mut block
                ),
                expected
            );
        }
    }
}
