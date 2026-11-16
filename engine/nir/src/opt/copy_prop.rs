//! Forward copy and constant propagation across basic blocks.
//!
//! Computes a conservative dataflow meet of `NirLocal -> NirValue` bindings across
//! CFG predecessors, propagating constants and copy chains (`x = Use(y)`) while
//! invalidating any mapping whose source local is overwritten.

use std::collections::HashMap;

use crate::opt::cfg::CfgInfo;
use crate::opt::{for_each_stmt_value_mut, for_each_terminator_value_mut, nir_value_bits_eq};
use crate::{NirExpr, NirFunction, NirLocal, NirStmt, NirValue};

pub(crate) fn propagate_copies(func: &mut NirFunction) {
    let n = func.blocks.len();
    if n == 0 {
        return;
    }

    // Fast path for single-def SSA-like temporaries + full iterative dataflow for multi-block locals.
    let cfg = CfgInfo::compute(func);
    let mut out_maps: Vec<Option<HashMap<NirLocal, NirValue>>> = vec![None; n];

    let mut changed = true;
    while changed {
        changed = false;
        for &bid in &cfg.rpo {
            let b_idx = bid.0 as usize;
            let mut env = if b_idx == 0 {
                HashMap::new()
            } else {
                meet_predecessor_maps(&cfg.preds[b_idx], &out_maps)
            };
            for stmt in &func.blocks[b_idx].stmts {
                step_transfer_stmt(stmt, &mut env);
            }
            let same = out_maps[b_idx]
                .as_ref()
                .is_some_and(|prev| maps_eq(prev, &env));
            if !same {
                out_maps[b_idx] = Some(env);
                changed = true;
            }
        }
    }

    // Rewrite every block using the converged entry map + intra-block transfer.
    for &bid in &cfg.rpo {
        let b_idx = bid.0 as usize;
        let mut env = if b_idx == 0 {
            HashMap::new()
        } else {
            meet_predecessor_maps(&cfg.preds[b_idx], &out_maps)
        };
        for stmt in &mut func.blocks[b_idx].stmts {
            for_each_stmt_value_mut(stmt, |val| {
                if let NirValue::Local(loc) = *val
                    && let Some(&replacement) = env.get(&loc)
                {
                    *val = replacement;
                }
            });
            step_transfer_stmt(stmt, &mut env);
        }
        for_each_terminator_value_mut(&mut func.blocks[b_idx].terminator, |val| {
            if let NirValue::Local(loc) = *val
                && let Some(&replacement) = env.get(&loc)
            {
                *val = replacement;
            }
        });
    }
}

fn meet_predecessor_maps(
    preds: &[crate::BlockId],
    out_maps: &[Option<HashMap<NirLocal, NirValue>>],
) -> HashMap<NirLocal, NirValue> {
    let mut reachable_maps = preds
        .iter()
        .filter_map(|p| out_maps.get(p.0 as usize).and_then(|m| m.as_ref()));
    let Some(first) = reachable_maps.next() else {
        return HashMap::new();
    };
    let mut acc = first.clone();
    for other in reachable_maps {
        acc.retain(|k, v| other.get(k).is_some_and(|ov| nir_value_bits_eq(v, ov)));
        if acc.is_empty() {
            break;
        }
    }
    acc
}

fn maps_eq(a: &HashMap<NirLocal, NirValue>, b: &HashMap<NirLocal, NirValue>) -> bool {
    a.len() == b.len()
        && a.iter()
            .all(|(k, v)| b.get(k).is_some_and(|bv| nir_value_bits_eq(v, bv)))
}

fn step_transfer_stmt(stmt: &NirStmt, env: &mut HashMap<NirLocal, NirValue>) {
    let NirStmt::Assign(dst, expr) = stmt.unscoped() else {
        return;
    };
    let dst = *dst;
    // Invalidate both `dst`'s own value AND any alias `k -> Local(dst)` whose source was `dst`!
    env.remove(&dst);
    env.retain(|_, v| *v != NirValue::Local(dst));

    if let NirExpr::Use(src) = expr.unscoped() {
        let resolved_src = match *src {
            NirValue::Local(src_loc) => env.get(&src_loc).copied().unwrap_or(*src),
            other => other,
        };
        if resolved_src != NirValue::Local(dst) {
            env.insert(dst, resolved_src);
        }
    }
}
