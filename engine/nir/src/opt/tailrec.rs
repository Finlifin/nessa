//! Self-tail-recursion elimination (`O3`).
//!
//! Transforms tail-recursive `Call(self_id, args)` followed immediately by `Return`
//! into a post-prologue loop header at `BlockId(1)` (`BlockId(0)` falls through via
//! `Goto(BlockId(1))`), staging updated parameter arguments through fresh temporaries
//! so parallel parameter dependencies (`f(b, a + b)`) never clobber one another.
//!
//! Because backward jumps target `BlockId(1)` (never `PC == 0` `AllocateSlots`),
//! `Codegen` automatically emits a cooperative `Safepoint` at `BlockId(1)` and the
//! resulting loop is directly eligible for Tier-2 native loop JIT compilation.

use type_pool::{Intrinsic, TypeIndex, TypePool};

use crate::opt::cfg::remap_terminator_targets;
use crate::opt::purity::stmt_uses_handlers_or_defer;
use crate::opt::wrap_expr_preserving_scope;
use crate::{
    BasicBlock, BlockId, NirExpr, NirFunction, NirLocal, NirParamRole, NirStmt, NirValue,
    Terminator,
};

pub(crate) fn eliminate_self_tail_recursion(func: &mut NirFunction, pool: &TypePool) -> bool {
    if func.is_closure || func.blocks.is_empty() {
        return false;
    }
    if func.params.iter().any(|p| p.role != NirParamRole::User) {
        return false;
    }
    let has_typed_param = func
        .params
        .iter()
        .any(|p| p.type_index != TypeIndex::INVALID && p.type_index != Intrinsic::Any.type_index());
    if has_typed_param && func.entry_scope.is_none() {
        return false;
    }
    for block in &func.blocks {
        for stmt in &block.stmts {
            if stmt_uses_handlers_or_defer(stmt) {
                return false;
            }
        }
    }

    // Locate all blocks ending with a self-tail-call + Return.
    let mut tail_sites: Vec<(usize, usize, Vec<NirValue>)> = Vec::new();
    for (b_idx, block) in func.blocks.iter().enumerate() {
        if let Some((call_stmt_idx, args)) = match_self_tail_call(func, block, pool) {
            tail_sites.push((b_idx, call_stmt_idx, args));
        }
    }
    if tail_sites.is_empty() {
        return false;
    }

    // Allocate staging locals `stage_0 .. stage_{P-1}` for parallel parameter updates.
    let param_count = func.params.len();
    let stage_base = func.local_count;
    func.local_count += param_count as u32;

    // Shift all existing blocks `0..B` -> `1..=B` and insert prologue `BlockId(0) -> Goto(BlockId(1))`.
    let old_len = func.blocks.len();
    let shift_map: Vec<u32> = (0..old_len).map(|i| (i + 1) as u32).collect();
    for block in &mut func.blocks {
        block.id = BlockId(block.id.0 + 1);
        remap_terminator_targets(&mut block.terminator, &shift_map);
    }
    func.blocks.insert(
        0,
        BasicBlock {
            id: BlockId(0),
            stmts: Vec::new(),
            terminator: Terminator::Goto(BlockId(1)),
        },
    );
    func.entry_block = BlockId(0);

    let loop_header = BlockId(1);
    for (old_b_idx, call_stmt_idx, args) in tail_sites {
        let b_idx = old_b_idx + 1;
        // Truncate from `call_stmt_idx` (removing the self-call and any trivial copy to return local).
        func.blocks[b_idx].stmts.truncate(call_stmt_idx);

        // 1. Stage new argument values in parallel (checking gradual parameter assertions if needed).
        for (i, (&arg_val, param)) in args.iter().zip(&func.params).enumerate() {
            let stage_loc = NirLocal(stage_base + i as u32);
            let expr = if param.type_index != TypeIndex::INVALID
                && param.type_index != Intrinsic::Any.type_index()
            {
                wrap_expr_preserving_scope(
                    NirExpr::TypeAssert(arg_val, param.type_index),
                    func.entry_scope,
                )
            } else {
                NirExpr::Use(arg_val)
            };
            func.blocks[b_idx]
                .stmts
                .push(NirStmt::Assign(stage_loc, expr));
        }
        // 2. Commit staged values into parameter locals.
        for (i, param) in func.params.iter().enumerate() {
            let stage_loc = NirLocal(stage_base + i as u32);
            func.blocks[b_idx].stmts.push(NirStmt::Assign(
                param.local,
                NirExpr::Use(NirValue::Local(stage_loc)),
            ));
        }
        // 3. Jump back to `BlockId(1)` loop header.
        func.blocks[b_idx].terminator = Terminator::Goto(loop_header);
    }

    true
}

fn same_return_type(pool: &TypePool, a: TypeIndex, b: TypeIndex) -> bool {
    a == b
        || (a != TypeIndex::INVALID
            && b != TypeIndex::INVALID
            && pool.canonical_type(a).is_some()
            && pool.canonical_type(a) == pool.canonical_type(b))
}

fn resolve_return_local(
    func: &NirFunction,
    term: &Terminator,
    pool: &TypePool,
) -> Option<NirLocal> {
    match *term {
        Terminator::Return(NirValue::Local(loc)) => Some(loc),
        Terminator::Goto(mut target) => {
            for _ in 0..3 {
                let tb = func.blocks.get(target.0 as usize)?;
                match tb.terminator {
                    Terminator::Return(NirValue::Local(mut ret_loc)) => {
                        for stmt in tb.stmts.iter().rev() {
                            let NirStmt::Assign(dst, expr) = stmt.unscoped() else {
                                return None;
                            };
                            if *dst != ret_loc {
                                return None;
                            }
                            match expr.unscoped() {
                                NirExpr::Use(NirValue::Local(src)) => {
                                    ret_loc = *src;
                                }
                                NirExpr::TypeAssert(NirValue::Local(src), ty)
                                | NirExpr::TypeCast(NirValue::Local(src), ty)
                                    if same_return_type(pool, *ty, func.return_type) =>
                                {
                                    ret_loc = *src;
                                }
                                _ => return None,
                            }
                        }
                        return Some(ret_loc);
                    }
                    Terminator::Goto(next) if tb.stmts.is_empty() => {
                        target = next;
                    }
                    _ => return None,
                }
            }
            None
        }
        _ => None,
    }
}

fn match_self_tail_call(
    func: &NirFunction,
    block: &BasicBlock,
    pool: &TypePool,
) -> Option<(usize, Vec<NirValue>)> {
    let mut ret_loc = resolve_return_local(func, &block.terminator, pool)?;
    if block.stmts.is_empty() {
        return None;
    }

    // Walk backwards across zero or more pure `Use(Local(...))` / return `TypeAssert` copies
    // at the end of `block.stmts` to find the producing `Call(func_id, args)`.
    let mut idx = block.stmts.len();
    while idx > 0 {
        idx -= 1;
        let NirStmt::Assign(dst, expr) = block.stmts[idx].unscoped() else {
            return None;
        };
        if *dst != ret_loc {
            return None;
        }
        match expr.unscoped() {
            NirExpr::Use(NirValue::Local(src)) => {
                ret_loc = *src;
            }
            NirExpr::TypeAssert(NirValue::Local(src), ty)
            | NirExpr::TypeCast(NirValue::Local(src), ty)
                if same_return_type(pool, *ty, func.return_type) =>
            {
                ret_loc = *src;
            }
            NirExpr::Call(callee_id, args)
                if *callee_id == func.func_id && args.len() == func.params.len() =>
            {
                return Some((idx, args.clone()));
            }
            _ => return None,
        }
    }
    None
}
