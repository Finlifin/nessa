//! Statement lowering — converts AST statement nodes into NIR statements
//! within basic blocks.

use ast::{Ast, NodeIndex, NodeKind};
use resolution::{ResolvedAst, SymbolKind};

use crate::builder::FunctionBuilder;
use crate::expr::lower_expr;
use crate::{BinOp, BlockId, NirExpr, NirStmt, NirValue, Terminator};

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
        NodeKind::Block => {
            let stmts = ast.multi_children(node_idx);
            let last_idx = stmts.len().saturating_sub(1);
            for (i, &stmt) in stmts.iter().enumerate() {
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
        // ── Return ─────────────────────────────────────────────────
        NodeKind::ReturnStatement => {
            let children = ast.fixed_children(node_idx);
            let val = if !children[0].is_null() {
                lower_expr(resolved, children[0], builder, block)
            } else {
                NirValue::Unit
            };
            builder.blocks[block.0 as usize].terminator = Terminator::Return(val);
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
    let local = if let Some(&sym_id) = resolved.node_symbols.get(&pattern_node) {
        builder.local_for_symbol(sym_id)
    } else {
        builder.alloc_local()
    };
    if children.len() > 2 && !children[2].is_null() {
        let val = lower_expr(resolved, children[2], builder, block);
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

    lower_body_into(resolved, children[1], builder, &mut then_block);
    // Patch then to jump to merge if not already terminated with Return.
    if matches!(
        builder.blocks[then_block.0 as usize].terminator,
        Terminator::Return(NirValue::Unit)
    ) && !is_last
    {
        builder.blocks[then_block.0 as usize].terminator = Terminator::Goto(merge_block);
    }

    if children.len() > 2 && !children[2].is_null() {
        lower_body_into(resolved, children[2], builder, &mut else_block);
        if matches!(
            builder.blocks[else_block.0 as usize].terminator,
            Terminator::Return(NirValue::Unit)
        ) && !is_last
        {
            builder.blocks[else_block.0 as usize].terminator = Terminator::Goto(merge_block);
        }
    } else {
        builder.blocks[else_block.0 as usize].terminator = Terminator::Goto(merge_block);
    }

    *block = merge_block;
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

    // Evaluate the iterable in the current block.
    let iterable = lower_expr(resolved, children[2], builder, block);

    // Desugar: let __iter = iterable.into_iter()
    // Check if the iterable's type has IntoIterator impl.
    let iterable_ti = resolved.node_types.get(&children[2]).copied();
    let wk = &resolved.type_pool.well_known;
    let has_into_iter = iterable_ti
        .map(|ti| resolved.type_pool.has_trait_impl(ti, wk.into_iterator))
        .unwrap_or(false);

    let iter_local = builder.alloc_local();
    if has_into_iter {
        // Call into_iter() on the iterable.
        let into_iter_str = str_interner::intern("into_iter");
        builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
            iter_local,
            NirExpr::MethodCall(iterable, into_iter_str, vec![]),
        ));
    } else {
        // Assume the iterable IS an iterator (or no trait dispatch available).
        builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
            iter_local,
            NirExpr::Use(iterable),
        ));
    }
    let iter_val = NirValue::Local(iter_local);

    // Create basic blocks for the loop structure.
    let header_block = builder.new_block();
    let body_block = builder.new_block();
    let exit_block = builder.new_block();

    // Jump from current block to header.
    builder.blocks[block.0 as usize].terminator = Terminator::Goto(header_block);

    // Header: check iterator has_next().
    let has_next_str = str_interner::intern("has_next");
    let has_next_local = builder.alloc_local();
    builder.blocks[header_block.0 as usize].stmts.push(NirStmt::Assign(
        has_next_local,
        NirExpr::MethodCall(iter_val.clone(), has_next_str, vec![]),
    ));
    builder.blocks[header_block.0 as usize].terminator =
        Terminator::Branch(NirValue::Local(has_next_local), body_block, exit_block);

    // Body: call next() and bind the result to the loop variable.
    let next_str = str_interner::intern("next");
    let next_local = builder.alloc_local();
    builder.blocks[body_block.0 as usize].stmts.push(NirStmt::Assign(
        next_local,
        NirExpr::MethodCall(iter_val, next_str, vec![]),
    ));

    // Bind the pattern variable to the next() result.
    let pat = children[1];
    if !pat.is_null() {
        if let Some(&sym_id) = resolved.node_symbols.get(&pat) {
            let pat_local = builder.local_for_symbol(sym_id);
            builder.blocks[body_block.0 as usize].stmts.push(NirStmt::Assign(
                pat_local,
                NirExpr::Use(NirValue::Local(next_local)),
            ));
        }
    }

    // Lower body into body block.
    let mut body_block_cur = body_block;
    lower_body_into(resolved, children[3], builder, &mut body_block_cur);
    // After body, jump back to header (loop back edge).
    if matches!(
        builder.blocks[body_block_cur.0 as usize].terminator,
        Terminator::Return(NirValue::Unit) | Terminator::Unreachable
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
    lower_body_into(resolved, children[2], builder, &mut body_block_cur);
    // After body, jump back to header.
    if matches!(
        builder.blocks[body_block_cur.0 as usize].terminator,
        Terminator::Return(NirValue::Unit) | Terminator::Unreachable
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
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
    is_last: bool,
) {
    // Conservative fallback for non-tail match statements.
    if !is_last {
        let _ = lower_expr(resolved, node_idx, builder, block);
        return;
    }

    let children = ast.fixed_children(node_idx);
    let scrutinee = lower_expr(resolved, children[0], builder, block);
    let arms = ast.multi_children(node_idx);
    if arms.is_empty() {
        builder.blocks[block.0 as usize].terminator = Terminator::Return(NirValue::Unit);
        return;
    }

    let mut current_check_block = *block;
    for (i, &arm_node) in arms.iter().enumerate() {
        let arm_children = ast.fixed_children(arm_node);
        let pattern_node = arm_children[0];
        let body_node = arm_children[1];

        let is_last_arm = i == arms.len() - 1;
        if is_wildcard_pattern(ast, pattern_node) || is_last_arm {
            let body_val = lower_expr(resolved, body_node, builder, &mut current_check_block);
            builder.blocks[current_check_block.0 as usize].terminator =
                Terminator::Return(body_val);
            return;
        }

        let pattern_val = lower_case_pattern_value(
            resolved,
            ast,
            pattern_node,
            builder,
            &mut current_check_block,
        );
        let cmp_local = builder.alloc_local();
        builder.blocks[current_check_block.0 as usize]
            .stmts
            .push(NirStmt::Assign(
                cmp_local,
                NirExpr::BinOp(BinOp::Eq, scrutinee, pattern_val),
            ));

        let mut then_block = builder.new_block();
        let else_block = builder.new_block();
        builder.blocks[current_check_block.0 as usize].terminator =
            Terminator::Branch(NirValue::Local(cmp_local), then_block, else_block);

        let body_val = lower_expr(resolved, body_node, builder, &mut then_block);
        builder.blocks[then_block.0 as usize].terminator = Terminator::Return(body_val);

        current_check_block = else_block;
    }

    // Should not happen when a last arm exists, but keep deterministic behavior.
    builder.blocks[current_check_block.0 as usize].terminator = Terminator::Return(NirValue::Unit);
}

fn is_wildcard_pattern(ast: &Ast, node_idx: NodeIndex) -> bool {
    if node_idx.is_null() {
        return false;
    }
    matches!(ast.node(node_idx).kind, NodeKind::Underscore)
}

fn lower_case_pattern_value(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    _builder: &mut FunctionBuilder,
    _block: &mut BlockId,
) -> NirValue {
    if node_idx.is_null() {
        return NirValue::Unit;
    }
    let node = ast.node(node_idx);
    match node.kind {
        NodeKind::PropertyPattern => {
            let children = ast.fixed_children(node_idx);
            let member_node = children[1];
            if let Some(&sym_id) = resolved.node_symbols.get(&member_node) {
                let sym = &resolved.symbols[sym_id.0 as usize];
                if sym.kind == SymbolKind::EnumVariant {
                    if let Some(&idx) = resolved.enum_variant_indices.get(&sym_id) {
                        return NirValue::ConstInt(idx as i64);
                    }
                }
            }
            NirValue::Unit
        }
        NodeKind::Int => {
            let text = str_interner::get(node.str_id);
            NirValue::ConstInt(text.parse().unwrap_or(0))
        }
        NodeKind::Bool => {
            let text = str_interner::get(node.str_id);
            NirValue::ConstBool(text == "true")
        }
        _ => NirValue::Unit,
    }
}
