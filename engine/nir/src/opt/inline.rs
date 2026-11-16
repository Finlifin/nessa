//! Call-graph non-recursive small-function inlining.
//!
//! Inlines direct `NirExpr::Call(callee_id, args)` sites whose callee is non-closure,
//! non-recursive (outside every call-graph cycle/SCC), free of handler/continuation/`defer`
//! frames, uses plain value parameters (`NirParamRole::User`), and fits within the tier's
//! statement/slot budget. Preserves gradual parameter type assertions and lexical scopes.

use std::collections::{HashMap, HashSet};

use nsbc::FuncId;
use type_pool::{Intrinsic, TypeIndex};

use crate::opt::cfg::{prune_and_renumber_blocks, remap_terminator_targets};
use crate::opt::purity::stmt_uses_handlers_or_defer;
use crate::opt::{
    for_each_stmt_value_mut, for_each_terminator_value_mut, wrap_expr_preserving_scope,
    wrap_stmt_preserving_scope,
};
use crate::{
    BasicBlock, BlockId, NirExpr, NirFunction, NirLocal, NirModule, NirParamRole, NirStmt,
    NirValue, Terminator,
};

#[derive(Clone, Copy)]
pub(crate) struct InlineConfig {
    pub max_callee_stmts: usize,
    pub max_caller_stmts: usize,
    pub rounds: usize,
}

impl InlineConfig {
    pub const O2: Self = Self {
        max_callee_stmts: 16,
        max_caller_stmts: 256,
        rounds: 1,
    };

    pub const O3: Self = Self {
        max_callee_stmts: 40,
        max_caller_stmts: 512,
        rounds: 2,
    };
}

const MAX_INLINE_LOCALS: u32 = 4000;

pub(crate) fn inline_functions(module: &mut NirModule, config: InlineConfig) {
    if module.functions.len() <= 1 {
        return;
    }

    for _ in 0..config.rounds {
        let recursive_ids = compute_recursive_func_ids(module);
        let mut eligible_callees: HashMap<FuncId, NirFunction> = HashMap::new();
        for func in &module.functions {
            if is_inline_candidate(func, &recursive_ids, config.max_callee_stmts) {
                eligible_callees.insert(func.func_id, func.clone());
            }
        }
        if eligible_callees.is_empty() {
            break;
        }

        let mut any_inlined = false;
        for caller in &mut module.functions {
            if inline_into_caller(caller, &eligible_callees, config.max_caller_stmts) {
                any_inlined = true;
            }
        }
        if !any_inlined {
            break;
        }
    }
}

fn function_stmt_count(func: &NirFunction) -> usize {
    func.blocks.iter().map(|b| b.stmts.len()).sum()
}

fn is_inline_candidate(
    func: &NirFunction,
    recursive_ids: &HashSet<FuncId>,
    max_stmts: usize,
) -> bool {
    if func.is_closure || func.blocks.is_empty() || recursive_ids.contains(&func.func_id) {
        return false;
    }
    if func.params.iter().any(|p| p.role != NirParamRole::User) {
        return false;
    }
    // Parameter type assertion requires `entry_scope` when any parameter is typed.
    let has_typed_param = func
        .params
        .iter()
        .any(|p| p.type_index != TypeIndex::INVALID && p.type_index != Intrinsic::Any.type_index());
    if has_typed_param && func.entry_scope.is_none() {
        return false;
    }
    if function_stmt_count(func) > max_stmts {
        return false;
    }
    for block in &func.blocks {
        for stmt in &block.stmts {
            if stmt_uses_handlers_or_defer(stmt) {
                return false;
            }
        }
    }
    true
}

fn inline_into_caller(
    caller: &mut NirFunction,
    eligible_callees: &HashMap<FuncId, NirFunction>,
    max_caller_stmts: usize,
) -> bool {
    let mut inlined_any = false;

    loop {
        if function_stmt_count(caller) >= max_caller_stmts {
            break;
        }
        let Some((b_idx, s_idx, dst_local, callee_id, args)) =
            find_next_inline_site(caller, eligible_callees)
        else {
            break;
        };
        let callee = &eligible_callees[&callee_id];
        if caller
            .local_count
            .saturating_add(callee.local_count)
            .saturating_add(4)
            > MAX_INLINE_LOCALS
        {
            break;
        }

        splice_callee_at_site(caller, b_idx, s_idx, dst_local, callee, &args);
        prune_and_renumber_blocks(caller);
        inlined_any = true;
    }

    inlined_any
}

fn find_next_inline_site(
    caller: &NirFunction,
    eligible_callees: &HashMap<FuncId, NirFunction>,
) -> Option<(usize, usize, NirLocal, FuncId, Vec<NirValue>)> {
    for (b_idx, block) in caller.blocks.iter().enumerate() {
        for (s_idx, stmt) in block.stmts.iter().enumerate() {
            let NirStmt::Assign(dst, expr) = stmt.unscoped() else {
                continue;
            };
            if let NirExpr::Call(callee_id, args) = expr.unscoped()
                && *callee_id != caller.func_id
                && let Some(callee) = eligible_callees.get(callee_id)
                && args.len() == callee.params.len()
                && caller.local_count + callee.local_count + 4 <= MAX_INLINE_LOCALS
            {
                return Some((b_idx, s_idx, *dst, *callee_id, args.clone()));
            }
        }
    }
    None
}

fn splice_callee_at_site(
    caller: &mut NirFunction,
    b_idx: usize,
    s_idx: usize,
    dst_local: NirLocal,
    callee: &NirFunction,
    args: &[NirValue],
) {
    let local_offset = caller.local_count;
    caller.local_count += callee.local_count;

    let cont_block_id = BlockId(caller.blocks.len() as u32);
    let callee_block_base = cont_block_id.0 + 1;

    // Split `caller.blocks[b_idx]` at `s_idx`: tail statements `[s_idx + 1 ..]` move to `cont_block`.
    let tail_stmts: Vec<NirStmt> = caller.blocks[b_idx].stmts.drain((s_idx + 1)..).collect();
    // Remove the `Call` statement itself at `s_idx`.
    caller.blocks[b_idx].stmts.pop();
    let orig_term = std::mem::replace(
        &mut caller.blocks[b_idx].terminator,
        Terminator::Goto(BlockId(callee_block_base)),
    );

    // Bind callee parameters in `caller.blocks[b_idx]` before jumping into callee entry block.
    for (param, &arg_val) in callee.params.iter().zip(args) {
        let mapped_param = NirLocal(local_offset + param.local.0);
        let init_expr = if param.type_index != TypeIndex::INVALID
            && param.type_index != Intrinsic::Any.type_index()
        {
            wrap_expr_preserving_scope(
                NirExpr::TypeAssert(arg_val, param.type_index),
                callee.entry_scope,
            )
        } else {
            NirExpr::Use(arg_val)
        };
        caller.blocks[b_idx]
            .stmts
            .push(NirStmt::Assign(mapped_param, init_expr));
    }

    caller.blocks.push(BasicBlock {
        id: cont_block_id,
        stmts: tail_stmts,
        terminator: orig_term,
    });

    let block_map: Vec<u32> = (0..callee.blocks.len())
        .map(|i| callee_block_base + i as u32)
        .collect();

    for (i, c_block) in callee.blocks.iter().enumerate() {
        let mut cloned_stmts = Vec::with_capacity(c_block.stmts.len() + 1);
        for stmt in &c_block.stmts {
            let mut s = stmt.clone();
            for_each_stmt_value_mut(&mut s, |val| {
                if let NirValue::Local(loc) = *val {
                    *val = NirValue::Local(NirLocal(local_offset + loc.0));
                }
            });
            if let NirStmt::Assign(lhs, expr) = s.unscoped_mut() {
                *lhs = NirLocal(local_offset + lhs.0);
                let taken = std::mem::replace(expr, NirExpr::Use(NirValue::Unit));
                *expr = wrap_expr_preserving_scope(taken, callee.entry_scope);
            }
            s = wrap_stmt_preserving_scope(s, callee.entry_scope);
            cloned_stmts.push(s);
        }

        let mut cloned_term = c_block.terminator.clone();
        for_each_terminator_value_mut(&mut cloned_term, |val| {
            if let NirValue::Local(loc) = *val {
                *val = NirValue::Local(NirLocal(local_offset + loc.0));
            }
        });

        let final_term = match cloned_term {
            Terminator::Return(ret_val) => {
                cloned_stmts.push(NirStmt::Assign(dst_local, NirExpr::Use(ret_val)));
                Terminator::Goto(cont_block_id)
            }
            mut other => {
                remap_terminator_targets(&mut other, &block_map);
                other
            }
        };

        caller.blocks.push(BasicBlock {
            id: BlockId(callee_block_base + i as u32),
            stmts: cloned_stmts,
            terminator: final_term,
        });
    }
}

/// Tarjan's strongly connected components over direct `Call`/`CallWithProof` edges
/// to identify all directly and mutually recursive functions.
fn compute_recursive_func_ids(module: &NirModule) -> HashSet<FuncId> {
    let mut adj: HashMap<FuncId, Vec<FuncId>> = HashMap::new();
    for func in &module.functions {
        let mut callees = Vec::new();
        for block in &func.blocks {
            for stmt in &block.stmts {
                if let NirStmt::Assign(_, expr) = stmt.unscoped()
                    && let NirExpr::Call(fid, _) | NirExpr::CallWithProof(fid, _) = expr.unscoped()
                {
                    callees.push(*fid);
                }
            }
        }
        callees.sort_by_key(|f| f.0);
        callees.dedup();
        adj.insert(func.func_id, callees);
    }

    struct Tarjan<'a> {
        adj: &'a HashMap<FuncId, Vec<FuncId>>,
        next_index: u32,
        indices: HashMap<FuncId, u32>,
        lowlink: HashMap<FuncId, u32>,
        stack: Vec<FuncId>,
        on_stack: HashSet<FuncId>,
        recursive: HashSet<FuncId>,
    }

    impl Tarjan<'_> {
        fn strongconnect(&mut self, v: FuncId) {
            let idx = self.next_index;
            self.next_index += 1;
            self.indices.insert(v, idx);
            self.lowlink.insert(v, idx);
            self.stack.push(v);
            self.on_stack.insert(v);

            if let Some(neighbors) = self.adj.get(&v) {
                for &w in neighbors {
                    if !self.adj.contains_key(&w) {
                        continue;
                    }
                    if !self.indices.contains_key(&w) {
                        self.strongconnect(w);
                        let lw = self.lowlink[&w];
                        let lv = self.lowlink[&v];
                        if lw < lv {
                            self.lowlink.insert(v, lw);
                        }
                    } else if self.on_stack.contains(&w) {
                        let iw = self.indices[&w];
                        let lv = self.lowlink[&v];
                        if iw < lv {
                            self.lowlink.insert(v, iw);
                        }
                    }
                }
            }

            if self.lowlink[&v] == self.indices[&v] {
                let mut scc = Vec::new();
                while let Some(w) = self.stack.pop() {
                    self.on_stack.remove(&w);
                    scc.push(w);
                    if w == v {
                        break;
                    }
                }
                if scc.len() > 1 || self.adj.get(&v).is_some_and(|ns| ns.contains(&v)) {
                    self.recursive.extend(scc);
                }
            }
        }
    }

    let mut tarjan = Tarjan {
        adj: &adj,
        next_index: 0,
        indices: HashMap::new(),
        lowlink: HashMap::new(),
        stack: Vec::new(),
        on_stack: HashSet::new(),
        recursive: HashSet::new(),
    };
    let mut all_ids: Vec<FuncId> = adj.keys().copied().collect();
    all_ids.sort_by_key(|f| f.0);
    for id in all_ids {
        if !tarjan.indices.contains_key(&id) {
            tarjan.strongconnect(id);
        }
    }
    tarjan.recursive
}
