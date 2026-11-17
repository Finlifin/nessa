//! Homogeneous element and value provenance inference for Nessa's dynamic
//! collections (`List`, `Set`, `Deque`, `Map`, `HashMap`).
//!
//! Runtime collection types remain unified (`List`, `Map`, etc.), preserving gradual
//! interoperability and existing ABI contracts. When all writes and initializers for
//! an unescaped collection expression or local binding share a concrete element/value
//! type `T != Any`, this module propagates `T` into inline collection callbacks
//! (`.map`, `.filter`, `.fold`, `.each`, `.foreach`, `.any`, `.all`, `.find`,
//! `.position`, `.rposition`, `.partition`, `.retain`, `.sort_by`, `.sort_by_lt`,
//! `.sort_by_key`, `.is_sorted_by_lt`, `.map_values`) and `for x in xs` loop binders.

use std::collections::HashMap;

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{CollectionRole, Intrinsic, TypeIndex, TypeKind};

use crate::resolver::Resolver;
use crate::{SymbolId, SymbolKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TrackedElem {
    /// Known empty collection; adopts the type of subsequent homogeneous writes.
    Empty,
    /// Homogeneous non-`Any` element (for `List`/`Set`/`Deque`) or value (for `Map`/`HashMap`) type.
    Known(TypeIndex),
    /// Heterogeneous, `Any`, or escaped collection.
    Dynamic,
}

#[derive(Clone, Default)]
pub(crate) struct CollectionProvenanceState {
    pub(crate) node_elements: HashMap<NodeIndex, TrackedElem>,
    pub(crate) symbol_elements: HashMap<SymbolId, TrackedElem>,
    /// Call-site inferred parameter types for local unannotated lambdas (`let f = |a, b| ...`),
    /// passed out-of-band so no incomplete `Function` type is registered in `TypePool`.
    pub(crate) inferred_lambda_params: HashMap<NodeIndex, Vec<TypeIndex>>,
    /// Concrete inferred return type of a `NodeKind::Lambda` body, recorded even when
    /// contextual expected return type was `Any` (so `.map` knows the output element type
    /// without altering the closure's runtime `fn(...) -> Any` compatibility).
    pub(crate) lambda_body_returns: HashMap<NodeIndex, TypeIndex>,
    /// `Call` nodes of `List.fold` / `List.reduce` whose inductive accumulator return type
    /// was proven to stay within a concrete type `T`.
    pub(crate) refined_call_results: HashMap<NodeIndex, TypeIndex>,
}

fn peel_expr(ast: &Ast, mut expr: NodeIndex) -> NodeIndex {
    while !expr.is_null() && ast.node(expr).kind == NodeKind::ExprStatement {
        expr = ast.fixed_children(expr)[0];
    }
    expr
}

fn combine_elem(r: &mut Resolver<'_>, left: TrackedElem, right: TrackedElem) -> TrackedElem {
    match (left, right) {
        (TrackedElem::Dynamic, _) | (_, TrackedElem::Dynamic) => TrackedElem::Dynamic,
        (TrackedElem::Empty, other) | (other, TrackedElem::Empty) => other,
        (TrackedElem::Known(a), TrackedElem::Known(b)) => {
            let joined = crate::optional::join(r, a, b);
            normalize_elem_type(r, joined)
        }
    }
}

fn normalize_elem_type(r: &Resolver<'_>, ty: TypeIndex) -> TrackedElem {
    if ty == TypeIndex::INVALID {
        return TrackedElem::Dynamic;
    }
    let Some(canon) = r.type_pool.canonical_type(ty) else {
        return TrackedElem::Dynamic;
    };
    if matches!(
        r.type_pool.as_intrinsic(canon),
        Some(Intrinsic::Any | Intrinsic::NoReturn)
    ) {
        return TrackedElem::Dynamic;
    }
    if canon == r.type_pool.null_type() {
        return TrackedElem::Dynamic;
    }
    TrackedElem::Known(ty)
}

/// Record element provenance for a `[e1, e2, ...]` list literal after its elements are resolved.
pub(crate) fn record_list_literal(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) {
    let mut state = TrackedElem::Empty;
    for &elem in ast.multi_children(node) {
        let elem_state = match r.node_types.get(&elem).copied() {
            Some(ty) => normalize_elem_type(r, ty),
            None => TrackedElem::Dynamic,
        };
        state = combine_elem(r, state, elem_state);
        if state == TrackedElem::Dynamic {
            break;
        }
    }
    r.collection_provenance.node_elements.insert(node, state);
}

/// Record provenance for `xs ++ ys` when concatenating two `List` values.
pub(crate) fn record_concat(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    result_ty: TypeIndex,
) {
    if r.type_pool.collection_role(result_ty) != Some(CollectionRole::List) {
        return;
    }
    let ch = ast.fixed_children(node);
    let left = lookup_expr_elem(r, ast, ch[0]);
    let right = lookup_expr_elem(r, ast, ch[1]);
    let combined = combine_elem(r, left, right);
    r.collection_provenance.node_elements.insert(node, combined);
}

/// Look up the tracked element/value provenance of an expression.
pub(crate) fn lookup_expr_elem(r: &Resolver<'_>, ast: &Ast, expr: NodeIndex) -> TrackedElem {
    let expr = peel_expr(ast, expr);
    if expr.is_null() {
        return TrackedElem::Dynamic;
    }
    if let Some(&state) = r.collection_provenance.node_elements.get(&expr) {
        return state;
    }
    // Bridge `await_all` provenance when available.
    if let Some(elem_ty) = crate::task_combinators::lookup_await_all_element(r, ast, expr) {
        return normalize_elem_type(r, elem_ty);
    }
    if ast.node(expr).kind == NodeKind::ErrorPropagation
        || ast.node(expr).kind == NodeKind::EffectPropagation
    {
        return lookup_expr_elem(r, ast, ast.fixed_children(expr)[0]);
    }
    if ast.node(expr).kind == NodeKind::Id
        && let Some(&sym) = r.node_symbols.get(&expr)
        && let Some(&state) = r.collection_provenance.symbol_elements.get(&sym)
    {
        return state;
    }
    TrackedElem::Dynamic
}

fn static_iterator_item_type(
    r: &Resolver<'_>,
    expr: NodeIndex,
    canon_ty: TypeIndex,
) -> Option<TypeIndex> {
    let scope = r.node_scopes.get(&expr).copied().unwrap_or(r.current_scope);
    let wk = r.type_pool.well_known;
    let iter_ty = if let Ok(Some(record)) =
        r.type_pool
            .find_trait_impl_scoped(canon_ty, wk.iterator, scope.0)
    {
        record.implementor
    } else if let Ok(Some(into_rec)) =
        r.type_pool
            .find_trait_impl_scoped(canon_ty, wk.into_iterator, scope.0)
    {
        let iter_bind = r
            .type_pool
            .associated_bindings_snapshot()
            .iter()
            .find(|b| {
                b.implementor == into_rec.implementor
                    && b.trait_type == into_rec.trait_type
                    && b.visible_scope == into_rec.visible_scope
                    && b.trait_owner == wk.into_iterator
                    && b.name == str_interner::intern("Iter")
            })?;
        r.type_pool.canonical_type(iter_bind.value)?
    } else {
        return None;
    };
    let record = r
        .type_pool
        .find_trait_impl_scoped(iter_ty, wk.iterator, scope.0)
        .ok()
        .flatten()?;
    let binding = r
        .type_pool
        .associated_bindings_snapshot()
        .iter()
        .find(|b| {
            b.implementor == record.implementor
                && b.trait_type == record.trait_type
                && b.visible_scope == record.visible_scope
                && b.trait_owner == wk.iterator
                && b.name == str_interner::intern("Item")
        })?;
    let item_ty = r.type_pool.canonical_type(binding.value)?;
    (r.type_pool.as_intrinsic(item_ty) != Some(Intrinsic::Any)).then_some(item_ty)
}

fn iterator_item_for_expr(r: &mut Resolver<'_>, ast: &Ast, expr: NodeIndex) -> Option<TypeIndex> {
    let ty = *r.node_types.get(&expr)?;
    let canon = r.type_pool.canonical_type(ty)?;
    if r.type_pool.collection_role(canon) == Some(CollectionRole::Map) {
        let val_ty = match lookup_expr_elem(r, ast, expr) {
            TrackedElem::Known(v) => v,
            _ => Intrinsic::Any.type_index(),
        };
        return Some(r.register_type(TypeKind::Tuple {
            elements: vec![Intrinsic::Str.type_index(), val_ty],
        }));
    }
    if let TrackedElem::Known(t) = lookup_expr_elem(r, ast, expr) {
        return Some(t);
    }
    static_iterator_item_type(r, expr, canon)
}

/// Return a concrete element type `T != Any` when `iterable` is a known homogeneous
/// `List`, `Set`, `Deque`, `Map`, or `Iterator` pipeline.
pub(crate) fn iterable_element_type(
    r: &mut Resolver<'_>,
    ast: &Ast,
    iterable: NodeIndex,
) -> Option<TypeIndex> {
    let ty = *r.node_types.get(&iterable)?;
    let canon = r.type_pool.canonical_type(ty)?;
    let elem = match lookup_expr_elem(r, ast, iterable) {
        TrackedElem::Known(t) => t,
        TrackedElem::Empty | TrackedElem::Dynamic => return None,
    };
    if r.type_pool.collection_role(canon) == Some(CollectionRole::List) {
        return Some(elem);
    }
    if r.type_pool.collection_role(canon) == Some(CollectionRole::Map) {
        return Some(r.register_type(TypeKind::Tuple {
            elements: vec![Intrinsic::Str.type_index(), elem],
        }));
    }
    if let TypeKind::Struct { name, .. } = r.type_pool.get(canon).kind {
        let sname = str_interner::get(name);
        if matches!(sname.as_str(), "Set" | "Deque") {
            return Some(elem);
        }
    }
    let scope = r
        .node_scopes
        .get(&iterable)
        .copied()
        .unwrap_or(r.current_scope);
    if r.type_pool
        .find_trait_impl_scoped(canon, r.type_pool.well_known.iterator, scope.0)
        .ok()
        .flatten()
        .is_some()
    {
        return Some(elem);
    }
    None
}

fn is_tracked_collection_type(r: &Resolver<'_>, ty: TypeIndex) -> bool {
    let Some(canon) = r.type_pool.canonical_type(ty) else {
        return false;
    };
    if matches!(
        r.type_pool.collection_role(canon),
        Some(CollectionRole::List | CollectionRole::Map)
    ) {
        return true;
    }
    if let TypeKind::Struct { name, .. } = r.type_pool.get(canon).kind {
        let sname = str_interner::get(name);
        if matches!(sname.as_str(), "Set" | "Deque" | "HashMap") {
            return true;
        }
    }
    r.type_pool
        .has_trait_impl(canon, r.type_pool.well_known.iterator)
}

/// Record local variable collection provenance at `let`/`var`/`const` declaration time,
/// incorporating any direct homogeneous mutations (`push`, `insert`, `set`, `xs(i) = v`, ...)
/// in the enclosing block and invalidating to `Dynamic` if the collection escapes.
pub(crate) fn record_decl_provenance(
    r: &mut Resolver<'_>,
    ast: &Ast,
    decl_node: NodeIndex,
    pattern_node: NodeIndex,
    value_node: NodeIndex,
    has_else: bool,
) {
    if has_else
        || pattern_node.is_null()
        || value_node.is_null()
        || ast.node(pattern_node).kind != NodeKind::Id
    {
        return;
    }
    let Some(&sym_id) = r.node_symbols.get(&pattern_node) else {
        return;
    };
    let sym_ty = r.symbols[sym_id.0 as usize].type_index;
    if sym_ty == TypeIndex::INVALID || !is_tracked_collection_type(r, sym_ty) {
        return;
    }
    if r.closure_mutated_symbols.contains(&sym_id) {
        r.collection_provenance
            .symbol_elements
            .insert(sym_id, TrackedElem::Dynamic);
        return;
    }

    let mut state = lookup_expr_elem(r, ast, value_node);
    if state == TrackedElem::Dynamic {
        r.collection_provenance
            .symbol_elements
            .insert(sym_id, TrackedElem::Dynamic);
        return;
    }

    if let Some(&block) = r.active_blocks.last() {
        let stmts = ast.multi_children(block);
        if let Some(pos) = stmts.iter().position(|&s| s == decl_node) {
            let mut scanner = CollectionMutationScan {
                target_sym: sym_id,
                state,
                loop_vars: HashMap::new(),
            };
            for &stmt in &stmts[pos + 1..] {
                scanner.visit(r, ast, stmt, UsageRole::Other);
                if scanner.state == TrackedElem::Dynamic {
                    break;
                }
            }
            state = scanner.state;
        } else {
            // Module-scope bindings could be mutated from any function unless `const` and private.
            if r.symbols[sym_id.0 as usize].kind != SymbolKind::Constant {
                state = TrackedElem::Dynamic;
            }
        }
    } else if r.symbols[sym_id.0 as usize].kind != SymbolKind::Constant {
        state = TrackedElem::Dynamic;
    }

    r.collection_provenance
        .symbol_elements
        .insert(sym_id, state);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UsageRole {
    SafeReceiver,
    SafeIterable,
    Other,
}

struct CollectionMutationScan {
    target_sym: SymbolId,
    state: TrackedElem,
    /// Lightweight element/index types for enclosing `for i in 0..n` loops inside the block
    /// (e.g. `let fs = List(); for i in 0..3 { fs.push(|| i) }`).
    loop_vars: HashMap<SymbolId, TypeIndex>,
}

impl CollectionMutationScan {
    fn preview_expr(&self, r: &mut Resolver<'_>, ast: &Ast, expr: NodeIndex) -> Option<TypeIndex> {
        let expr = peel_expr(ast, expr);
        if expr.is_null() {
            return None;
        }
        if ast.node(expr).kind == NodeKind::Lambda {
            let params = ast.multi_children(expr);
            let mut param_tys = Vec::with_capacity(params.len());
            for &p in params {
                let ch = ast.fixed_children(p);
                let ty = if !ch[1].is_null() {
                    crate::typing::resolve_type_expr_inner(r, ast, ch[1])?
                } else {
                    Intrinsic::Any.type_index()
                };
                param_tys.push(ty);
            }
            let mut saved = Vec::new();
            for (&sym, &ty) in &self.loop_vars {
                let prev = r.narrowed_symbols.insert(sym, ty);
                saved.push((sym, prev));
            }
            let ret_ann = ast.fixed_children(expr)[1];
            let ret_ty = if !ret_ann.is_null() {
                crate::typing::resolve_type_expr_inner(r, ast, ret_ann)
            } else {
                crate::flow_narrowing::lightweight_expr_type(r, ast, ast.fixed_children(expr)[0])
            };
            for (sym, prev) in saved.into_iter().rev() {
                match prev {
                    Some(old) => {
                        r.narrowed_symbols.insert(sym, old);
                    }
                    None => {
                        r.narrowed_symbols.remove(&sym);
                    }
                }
            }
            let ret = ret_ty?;
            return Some(r.register_type(TypeKind::Function {
                params: param_tys,
                ret,
            }));
        }
        let mut saved = Vec::new();
        for (&sym, &ty) in &self.loop_vars {
            let prev = r.narrowed_symbols.insert(sym, ty);
            saved.push((sym, prev));
        }
        let out = crate::flow_narrowing::lightweight_expr_type(r, ast, expr);
        for (sym, prev) in saved.into_iter().rev() {
            match prev {
                Some(old) => {
                    r.narrowed_symbols.insert(sym, old);
                }
                None => {
                    r.narrowed_symbols.remove(&sym);
                }
            }
        }
        out
    }

    fn merge_value_expr(&mut self, r: &mut Resolver<'_>, ast: &Ast, val_expr: NodeIndex) {
        let elem = match self.preview_expr(r, ast, val_expr) {
            Some(ty) => normalize_elem_type(r, ty),
            None => TrackedElem::Dynamic,
        };
        self.state = combine_elem(r, self.state, elem);
    }

    fn is_target_id(&self, r: &Resolver<'_>, ast: &Ast, node: NodeIndex) -> bool {
        !node.is_null()
            && ast.node(node).kind == NodeKind::Id
            && r.node_symbols.get(&node).copied() == Some(self.target_sym)
    }

    fn visit(&mut self, r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex, role: UsageRole) {
        if node.is_null() || self.state == TrackedElem::Dynamic {
            return;
        }
        let kind = ast.node(node).kind;
        if kind == NodeKind::FunctionDef {
            return;
        }
        if self.is_target_id(r, ast, node) {
            if role == UsageRole::Other {
                // Escaped into an alias, unknown call argument, return, or outer container.
                self.state = TrackedElem::Dynamic;
            }
            return;
        }

        match kind {
            NodeKind::Assign => {
                let ch = ast.fixed_children(node);
                if self.is_target_id(r, ast, ch[0]) {
                    // Reassigning the variable itself: inspect RHS if trivial, else mark Dynamic.
                    self.visit(r, ast, ch[1], UsageRole::Other);
                    let rhs_elem = lookup_expr_elem(r, ast, ch[1]);
                    if rhs_elem == TrackedElem::Dynamic {
                        self.state = TrackedElem::Dynamic;
                    } else {
                        self.state = combine_elem(r, self.state, rhs_elem);
                    }
                    return;
                }
                // `xs(idx) = val`
                if !ch[0].is_null() && ast.node(ch[0]).kind == NodeKind::Call {
                    let call_callee = ast.fixed_children(ch[0])[0];
                    if self.is_target_id(r, ast, call_callee) {
                        for &idx_arg in ast.multi_children(ch[0]) {
                            self.visit(r, ast, idx_arg, UsageRole::Other);
                        }
                        self.visit(r, ast, ch[1], UsageRole::Other);
                        self.merge_value_expr(r, ast, ch[1]);
                        return;
                    }
                }
            }
            NodeKind::Call => {
                let callee = ast.fixed_children(node)[0];
                // Direct index read `xs(i)`
                if self.is_target_id(r, ast, callee) {
                    for &arg in ast.multi_children(node) {
                        self.visit(r, ast, arg, UsageRole::Other);
                    }
                    return;
                }
                // Method call `xs.method(args...)`
                if !callee.is_null() && ast.node(callee).kind == NodeKind::Projection {
                    let proj_ch = ast.fixed_children(callee);
                    if self.is_target_id(r, ast, proj_ch[0]) && !proj_ch[1].is_null() {
                        let mname = str_interner::get(ast.node(proj_ch[1]).str_id);
                        let args = ast.multi_children(node);
                        for &arg in args {
                            self.visit(r, ast, arg, UsageRole::Other);
                        }
                        match mname.as_str() {
                            // Pure / element-preserving / removing methods on List, Set, Deque, Map, HashMap
                            "len"
                            | "is_empty"
                            | "get"
                            | "apply"
                            | "pop"
                            | "pop_back"
                            | "pop_front"
                            | "front"
                            | "back"
                            | "first"
                            | "last"
                            | "clear"
                            | "truncate"
                            | "swap"
                            | "swap_remove"
                            | "remove"
                            | "dedup"
                            | "reverse"
                            | "reverse_in_place"
                            | "copy"
                            | "slice"
                            | "take"
                            | "drop"
                            | "unique"
                            | "enumerate"
                            | "chunks"
                            | "windows"
                            | "chunks_exact"
                            | "rchunks"
                            | "rchunks_exact"
                            | "chunks_remainder"
                            | "rchunks_remainder"
                            | "split_at"
                            | "split_off"
                            | "rotate_left"
                            | "rotate_right"
                            | "repeat"
                            | "flatten"
                            | "zip"
                            | "join"
                            | "join_range"
                            | "sort"
                            | "sort_by"
                            | "sort_by_lt"
                            | "sort_by_key"
                            | "is_sorted"
                            | "is_sorted_by_lt"
                            | "binary_search"
                            | "binary_search_by"
                            | "copy_within"
                            | "map"
                            | "filter"
                            | "fold"
                            | "reduce"
                            | "each"
                            | "foreach"
                            | "any"
                            | "all"
                            | "find"
                            | "position"
                            | "rposition"
                            | "partition"
                            | "retain"
                            | "flat_map"
                            | "format_list"
                            | "format_map"
                            | "await_all"
                            | "await_any"
                            | "contains"
                            | "contains_key"
                            | "index_of"
                            | "keys"
                            | "values"
                            | "entries"
                            | "get_or"
                            | "map_values"
                            | "to_list"
                            | "union"
                            | "intersection"
                            | "difference"
                            | "symmetric_difference"
                            | "is_subset"
                            | "is_superset"
                            | "iter"
                            | "into_iter"
                            | "next"
                            | "peek"
                            | "filter_map"
                            | "skip"
                            | "take_while"
                            | "skip_while"
                            | "step_by"
                            | "chain"
                            | "inspect"
                            | "peekable"
                            | "collect"
                            | "to_set"
                            | "for_each"
                            | "find_map"
                            | "count"
                            | "sum"
                            | "product"
                            | "min"
                            | "max"
                            | "nth"
                            | "eq" => {}
                            // Single-argument element insertion
                            "push" | "push_back" | "push_front" | "fill" if args.len() == 1 => {
                                self.merge_value_expr(r, ast, args[0]);
                            }
                            // Set.insert(item) (1 arg) vs List.insert(idx, val) / Map.insert(key, val) (2 args)
                            "insert" => match args.len() {
                                1 => self.merge_value_expr(r, ast, args[0]),
                                2 => self.merge_value_expr(r, ast, args[1]),
                                _ => self.state = TrackedElem::Dynamic,
                            },
                            // Two-argument `(index_or_key, value)` writes
                            "set" | "update" | "resize" | "get_or_insert" | "insert_absent"
                                if args.len() == 2 =>
                            {
                                self.merge_value_expr(r, ast, args[1]);
                            }
                            // Bulk merge/append from another collection
                            "append" | "merge" if args.len() == 1 => {
                                let other_elem = lookup_expr_elem(r, ast, args[0]);
                                self.state = combine_elem(r, self.state, other_elem);
                            }
                            _ => {
                                self.state = TrackedElem::Dynamic;
                            }
                        }
                        return;
                    }
                }
            }
            NodeKind::Concat => {
                let ch = ast.fixed_children(node);
                let l_role = if self.is_target_id(r, ast, ch[0]) {
                    UsageRole::SafeReceiver
                } else {
                    UsageRole::Other
                };
                let r_role = if self.is_target_id(r, ast, ch[1]) {
                    UsageRole::SafeReceiver
                } else {
                    UsageRole::Other
                };
                self.visit(r, ast, ch[0], l_role);
                self.visit(r, ast, ch[1], r_role);
                return;
            }
            NodeKind::ForLoop => {
                let ch = ast.fixed_children(node);
                let iter_role = if self.is_target_id(r, ast, ch[2]) {
                    UsageRole::SafeIterable
                } else {
                    UsageRole::Other
                };
                self.visit(r, ast, ch[2], iter_role);
                let mut added_loop_var = None;
                if !ch[1].is_null()
                    && ast.node(ch[1]).kind == NodeKind::Id
                    && !ch[2].is_null()
                    && matches!(
                        ast.node(ch[2]).kind,
                        NodeKind::RangeFromTo
                            | NodeKind::RangeFromToInclusive
                            | NodeKind::RangeFrom
                    )
                    && let Some(&lsym) = r.node_symbols.get(&ch[1])
                {
                    self.loop_vars.insert(lsym, Intrinsic::I64.type_index());
                    added_loop_var = Some(lsym);
                }
                self.visit(r, ast, ch[3], UsageRole::Other);
                if let Some(lsym) = added_loop_var {
                    self.loop_vars.remove(&lsym);
                }
                return;
            }
            _ => {}
        }

        for &c in ast.fixed_children(node) {
            self.visit(r, ast, c, UsageRole::Other);
        }
        for &c in ast.multi_children(node) {
            self.visit(r, ast, c, UsageRole::Other);
        }
    }
}

/// Compute refined expected argument type for an inline callback argument `arg` in a
/// `List` or `Map` method call, if the receiver has a known homogeneous element/value type.
pub(crate) fn expected_callback_arg_type(
    r: &mut Resolver<'_>,
    ast: &Ast,
    call_node: NodeIndex,
    callee: NodeIndex,
    arg_index: usize,
    arg_node: NodeIndex,
    default_expected: Option<TypeIndex>,
) -> Option<TypeIndex> {
    try_expected_callback_arg_type(
        r,
        ast,
        call_node,
        callee,
        arg_index,
        arg_node,
        default_expected,
    )
    .or(default_expected)
}

fn try_expected_callback_arg_type(
    r: &mut Resolver<'_>,
    ast: &Ast,
    call_node: NodeIndex,
    callee: NodeIndex,
    arg_index: usize,
    arg_node: NodeIndex,
    default_expected: Option<TypeIndex>,
) -> Option<TypeIndex> {
    if arg_node.is_null() {
        return default_expected;
    }
    let arg_kind = ast.node(arg_node).kind;
    let is_inline_closure = matches!(
        arg_kind,
        NodeKind::Lambda | NodeKind::CaseMap | NodeKind::CaseAlternative
    );
    if !is_inline_closure || ast.node(callee).kind != NodeKind::Projection {
        return default_expected;
    }
    let method_sym = *r.instance_methods.get(&callee)?;
    let method_info = &r.symbols[method_sym.0 as usize];
    if !r.options.privileged_nodes.contains(&method_info.def_node) {
        return default_expected;
    }
    let receiver = ast.fixed_children(callee)[0];
    let receiver_ty = *r.node_types.get(&receiver)?;
    let method_name = str_interner::get(method_info.name);
    let args = ast.multi_children(call_node);

    // Extract declared callback return type from `default_expected` when available.
    let declared_cb_ret = default_expected
        .and_then(|exp| r.type_pool.canonical_type(exp))
        .and_then(|exp| match r.type_pool.get(exp).kind {
            TypeKind::Function { ret, .. } => Some(ret),
            _ => None,
        })
        .unwrap_or(Intrinsic::Any.type_index());

    if crate::iterator_defaults::is_default_template(r, method_sym) {
        let elem_opt = iterator_item_for_expr(r, ast, receiver);
        return match (method_name.as_str(), arg_index) {
            (
                "map" | "filter" | "filter_map" | "flat_map" | "take_while" | "skip_while"
                | "inspect" | "for_each" | "any" | "all" | "find" | "find_map" | "position"
                | "partition",
                0,
            ) if args.len() == 1 => {
                let elem_ty = elem_opt?;
                Some(r.register_type(TypeKind::Function {
                    params: vec![elem_ty],
                    ret: declared_cb_ret,
                }))
            }
            ("reduce", 0) if args.len() == 1 && arg_kind == NodeKind::Lambda => {
                let elem_ty = elem_opt?;
                let acc_ty =
                    if let Some(body_ret) = crate::flow_narrowing::preview_binary_lambda_return(
                        r, ast, arg_node, elem_ty, elem_ty,
                    ) && r.type_pool.is_subtype(body_ret, elem_ty)
                    {
                        elem_ty
                    } else {
                        Intrinsic::Any.type_index()
                    };
                Some(r.register_type(TypeKind::Function {
                    params: vec![acc_ty, elem_ty],
                    ret: Intrinsic::Any.type_index(),
                }))
            }
            ("fold", 1) if args.len() == 2 && arg_kind == NodeKind::Lambda => {
                let elem_ty = elem_opt.unwrap_or(Intrinsic::Any.type_index());
                let init_ty = r
                    .node_types
                    .get(&args[0])
                    .copied()
                    .unwrap_or(Intrinsic::Any.type_index());
                let init_is_concrete = init_ty != TypeIndex::INVALID
                    && r.type_pool.as_intrinsic(init_ty) != Some(Intrinsic::Any)
                    && r.type_pool.as_intrinsic(init_ty) != Some(Intrinsic::NoReturn)
                    && init_ty != r.type_pool.null_type();
                let acc_ty = if init_is_concrete
                    && let Some(body_ret) = crate::flow_narrowing::preview_binary_lambda_return(
                        r, ast, arg_node, init_ty, elem_ty,
                    )
                    && r.type_pool.is_subtype(body_ret, init_ty)
                {
                    r.collection_provenance
                        .refined_call_results
                        .insert(call_node, init_ty);
                    init_ty
                } else {
                    Intrinsic::Any.type_index()
                };
                if acc_ty == Intrinsic::Any.type_index() && elem_ty == Intrinsic::Any.type_index() {
                    return default_expected;
                }
                Some(r.register_type(TypeKind::Function {
                    params: vec![acc_ty, elem_ty],
                    ret: Intrinsic::Any.type_index(),
                }))
            }
            _ => default_expected,
        };
    }

    let role = r.type_pool.collection_role(receiver_ty)?;

    match role {
        CollectionRole::List => {
            let elem_opt = match lookup_expr_elem(r, ast, receiver) {
                TrackedElem::Known(t) => Some(t),
                TrackedElem::Empty | TrackedElem::Dynamic => None,
            };
            match (method_name.as_str(), arg_index) {
                (
                    "map" | "filter" | "each" | "foreach" | "any" | "all" | "find" | "position"
                    | "rposition" | "partition" | "retain" | "sort_by_key" | "binary_search_by"
                    | "flat_map",
                    0,
                ) if args.len() == 1 => {
                    let elem_ty = elem_opt?;
                    Some(r.register_type(TypeKind::Function {
                        params: vec![elem_ty],
                        ret: declared_cb_ret,
                    }))
                }
                ("sort_by" | "sort_by_lt" | "is_sorted_by_lt", 0)
                    if args.len() == 1 && arg_kind == NodeKind::Lambda =>
                {
                    let elem_ty = elem_opt?;
                    Some(r.register_type(TypeKind::Function {
                        params: vec![elem_ty, elem_ty],
                        ret: declared_cb_ret,
                    }))
                }
                ("reduce", 0) if args.len() == 1 && arg_kind == NodeKind::Lambda => {
                    let elem_ty = elem_opt?;
                    let acc_ty = if let Some(body_ret) =
                        crate::flow_narrowing::preview_binary_lambda_return(
                            r, ast, arg_node, elem_ty, elem_ty,
                        )
                        && r.type_pool.is_subtype(body_ret, elem_ty)
                    {
                        elem_ty
                    } else {
                        Intrinsic::Any.type_index()
                    };
                    Some(r.register_type(TypeKind::Function {
                        params: vec![acc_ty, elem_ty],
                        ret: Intrinsic::Any.type_index(),
                    }))
                }
                ("fold", 1) if args.len() == 2 && arg_kind == NodeKind::Lambda => {
                    let elem_ty = elem_opt.unwrap_or(Intrinsic::Any.type_index());
                    let init_ty = r
                        .node_types
                        .get(&args[0])
                        .copied()
                        .unwrap_or(Intrinsic::Any.type_index());
                    let init_is_concrete = init_ty != TypeIndex::INVALID
                        && r.type_pool.as_intrinsic(init_ty) != Some(Intrinsic::Any)
                        && r.type_pool.as_intrinsic(init_ty) != Some(Intrinsic::NoReturn)
                        && init_ty != r.type_pool.null_type();
                    let acc_ty = if init_is_concrete
                        && let Some(body_ret) = crate::flow_narrowing::preview_binary_lambda_return(
                            r, ast, arg_node, init_ty, elem_ty,
                        )
                        && r.type_pool.is_subtype(body_ret, init_ty)
                    {
                        r.collection_provenance
                            .refined_call_results
                            .insert(call_node, init_ty);
                        init_ty
                    } else {
                        Intrinsic::Any.type_index()
                    };
                    if acc_ty == Intrinsic::Any.type_index()
                        && elem_ty == Intrinsic::Any.type_index()
                    {
                        return default_expected;
                    }
                    Some(r.register_type(TypeKind::Function {
                        params: vec![acc_ty, elem_ty],
                        ret: Intrinsic::Any.type_index(),
                    }))
                }
                _ => default_expected,
            }
        }
        CollectionRole::Map => {
            let val_ty = match lookup_expr_elem(r, ast, receiver) {
                TrackedElem::Known(t) => t,
                TrackedElem::Empty | TrackedElem::Dynamic => return default_expected,
            };
            match (method_name.as_str(), arg_index) {
                ("filter" | "retain" | "map_values", 0)
                    if args.len() == 1 && arg_kind == NodeKind::Lambda =>
                {
                    Some(r.register_type(TypeKind::Function {
                        params: vec![Intrinsic::Str.type_index(), val_ty],
                        ret: declared_cb_ret,
                    }))
                }
                _ => default_expected,
            }
        }
        _ => default_expected,
    }
}

/// Record collection output provenance on `call_node` after its arguments have been resolved,
/// and return an upgraded return type if `List.fold` proved an inductive accumulator type.
pub(crate) fn finalize_collection_call(
    r: &mut Resolver<'_>,
    ast: &Ast,
    call_node: NodeIndex,
    callee: NodeIndex,
    args: &[NodeIndex],
) -> Option<TypeIndex> {
    // 1. Constructors via `Type(...)` (`List()`, `Map()`, `Set()`, `Deque()`, `HashMap()`).
    if args.is_empty()
        && let Some(&ctor_sym) = r.constructor_types.get(&call_node)
    {
        let ty = r.symbols[ctor_sym.0 as usize].type_index;
        if ty != TypeIndex::INVALID && is_tracked_collection_type(r, ty) {
            r.collection_provenance
                .node_elements
                .insert(call_node, TrackedElem::Empty);
        }
    }

    // 2. Associated constructors (`List.new()`, `Set.new()`, `Set.from_list(xs)`, `List.pack(...)`, ...).
    if ast.node(callee).kind == NodeKind::Projection {
        let proj_ch = ast.fixed_children(callee);
        if !proj_ch[1].is_null() {
            let mname = str_interner::get(ast.node(proj_ch[1]).str_id);
            if let Some(&sym_id) = r.node_symbols.get(&callee) {
                let sym = &r.symbols[sym_id.0 as usize];
                if let TypeKind::Function { ret, .. } = r.type_pool.get(sym.type_index).kind
                    && is_tracked_collection_type(r, ret)
                {
                    match (mname.as_str(), args.len()) {
                        ("new", 0) => {
                            r.collection_provenance
                                .node_elements
                                .insert(call_node, TrackedElem::Empty);
                        }
                        ("from_list", 1) => {
                            let elem = lookup_expr_elem(r, ast, args[0]);
                            r.collection_provenance
                                .node_elements
                                .insert(call_node, elem);
                        }
                        ("pack", _) => {
                            let mut state = TrackedElem::Empty;
                            for &arg in args {
                                let s = if ast.node(arg).kind == NodeKind::SpreadArg {
                                    lookup_expr_elem(r, ast, ast.fixed_children(arg)[0])
                                } else {
                                    match r.node_types.get(&arg).copied() {
                                        Some(t) => normalize_elem_type(r, t),
                                        None => TrackedElem::Dynamic,
                                    }
                                };
                                state = combine_elem(r, state, s);
                            }
                            r.collection_provenance
                                .node_elements
                                .insert(call_node, state);
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // 2b. `std.iter.{once, repeat}` element provenance.
    if let Some(&fn_sym) = r.node_symbols.get(&callee) {
        let fn_info = &r.symbols[fn_sym.0 as usize];
        if r.options.privileged_nodes.contains(&fn_info.def_node)
            && (fn_info.name == str_interner::intern("once")
                || fn_info.name == str_interner::intern("repeat"))
            && (crate::trusted::std_function(r, "iter", "once") == Some(fn_sym)
                || crate::trusted::std_function(r, "iter", "repeat") == Some(fn_sym))
            && let Some(&val_node) = args.first()
            && let Some(&val_ty) = r.node_types.get(&crate::argument_value_node(ast, val_node))
        {
            let tracked = normalize_elem_type(r, val_ty);
            r.collection_provenance
                .node_elements
                .insert(call_node, tracked);
        }
    }

    // 3. Instance method calls (`xs.map(...)`, `xs.filter(...)`, `xs.fold(...)`, `str.split(...)`, ...).
    if ast.node(callee).kind == NodeKind::Projection
        && let Some(&method_sym) = r.instance_methods.get(&callee)
    {
        let method_info = &r.symbols[method_sym.0 as usize];
        let mname = str_interner::get(method_info.name);
        let receiver = ast.fixed_children(callee)[0];
        if let Some(&recv_ty) = r.node_types.get(&receiver)
            && let Some(recv_canon) = r.type_pool.canonical_type(recv_ty)
        {
            if matches!(mname.as_str(), "iter" | "into_iter") && args.is_empty() {
                if let Some(item_ty) = iterator_item_for_expr(r, ast, receiver) {
                    let tracked = normalize_elem_type(r, item_ty);
                    r.collection_provenance
                        .node_elements
                        .insert(call_node, tracked);
                }
            } else if crate::iterator_defaults::is_default_template(r, method_sym) {
                let recv_item = iterator_item_for_expr(r, ast, receiver)
                    .map(|t| normalize_elem_type(r, t))
                    .unwrap_or(TrackedElem::Dynamic);
                match mname.as_str() {
                    "filter" | "take" | "skip" | "take_while" | "skip_while" | "step_by"
                    | "inspect" | "peekable" | "collect" | "to_list" | "to_set" => {
                        r.collection_provenance
                            .node_elements
                            .insert(call_node, recv_item);
                    }
                    "chain" if args.len() == 1 => {
                        let other_arg = crate::argument_value_node(ast, args[0]);
                        let other_item = iterator_item_for_expr(r, ast, other_arg)
                            .map(|t| normalize_elem_type(r, t))
                            .unwrap_or(TrackedElem::Dynamic);
                        let combined = combine_elem(r, recv_item, other_item);
                        r.collection_provenance
                            .node_elements
                            .insert(call_node, combined);
                    }
                    "enumerate" if args.is_empty() => {
                        if let TrackedElem::Known(item_ty) = recv_item {
                            let pair_ty = r.register_type(TypeKind::Tuple {
                                elements: vec![Intrinsic::Usize.type_index(), item_ty],
                            });
                            r.collection_provenance
                                .node_elements
                                .insert(call_node, TrackedElem::Known(pair_ty));
                        }
                    }
                    "zip" if args.len() == 1 => {
                        let other_arg = crate::argument_value_node(ast, args[0]);
                        let other_item = iterator_item_for_expr(r, ast, other_arg)
                            .map(|t| normalize_elem_type(r, t))
                            .unwrap_or(TrackedElem::Dynamic);
                        let a_ty = match recv_item {
                            TrackedElem::Known(t) => Some(t),
                            _ => None,
                        };
                        let b_ty = match other_item {
                            TrackedElem::Known(t) => Some(t),
                            _ => None,
                        };
                        if a_ty.is_some() || b_ty.is_some() {
                            let pair_ty = r.register_type(TypeKind::Tuple {
                                elements: vec![
                                    a_ty.unwrap_or(Intrinsic::Any.type_index()),
                                    b_ty.unwrap_or(Intrinsic::Any.type_index()),
                                ],
                            });
                            r.collection_provenance
                                .node_elements
                                .insert(call_node, TrackedElem::Known(pair_ty));
                        }
                    }
                    "map" if args.len() == 1 => {
                        let cb = crate::argument_value_node(ast, args[0]);
                        let raw_ret = r
                            .collection_provenance
                            .lambda_body_returns
                            .get(&cb)
                            .copied()
                            .or_else(|| {
                                let cb_ty = *r.node_types.get(&cb)?;
                                let cb_canon = r.type_pool.canonical_type(cb_ty)?;
                                match r.type_pool.get(cb_canon).kind {
                                    TypeKind::Function { ret, .. } => Some(ret),
                                    _ => None,
                                }
                            });
                        let mapped_elem = match raw_ret {
                            Some(ret_ty) => {
                                let (_, _, _, core_ret) =
                                    crate::stdlib_refinements::decompose_return_qualifiers(
                                        r, ret_ty,
                                    );
                                normalize_elem_type(r, core_ret)
                            }
                            None => TrackedElem::Dynamic,
                        };
                        r.collection_provenance
                            .node_elements
                            .insert(call_node, mapped_elem);
                    }
                    "filter_map" if args.len() == 1 => {
                        let cb = crate::argument_value_node(ast, args[0]);
                        let raw_ret = r
                            .collection_provenance
                            .lambda_body_returns
                            .get(&cb)
                            .copied()
                            .or_else(|| {
                                let cb_ty = *r.node_types.get(&cb)?;
                                let cb_canon = r.type_pool.canonical_type(cb_ty)?;
                                match r.type_pool.get(cb_canon).kind {
                                    TypeKind::Function { ret, .. } => Some(ret),
                                    _ => None,
                                }
                            });
                        if let Some(ret_ty) = raw_ret {
                            let (_, _, _, core_ret) =
                                crate::stdlib_refinements::decompose_return_qualifiers(r, ret_ty);
                            if let Some(canon_core) = r.type_pool.canonical_type(core_ret)
                                && let TypeKind::Optional { inner } =
                                    r.type_pool.get(canon_core).kind
                            {
                                let tracked = normalize_elem_type(r, inner);
                                r.collection_provenance
                                    .node_elements
                                    .insert(call_node, tracked);
                            }
                        }
                    }
                    _ => {}
                }
            } else if r.type_pool.collection_role(recv_canon) == Some(CollectionRole::List) {
                let recv_elem = lookup_expr_elem(r, ast, receiver);
                match mname.as_str() {
                    "copy" | "slice" | "take" | "drop" | "reverse" | "unique" | "filter"
                    | "rotate_left" | "rotate_right" | "repeat" | "split_off" => {
                        r.collection_provenance
                            .node_elements
                            .insert(call_node, recv_elem);
                    }
                    "concat" if args.len() == 1 => {
                        let other_elem = lookup_expr_elem(r, ast, args[0]);
                        let combined = combine_elem(r, recv_elem, other_elem);
                        r.collection_provenance
                            .node_elements
                            .insert(call_node, combined);
                    }
                    "map" if args.len() == 1 => {
                        let cb = crate::argument_value_node(ast, args[0]);
                        let mapped_elem = if let Some(&ret_ty) =
                            r.collection_provenance.lambda_body_returns.get(&cb)
                        {
                            let (_, _, _, core_ret) =
                                crate::stdlib_refinements::decompose_return_qualifiers(r, ret_ty);
                            normalize_elem_type(r, core_ret)
                        } else if let Some(&cb_ty) = r.node_types.get(&cb)
                            && let Some(cb_canon) = r.type_pool.canonical_type(cb_ty)
                            && let TypeKind::Function { ret, .. } = r.type_pool.get(cb_canon).kind
                        {
                            let (_, _, _, core_ret) =
                                crate::stdlib_refinements::decompose_return_qualifiers(r, ret);
                            normalize_elem_type(r, core_ret)
                        } else {
                            TrackedElem::Dynamic
                        };
                        r.collection_provenance
                            .node_elements
                            .insert(call_node, mapped_elem);
                    }
                    _ => {}
                }
            } else if r.type_pool.collection_role(recv_canon) == Some(CollectionRole::Map) {
                let recv_val = lookup_expr_elem(r, ast, receiver);
                match mname.as_str() {
                    "copy" | "filter" | "values" => {
                        r.collection_provenance
                            .node_elements
                            .insert(call_node, recv_val);
                    }
                    "keys" => {
                        r.collection_provenance
                            .node_elements
                            .insert(call_node, TrackedElem::Known(Intrinsic::Str.type_index()));
                    }
                    "entries" => {
                        let val_ty = match recv_val {
                            TrackedElem::Known(v) => v,
                            _ => Intrinsic::Any.type_index(),
                        };
                        let pair_ty = r.register_type(TypeKind::Tuple {
                            elements: vec![Intrinsic::Str.type_index(), val_ty],
                        });
                        r.collection_provenance
                            .node_elements
                            .insert(call_node, TrackedElem::Known(pair_ty));
                    }
                    "map_values" if args.len() == 1 => {
                        let cb = crate::argument_value_node(ast, args[0]);
                        let mapped_val = if let Some(&ret_ty) =
                            r.collection_provenance.lambda_body_returns.get(&cb)
                        {
                            let (_, _, _, core_ret) =
                                crate::stdlib_refinements::decompose_return_qualifiers(r, ret_ty);
                            normalize_elem_type(r, core_ret)
                        } else {
                            TrackedElem::Dynamic
                        };
                        r.collection_provenance
                            .node_elements
                            .insert(call_node, mapped_val);
                    }
                    _ => {}
                }
            } else if r.type_pool.as_intrinsic(recv_canon) == Some(Intrinsic::Str) {
                match mname.as_str() {
                    "split"
                    | "lines"
                    | "lines_iter"
                    | "split_whitespace"
                    | "split_ascii_whitespace"
                    | "splitn"
                    | "rsplit"
                    | "rsplitn"
                    | "split_inclusive"
                    | "split_terminator"
                    | "rsplit_terminator" => {
                        r.collection_provenance
                            .node_elements
                            .insert(call_node, TrackedElem::Known(Intrinsic::Str.type_index()));
                    }
                    "chars" | "chars_iter" => {
                        r.collection_provenance
                            .node_elements
                            .insert(call_node, TrackedElem::Known(Intrinsic::Char.type_index()));
                    }
                    "char_indices" => {
                        r.collection_provenance
                            .node_elements
                            .insert(call_node, TrackedElem::Known(Intrinsic::I64.type_index()));
                    }
                    "char_indices_iter" => {
                        let pair_ty = r.register_type(TypeKind::Tuple {
                            elements: vec![
                                Intrinsic::Usize.type_index(),
                                Intrinsic::Char.type_index(),
                            ],
                        });
                        r.collection_provenance
                            .node_elements
                            .insert(call_node, TrackedElem::Known(pair_ty));
                    }
                    "bytes" | "bytes_iter" => {
                        r.collection_provenance
                            .node_elements
                            .insert(call_node, TrackedElem::Known(Intrinsic::U8.type_index()));
                    }
                    _ => {}
                }
            } else if Some(recv_canon) == r.type_pool.bytes_type()
                && matches!(mname.as_str(), "to_list" | "iter")
            {
                r.collection_provenance
                    .node_elements
                    .insert(call_node, TrackedElem::Known(Intrinsic::U8.type_index()));
            } else if let TypeKind::Struct { name, .. } = r.type_pool.get(recv_canon).kind {
                let sname = str_interner::get(name);
                if matches!(sname.as_str(), "Range" | "RangeInclusive") && mname == "rev" {
                    r.collection_provenance
                        .node_elements
                        .insert(call_node, TrackedElem::Known(Intrinsic::I64.type_index()));
                } else if matches!(sname.as_str(), "Set" | "Deque")
                    && matches!(
                        mname.as_str(),
                        "copy"
                            | "to_list"
                            | "union"
                            | "intersection"
                            | "difference"
                            | "symmetric_difference"
                    )
                {
                    let recv_elem = lookup_expr_elem(r, ast, receiver);
                    let final_elem = if args.len() == 1
                        && matches!(mname.as_str(), "union" | "symmetric_difference")
                    {
                        let other = lookup_expr_elem(r, ast, args[0]);
                        combine_elem(r, recv_elem, other)
                    } else {
                        recv_elem
                    };
                    r.collection_provenance
                        .node_elements
                        .insert(call_node, final_elem);
                } else if sname == "HashMap" && matches!(mname.as_str(), "copy" | "values") {
                    let recv_val = lookup_expr_elem(r, ast, receiver);
                    r.collection_provenance
                        .node_elements
                        .insert(call_node, recv_val);
                }
            }
        }
    }

    if let Some(refined_ret) = r
        .collection_provenance
        .refined_call_results
        .get(&call_node)
        .copied()
    {
        crate::task_combinators::emit_runtime_assert_on_expr(r, call_node, refined_ret);
        return Some(refined_ret);
    }
    None
}
