//! Whole-module Dead Function Elimination (DFE) and dense `FuncId(0..M)` relocation.
//!
//! Starting from `module.entry`, all `TypePool` method/trait/vtable table entries,
//! `derived_methods`, and `display_owner` functions, computes transitive reachability
//! over `Call`, `CallWithProof`, and `NewClosure`, drops unreachable functions (such as
//! unused standard-library free functions or fully-inlined helpers), and densely renumbers
//! surviving `FuncId`s across both `NirModule` and `TypePool`.

use std::collections::{HashMap, HashSet, VecDeque};

use nsbc::FuncId;
use type_pool::TypePool;

use crate::{NirExpr, NirFunction, NirModule, NirStmt};

pub(crate) fn eliminate_dead_functions(module: &mut NirModule, type_pool: &mut TypePool) {
    // Library compilations without `entry` retain all functions for external callers.
    let Some(entry_id) = module.entry else {
        return;
    };
    if module.functions.is_empty() {
        return;
    }

    let mut by_id: HashMap<u32, &NirFunction> = HashMap::with_capacity(module.functions.len());
    for f in &module.functions {
        by_id.insert(f.func_id.0, f);
    }

    let mut reachable: HashSet<u32> = HashSet::new();
    let mut queue: VecDeque<u32> = VecDeque::new();
    let enqueue = |id: u32, reachable: &mut HashSet<u32>, queue: &mut VecDeque<u32>| {
        if by_id.contains_key(&id) && reachable.insert(id) {
            queue.push_back(id);
        }
    };

    enqueue(entry_id.0, &mut reachable, &mut queue);
    for id in type_pool.referenced_function_ids() {
        enqueue(id, &mut reachable, &mut queue);
    }
    for dm in &module.derived_methods {
        enqueue(dm.func_id.0, &mut reachable, &mut queue);
    }
    for f in &module.functions {
        if f.display_owner.is_some() {
            enqueue(f.func_id.0, &mut reachable, &mut queue);
        }
    }

    while let Some(curr) = queue.pop_front() {
        let Some(&func) = by_id.get(&curr) else {
            continue;
        };
        for block in &func.blocks {
            for stmt in &block.stmts {
                let NirStmt::Assign(_, expr) = stmt.unscoped() else {
                    continue;
                };
                match expr.unscoped() {
                    NirExpr::Call(target, _)
                    | NirExpr::CallWithProof(target, _)
                    | NirExpr::NewClosure(target, _) => {
                        enqueue(target.0, &mut reachable, &mut queue);
                    }
                    _ => {}
                }
            }
        }
    }

    if reachable.len() == module.functions.len() {
        return;
    }

    module
        .functions
        .retain(|f| reachable.contains(&f.func_id.0));
    module.functions.sort_by_key(|f| f.func_id.0);

    let mut remap: HashMap<u32, u32> = HashMap::with_capacity(module.functions.len());
    for (new_idx, func) in module.functions.iter_mut().enumerate() {
        let new_id = new_idx as u32;
        remap.insert(func.func_id.0, new_id);
        func.func_id = FuncId(new_id);
    }

    for func in &mut module.functions {
        for block in &mut func.blocks {
            for stmt in &mut block.stmts {
                let NirStmt::Assign(_, expr) = stmt.unscoped_mut() else {
                    continue;
                };
                remap_expr_func_ids(expr, &remap);
            }
        }
    }

    module.entry = Some(FuncId(remap[&entry_id.0]));
    for dm in &mut module.derived_methods {
        if let Some(&new_id) = remap.get(&dm.func_id.0) {
            dm.func_id = FuncId(new_id);
        }
    }
    module
        .function_symbols
        .retain(|_, fid| remap.contains_key(&fid.0));
    for fid in module.function_symbols.values_mut() {
        *fid = FuncId(remap[&fid.0]);
    }

    type_pool
        .remap_function_ids(|old_id| remap.get(&old_id).copied())
        .expect("all TypePool method/vtable function IDs were retained in DFE roots");
}

fn remap_expr_func_ids(expr: &mut NirExpr, remap: &HashMap<u32, u32>) {
    match expr {
        NirExpr::ScopedCall { call, .. } => remap_expr_func_ids(call, remap),
        NirExpr::Call(fid, _) | NirExpr::CallWithProof(fid, _) | NirExpr::NewClosure(fid, _) => {
            if let Some(&new_id) = remap.get(&fid.0) {
                *fid = FuncId(new_id);
            }
        }
        _ => {}
    }
}
