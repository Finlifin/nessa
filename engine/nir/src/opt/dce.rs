//! Backward liveness Dead Assignment Elimination (DCE) and Chaitin-style
//! live-range interference graph coloring slot compaction.

use std::collections::HashSet;

use type_pool::{Intrinsic, TypeIndex, TypePool};

use crate::opt::purity::is_removable_when_unused;
use crate::opt::{for_each_stmt_value_mut, for_each_terminator_value_mut, terminator_successors};
use crate::{NirFunction, NirLocal, NirStmt, NirValue};

/// Eliminate dead pure/unused-allocation assignments to a fixed point.
pub(crate) fn eliminate_dead_code(func: &mut NirFunction, type_pool: &TypePool) {
    if func.blocks.is_empty() || func.local_count == 0 {
        return;
    }

    loop {
        let live_out = compute_block_live_out(func);
        let mut removed_any = false;

        for (b_idx, block) in func.blocks.iter_mut().enumerate() {
            let mut live = live_out[b_idx].clone();
            add_terminator_reads(&block.terminator, &mut live);

            let mut keep = vec![true; block.stmts.len()];
            for s_idx in (0..block.stmts.len()).rev() {
                let stmt = &block.stmts[s_idx];
                if matches!(stmt.unscoped(), NirStmt::Nop) {
                    keep[s_idx] = false;
                    removed_any = true;
                    continue;
                }
                if let NirStmt::Assign(dst, expr) = stmt.unscoped() {
                    if !live.contains(&dst.0) && is_removable_when_unused(expr, type_pool) {
                        keep[s_idx] = false;
                        removed_any = true;
                        continue;
                    }
                    live.remove(&dst.0);
                }
                add_stmt_reads(stmt, &mut live);
            }

            if keep.iter().any(|&k| !k) {
                let mut iter = keep.into_iter();
                block.stmts.retain(|_| iter.next().unwrap_or(true));
            }
        }

        if !removed_any {
            break;
        }
    }
}

/// Eliminate dead assignments and compact `NirLocal` frame slots via live-range
/// interference graph coloring (keeping parameter slots pinned in `0..param_ceiling`).
pub(crate) fn eliminate_dead_code_and_compact_slots(func: &mut NirFunction, type_pool: &TypePool) {
    eliminate_dead_code(func, type_pool);
    compact_local_slots(func, type_pool);
}

fn compact_local_slots(func: &mut NirFunction, type_pool: &TypePool) {
    let total = func.local_count as usize;
    let param_ceiling = func.params.iter().map(|p| p.local.0 + 1).max().unwrap_or(0) as usize;
    if total <= param_ceiling {
        func.local_count = param_ceiling as u32;
        return;
    }

    // Find all non-parameter locals (`>= param_ceiling`) actually referenced anywhere in `func`.
    let mut used_temps = vec![false; total];
    for block in &func.blocks {
        for stmt in &block.stmts {
            if let NirStmt::Assign(dst, _) = stmt.unscoped()
                && (dst.0 as usize) >= param_ceiling
                && (dst.0 as usize) < total
            {
                used_temps[dst.0 as usize] = true;
            }
            let mut tmp = stmt.clone();
            for_each_stmt_value_mut(&mut tmp, |v| {
                if let NirValue::Local(loc) = *v
                    && (loc.0 as usize) >= param_ceiling
                    && (loc.0 as usize) < total
                {
                    used_temps[loc.0 as usize] = true;
                }
            });
        }
        let mut tcopy = block.terminator.clone();
        for_each_terminator_value_mut(&mut tcopy, |v| {
            if let NirValue::Local(loc) = *v
                && (loc.0 as usize) >= param_ceiling
                && (loc.0 as usize) < total
            {
                used_temps[loc.0 as usize] = true;
            }
        });
    }

    let active_temps: Vec<usize> = (param_ceiling..total)
        .filter(|&loc| used_temps[loc])
        .collect();
    if active_temps.is_empty() {
        func.local_count = param_ceiling as u32;
        return;
    }

    // Partition temporaries by scalar representation class so Tier-2 loop specialization
    // never sees a single slot shared between incompatible scalar types (e.g. `bool` and `f64`).
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum SlotClass {
        Unknown,
        I64,
        U64,
        F64,
        Bool,
        Other,
        Mixed,
    }
    let classify_type = |ty: TypeIndex| -> SlotClass {
        let canon = type_pool.canonical_type(ty).unwrap_or(ty);
        match Intrinsic::from_type_index(canon) {
            Some(Intrinsic::I64 | Intrinsic::Isize) => SlotClass::I64,
            Some(Intrinsic::U64 | Intrinsic::Usize) => SlotClass::U64,
            Some(Intrinsic::F64) => SlotClass::F64,
            Some(Intrinsic::Bool) => SlotClass::Bool,
            Some(_) | None => SlotClass::Other,
        }
    };
    let mut temp_class = vec![SlotClass::Unknown; total];
    for p in &func.params {
        let idx = p.local.0 as usize;
        if idx < total {
            temp_class[idx] = classify_type(p.type_index);
        }
    }
    for _pass in 0..2 {
        for block in &func.blocks {
            for stmt in &block.stmts {
                let NirStmt::Assign(dst, expr) = stmt.unscoped() else {
                    continue;
                };
                let d = dst.0 as usize;
                if d >= total {
                    continue;
                }
                let inferred = match expr.unscoped() {
                    crate::NirExpr::Use(v) => match *v {
                        NirValue::ConstInt(_) => SlotClass::I64,
                        NirValue::ConstUInt(_) => SlotClass::U64,
                        NirValue::ConstFloat(_) => SlotClass::F64,
                        NirValue::ConstBool(_) => SlotClass::Bool,
                        NirValue::Local(src) => temp_class
                            .get(src.0 as usize)
                            .copied()
                            .unwrap_or(SlotClass::Other),
                        _ => SlotClass::Other,
                    },
                    crate::NirExpr::BinOp(op, a, b) => match op {
                        crate::BinOp::Eq
                        | crate::BinOp::Ne
                        | crate::BinOp::Lt
                        | crate::BinOp::Le
                        | crate::BinOp::Gt
                        | crate::BinOp::Ge => SlotClass::Bool,
                        _ => {
                            let ca = match *a {
                                NirValue::ConstFloat(_) => SlotClass::F64,
                                NirValue::ConstInt(_) => SlotClass::I64,
                                NirValue::ConstUInt(_) => SlotClass::U64,
                                NirValue::Local(l) => temp_class
                                    .get(l.0 as usize)
                                    .copied()
                                    .unwrap_or(SlotClass::Unknown),
                                _ => SlotClass::Other,
                            };
                            let cb = match *b {
                                NirValue::ConstFloat(_) => SlotClass::F64,
                                NirValue::ConstInt(_) => SlotClass::I64,
                                NirValue::ConstUInt(_) => SlotClass::U64,
                                NirValue::Local(l) => temp_class
                                    .get(l.0 as usize)
                                    .copied()
                                    .unwrap_or(SlotClass::Unknown),
                                _ => SlotClass::Other,
                            };
                            if ca == SlotClass::F64 || cb == SlotClass::F64 {
                                SlotClass::F64
                            } else if ca != SlotClass::Unknown {
                                ca
                            } else {
                                cb
                            }
                        }
                    },
                    crate::NirExpr::UnaryOp(crate::UnaryOp::Not, _)
                    | crate::NirExpr::TypeCheck(_, _)
                    | crate::NirExpr::EnumIs(_, _, _)
                    | crate::NirExpr::ErrorIsOk(_) => SlotClass::Bool,
                    crate::NirExpr::UnaryOp(_, a) => match *a {
                        NirValue::ConstFloat(_) => SlotClass::F64,
                        NirValue::ConstInt(_) => SlotClass::I64,
                        NirValue::Local(l) => temp_class
                            .get(l.0 as usize)
                            .copied()
                            .unwrap_or(SlotClass::Unknown),
                        _ => SlotClass::Other,
                    },
                    crate::NirExpr::TypeCast(_, ty) | crate::NirExpr::TypeAssert(_, ty) => {
                        classify_type(*ty)
                    }
                    _ => SlotClass::Other,
                };
                if inferred != SlotClass::Unknown {
                    temp_class[d] = match temp_class[d] {
                        SlotClass::Unknown => inferred,
                        prev if prev == inferred => prev,
                        _ => SlotClass::Mixed,
                    };
                }
            }
        }
    }

    // For very large functions (>1024 temporaries), perform linear dense compaction `O(L)`
    // to avoid quadratic interference matrix memory; otherwise run full Chaitin coloring!
    if active_temps.len() > 1024 {
        let mut remap = vec![0u32; total];
        for (i, r) in remap.iter_mut().enumerate().take(param_ceiling) {
            *r = i as u32;
        }
        for (offset, &old_loc) in active_temps.iter().enumerate() {
            remap[old_loc] = (param_ceiling + offset) as u32;
        }
        apply_slot_remap(func, &remap, (param_ceiling + active_temps.len()) as u32);
        return;
    }

    let live_out = compute_block_live_out(func);
    let mut adj: Vec<HashSet<usize>> = vec![HashSet::new(); total];

    let record_clique_with = |x: usize, others: &HashSet<u32>, adj: &mut [HashSet<usize>]| {
        if x < param_ceiling || x >= total {
            return;
        }
        for &y_u32 in others {
            let y = y_u32 as usize;
            if y >= param_ceiling && y < total && y != x {
                adj[x].insert(y);
                adj[y].insert(x);
            }
        }
    };

    for (b_idx, block) in func.blocks.iter().enumerate() {
        let mut live: HashSet<u32> = live_out[b_idx]
            .iter()
            .copied()
            .filter(|&l| (l as usize) >= param_ceiling)
            .collect();
        add_terminator_temp_reads(&block.terminator, param_ceiling, &mut live);
        // All temps simultaneously live at terminator interfere with one another.
        let live_vec: Vec<u32> = live.iter().copied().collect();
        for &l in &live_vec {
            record_clique_with(l as usize, &live, &mut adj);
        }

        for stmt in block.stmts.iter().rev() {
            if let NirStmt::Assign(dst, _) = stmt.unscoped() {
                let d = dst.0 as usize;
                // Chaitin's rule: `dst` interferes with everything live right after its definition!
                record_clique_with(d, &live, &mut adj);
                live.remove(&dst.0);
            }
            add_stmt_temp_reads(stmt, param_ceiling, &mut live);
            let cur_vec: Vec<u32> = live.iter().copied().collect();
            for &l in &cur_vec {
                record_clique_with(l as usize, &live, &mut adj);
            }
        }
    }

    // Greedy Welsh-Powell coloring (highest interference degree first, deterministic tie-break).
    let mut order = active_temps;
    order.sort_by(|&a, &b| adj[b].len().cmp(&adj[a].len()).then_with(|| a.cmp(&b)));

    let mut color_of: Vec<Option<u32>> = vec![None; total];
    let mut color_class: std::collections::HashMap<u32, SlotClass> =
        std::collections::HashMap::new();
    let mut max_color: Option<u32> = None;
    let mut forbidden = HashSet::new();
    for loc in order {
        forbidden.clear();
        for &nbr in &adj[loc] {
            if let Some(c) = color_of[nbr] {
                forbidden.insert(c);
            }
        }
        let my_class = temp_class[loc];
        let mut c = param_ceiling as u32;
        while forbidden.contains(&c)
            || (my_class == SlotClass::Mixed && color_class.contains_key(&c))
            || color_class
                .get(&c)
                .is_some_and(|&cc| cc == SlotClass::Mixed || cc != my_class)
        {
            c += 1;
        }
        color_of[loc] = Some(c);
        color_class.insert(c, my_class);
        max_color = Some(max_color.map_or(c, |m| m.max(c)));
    }

    let mut remap = vec![0u32; total];
    for (i, r) in remap.iter_mut().enumerate().take(param_ceiling) {
        *r = i as u32;
    }
    for loc in param_ceiling..total {
        if let Some(c) = color_of[loc] {
            remap[loc] = c;
        }
    }
    let new_local_count = max_color.map_or(param_ceiling as u32, |m| m + 1);
    apply_slot_remap(func, &remap, new_local_count);
}

fn apply_slot_remap(func: &mut NirFunction, remap: &[u32], new_local_count: u32) {
    for block in &mut func.blocks {
        for stmt in &mut block.stmts {
            for_each_stmt_value_mut(stmt, |val| {
                if let NirValue::Local(loc) = *val
                    && let Some(&mapped) = remap.get(loc.0 as usize)
                {
                    *val = NirValue::Local(NirLocal(mapped));
                }
            });
            if let NirStmt::Assign(dst, _) = stmt.unscoped_mut()
                && let Some(&mapped) = remap.get(dst.0 as usize)
            {
                *dst = NirLocal(mapped);
            }
        }
        for_each_terminator_value_mut(&mut block.terminator, |val| {
            if let NirValue::Local(loc) = *val
                && let Some(&mapped) = remap.get(loc.0 as usize)
            {
                *val = NirValue::Local(NirLocal(mapped));
            }
        });
    }
    func.local_count = new_local_count;
}

fn compute_block_live_out(func: &NirFunction) -> Vec<HashSet<u32>> {
    let n = func.blocks.len();
    let mut use_sets = vec![HashSet::new(); n];
    let mut def_sets = vec![HashSet::new(); n];

    for (b_idx, block) in func.blocks.iter().enumerate() {
        for stmt in &block.stmts {
            let mut reads = HashSet::new();
            add_stmt_reads(stmt, &mut reads);
            for r in reads {
                if !def_sets[b_idx].contains(&r) {
                    use_sets[b_idx].insert(r);
                }
            }
            if let NirStmt::Assign(dst, _) = stmt.unscoped() {
                def_sets[b_idx].insert(dst.0);
            }
        }
        let mut term_reads = HashSet::new();
        add_terminator_reads(&block.terminator, &mut term_reads);
        for r in term_reads {
            if !def_sets[b_idx].contains(&r) {
                use_sets[b_idx].insert(r);
            }
        }
    }

    let mut live_in = vec![HashSet::new(); n];
    let mut live_out = vec![HashSet::new(); n];
    let mut changed = true;
    while changed {
        changed = false;
        for b_idx in (0..n).rev() {
            let mut new_out = HashSet::new();
            for succ in terminator_successors(&func.blocks[b_idx].terminator) {
                if (succ.0 as usize) < n {
                    new_out.extend(&live_in[succ.0 as usize]);
                }
            }
            let mut new_in = use_sets[b_idx].clone();
            for &v in &new_out {
                if !def_sets[b_idx].contains(&v) {
                    new_in.insert(v);
                }
            }
            if new_out != live_out[b_idx] || new_in != live_in[b_idx] {
                live_out[b_idx] = new_out;
                live_in[b_idx] = new_in;
                changed = true;
            }
        }
    }
    live_out
}

fn add_stmt_reads(stmt: &NirStmt, out: &mut HashSet<u32>) {
    let mut cloned = stmt.clone();
    for_each_stmt_value_mut(&mut cloned, |v| {
        if let NirValue::Local(loc) = *v {
            out.insert(loc.0);
        }
    });
}

fn add_terminator_reads(term: &crate::Terminator, out: &mut HashSet<u32>) {
    let mut cloned = term.clone();
    for_each_terminator_value_mut(&mut cloned, |v| {
        if let NirValue::Local(loc) = *v {
            out.insert(loc.0);
        }
    });
}

fn add_stmt_temp_reads(stmt: &NirStmt, param_ceiling: usize, out: &mut HashSet<u32>) {
    let mut cloned = stmt.clone();
    for_each_stmt_value_mut(&mut cloned, |v| {
        if let NirValue::Local(loc) = *v
            && (loc.0 as usize) >= param_ceiling
        {
            out.insert(loc.0);
        }
    });
}

fn add_terminator_temp_reads(
    term: &crate::Terminator,
    param_ceiling: usize,
    out: &mut HashSet<u32>,
) {
    let mut cloned = term.clone();
    for_each_terminator_value_mut(&mut cloned, |v| {
        if let NirValue::Local(loc) = *v
            && (loc.0 as usize) >= param_ceiling
        {
            out.insert(loc.0);
        }
    });
}
