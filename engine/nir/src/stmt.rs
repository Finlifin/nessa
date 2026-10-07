//! Statement lowering — converts AST statement nodes into NIR statements
//! within basic blocks.

use ast::{Ast, NodeIndex, NodeKind};
use resolution::ResolvedAst;

use crate::builder::{FunctionBuilder, LoopTargets};
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
            let stmts = ast.multi_children(node_idx);
            let last_idx = stmts.len().saturating_sub(1);
            for (i, &stmt) in stmts.iter().enumerate() {
                if !matches!(
                    builder.blocks[block.0 as usize].terminator,
                    Terminator::Unreachable
                ) {
                    break;
                }
                lower_stmt(resolved, stmt, builder, block, i == last_idx);
            }
            // If block is still unterminated, add return unit.
            if matches!(
                builder.blocks[block.0 as usize].terminator,
                Terminator::Unreachable
            ) {
                builder.blocks[block.0 as usize].terminator = Terminator::Return(NirValue::Unit);
            }
        }
        _ => {
            // Single expression body — return its value.
            let val = lower_expr(resolved, node_idx, builder, block);
            builder.blocks[block.0 as usize].terminator = Terminator::Return(val);
        }
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
        NodeKind::PubDef | NodeKind::PrivateDef => {
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
                builder.blocks[block.0 as usize].terminator = Terminator::Return(value);
                *block = continue_block;
                return;
            }
            let val = if !children[0].is_null() {
                lower_expr(resolved, children[0], builder, block)
            } else {
                NirValue::Unit
            };
            builder.blocks[block.0 as usize].terminator = Terminator::Return(val);
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
            if children[1].is_null() {
                builder.blocks[block.0 as usize].terminator = Terminator::Goto(target);
            } else {
                let condition = lower_expr(resolved, children[1], builder, block);
                let remainder = builder.new_block();
                builder.blocks[block.0 as usize].terminator =
                    Terminator::Branch(condition, target, remainder);
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
                    builder.blocks[block.0 as usize].terminator = Terminator::Return(val);
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

        // ── For loop ───────────────────────────────────────────────
        NodeKind::ForLoop => {
            lower_for_loop(resolved, ast, node_idx, builder, block);
        }

        // ── While loop ─────────────────────────────────────────────
        NodeKind::WhileLoop => {
            lower_while_loop(resolved, ast, node_idx, builder, block);
        }

        // ── Default: treat as expression ───────────────────────────
        _ => {
            let val = lower_expr(resolved, node_idx, builder, block);
            if is_last
                && matches!(
                    builder.blocks[block.0 as usize].terminator,
                    Terminator::Unreachable
                )
            {
                builder.blocks[block.0 as usize].terminator = Terminator::Return(val);
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
    if ast.node(pattern_node).kind == NodeKind::PatternTuple {
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
        builder.blocks[else_block.0 as usize].terminator = Terminator::Goto(merge_block);
    }

    *block = merge_block;
}

/// Discard ordinary expression values while preserving explicit control flow.
fn lower_non_tail_body(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) {
    if node.is_null() {
        return;
    }
    if resolved.ast.node(node).kind == NodeKind::Block {
        for &statement in resolved.ast.multi_children(node) {
            if !matches!(
                builder.blocks[block.0 as usize].terminator,
                Terminator::Unreachable
            ) {
                break;
            }
            lower_stmt(resolved, statement, builder, block, false);
        }
    } else {
        lower_stmt(resolved, node, builder, block, false);
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
    let iter_expression = if let Some(call) = plan.into_iter {
        let function = builder.func_map[&call.function.expect("checked into_iter target")];
        NirExpr::Call(function, vec![iterable])
    } else {
        NirExpr::Use(iterable)
    };
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(iter_local, iter_expression));
    let iter_val = NirValue::Local(iter_local);
    let header_block = builder.new_block();
    let payload_block = builder.new_block();
    let body_block = builder.new_block();
    let exit_block = builder.new_block();
    builder.blocks[block.0 as usize].terminator = Terminator::Goto(header_block);

    // Each visit consumes exactly one next result. Only the explicit done tag
    // terminates; yielded null/Unit is still an element.
    let step_local = builder.alloc_local();
    let function = builder.func_map[&plan.next.function.expect("checked next target")];
    builder.blocks[header_block.0 as usize]
        .stmts
        .push(NirStmt::Assign(
            step_local,
            NirExpr::Call(function, vec![iter_val]),
        ));
    let done = builder.alloc_local();
    builder.blocks[header_block.0 as usize]
        .stmts
        .push(NirStmt::Assign(
            done,
            NirExpr::EnumIs(
                NirValue::Local(step_local),
                plan.step_type,
                type_pool::ITERATION_DONE_TAG,
            ),
        ));
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
    builder.loop_targets.push(LoopTargets {
        label: (!children[0].is_null()).then(|| ast.node(children[0]).str_id),
        break_to: exit_block,
        continue_to: header_block,
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
    builder.loop_targets.push(LoopTargets {
        label: (!children[0].is_null()).then(|| ast.node(children[0]).str_id),
        break_to: exit_block,
        continue_to: header_block,
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
        builder.blocks[block.0 as usize].terminator = Terminator::Return(value);
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
