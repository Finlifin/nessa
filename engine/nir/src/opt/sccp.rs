//! Wegman-Zadeck Sparse Conditional Constant Propagation (SCCP) adapted to
//! Nessa's block-structured NIR.
//!
//! Jointly propagates reachable CFG edges and per-local constant lattice values
//! (`Top -> Const(NirValue) -> Overdefined`), eliminating dead branches before
//! their unexecuted assignments can pollute downstream join points.

use std::collections::{HashSet, VecDeque};

use type_pool::TypePool;

use crate::opt::const_fold::try_eval_const_expr;
use crate::opt::{
    for_each_expr_value_mut, for_each_stmt_value_mut, for_each_terminator_value_mut,
    nir_value_bits_eq, split_scoped_expr, wrap_expr_preserving_scope,
};
use crate::{BlockId, NirExpr, NirFunction, NirStmt, NirValue, Terminator};

#[derive(Clone, Copy, Debug)]
enum Lattice {
    Top,
    Const(NirValue),
    Overdefined,
}

impl Lattice {
    fn meet(self, other: Self) -> Self {
        match (self, other) {
            (Self::Top, x) | (x, Self::Top) => x,
            (Self::Overdefined, _) | (_, Self::Overdefined) => Self::Overdefined,
            (Self::Const(a), Self::Const(b)) => {
                if nir_value_bits_eq(&a, &b) {
                    Self::Const(a)
                } else {
                    Self::Overdefined
                }
            }
        }
    }

    fn same(self, other: Self) -> bool {
        match (self, other) {
            (Self::Top, Self::Top) | (Self::Overdefined, Self::Overdefined) => true,
            (Self::Const(a), Self::Const(b)) => nir_value_bits_eq(&a, &b),
            _ => false,
        }
    }
}

pub(crate) fn run_sccp(func: &mut NirFunction, type_pool: &TypePool) {
    let num_blocks = func.blocks.len();
    let num_locals = func.local_count as usize;
    if num_blocks == 0 || num_locals == 0 {
        return;
    }

    let mut reachable = vec![false; num_blocks];
    let mut exec_edges: HashSet<(usize, usize)> = HashSet::new();
    let mut out_states: Vec<Vec<Lattice>> = vec![vec![Lattice::Top; num_locals]; num_blocks];
    let mut worklist: VecDeque<usize> = VecDeque::new();

    reachable[0] = true;
    worklist.push_back(0);

    let mut entry_init = vec![Lattice::Top; num_locals];
    for param in &func.params {
        if (param.local.0 as usize) < num_locals {
            entry_init[param.local.0 as usize] = Lattice::Overdefined;
        }
    }

    while let Some(b_idx) = worklist.pop_front() {
        let mut state = compute_in_state(b_idx, num_locals, &entry_init, &exec_edges, &out_states);
        for stmt in &func.blocks[b_idx].stmts {
            transfer_stmt(stmt, &mut state, type_pool);
        }

        let state_changed = out_states[b_idx]
            .iter()
            .zip(&state)
            .any(|(&old, &new)| !old.same(new));
        if state_changed {
            out_states[b_idx] = state.clone();
        }

        let next_targets: Vec<usize> = match func.blocks[b_idx].terminator {
            Terminator::Goto(t) => vec![t.0 as usize],
            Terminator::Branch(cond, t, e) => match eval_operand(cond, &state) {
                Some(NirValue::ConstBool(true)) => vec![t.0 as usize],
                Some(NirValue::ConstBool(false)) => vec![e.0 as usize],
                _ => vec![t.0 as usize, e.0 as usize],
            },
            Terminator::Return(_) | Terminator::MatchFail | Terminator::Unreachable => Vec::new(),
        };

        for succ in next_targets {
            if succ >= num_blocks {
                continue;
            }
            let new_edge = exec_edges.insert((b_idx, succ));
            if !reachable[succ] {
                reachable[succ] = true;
                worklist.push_back(succ);
            } else if new_edge || state_changed {
                worklist.push_back(succ);
            }
        }
    }

    // Materialize discovered constants and fold dead conditional branches in reachable blocks.
    for (b_idx, &is_reach) in reachable.iter().enumerate() {
        if !is_reach {
            continue;
        }
        let mut state = compute_in_state(b_idx, num_locals, &entry_init, &exec_edges, &out_states);
        for stmt in &mut func.blocks[b_idx].stmts {
            for_each_stmt_value_mut(stmt, |val| {
                if let NirValue::Local(loc) = *val
                    && let Some(Lattice::Const(c)) = state.get(loc.0 as usize).copied()
                {
                    *val = c;
                }
            });
            if let NirStmt::Assign(dst, expr) = stmt.unscoped_mut() {
                let dst_idx = dst.0 as usize;
                if let Some(c) = try_eval_const_expr(expr, type_pool) {
                    let (_, scope) =
                        split_scoped_expr(std::mem::replace(expr, NirExpr::Use(NirValue::Unit)));
                    *expr = wrap_expr_preserving_scope(NirExpr::Use(c), scope);
                    if dst_idx < num_locals {
                        state[dst_idx] = Lattice::Const(c);
                    }
                } else if dst_idx < num_locals {
                    state[dst_idx] = Lattice::Overdefined;
                }
            }
        }
        for_each_terminator_value_mut(&mut func.blocks[b_idx].terminator, |val| {
            if let NirValue::Local(loc) = *val
                && let Some(Lattice::Const(c)) = state.get(loc.0 as usize).copied()
            {
                *val = c;
            }
        });
        if let Terminator::Branch(NirValue::ConstBool(b), t, e) = func.blocks[b_idx].terminator {
            func.blocks[b_idx].terminator = Terminator::Goto(if b { t } else { e });
        }
    }

    // Any block proven unreachable can have its outgoing edges severed so `simplify_cfg` prunes it.
    for (b_idx, is_reach) in reachable.into_iter().enumerate() {
        if !is_reach && b_idx > 0 {
            func.blocks[b_idx].stmts.clear();
            func.blocks[b_idx].terminator = Terminator::Unreachable;
        }
    }
}

fn compute_in_state(
    b_idx: usize,
    num_locals: usize,
    entry_init: &[Lattice],
    exec_edges: &HashSet<(usize, usize)>,
    out_states: &[Vec<Lattice>],
) -> Vec<Lattice> {
    let mut state = if b_idx == 0 {
        entry_init.to_vec()
    } else {
        vec![Lattice::Top; num_locals]
    };
    for (pred, pred_out) in out_states.iter().enumerate() {
        if exec_edges.contains(&(pred, b_idx)) {
            for (slot, &pred_val) in state.iter_mut().zip(pred_out) {
                *slot = slot.meet(pred_val);
            }
        }
    }
    // Note: entry block may also have back-edges (though rare since AllocateSlots is before block 0).
    let _ = BlockId(b_idx as u32);
    state
}

fn eval_operand(val: NirValue, state: &[Lattice]) -> Option<NirValue> {
    match val {
        NirValue::Local(loc) => match state.get(loc.0 as usize).copied()? {
            Lattice::Const(c) => Some(c),
            Lattice::Top | Lattice::Overdefined => None,
        },
        other => Some(other),
    }
}

fn transfer_stmt(stmt: &NirStmt, state: &mut [Lattice], type_pool: &TypePool) {
    let NirStmt::Assign(dst, expr) = stmt.unscoped() else {
        return;
    };
    let dst_idx = dst.0 as usize;
    if dst_idx >= state.len() {
        return;
    }
    let mut substituted = expr.clone();
    let mut has_unknown_local = false;
    for_each_expr_value_mut(&mut substituted, |val| {
        if let NirValue::Local(loc) = *val {
            match state.get(loc.0 as usize).copied() {
                Some(Lattice::Const(c)) => *val = c,
                _ => has_unknown_local = true,
            }
        }
    });
    if !has_unknown_local && let Some(c) = try_eval_const_expr(&substituted, type_pool) {
        state[dst_idx] = Lattice::Const(c);
    } else {
        state[dst_idx] = Lattice::Overdefined;
    }
}
