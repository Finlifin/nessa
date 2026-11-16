//! Natural-loop preheader insertion and Loop-Invariant Code Motion (LICM).
//!
//! Hoists pure, trap-free loop-invariant `Assign(dst, expr)` computations into a
//! dedicated loop preheader placed immediately before the loop header so contiguous
//! loop PC ranges remain intact for Tier-2 JIT compilation.

use std::collections::{HashMap, HashSet};

use crate::opt::cfg::{CfgInfo, find_natural_loops, remap_terminator_targets};
use crate::opt::for_each_expr_read_local;
use crate::opt::purity::{is_pure_and_trap_free, stmt_uses_handlers_or_defer};
use crate::{BasicBlock, BlockId, NirExpr, NirFunction, NirLocal, NirStmt, Terminator};

pub(crate) fn hoist_loop_invariants(func: &mut NirFunction) {
    if func.blocks.len() < 2 {
        return;
    }
    // Preserve exact handler stack depth invariants by skipping functions with handler scopes.
    for block in &func.blocks {
        for stmt in &block.stmts {
            if stmt_uses_handlers_or_defer(stmt) {
                return;
            }
        }
    }

    // Iterate up to 8 loops to avoid quadratic blowup on deeply nested synthetic CFGs.
    let mut hoisted_headers: HashSet<BlockId> = HashSet::new();
    for _ in 0..8 {
        let cfg = CfgInfo::compute(func);
        let loops = find_natural_loops(func, &cfg);

        let mut selected = None;
        for lp in loops {
            // Never split `BlockId(0)` directly without a prologue; normal `while`/`for` headers have `id > 0`.
            if lp.header.0 == 0 || hoisted_headers.contains(&lp.header) {
                continue;
            }
            let candidates = find_hoistable_stmts(func, &lp.body);
            if !candidates.is_empty() {
                selected = Some((lp, candidates));
                break;
            }
        }

        let Some((lp, candidates)) = selected else {
            break;
        };
        let new_header = insert_preheader_and_hoist(func, &lp.header, &lp.body, candidates);
        hoisted_headers.insert(new_header);
    }
}

fn find_hoistable_stmts(func: &NirFunction, loop_body: &HashSet<BlockId>) -> Vec<(BlockId, usize)> {
    // Count assignments across the whole function so we only hoist truly single-assignment locals
    // that are never read before their definition.
    let mut global_defs: HashMap<NirLocal, usize> = HashMap::new();
    for p in &func.params {
        *global_defs.entry(p.local).or_default() += 1;
    }
    let mut loop_defined: HashSet<NirLocal> = HashSet::new();
    for block in &func.blocks {
        let in_loop = loop_body.contains(&block.id);
        for stmt in &block.stmts {
            if let NirStmt::Assign(dst, _) = stmt.unscoped() {
                *global_defs.entry(*dst).or_default() += 1;
                if in_loop {
                    loop_defined.insert(*dst);
                }
            }
        }
    }

    // Fixed-point discovery so chains of invariant temporaries (`t1 = a; t2 = t1 == null`) hoist together!
    let mut hoist_locals: HashSet<NirLocal> = HashSet::new();
    let mut hoist_positions: Vec<(BlockId, usize)> = Vec::new();
    let mut body_blocks: Vec<BlockId> = loop_body.iter().copied().collect();
    body_blocks.sort_by_key(|b| b.0);

    loop {
        let mut progress = false;
        for &bid in &body_blocks {
            let block = &func.blocks[bid.0 as usize];
            for (s_idx, stmt) in block.stmts.iter().enumerate() {
                let NirStmt::Assign(dst, expr) = stmt.unscoped() else {
                    continue;
                };
                if hoist_locals.contains(dst) {
                    continue;
                }
                if global_defs.get(dst).copied() != Some(1) {
                    continue;
                }
                // Do not hoist `NewClosure` out of loops in case closure identity/captures matter;
                // hoist pure trap-free non-allocating expressions (`TypeCheck`, `EnumIs`, `ErrorIsOk`, `Use`, const math).
                if matches!(expr.unscoped(), NirExpr::NewClosure(_, _))
                    || !is_pure_and_trap_free(expr)
                {
                    continue;
                }
                let mut operands_invariant = true;
                for_each_expr_read_local(expr, |loc| {
                    if loop_defined.contains(&loc) && !hoist_locals.contains(&loc) {
                        operands_invariant = false;
                    }
                });
                if operands_invariant {
                    hoist_locals.insert(*dst);
                    hoist_positions.push((bid, s_idx));
                    progress = true;
                }
            }
        }
        if !progress {
            break;
        }
    }

    hoist_positions
}

fn insert_preheader_and_hoist(
    func: &mut NirFunction,
    old_header: &BlockId,
    loop_body: &HashSet<BlockId>,
    candidates: Vec<(BlockId, usize)>,
) -> BlockId {
    let h_idx = old_header.0 as usize;

    // Extract hoisted statements in order, replacing their original slots with `Nop` first,
    // then stripping `Nop`s.
    let mut hoisted_stmts = Vec::with_capacity(candidates.len());
    for &(bid, s_idx) in &candidates {
        let taken = std::mem::replace(&mut func.blocks[bid.0 as usize].stmts[s_idx], NirStmt::Nop);
        hoisted_stmts.push(taken);
    }
    for block in &mut func.blocks {
        block
            .stmts
            .retain(|s| !matches!(s.unscoped(), NirStmt::Nop));
    }

    // Shift all block IDs `>= h_idx` up by `+1` so the preheader sits at `h_idx`
    // directly before the shifted header `h_idx + 1`.
    let old_len = func.blocks.len();
    let shift_map: Vec<u32> = (0..old_len)
        .map(|i| if i >= h_idx { (i + 1) as u32 } else { i as u32 })
        .collect();
    let new_header = BlockId((h_idx + 1) as u32);
    let preheader_id = BlockId(h_idx as u32);

    for (orig_idx, block) in func.blocks.iter_mut().enumerate() {
        let orig_bid = BlockId(orig_idx as u32);
        let in_loop = loop_body.contains(&orig_bid);
        block.id = BlockId(shift_map[orig_idx]);
        remap_terminator_targets(&mut block.terminator, &shift_map);

        // Outside-loop predecessors that targeted `old_header` (now `new_header`) jump to `preheader_id`!
        if !in_loop {
            match &mut block.terminator {
                Terminator::Goto(t) if *t == new_header => {
                    *t = preheader_id;
                }
                Terminator::Branch(_, t, e) => {
                    if *t == new_header {
                        *t = preheader_id;
                    }
                    if *e == new_header {
                        *e = preheader_id;
                    }
                }
                _ => {}
            }
        }
    }

    func.blocks.insert(
        h_idx,
        BasicBlock {
            id: preheader_id,
            stmts: hoisted_stmts,
            terminator: Terminator::Goto(new_header),
        },
    );
    new_header
}
