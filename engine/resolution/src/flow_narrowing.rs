//! Flow-sensitive control-flow type narrowing, deferred `null` local binding
//! inference, and local call-site parameter inference for unannotated lambdas.

use std::collections::{HashMap, HashSet};

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::resolver::Resolver;
use crate::{SymbolId, SymbolKind};

/// Pre-scan `root` to find local symbols that are mutated (`=`, `+=`, etc.) inside
/// any closure (`Lambda` / `CaseMap` / `CaseAlternative`). Such variables can be
/// mutated asynchronously or across arbitrary calls, so they are excluded from
/// lexical flow narrowing.
pub(crate) fn collect_closure_mutated_symbols(
    r: &Resolver<'_>,
    ast: &Ast,
    root: NodeIndex,
) -> HashSet<SymbolId> {
    let mut out = HashSet::new();
    walk_closure_mutations(r, ast, root, None, &mut out);
    out
}

fn walk_closure_mutations(
    r: &Resolver<'_>,
    ast: &Ast,
    root: NodeIndex,
    initial_closure: Option<NodeIndex>,
    out: &mut HashSet<SymbolId>,
) {
    let mut stack = vec![(root, initial_closure)];
    while let Some((node, enclosing_closure)) = stack.pop() {
        if node.is_null() {
            continue;
        }
        let kind = ast.node(node).kind;
        let next_closure = if matches!(
            kind,
            NodeKind::Lambda | NodeKind::CaseMap | NodeKind::CaseAlternative
        ) {
            Some(node)
        } else {
            enclosing_closure
        };

        if let Some(closure_node) = next_closure
            && matches!(
                kind,
                NodeKind::Assign
                    | NodeKind::AddAssign
                    | NodeKind::SubAssign
                    | NodeKind::MulAssign
                    | NodeKind::DivAssign
                    | NodeKind::ModAssign
            )
        {
            let lhs = ast.fixed_children(node)[0];
            if !lhs.is_null()
                && ast.node(lhs).kind == NodeKind::Id
                && let Some(&sym_id) = r.node_symbols.get(&lhs)
            {
                let def_scope = r.symbols[sym_id.0 as usize].scope;
                if !scope_is_within_node(r, def_scope, closure_node) {
                    out.insert(sym_id);
                }
            }
        }

        for &c in ast.fixed_children(node) {
            stack.push((c, next_closure));
        }
        for &c in ast.multi_children(node) {
            stack.push((c, next_closure));
        }
    }
}

fn scope_is_within_node(r: &Resolver<'_>, mut scope: crate::ScopeId, owner: NodeIndex) -> bool {
    loop {
        let entry = &r.scopes[scope.0 as usize];
        if entry.node == owner {
            return true;
        }
        let Some(parent) = entry.parent else {
            return false;
        };
        scope = parent;
    }
}

/// Collect all symbols directly assigned (`=`, `+=`, ...) anywhere within `subtree`
/// (excluding nested `FunctionDef` declarations).
pub(crate) fn collect_mutated_symbols_in_subtree(
    r: &Resolver<'_>,
    ast: &Ast,
    subtree: NodeIndex,
    out: &mut HashSet<SymbolId>,
) {
    let mut stack = vec![subtree];
    while let Some(node) = stack.pop() {
        if node.is_null() || ast.node(node).kind == NodeKind::FunctionDef {
            continue;
        }
        if matches!(
            ast.node(node).kind,
            NodeKind::Assign
                | NodeKind::AddAssign
                | NodeKind::SubAssign
                | NodeKind::MulAssign
                | NodeKind::DivAssign
                | NodeKind::ModAssign
        ) {
            let lhs = ast.fixed_children(node)[0];
            if !lhs.is_null()
                && ast.node(lhs).kind == NodeKind::Id
                && let Some(&sym_id) = r.node_symbols.get(&lhs)
            {
                out.insert(sym_id);
            }
        }
        for &c in ast.fixed_children(node) {
            stack.push(c);
        }
        for &c in ast.multi_children(node) {
            stack.push(c);
        }
    }
}

/// Extract `(when_true, when_false)` narrowing facts from an already type-resolved
/// boolean condition expression `cond`.
pub(crate) fn extract_condition_facts(
    r: &mut Resolver<'_>,
    ast: &Ast,
    cond: NodeIndex,
) -> (HashMap<SymbolId, TypeIndex>, HashMap<SymbolId, TypeIndex>) {
    if cond.is_null() {
        return (HashMap::new(), HashMap::new());
    }
    match ast.node(cond).kind {
        NodeKind::BoolNot => {
            let inner = ast.fixed_children(cond)[0];
            let (t, f) = extract_condition_facts(r, ast, inner);
            (f, t)
        }
        NodeKind::BoolAnd => {
            let ch = ast.fixed_children(cond);
            let (mut t1, _) = extract_condition_facts(r, ast, ch[0]);
            let (t2, _) = extract_condition_facts(r, ast, ch[1]);
            t1.extend(t2);
            (t1, HashMap::new())
        }
        NodeKind::BoolOr => {
            let ch = ast.fixed_children(cond);
            let (_, mut f1) = extract_condition_facts(r, ast, ch[0]);
            let (_, f2) = extract_condition_facts(r, ast, ch[1]);
            f1.extend(f2);
            (HashMap::new(), f1)
        }
        NodeKind::BoolEq | NodeKind::BoolNotEq => {
            let ch = ast.fixed_children(cond);
            let is_not_eq = ast.node(cond).kind == NodeKind::BoolNotEq;
            if let Some((sym_id, non_null_ty)) = null_comparison_target(r, ast, ch[0], ch[1])
                .or_else(|| null_comparison_target(r, ast, ch[1], ch[0]))
                && !r.closure_mutated_symbols.contains(&sym_id)
            {
                let mut map = HashMap::new();
                map.insert(sym_id, non_null_ty);
                if is_not_eq {
                    return (map, HashMap::new());
                } else {
                    return (HashMap::new(), map);
                }
            }
            (HashMap::new(), HashMap::new())
        }
        NodeKind::BoolMatches => {
            let ch = ast.fixed_children(cond);
            if let Some((sym_id, narrowed_ty)) = matches_narrowing_target(r, ast, ch[0], ch[1])
                && !r.closure_mutated_symbols.contains(&sym_id)
            {
                let mut map = HashMap::new();
                map.insert(sym_id, narrowed_ty);
                return (map, HashMap::new());
            }
            (HashMap::new(), HashMap::new())
        }
        _ => (HashMap::new(), HashMap::new()),
    }
}

fn null_comparison_target(
    r: &Resolver<'_>,
    ast: &Ast,
    var_node: NodeIndex,
    null_node: NodeIndex,
) -> Option<(SymbolId, TypeIndex)> {
    if var_node.is_null() || null_node.is_null() || ast.node(null_node).kind != NodeKind::Null {
        return None;
    }
    if ast.node(var_node).kind != NodeKind::Id {
        return None;
    }
    let sym_id = *r.node_symbols.get(&var_node)?;
    let sym = &r.symbols[sym_id.0 as usize];
    if !matches!(
        sym.kind,
        SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::Constant
    ) {
        return None;
    }
    let current_ty = r
        .narrowed_symbols
        .get(&sym_id)
        .copied()
        .unwrap_or(sym.type_index);
    let canon = r.type_pool.canonical_type(current_ty)?;
    if let TypeKind::Optional { inner } = r.type_pool.get(canon).kind
        && r.type_pool.as_intrinsic(inner) != Some(Intrinsic::NoReturn)
    {
        return Some((sym_id, inner));
    }
    None
}

fn matches_narrowing_target(
    r: &Resolver<'_>,
    ast: &Ast,
    scrutinee: NodeIndex,
    pat: NodeIndex,
) -> Option<(SymbolId, TypeIndex)> {
    if scrutinee.is_null() || pat.is_null() || ast.node(scrutinee).kind != NodeKind::Id {
        return None;
    }
    let sym_id = *r.node_symbols.get(&scrutinee)?;
    let sym = &r.symbols[sym_id.0 as usize];
    if !matches!(
        sym.kind,
        SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::Constant
    ) {
        return None;
    }
    match ast.node(pat).kind {
        NodeKind::PatternTyped => {
            if r.narrowed_symbols
                .get(&sym_id)
                .is_some_and(|&cur| r.type_pool.as_intrinsic(cur) != Some(Intrinsic::Any))
            {
                return None;
            }
            let ch = ast.fixed_children(pat);
            let binder = ch[0];
            if !binder.is_null()
                && matches!(ast.node(binder).kind, NodeKind::Id | NodeKind::Underscore)
            {
                let target = *r.node_types.get(&ch[1])?;
                if target != TypeIndex::INVALID {
                    return Some((sym_id, target));
                }
            }
            None
        }
        NodeKind::PatternOptionSome => {
            let inner_pat = ast.fixed_children(pat)[0];
            if !inner_pat.is_null()
                && matches!(
                    ast.node(inner_pat).kind,
                    NodeKind::Id | NodeKind::Underscore
                )
            {
                let base_canon = r.type_pool.canonical_type(sym.type_index)?;
                if let TypeKind::Optional { inner } = r.type_pool.get(base_canon).kind
                    && r.type_pool.as_intrinsic(inner) != Some(Intrinsic::NoReturn)
                {
                    return Some((sym_id, inner));
                }
            }
            None
        }
        _ => None,
    }
}

/// Infer `?T` for an unannotated local `let`/`var` initialized to `null` (`?NoReturn`)
/// by inspecting downstream assignments `x = rhs` in the enclosing block.
pub(crate) fn infer_deferred_null_local_type(
    r: &mut Resolver<'_>,
    ast: &Ast,
    decl_node: NodeIndex,
    sym_id: SymbolId,
) -> Option<TypeIndex> {
    let block = *r.active_blocks.last()?;
    let stmts = ast.multi_children(block);
    let pos = stmts.iter().position(|&s| s == decl_node)?;

    let mut assigned_types = Vec::new();
    for &stmt in &stmts[pos + 1..] {
        collect_assigned_rhs_types(r, ast, stmt, sym_id, &mut assigned_types);
    }
    if assigned_types.is_empty() {
        return None;
    }
    let null_ty = r.type_pool.null_type();
    let is_any_payload = |r: &Resolver<'_>, t: TypeIndex| {
        let payload = crate::optional::inner(r, t).unwrap_or(t);
        r.type_pool.as_intrinsic(payload) == Some(Intrinsic::Any)
    };
    let mut acc = null_ty;
    for rhs_ty in assigned_types {
        if rhs_ty == TypeIndex::INVALID || rhs_ty == null_ty {
            continue;
        }
        let candidate = crate::optional::join(r, acc, rhs_ty);
        if acc != null_ty
            && !is_any_payload(r, acc)
            && !is_any_payload(r, rhs_ty)
            && is_any_payload(r, candidate)
        {
            continue;
        }
        acc = candidate;
    }
    (acc != null_ty).then_some(acc)
}

fn collect_assigned_rhs_types(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    target_sym: SymbolId,
    out: &mut Vec<TypeIndex>,
) {
    if node.is_null() || ast.node(node).kind == NodeKind::FunctionDef {
        return;
    }
    match ast.node(node).kind {
        NodeKind::Assign => {
            let ch = ast.fixed_children(node);
            if !ch[0].is_null()
                && ast.node(ch[0]).kind == NodeKind::Id
                && r.node_symbols.get(&ch[0]).copied() == Some(target_sym)
                && let Some(ty) = lightweight_expr_type(r, ast, ch[1])
            {
                out.push(ty);
            }
        }
        NodeKind::ForLoop => {
            let ch = ast.fixed_children(node);
            collect_assigned_rhs_types(r, ast, ch[2], target_sym, out);
            let mut saved_binder: Option<(SymbolId, Option<TypeIndex>)> = None;
            if !ch[1].is_null()
                && ast.node(ch[1]).kind == NodeKind::Id
                && let Some(&loop_sym) = r.node_symbols.get(&ch[1])
                && loop_sym != target_sym
            {
                let item_ty = if !ch[2].is_null()
                    && matches!(
                        ast.node(ch[2]).kind,
                        NodeKind::RangeFrom
                            | NodeKind::RangeTo
                            | NodeKind::RangeToInclusive
                            | NodeKind::RangeFromTo
                            | NodeKind::RangeFromToInclusive
                    ) {
                    Some(Intrinsic::I64.type_index())
                } else {
                    crate::collection_inference::iterable_element_type(r, ast, ch[2])
                };
                if let Some(ty) = item_ty {
                    let prev = r.narrowed_symbols.insert(loop_sym, ty);
                    saved_binder = Some((loop_sym, prev));
                }
            }
            collect_assigned_rhs_types(r, ast, ch[3], target_sym, out);
            if let Some((loop_sym, prev)) = saved_binder {
                match prev {
                    Some(old) => {
                        r.narrowed_symbols.insert(loop_sym, old);
                    }
                    None => {
                        r.narrowed_symbols.remove(&loop_sym);
                    }
                }
            }
            return;
        }
        NodeKind::Block => {
            let mut temp_bindings: Vec<(SymbolId, Option<TypeIndex>)> = Vec::new();
            for &stmt in ast.multi_children(node) {
                collect_assigned_rhs_types(r, ast, stmt, target_sym, out);
                if !stmt.is_null()
                    && matches!(
                        ast.node(stmt).kind,
                        NodeKind::LetDecl | NodeKind::VarDecl | NodeKind::ConstDecl
                    )
                {
                    let ch = ast.fixed_children(stmt);
                    if ch.len() > 2
                        && (ch.len() <= 3 || ch[3].is_null())
                        && !ch[0].is_null()
                        && ast.node(ch[0]).kind == NodeKind::Id
                        && let Some(&sym_id) = r.node_symbols.get(&ch[0])
                        && sym_id != target_sym
                    {
                        let ty = if !ch[1].is_null() {
                            crate::typing::resolve_type_expr_inner(r, ast, ch[1])
                        } else {
                            lightweight_expr_type(r, ast, ch[2])
                        };
                        if let Some(ty) = ty {
                            let prev = r.narrowed_symbols.insert(sym_id, ty);
                            temp_bindings.push((sym_id, prev));
                        }
                    }
                }
            }
            for (sym_id, prev) in temp_bindings.into_iter().rev() {
                match prev {
                    Some(old) => {
                        r.narrowed_symbols.insert(sym_id, old);
                    }
                    None => {
                        r.narrowed_symbols.remove(&sym_id);
                    }
                }
            }
            return;
        }
        _ => {}
    }
    for &c in ast.fixed_children(node) {
        collect_assigned_rhs_types(r, ast, c, target_sym, out);
    }
    for &c in ast.multi_children(node) {
        collect_assigned_rhs_types(r, ast, c, target_sym, out);
    }
}

/// Side-effect-free, diagnostic-free static type peek used for lookahead inference
/// (deferred `null` bindings and unannotated local lambda call-site arguments).
pub(crate) fn lightweight_expr_type(
    r: &mut Resolver<'_>,
    ast: &Ast,
    expr: NodeIndex,
) -> Option<TypeIndex> {
    if expr.is_null() {
        return None;
    }
    if let Some(&existing) = r.node_types.get(&expr)
        && existing != TypeIndex::INVALID
    {
        return Some(existing);
    }
    match ast.node(expr).kind {
        NodeKind::Int => Some(Intrinsic::I64.type_index()),
        NodeKind::Real => Some(Intrinsic::F64.type_index()),
        NodeKind::Str | NodeKind::FStringConcat => Some(Intrinsic::Str.type_index()),
        NodeKind::Char => Some(Intrinsic::Char.type_index()),
        NodeKind::Bool
        | NodeKind::BoolEq
        | NodeKind::BoolNotEq
        | NodeKind::BoolLt
        | NodeKind::BoolLtEq
        | NodeKind::BoolGt
        | NodeKind::BoolGtEq
        | NodeKind::BoolAnd
        | NodeKind::BoolOr
        | NodeKind::BoolNot
        | NodeKind::BoolMatches => Some(Intrinsic::Bool.type_index()),
        NodeKind::Unit => Some(Intrinsic::Unit.type_index()),
        NodeKind::Null => Some(r.type_pool.null_type()),
        NodeKind::ListOf => r.type_pool.list_type(),
        NodeKind::Negative => {
            let inner = ast.fixed_children(expr)[0];
            lightweight_expr_type(r, ast, inner)
        }
        NodeKind::Id | NodeKind::SelfLower => {
            let sym_id = *r.node_symbols.get(&expr)?;
            if let Some(&narrowed) = r.narrowed_symbols.get(&sym_id) {
                return Some(narrowed);
            }
            let ty = r.symbols[sym_id.0 as usize].type_index;
            (ty != TypeIndex::INVALID).then_some(ty)
        }
        NodeKind::ExtendedCall => {
            let callee = ast.fixed_children(expr)[0];
            let sym_id = *r.node_symbols.get(&callee)?;
            let sym = &r.symbols[sym_id.0 as usize];
            if sym.kind == SymbolKind::Type && sym.type_index != TypeIndex::INVALID {
                return Some(sym.type_index);
            }
            None
        }
        NodeKind::Projection => {
            if let Some(&sym_id) = r.node_symbols.get(&expr) {
                let ty = r.symbols[sym_id.0 as usize].type_index;
                if ty != TypeIndex::INVALID {
                    return Some(ty);
                }
            }
            if let Some(vref) = r.enum_variants.get(&expr) {
                return Some(vref.type_index);
            }
            let ch = ast.fixed_children(expr);
            let obj_ty = lightweight_expr_type(r, ast, ch[0])?;
            let canon = r.type_pool.canonical_type(obj_ty)?;
            if let TypeKind::Struct { fields, .. } = &r.type_pool.get(canon).kind
                && !ch[1].is_null()
            {
                let fname = ast.node(ch[1]).str_id;
                if let Some(f) = fields.iter().find(|f| f.name == fname) {
                    return Some(f.ty);
                }
            }
            None
        }
        NodeKind::TypeCast => {
            let ch = ast.fixed_children(expr);
            crate::typing::resolve_type_expr_inner(r, ast, ch[1])
        }
        NodeKind::Call => {
            let callee = ast.fixed_children(expr)[0];
            if let Some(vref) = r.enum_variants.get(&callee) {
                return Some(vref.type_index);
            }
            if !callee.is_null() && ast.node(callee).kind == NodeKind::Projection {
                let mnode = ast.fixed_children(callee)[1];
                if !mnode.is_null() {
                    let mname = str_interner::get(ast.node(mnode).str_id);
                    match mname.as_str() {
                        "len" | "count" => return Some(Intrinsic::I64.type_index()),
                        "is_empty" | "contains" | "starts_with" | "ends_with" => {
                            return Some(Intrinsic::Bool.type_index());
                        }
                        "to_string" | "trim" | "to_upper" | "to_lower" => {
                            return Some(Intrinsic::Str.type_index());
                        }
                        _ => {}
                    }
                }
            }
            if let Some(&sym_id) = r.node_symbols.get(&callee) {
                let sym = &r.symbols[sym_id.0 as usize];
                if sym.kind == SymbolKind::Type && sym.type_index != TypeIndex::INVALID {
                    return Some(sym.type_index);
                }
                if sym.type_index != TypeIndex::INVALID
                    && let TypeKind::Function { ret, .. } = r.type_pool.get(sym.type_index).kind
                    && ret != TypeIndex::INVALID
                {
                    return Some(ret);
                }
            }
            None
        }
        NodeKind::Tuple => {
            let elems: Option<Vec<TypeIndex>> = ast
                .multi_children(expr)
                .iter()
                .map(|&e| lightweight_expr_type(r, ast, e))
                .collect();
            let elems = elems?;
            (!elems.is_empty()).then(|| r.register_type(TypeKind::Tuple { elements: elems }))
        }
        NodeKind::Add | NodeKind::Sub | NodeKind::Mul | NodeKind::Div | NodeKind::Mod => {
            let ch = ast.fixed_children(expr);
            let l = lightweight_expr_type(r, ast, ch[0])?;
            let rr = lightweight_expr_type(r, ast, ch[1])?;
            r.type_pool.numeric_lift(l, rr)
        }
        NodeKind::Concat => {
            let ch = ast.fixed_children(expr);
            lightweight_expr_type(r, ast, ch[0])
        }
        NodeKind::IfStatement => {
            let ch = ast.fixed_children(expr);
            if ch[2].is_null() {
                return Some(Intrinsic::Unit.type_index());
            }
            let t1 = lightweight_expr_type(r, ast, ch[1])?;
            let t2 = lightweight_expr_type(r, ast, ch[2])?;
            Some(crate::errors::join(r, t1, t2))
        }
        NodeKind::ExprStatement => lightweight_expr_type(r, ast, ast.fixed_children(expr)[0]),
        NodeKind::Block => {
            let stmts = ast.multi_children(expr);
            if stmts.is_empty() {
                return Some(Intrinsic::Unit.type_index());
            }
            let mut temp_bindings: Vec<(SymbolId, Option<TypeIndex>)> = Vec::new();
            for &stmt in &stmts[..stmts.len() - 1] {
                if !stmt.is_null()
                    && matches!(
                        ast.node(stmt).kind,
                        NodeKind::LetDecl | NodeKind::VarDecl | NodeKind::ConstDecl
                    )
                {
                    let ch = ast.fixed_children(stmt);
                    if ch.len() > 2
                        && (ch.len() <= 3 || ch[3].is_null())
                        && !ch[0].is_null()
                        && ast.node(ch[0]).kind == NodeKind::Id
                        && let Some(&sym_id) = r.node_symbols.get(&ch[0])
                    {
                        let ty = if !ch[1].is_null() {
                            crate::typing::resolve_type_expr_inner(r, ast, ch[1])
                        } else {
                            lightweight_expr_type(r, ast, ch[2])
                        };
                        if let Some(ty) = ty {
                            let prev = r.narrowed_symbols.insert(sym_id, ty);
                            temp_bindings.push((sym_id, prev));
                        }
                    }
                }
            }
            let tail = stmts
                .last()
                .and_then(|&last| lightweight_expr_type(r, ast, last));
            for (sym_id, prev) in temp_bindings.into_iter().rev() {
                match prev {
                    Some(old) => {
                        r.narrowed_symbols.insert(sym_id, old);
                    }
                    None => {
                        r.narrowed_symbols.remove(&sym_id);
                    }
                }
            }
            tail
        }
        _ => None,
    }
}

/// Side-effect-free inductive return preview for `List.fold(init, |acc, x| body)` and
/// `List.reduce(|acc, x| body)`. Returns `Some(ret_ty)` only when both the implicit tail
/// expression and all explicit `return` statements inside `lambda_node` have statically
/// previewable types.
pub(crate) fn preview_binary_lambda_return(
    r: &mut Resolver<'_>,
    ast: &Ast,
    lambda_node: NodeIndex,
    first_ty: TypeIndex,
    second_ty: TypeIndex,
) -> Option<TypeIndex> {
    if lambda_node.is_null() || ast.node(lambda_node).kind != NodeKind::Lambda {
        return None;
    }
    let params = ast.multi_children(lambda_node);
    if params.len() != 2 {
        return None;
    }
    let mut saved: Vec<(SymbolId, Option<TypeIndex>)> = Vec::new();
    for (&param, assumed_ty) in params.iter().zip([first_ty, second_ty]) {
        let ch = ast.fixed_children(param);
        let pat = if !ch[1].is_null() {
            // Respect explicit parameter annotation when present.
            continue;
        } else if !ch[0].is_null() && ast.node(ch[0]).kind == NodeKind::PatternTyped {
            continue;
        } else {
            ch[0]
        };
        if !pat.is_null()
            && ast.node(pat).kind == NodeKind::Id
            && let Some(&sym_id) = r.node_symbols.get(&pat)
        {
            let prev = r.narrowed_symbols.insert(sym_id, assumed_ty);
            saved.push((sym_id, prev));
        }
    }

    let body = ast.fixed_children(lambda_node)[0];
    let mut explicit_returns = Vec::new();
    let mut all_returns_known = true;
    collect_preview_returns(r, ast, body, &mut explicit_returns, &mut all_returns_known);
    let tail_ty = if crate::typing::body_definitely_returns(ast, body) {
        None
    } else {
        Some(lightweight_expr_type(r, ast, body))
    };

    for (sym_id, prev) in saved.into_iter().rev() {
        match prev {
            Some(old) => {
                r.narrowed_symbols.insert(sym_id, old);
            }
            None => {
                r.narrowed_symbols.remove(&sym_id);
            }
        }
    }

    if !all_returns_known {
        return None;
    }
    if let Some(tail_opt) = tail_ty {
        explicit_returns.push(tail_opt?);
    }
    if explicit_returns.is_empty() {
        return None;
    }
    let mut joined = explicit_returns[0];
    for &ty in &explicit_returns[1..] {
        joined = crate::errors::join(r, joined, ty);
    }
    Some(joined)
}

fn collect_preview_returns(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    out: &mut Vec<TypeIndex>,
    ok: &mut bool,
) {
    if node.is_null() || !*ok {
        return;
    }
    match ast.node(node).kind {
        NodeKind::FunctionDef
        | NodeKind::Lambda
        | NodeKind::CaseMap
        | NodeKind::CaseAlternative => return,
        NodeKind::ReturnStatement => {
            let val = ast.fixed_children(node)[0];
            let ty = if val.is_null() {
                Some(Intrinsic::Unit.type_index())
            } else {
                lightweight_expr_type(r, ast, val)
            };
            if let Some(ty) = ty {
                out.push(ty);
            } else {
                *ok = false;
            }
            return;
        }
        _ => {}
    }
    for &c in ast.fixed_children(node) {
        collect_preview_returns(r, ast, c, out, ok);
    }
    for &c in ast.multi_children(node) {
        collect_preview_returns(r, ast, c, out, ok);
    }
}

/// Infer parameter types for an unannotated local lambda `let f = |a, b| body`
/// from direct call sites `f(arg1, arg2)` in the enclosing block when `f` is not reassigned.
pub(crate) fn infer_local_lambda_call_site_params(
    r: &mut Resolver<'_>,
    ast: &Ast,
    decl_node: NodeIndex,
    fn_sym: SymbolId,
    lambda_node: NodeIndex,
) -> Option<Vec<TypeIndex>> {
    let params = ast.multi_children(lambda_node);
    if params.is_empty() {
        return None;
    }
    // Only apply to plain positional parameters where at least one lacks an annotation.
    let mut has_unannotated = false;
    for &p in params {
        if !matches!(
            ast.node(p).kind,
            NodeKind::ParamTyped | NodeKind::ParamLambda
        ) {
            return None;
        }
        let ch = ast.fixed_children(p);
        if ch[1].is_null() && (ch[0].is_null() || ast.node(ch[0]).kind != NodeKind::PatternTyped) {
            has_unannotated = true;
        }
    }
    if !has_unannotated {
        return None;
    }

    let block = *r.active_blocks.last()?;
    let stmts = ast.multi_children(block);
    let pos = stmts.iter().position(|&s| s == decl_node)?;

    // Do not specialize if `f` is reassigned later in the block.
    let mut mutated = HashSet::new();
    for &stmt in &stmts[pos + 1..] {
        collect_mutated_symbols_in_subtree(r, ast, stmt, &mut mutated);
    }
    if mutated.contains(&fn_sym) {
        return None;
    }

    let mut scan = LambdaCallScan {
        fn_sym,
        arity: params.len(),
        calls: Vec::new(),
        escaped: false,
    };
    for &stmt in &stmts[pos + 1..] {
        scan.visit(r, ast, stmt, None);
    }
    if scan.escaped || scan.calls.is_empty() {
        return None;
    }

    let mut inferred = Vec::with_capacity(params.len());
    for col in 0..params.len() {
        let mut acc = scan.calls[0][col];
        for row in &scan.calls[1..] {
            acc = crate::errors::join(r, acc, row[col]);
        }
        inferred.push(acc);
    }
    Some(inferred)
}

struct LambdaCallScan {
    fn_sym: SymbolId,
    arity: usize,
    calls: Vec<Vec<TypeIndex>>,
    escaped: bool,
}

impl LambdaCallScan {
    fn visit(
        &mut self,
        r: &mut Resolver<'_>,
        ast: &Ast,
        node: NodeIndex,
        parent_call_callee: Option<NodeIndex>,
    ) {
        if node.is_null() || self.escaped {
            return;
        }
        let kind = ast.node(node).kind;
        if kind == NodeKind::Id && r.node_symbols.get(&node).copied() == Some(self.fn_sym) {
            if parent_call_callee != Some(node) {
                self.escaped = true;
            }
            return;
        }
        if kind == NodeKind::Call {
            let callee = ast.fixed_children(node)[0];
            if !callee.is_null()
                && ast.node(callee).kind == NodeKind::Id
                && r.node_symbols.get(&callee).copied() == Some(self.fn_sym)
            {
                let args = ast.multi_children(node);
                if args.len() != self.arity
                    || args.iter().any(|&a| {
                        matches!(ast.node(a).kind, NodeKind::SpreadArg | NodeKind::NamedArg)
                    })
                {
                    self.escaped = true;
                    return;
                }
                let mut arg_tys = Vec::with_capacity(self.arity);
                for &arg in args {
                    self.visit(r, ast, arg, None);
                    if let Some(ty) = lightweight_expr_type(r, ast, arg) {
                        arg_tys.push(ty);
                    } else {
                        self.escaped = true;
                        return;
                    }
                }
                self.calls.push(arg_tys);
                return;
            }
        }
        let next_callee = (kind == NodeKind::Call).then(|| ast.fixed_children(node)[0]);
        for &c in ast.fixed_children(node) {
            self.visit(r, ast, c, next_callee);
        }
        for &c in ast.multi_children(node) {
            self.visit(r, ast, c, None);
        }
    }
}
