//! Static element validation and payload refinement for `std.task` Future combinators.

use std::collections::{HashMap, HashSet};

use ast::{Ast, NodeIndex, NodeKind};
use diagnostic::Level;
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::resolver::Resolver;
use crate::{
    CallArgumentValue, Coercion, CoercionKind, ErrorConversionKind, ErrorConversionPlan, SymbolId,
    SymbolKind, typing,
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum TrackedFutureList {
    Empty,
    Futures(TypeIndex),
    NonFuture {
        span_node: NodeIndex,
        found: TypeIndex,
    },
}

#[derive(Default)]
pub(crate) struct TaskCombinatorState {
    pub(crate) future_list_nodes: HashMap<NodeIndex, TrackedFutureList>,
    pub(crate) future_list_symbols: HashMap<SymbolId, TrackedFutureList>,
    pub(crate) await_all_nodes: HashMap<NodeIndex, TypeIndex>,
    pub(crate) await_all_symbols: HashMap<SymbolId, TypeIndex>,
    pub(crate) await_first_nodes: HashMap<NodeIndex, (TypeIndex, TypeIndex)>,
    pub(crate) await_first_symbols: HashMap<SymbolId, (TypeIndex, TypeIndex)>,
    pub(crate) poisoned_symbols: HashSet<SymbolId>,
    pub(crate) pattern_list_elements: HashMap<NodeIndex, TypeIndex>,
    pub(crate) pattern_either_types: HashMap<NodeIndex, (TypeIndex, TypeIndex)>,
}

fn classify_future_element(
    r: &mut Resolver<'_>,
    elem_node: NodeIndex,
    acc: TrackedFutureList,
) -> TrackedFutureList {
    if matches!(acc, TrackedFutureList::NonFuture { .. }) {
        return acc;
    }
    let Some(&raw_ty) = r.node_types.get(&elem_node) else {
        return TrackedFutureList::Futures(Intrinsic::Any.type_index());
    };
    let Some(canon) = r.type_pool.canonical_type(raw_ty) else {
        return TrackedFutureList::Futures(Intrinsic::Any.type_index());
    };
    let payload = match r.type_pool.get(canon).kind {
        TypeKind::Future { inner } => inner,
        TypeKind::Intrinsic(Intrinsic::Any) => Intrinsic::Any.type_index(),
        _ => {
            return TrackedFutureList::NonFuture {
                span_node: elem_node,
                found: canon,
            };
        }
    };
    match acc {
        TrackedFutureList::Empty => TrackedFutureList::Futures(payload),
        TrackedFutureList::Futures(prev) => {
            let joined = crate::errors::join(r, prev, payload);
            TrackedFutureList::Futures(joined)
        }
        TrackedFutureList::NonFuture { .. } => acc,
    }
}

pub(crate) fn record_list_literal(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) {
    let mut state = TrackedFutureList::Empty;
    for &elem in ast.multi_children(node) {
        state = classify_future_element(r, elem, state);
    }
    r.task_combinators.future_list_nodes.insert(node, state);
}

fn unmutated_symbol(r: &Resolver<'_>, node: NodeIndex) -> Option<SymbolId> {
    let sym = *r.node_symbols.get(&node)?;
    if r.assigned_symbols.contains(&sym) || r.task_combinators.poisoned_symbols.contains(&sym) {
        return None;
    }
    let info = &r.symbols[sym.0 as usize];
    matches!(
        info.kind,
        SymbolKind::Variable | SymbolKind::Constant | SymbolKind::Parameter
    )
    .then_some(sym)
}

pub(crate) fn poison_symbol(r: &mut Resolver<'_>, sym: SymbolId) {
    r.task_combinators.future_list_symbols.remove(&sym);
    r.task_combinators.await_all_symbols.remove(&sym);
    r.task_combinators.await_first_symbols.remove(&sym);
    r.task_combinators.poisoned_symbols.insert(sym);
}

pub(crate) fn note_mutation_target(r: &mut Resolver<'_>, ast: &Ast, target: NodeIndex) {
    if target.is_null() {
        return;
    }
    if let Some(&sym) = r.node_symbols.get(&target) {
        poison_symbol(r, sym);
    }
    if ast.node(target).kind == NodeKind::Call {
        let receiver = ast.fixed_children(target)[0];
        if !receiver.is_null()
            && let Some(&sym) = r.node_symbols.get(&receiver)
        {
            poison_symbol(r, sym);
        }
    }
}

pub(crate) fn record_decl(
    r: &mut Resolver<'_>,
    ast: &Ast,
    pattern_node: NodeIndex,
    type_ann_node: NodeIndex,
    value_node: NodeIndex,
    has_else: bool,
) {
    if has_else
        || !type_ann_node.is_null()
        || pattern_node.is_null()
        || value_node.is_null()
        || ast.node(pattern_node).kind != NodeKind::Id
    {
        return;
    }
    let Some(sym) = unmutated_symbol(r, pattern_node) else {
        return;
    };
    if let Some(state) = lookup_tracked_future_list(r, ast, value_node) {
        r.task_combinators.future_list_symbols.insert(sym, state);
    }
    if let Some(elem_ty) = lookup_await_all_element(r, ast, value_node) {
        r.task_combinators.await_all_symbols.insert(sym, elem_ty);
    }
    if let Some(pair) = lookup_await_first_pair(r, ast, value_node) {
        r.task_combinators.await_first_symbols.insert(sym, pair);
    }
}

pub(crate) fn refresh_decl_in_lexical_order(r: &mut Resolver<'_>, ast: &Ast, decl_node: NodeIndex) {
    let children = ast.fixed_children(decl_node);
    if children.len() <= 2 {
        return;
    }
    let has_else = children.len() > 3 && !children[3].is_null();
    let pat = children[0];
    let ann = children[1];
    let val = peel_expr(ast, children[2]);
    if val.is_null() || ast.node(val).kind != NodeKind::Call {
        return;
    }
    let callee = ast.fixed_children(val)[0];
    let args = ast.multi_children(val);
    let Some(sym) = callee_symbol(r, callee) else {
        return;
    };
    let sym_info = &r.symbols[sym.0 as usize];
    let sym_name = str_interner::get(sym_info.name);
    let assoc_type = r
        .scopes
        .get(sym_info.scope.0 as usize)
        .and_then(|s| s.assoc_type);
    let is_privileged = r.options.privileged_nodes.contains(&sym_info.def_node);

    let list_combinator = if is_privileged
        && assoc_type == r.type_pool.list_type()
        && r.type_pool.list_type().is_some()
        && r.instance_methods.contains_key(&callee)
        && ast.node(callee).kind == NodeKind::Projection
        && args.is_empty()
    {
        match sym_name.as_str() {
            "await_all" => Some((ast.fixed_children(callee)[0], "await_all", false)),
            "await_any" => Some((ast.fixed_children(callee)[0], "await_any", true)),
            _ => None,
        }
    } else if crate::trusted::std_function(r, "task", "await_all") == Some(sym) {
        bound_arg_node(r, ast, val, args, 0).map(|n| (n, "await_all", false))
    } else if crate::trusted::std_function(r, "task", "await_any") == Some(sym) {
        bound_arg_node(r, ast, val, args, 0).map(|n| (n, "await_any", true))
    } else {
        None
    };

    let Some((list_expr, combinator_name, is_await_any)) = list_combinator else {
        return;
    };
    // Only re-process if `val` wasn't already refined during initial `resolve_decl`.
    if !is_await_any && r.task_combinators.await_all_nodes.contains_key(&val) {
        return;
    }
    if is_await_any
        && r.node_types
            .get(&val)
            .is_some_and(|&ty| r.type_pool.as_intrinsic(ty) != Some(Intrinsic::Any))
    {
        return;
    }
    if let Some(refined) =
        handle_await_list_combinator(r, ast, val, list_expr, combinator_name, is_await_any)
    {
        if is_await_any && r.type_pool.as_intrinsic(refined) != Some(Intrinsic::Any) {
            r.node_types.insert(val, refined);
            if !has_else
                && ann.is_null()
                && !pat.is_null()
                && ast.node(pat).kind == NodeKind::Id
                && let Some(&pat_sym) = r.node_symbols.get(&pat)
            {
                r.node_types.insert(pat, refined);
                r.symbols[pat_sym.0 as usize].type_index = refined;
            }
        } else if !is_await_any {
            if has_else {
                attach_scrutinee_pattern(r, ast, val, pat);
                if let Some(list_ty) = r.type_pool.list_type() {
                    crate::enums::pattern(r, ast, pat, Some(list_ty));
                }
            } else {
                record_decl(r, ast, pat, ann, val, false);
            }
        }
    }
}

fn peel_expr(ast: &Ast, mut expr: NodeIndex) -> NodeIndex {
    while !expr.is_null() && ast.node(expr).kind == NodeKind::ExprStatement {
        expr = ast.fixed_children(expr)[0];
    }
    expr
}

fn lookup_tracked_future_list(
    r: &Resolver<'_>,
    ast: &Ast,
    expr: NodeIndex,
) -> Option<TrackedFutureList> {
    let expr = peel_expr(ast, expr);
    if expr.is_null() {
        return None;
    }
    if let Some(&state) = r.task_combinators.future_list_nodes.get(&expr) {
        return Some(state);
    }
    let sym = unmutated_symbol(r, expr)?;
    r.task_combinators.future_list_symbols.get(&sym).copied()
}

pub(crate) fn lookup_await_all_element(
    r: &Resolver<'_>,
    ast: &Ast,
    expr: NodeIndex,
) -> Option<TypeIndex> {
    let expr = peel_expr(ast, expr);
    if expr.is_null() {
        return None;
    }
    if let Some(&elem_ty) = r.task_combinators.await_all_nodes.get(&expr) {
        return Some(elem_ty);
    }
    let sym = unmutated_symbol(r, expr)?;
    r.task_combinators.await_all_symbols.get(&sym).copied()
}

pub(crate) fn lookup_await_first_pair(
    r: &Resolver<'_>,
    ast: &Ast,
    expr: NodeIndex,
) -> Option<(TypeIndex, TypeIndex)> {
    let expr = peel_expr(ast, expr);
    if expr.is_null() {
        return None;
    }
    if let Some(&pair) = r.task_combinators.await_first_nodes.get(&expr) {
        return Some(pair);
    }
    let sym = unmutated_symbol(r, expr)?;
    r.task_combinators.await_first_symbols.get(&sym).copied()
}

pub(crate) fn emit_runtime_assert_on_expr(
    r: &mut Resolver<'_>,
    node: NodeIndex,
    target: TypeIndex,
) {
    let Some(canon) = r.type_pool.canonical_type(target) else {
        return;
    };
    if r.type_pool.as_intrinsic(canon) == Some(Intrinsic::Any) {
        return;
    }
    if matches!(r.type_pool.get(canon).kind, TypeKind::Trait { .. }) {
        r.node_coercions.insert(
            node,
            Coercion {
                target,
                kind: CoercionKind::Assert,
            },
        );
        return;
    }
    let mut check_target = target;
    while let Some(c) = r.type_pool.canonical_type(check_target) {
        if let TypeKind::EffectQualified { inner, .. } = &r.type_pool.get(c).kind {
            check_target = *inner;
        } else {
            break;
        }
    }
    if r.type_pool.as_intrinsic(check_target) == Some(Intrinsic::Any) {
        return;
    }
    r.error_conversions
        .entry(node)
        .or_default()
        .push(ErrorConversionPlan {
            source: Intrinsic::Any.type_index(),
            target: check_target,
            kind: ErrorConversionKind::CheckQualified,
        });
}

pub(crate) fn record_pattern_element_coercion(
    r: &mut Resolver<'_>,
    ast: &Ast,
    pattern: NodeIndex,
    target: TypeIndex,
) {
    if pattern.is_null() || r.type_pool.as_intrinsic(target) == Some(Intrinsic::Any) {
        return;
    }
    let mut check_target = target;
    while let Some(c) = r.type_pool.canonical_type(check_target) {
        if let TypeKind::EffectQualified { inner, .. } = &r.type_pool.get(c).kind {
            check_target = *inner;
        } else {
            break;
        }
    }
    if r.type_pool.as_intrinsic(check_target) == Some(Intrinsic::Any) {
        return;
    }
    match ast.node(pattern).kind {
        NodeKind::Id if !r.enum_variants.contains_key(&pattern) => {
            r.node_coercions.insert(
                pattern,
                Coercion {
                    target: check_target,
                    kind: CoercionKind::Assert,
                },
            );
        }
        NodeKind::PatternAsBind => {
            let children = ast.fixed_children(pattern);
            record_pattern_element_coercion(r, ast, children[0], target);
            let alias = children[1];
            if !alias.is_null() && ast.node(alias).kind == NodeKind::Id {
                r.node_coercions.insert(
                    alias,
                    Coercion {
                        target: check_target,
                        kind: CoercionKind::Assert,
                    },
                );
            }
        }
        NodeKind::PatternIfGuard => {
            record_pattern_element_coercion(r, ast, ast.fixed_children(pattern)[0], target);
        }
        NodeKind::PatternOr => {
            let children = ast.fixed_children(pattern);
            record_pattern_element_coercion(r, ast, children[0], target);
            record_pattern_element_coercion(r, ast, children[1], target);
        }
        _ => {}
    }
}

pub(crate) fn attach_scrutinee_pattern(
    r: &mut Resolver<'_>,
    ast: &Ast,
    scrutinee: NodeIndex,
    pattern: NodeIndex,
) {
    let elem_ty = lookup_await_all_element(r, ast, scrutinee);
    let either_pair = lookup_await_first_pair(r, ast, scrutinee);
    if elem_ty.is_none() && either_pair.is_none() {
        return;
    }
    attach_pattern_inner(r, ast, pattern, elem_ty, either_pair);
}

fn attach_pattern_inner(
    r: &mut Resolver<'_>,
    ast: &Ast,
    pattern: NodeIndex,
    elem_ty: Option<TypeIndex>,
    either_pair: Option<(TypeIndex, TypeIndex)>,
) {
    if pattern.is_null() {
        return;
    }
    match ast.node(pattern).kind {
        NodeKind::PatternIfGuard | NodeKind::PatternAsBind => {
            attach_pattern_inner(r, ast, ast.fixed_children(pattern)[0], elem_ty, either_pair);
        }
        NodeKind::PatternOr => {
            let children = ast.fixed_children(pattern);
            attach_pattern_inner(r, ast, children[0], elem_ty, either_pair);
            attach_pattern_inner(r, ast, children[1], elem_ty, either_pair);
        }
        NodeKind::PatternList => {
            if let Some(ty) = elem_ty {
                r.task_combinators.pattern_list_elements.insert(pattern, ty);
            }
        }
        NodeKind::PatternCall | NodeKind::PropertyPattern | NodeKind::Projection | NodeKind::Id => {
            if let Some(pair) = either_pair {
                r.task_combinators
                    .pattern_either_types
                    .insert(pattern, pair);
            }
        }
        _ => {}
    }
}

pub(crate) fn refined_either_variant_field(
    r: &Resolver<'_>,
    pattern: NodeIndex,
    enum_ty: TypeIndex,
    variant_tag: u32,
) -> Option<TypeIndex> {
    let &(left_ty, right_ty) = r.task_combinators.pattern_either_types.get(&pattern)?;
    let (either_ty, _) = crate::trusted::std_type(r, "task", "Either")?;
    if r.type_pool.canonical_type(enum_ty) != Some(either_ty) {
        return None;
    }
    match variant_tag {
        0 => Some(left_ty),
        1 => Some(right_ty),
        _ => None,
    }
}

fn callee_symbol(r: &Resolver<'_>, callee: NodeIndex) -> Option<SymbolId> {
    r.node_symbols
        .get(&callee)
        .copied()
        .or_else(|| r.instance_methods.get(&callee).copied())
}

pub(crate) fn should_infer_unconstrained_lambda(
    r: &Resolver<'_>,
    ast: &Ast,
    callee: NodeIndex,
    parameter_index: usize,
    arg: NodeIndex,
) -> bool {
    if parameter_index != 1 || arg.is_null() {
        return false;
    }
    let val = crate::argument_value_node(ast, arg);
    if val.is_null()
        || !matches!(
            ast.node(val).kind,
            NodeKind::Lambda | NodeKind::CaseMap | NodeKind::CaseAlternative
        )
    {
        return false;
    }
    let Some(sym) = callee_symbol(r, callee) else {
        return false;
    };
    crate::trusted::std_function(r, "task", "with_timeout") == Some(sym)
}

fn bound_arg_node(
    r: &Resolver<'_>,
    ast: &Ast,
    call_node: NodeIndex,
    args: &[NodeIndex],
    param_idx: usize,
) -> Option<NodeIndex> {
    if let Some(plan) = r.call_arguments.get(&call_node) {
        if let Some(binding) = plan.parameters.get(param_idx)
            && let CallArgumentValue::Explicit { source_index } = binding.value
        {
            return args
                .get(source_index)
                .map(|&arg| crate::argument_value_node(ast, arg));
        }
        return None;
    }
    args.get(param_idx)
        .map(|&arg| crate::argument_value_node(ast, arg))
}

fn handle_await_list_combinator(
    r: &mut Resolver<'_>,
    ast: &Ast,
    call_node: NodeIndex,
    list_expr: NodeIndex,
    combinator_name: &str,
    is_await_any: bool,
) -> Option<TypeIndex> {
    match lookup_tracked_future_list(r, ast, list_expr) {
        Some(TrackedFutureList::NonFuture { span_node, found }) => {
            let found_name = typing::type_display_name(&r.type_pool, found);
            r.diag_ctx
                .error(format!(
                    "`{combinator_name}` requires `Future` elements, found `{found_name}`"
                ))
                .with_primary_span(ast.node(span_node).span)
                .emit(r.diag_ctx);
            if is_await_any {
                Some(Intrinsic::Any.type_index())
            } else {
                r.type_pool.list_type()
            }
        }
        Some(TrackedFutureList::Futures(elem_ty)) => {
            if is_await_any {
                emit_runtime_assert_on_expr(r, call_node, elem_ty);
                Some(elem_ty)
            } else {
                r.task_combinators
                    .await_all_nodes
                    .insert(call_node, elem_ty);
                r.type_pool.list_type()
            }
        }
        Some(TrackedFutureList::Empty) | None => {
            if is_await_any {
                Some(Intrinsic::Any.type_index())
            } else {
                r.type_pool.list_type()
            }
        }
    }
}

pub(crate) fn refine_call(
    r: &mut Resolver<'_>,
    ast: &Ast,
    call_node: NodeIndex,
    callee: NodeIndex,
    args: &[NodeIndex],
) -> Option<TypeIndex> {
    let sym = callee_symbol(r, callee)?;
    let sym_info = &r.symbols[sym.0 as usize];
    let sym_name = str_interner::get(sym_info.name);
    let assoc_type = r
        .scopes
        .get(sym_info.scope.0 as usize)
        .and_then(|s| s.assoc_type);
    let is_privileged = r.options.privileged_nodes.contains(&sym_info.def_node);

    // 1. Track empty `List()` / `List.new()` and `List` instance methods.
    if is_privileged && assoc_type == r.type_pool.list_type() && r.type_pool.list_type().is_some() {
        if sym_name == "new" && args.is_empty() && !r.instance_methods.contains_key(&callee) {
            r.task_combinators
                .future_list_nodes
                .insert(call_node, TrackedFutureList::Empty);
            return None;
        }
        if r.instance_methods.contains_key(&callee) && ast.node(callee).kind == NodeKind::Projection
        {
            let receiver = ast.fixed_children(callee)[0];
            match sym_name.as_str() {
                "await_all" if args.is_empty() => {
                    return handle_await_list_combinator(
                        r,
                        ast,
                        call_node,
                        receiver,
                        "await_all",
                        false,
                    );
                }
                "await_any" if args.is_empty() => {
                    return handle_await_list_combinator(
                        r,
                        ast,
                        call_node,
                        receiver,
                        "await_any",
                        true,
                    );
                }
                "get" if args.len() == 1 => {
                    if let Some(elem_ty) = lookup_await_all_element(r, ast, receiver) {
                        emit_runtime_assert_on_expr(r, call_node, elem_ty);
                        return Some(elem_ty);
                    }
                    return None;
                }
                "push" if args.len() == 1 => {
                    if let Some(recv_sym) = unmutated_symbol(r, receiver) {
                        r.task_combinators.await_all_symbols.remove(&recv_sym);
                        if let Some(&prev) = r.task_combinators.future_list_symbols.get(&recv_sym) {
                            let val_node = crate::argument_value_node(ast, args[0]);
                            let next = classify_future_element(r, val_node, prev);
                            r.task_combinators
                                .future_list_symbols
                                .insert(recv_sym, next);
                        }
                    }
                    return None;
                }
                "len" | "is_empty" | "apply" | "first" | "last" | "contains" | "copy" | "pop"
                | "truncate" | "reverse_in_place" | "fold" | "map" | "filter" | "each" | "any"
                | "all" | "format_list" | "join" => {
                    return None;
                }
                _ => {
                    if !receiver.is_null()
                        && let Some(&recv_sym) = r.node_symbols.get(&receiver)
                    {
                        poison_symbol(r, recv_sym);
                    }
                    return None;
                }
            }
        }
    }

    // 2. `Either.unwrap_left()` / `Either.unwrap_right()` on `await_first` results.
    if is_privileged
        && r.instance_methods.contains_key(&callee)
        && ast.node(callee).kind == NodeKind::Projection
        && args.is_empty()
        && matches!(sym_name.as_str(), "unwrap_left" | "unwrap_right")
        && let Some((either_ty, _)) = crate::trusted::std_type(r, "task", "Either")
        && assoc_type == Some(either_ty)
    {
        let receiver = ast.fixed_children(callee)[0];
        if let Some((left_ty, right_ty)) = lookup_await_first_pair(r, ast, receiver) {
            let target = if sym_name == "unwrap_left" {
                left_ty
            } else {
                right_ty
            };
            emit_runtime_assert_on_expr(r, call_node, target);
            return Some(target);
        }
        return None;
    }

    // 3. `std.task.{await_all, await_any, await_first, with_timeout}` free functions.
    if crate::trusted::std_function(r, "task", "await_all") == Some(sym) {
        let list_arg = bound_arg_node(r, ast, call_node, args, 0)?;
        return handle_await_list_combinator(r, ast, call_node, list_arg, "await_all", false);
    }
    if crate::trusted::std_function(r, "task", "await_any") == Some(sym) {
        let list_arg = bound_arg_node(r, ast, call_node, args, 0)?;
        return handle_await_list_combinator(r, ast, call_node, list_arg, "await_any", true);
    }
    if crate::trusted::std_function(r, "task", "await_first") == Some(sym) {
        let first_arg = bound_arg_node(r, ast, call_node, args, 0)?;
        let second_arg = bound_arg_node(r, ast, call_node, args, 1)?;
        let extract_future_payload = |r: &Resolver<'_>, node: NodeIndex| -> TypeIndex {
            r.node_types
                .get(&node)
                .and_then(|&ty| r.type_pool.canonical_type(ty))
                .map(|canon| match r.type_pool.get(canon).kind {
                    TypeKind::Future { inner } => inner,
                    _ => Intrinsic::Any.type_index(),
                })
                .unwrap_or(Intrinsic::Any.type_index())
        };
        let left_ty = extract_future_payload(r, first_arg);
        let right_ty = extract_future_payload(r, second_arg);
        r.task_combinators
            .await_first_nodes
            .insert(call_node, (left_ty, right_ty));
        return None;
    }
    if crate::trusted::std_function(r, "task", "with_timeout") == Some(sym) {
        let thunk_arg = bound_arg_node(r, ast, call_node, args, 1)?;
        let thunk_ty = *r.node_types.get(&thunk_arg)?;
        let thunk_canon = r.type_pool.canonical_type(thunk_ty)?;
        let thunk_ret = match &r.type_pool.get(thunk_canon).kind {
            TypeKind::Function { params, ret } => {
                if !params.is_empty() {
                    let found = typing::type_display_name(&r.type_pool, thunk_canon);
                    r.diag_ctx
                        .error(format!(
                            "type mismatch in argument 2 of `with_timeout`: expected `fn() -> Any`, found `{found}`"
                        ))
                        .with_primary_span(ast.node(thunk_arg).span)
                        .with_label(
                            ast.node(thunk_arg).span,
                            format!("expected `fn() -> Any`, found `{found}`"),
                            Level::Error,
                        )
                        .emit(r.diag_ctx);
                    return None;
                }
                *ret
            }
            TypeKind::Intrinsic(Intrinsic::Any | Intrinsic::Closure) => Intrinsic::Any.type_index(),
            _ => return None,
        };
        let (timeout_err_ty, _) = crate::trusted::std_type(r, "task", "TimeoutError")?;
        let mut effects = Vec::new();
        let mut core_ret = thunk_ret;
        while let Some(c) = r.type_pool.canonical_type(core_ret) {
            if let TypeKind::EffectQualified {
                effects: effs,
                inner,
            } = &r.type_pool.get(c).kind
            {
                effects.extend(effs.iter().copied());
                core_ret = *inner;
            } else {
                break;
            }
        }
        let refined_err = r
            .type_pool
            .normalize_error_type(vec![timeout_err_ty], core_ret)
            .ok()?;
        r.error_conversions
            .entry(call_node)
            .or_default()
            .push(ErrorConversionPlan {
                source: Intrinsic::Any.type_index(),
                target: refined_err,
                kind: ErrorConversionKind::CheckQualified,
            });
        return Some(crate::effect::normalize(r, effects, refined_err));
    }

    None
}
