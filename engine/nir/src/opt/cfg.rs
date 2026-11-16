//! Control-Flow Graph (CFG) analyses and transformations:
//! - Reverse post-order (RPO) & predecessor maps
//! - Cooper-Harvey-Kennedy iterative immediate dominators (`idom`) & dominance queries
//! - Natural loop discovery (`header`, back-edge latches, loop body blocks)
//! - Constant & same-target `Branch` folding, empty-block jump threading,
//!   single-predecessor `Goto` block merging, unreachable block pruning, and
//!   dense `BlockId(0..N)` renumbering.

use std::collections::{HashSet, VecDeque};

use crate::opt::terminator_successors;
use crate::{BasicBlock, BlockId, NirFunction, NirStmt, NirValue, Terminator};

/// Computed CFG metadata for a dense `NirFunction`.
pub(crate) struct CfgInfo {
    pub preds: Vec<Vec<BlockId>>,
    pub rpo: Vec<BlockId>,
    /// Immediate dominator of each block (`idom[0] == Some(BlockId(0))`, `None` if unreachable).
    pub idom: Vec<Option<BlockId>>,
}

impl CfgInfo {
    pub fn compute(func: &NirFunction) -> Self {
        let n = func.blocks.len();
        let mut preds = vec![Vec::new(); n];
        for block in &func.blocks {
            let b = block.id.0 as usize;
            if b >= n {
                continue;
            }
            for succ in terminator_successors(&block.terminator) {
                if (succ.0 as usize) < n {
                    preds[succ.0 as usize].push(block.id);
                }
            }
        }

        let mut visited = vec![false; n];
        let mut postorder = Vec::with_capacity(n);
        if n > 0 {
            dfs_postorder(func, 0, &mut visited, &mut postorder);
        }
        postorder.reverse();
        let rpo = postorder;

        let mut rpo_num = vec![usize::MAX; n];
        for (idx, &bid) in rpo.iter().enumerate() {
            rpo_num[bid.0 as usize] = idx;
        }

        // Cooper-Harvey-Kennedy fast iterative dominator algorithm.
        let mut idom: Vec<Option<BlockId>> = vec![None; n];
        if !rpo.is_empty() {
            idom[rpo[0].0 as usize] = Some(rpo[0]);
            let mut changed = true;
            while changed {
                changed = false;
                for &b in &rpo[1..] {
                    let b_idx = b.0 as usize;
                    let mut new_idom: Option<usize> = None;
                    for &p in &preds[b_idx] {
                        let p_idx = p.0 as usize;
                        if idom[p_idx].is_some() {
                            new_idom = Some(match new_idom {
                                None => p_idx,
                                Some(curr) => intersect_dom(&idom, &rpo_num, p_idx, curr),
                            });
                        }
                    }
                    if let Some(dom_idx) = new_idom {
                        let dom_bid = BlockId(dom_idx as u32);
                        if idom[b_idx] != Some(dom_bid) {
                            idom[b_idx] = Some(dom_bid);
                            changed = true;
                        }
                    }
                }
            }
        }

        Self { preds, rpo, idom }
    }

    /// Return true if block `a` dominates block `b` (reflexive: `dominates(a, a) == true`).
    pub fn dominates(&self, a: BlockId, mut b: BlockId) -> bool {
        let a_idx = a.0 as usize;
        if a_idx >= self.idom.len() || (b.0 as usize) >= self.idom.len() {
            return false;
        }
        if self.idom[a_idx].is_none() || self.idom[b.0 as usize].is_none() {
            return false;
        }
        loop {
            if a == b {
                return true;
            }
            let Some(parent) = self.idom[b.0 as usize] else {
                return false;
            };
            if parent == b {
                return false;
            }
            b = parent;
        }
    }

    /// Return children of each block in the immediate dominator tree.
    pub fn dom_tree_children(&self) -> Vec<Vec<BlockId>> {
        let n = self.idom.len();
        let mut children = vec![Vec::new(); n];
        for (idx, &dom_opt) in self.idom.iter().enumerate() {
            if let Some(dom) = dom_opt
                && dom.0 as usize != idx
            {
                children[dom.0 as usize].push(BlockId(idx as u32));
            }
        }
        children
    }
}

fn dfs_postorder(
    func: &NirFunction,
    curr: usize,
    visited: &mut [bool],
    postorder: &mut Vec<BlockId>,
) {
    visited[curr] = true;
    for succ in terminator_successors(&func.blocks[curr].terminator) {
        let s = succ.0 as usize;
        if s < func.blocks.len() && !visited[s] {
            dfs_postorder(func, s, visited, postorder);
        }
    }
    postorder.push(BlockId(curr as u32));
}

fn intersect_dom(
    idom: &[Option<BlockId>],
    rpo_num: &[usize],
    mut b1: usize,
    mut b2: usize,
) -> usize {
    while b1 != b2 {
        while rpo_num[b1] > rpo_num[b2] {
            b1 = idom[b1].expect("reachable predecessor has idom").0 as usize;
        }
        while rpo_num[b2] > rpo_num[b1] {
            b2 = idom[b2].expect("reachable predecessor has idom").0 as usize;
        }
    }
    b1
}

/// Natural loop descriptor discovered via back-edges `latch -> header` where `header` dominates `latch`.
#[derive(Debug, Clone)]
pub(crate) struct NaturalLoop {
    pub header: BlockId,
    pub body: HashSet<BlockId>,
}

pub(crate) fn find_natural_loops(func: &NirFunction, cfg: &CfgInfo) -> Vec<NaturalLoop> {
    let mut header_latches: Vec<(BlockId, Vec<BlockId>)> = Vec::new();
    for block in &func.blocks {
        let b = block.id;
        if cfg.idom.get(b.0 as usize).copied().flatten().is_none() {
            continue;
        }
        for succ in terminator_successors(&block.terminator) {
            if cfg.dominates(succ, b) {
                if let Some(entry) = header_latches.iter_mut().find(|(h, _)| *h == succ) {
                    if !entry.1.contains(&b) {
                        entry.1.push(b);
                    }
                } else {
                    header_latches.push((succ, vec![b]));
                }
            }
        }
    }

    let mut loops = Vec::with_capacity(header_latches.len());
    for (header, latches) in header_latches {
        let mut body = HashSet::new();
        body.insert(header);
        let mut queue = VecDeque::new();
        for &latch in &latches {
            if body.insert(latch) {
                queue.push_back(latch);
            }
        }
        while let Some(curr) = queue.pop_front() {
            for &pred in &cfg.preds[curr.0 as usize] {
                if cfg.idom[pred.0 as usize].is_some() && body.insert(pred) {
                    queue.push_back(pred);
                }
            }
        }
        loops.push(NaturalLoop { header, body });
    }
    // Process inner (smaller) loops before outer loops.
    loops.sort_by_key(|lp| lp.body.len());
    loops
}

/// Simplify `func`'s CFG to a fixed point and densely renumber reachable blocks from `0`.
pub(crate) fn simplify_cfg(func: &mut NirFunction) {
    if func.blocks.is_empty() {
        return;
    }
    // Strip `Nop` statements first.
    for block in &mut func.blocks {
        block
            .stmts
            .retain(|s| !matches!(s.unscoped(), NirStmt::Nop));
    }

    loop {
        let mut changed = false;

        // 1. Fold constant or identical-target conditional branches into `Goto`.
        for block in &mut func.blocks {
            if let Terminator::Branch(cond, then_bb, else_bb) = block.terminator {
                if then_bb == else_bb {
                    block.terminator = Terminator::Goto(then_bb);
                    changed = true;
                } else if let NirValue::ConstBool(b) = cond {
                    block.terminator = Terminator::Goto(if b { then_bb } else { else_bb });
                    changed = true;
                }
            }
        }

        // 2. Thread jumps through empty `Goto` trampoline blocks (avoiding self-loops
        //    and preserving `entry_block == BlockId(0)`).
        let n = func.blocks.len();
        let mut redirect: Vec<Option<BlockId>> = vec![None; n];
        for (idx, block) in func.blocks.iter().enumerate() {
            if idx == 0 || !block.stmts.is_empty() {
                continue;
            }
            if let Terminator::Goto(target) = block.terminator
                && target.0 as usize != idx
            {
                redirect[idx] = Some(target);
            }
        }
        for i in 0..n {
            if redirect[i].is_some() {
                let mut curr = redirect[i].unwrap();
                let mut steps = 0;
                while let Some(next) = redirect.get(curr.0 as usize).copied().flatten() {
                    if next.0 as usize == i || steps >= n {
                        break;
                    }
                    curr = next;
                    steps += 1;
                }
                redirect[i] = Some(curr);
            }
        }
        for idx in 0..n {
            let block_id = BlockId(idx as u32);
            match &mut func.blocks[idx].terminator {
                Terminator::Goto(target) => {
                    if let Some(final_t) = redirect.get(target.0 as usize).copied().flatten()
                        && final_t != *target
                        && (final_t.0 > block_id.0 || target.0 <= block_id.0)
                    {
                        *target = final_t;
                        changed = true;
                    }
                }
                Terminator::Branch(_, then_bb, else_bb) => {
                    if let Some(final_t) = redirect.get(then_bb.0 as usize).copied().flatten()
                        && final_t != *then_bb
                        && (final_t.0 > block_id.0 || then_bb.0 <= block_id.0)
                    {
                        *then_bb = final_t;
                        changed = true;
                    }
                    if let Some(final_e) = redirect.get(else_bb.0 as usize).copied().flatten()
                        && final_e != *else_bb
                        && (final_e.0 > block_id.0 || else_bb.0 <= block_id.0)
                    {
                        *else_bb = final_e;
                        changed = true;
                    }
                }
                _ => {}
            }
        }

        // 3. Prune unreachable blocks and densely renumber `0..M`.
        if prune_and_renumber_blocks(func) {
            changed = true;
        }

        // 4. Merge block `A -> Goto(B)` when `B != 0` has `A` as its sole predecessor and `A != B`.
        if merge_single_predecessor_blocks(func) {
            prune_and_renumber_blocks(func);
            changed = true;
        }

        if !changed {
            break;
        }
    }
}

fn merge_single_predecessor_blocks(func: &mut NirFunction) -> bool {
    let n = func.blocks.len();
    let mut pred_counts = vec![0usize; n];
    for block in &func.blocks {
        match block.terminator {
            Terminator::Goto(t) => {
                if (t.0 as usize) < n {
                    pred_counts[t.0 as usize] += 1;
                }
            }
            Terminator::Branch(_, t, e) => {
                if (t.0 as usize) < n {
                    pred_counts[t.0 as usize] += 1;
                }
                if (e.0 as usize) < n {
                    pred_counts[e.0 as usize] += 1;
                }
            }
            _ => {}
        }
    }

    for i in 0..n {
        let Terminator::Goto(target) = func.blocks[i].terminator else {
            continue;
        };
        let t_idx = target.0 as usize;
        // Do not merge backward loop headers (`t_idx <= i`) into predecessors, so loop
        // safepoints and natural loop headers stay intact.
        if t_idx > i && t_idx < n && pred_counts[t_idx] == 1 {
            let mut succ_stmts = std::mem::take(&mut func.blocks[t_idx].stmts);
            let succ_term =
                std::mem::replace(&mut func.blocks[t_idx].terminator, Terminator::Unreachable);
            func.blocks[i].stmts.append(&mut succ_stmts);
            func.blocks[i].terminator = succ_term;
            return true;
        }
    }
    false
}

/// Remove blocks unreachable from `BlockId(0)` and renumber surviving blocks in RPO
/// (`BlockId(0)` always stays at index `0`). Returns `true` if the block list changed.
pub(crate) fn prune_and_renumber_blocks(func: &mut NirFunction) -> bool {
    let n = func.blocks.len();
    if n == 0 {
        return false;
    }
    let mut visited = vec![false; n];
    let mut postorder = Vec::with_capacity(n);
    dfs_postorder(func, 0, &mut visited, &mut postorder);
    postorder.reverse();
    let rpo = postorder;

    let is_identity = rpo.len() == n && rpo.iter().enumerate().all(|(i, b)| b.0 as usize == i);
    if is_identity {
        return false;
    }

    let mut old_to_new = vec![u32::MAX; n];
    for (new_idx, &old_bid) in rpo.iter().enumerate() {
        old_to_new[old_bid.0 as usize] = new_idx as u32;
    }

    let mut new_blocks = Vec::with_capacity(rpo.len());
    for (new_idx, &old_bid) in rpo.iter().enumerate() {
        let mut block: BasicBlock = func.blocks[old_bid.0 as usize].clone();
        block.id = BlockId(new_idx as u32);
        remap_terminator_targets(&mut block.terminator, &old_to_new);
        new_blocks.push(block);
    }
    func.blocks = new_blocks;
    func.entry_block = BlockId(0);
    true
}

pub(crate) fn remap_terminator_targets(term: &mut Terminator, old_to_new: &[u32]) {
    match term {
        Terminator::Goto(t) => {
            if let Some(&mapped) = old_to_new.get(t.0 as usize)
                && mapped != u32::MAX
            {
                *t = BlockId(mapped);
            }
        }
        Terminator::Branch(_, t, e) => {
            if let Some(&mapped) = old_to_new.get(t.0 as usize)
                && mapped != u32::MAX
            {
                *t = BlockId(mapped);
            }
            if let Some(&mapped) = old_to_new.get(e.0 as usize)
                && mapped != u32::MAX
            {
                *e = BlockId(mapped);
            }
        }
        Terminator::Return(_) | Terminator::MatchFail | Terminator::Unreachable => {}
    }
}
