//! Static type refinements for idiomatic `std` APIs:
//! - First-class `Type` parsing (`String.parse`, `String.try_parse`, `conv.parse`, `conv.try_parse`)
//! - First-class `Type` JSON decoding (`json.decode(T, input, ...)`)
//! - Higher-order callback parameter inference & `!Error` / `#Effect` / return-type propagation
//!   (`File.with_open`, `io.{with_stdio, capture_output}`, `time.{with_system_clock, with_mock_clock}`,
//!   `rand.{with_rng, with_seeded_rng}`, `sync.{Mutex, RwLock, Semaphore, Once}`,
//!   `task.{par_map, par_for_each, race}`, `stream.{collect, for_each, fold}`, and fallible/effectful
//!   `List` / `Map` / `Iterator` / `pattern.replace_with` callbacks).

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{CollectionRole, ErrorDomain, Intrinsic, TypeIndex, TypeKind};

use crate::collection_inference::{TrackedElem, lookup_expr_elem};
use crate::resolver::Resolver;
use crate::{
    CallArgumentValue, ErrorConversionKind, ErrorConversionPlan, SymbolId, SymbolKind, typing,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JsonDecodeCallPlan {
    pub from_json_symbol: SymbolId,
    pub takes_allow_unknown: bool,
}

fn peel_expr(ast: &Ast, mut expr: NodeIndex) -> NodeIndex {
    while !expr.is_null() && ast.node(expr).kind == NodeKind::ExprStatement {
        expr = ast.fixed_children(expr)[0];
    }
    expr
}

fn callee_symbol(r: &Resolver<'_>, callee: NodeIndex) -> Option<SymbolId> {
    r.node_symbols
        .get(&callee)
        .copied()
        .or_else(|| r.instance_methods.get(&callee).copied())
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

fn is_std_fn(r: &Resolver<'_>, sym: SymbolId, module: &str, fn_name: &str) -> bool {
    let sym_info = &r.symbols[sym.0 as usize];
    sym_info.kind == SymbolKind::Function
        && sym_info.name == str_interner::intern(fn_name)
        && r.options.privileged_nodes.contains(&sym_info.def_node)
        && crate::trusted::std_function(r, module, fn_name) == Some(sym)
}

fn is_std_method(
    r: &Resolver<'_>,
    sym: SymbolId,
    module: &str,
    type_name: &str,
    method_name: &str,
) -> bool {
    let sym_info = &r.symbols[sym.0 as usize];
    if sym_info.kind != SymbolKind::Function
        || sym_info.name != str_interner::intern(method_name)
        || !r.options.privileged_nodes.contains(&sym_info.def_node)
    {
        return false;
    }
    let Some(assoc) = r.scopes[sym_info.scope.0 as usize]
        .assoc_type
        .and_then(|t| r.type_pool.canonical_type(t))
    else {
        return false;
    };
    crate::trusted::std_type(r, module, type_name).is_some_and(|(ty, _)| ty == assoc)
}

fn std_effect_type(r: &Resolver<'_>, module: &str, effect_name: &str) -> Option<TypeIndex> {
    let target = str_interner::intern(effect_name);
    let mod_id = str_interner::intern(module);
    for &pkg_node in &r.options.package_roots {
        let std_scope_opt = r.imports.module_scopes.get(&pkg_node).copied().or_else(|| {
            r.scopes
                .iter()
                .find(|sc| sc.node == pkg_node)
                .map(|sc| sc.id)
        });
        if let Some(std_scope_id) = std_scope_opt
            && let Some(&mod_sym_id) = r.scopes[std_scope_id.0 as usize].bindings.get(&mod_id)
            && let Some(&mod_scope_id) = r
                .imports
                .module_scopes
                .get(&r.symbols[mod_sym_id.0 as usize].def_node)
            && let Some(&eff_sym_id) = r.scopes[mod_scope_id.0 as usize].bindings.get(&target)
        {
            let sym = &r.symbols[eff_sym_id.0 as usize];
            if sym.kind == SymbolKind::Effect && r.options.privileged_nodes.contains(&sym.def_node)
            {
                return r.type_pool.canonical_type(sym.type_index);
            }
        }
    }
    None
}

fn is_parseable_scalar(r: &Resolver<'_>, ty: TypeIndex) -> bool {
    let Some(canon) = r.type_pool.canonical_type(ty) else {
        return false;
    };
    let Some(intr) = r.type_pool.as_intrinsic(canon) else {
        return false;
    };
    intr.is_numeric() || matches!(intr, Intrinsic::Bool | Intrinsic::Char)
}

/// Decompose a function return type into `(effects, closed_errors, has_open_error, core_payload)`.
pub(crate) fn decompose_return_qualifiers(
    r: &Resolver<'_>,
    mut ty: TypeIndex,
) -> (Vec<TypeIndex>, Vec<TypeIndex>, bool, TypeIndex) {
    let mut effects = Vec::new();
    let mut errors = Vec::new();
    let mut open_error = false;
    while let Some(canon) = r.type_pool.canonical_type(ty) {
        if let TypeKind::EffectQualified {
            effects: effs,
            inner,
        } = &r.type_pool.get(canon).kind
        {
            for &e in effs {
                let ec = r.type_pool.canonical_type(e).unwrap_or(e);
                if !effects
                    .iter()
                    .any(|&prev| r.type_pool.canonical_type(prev).unwrap_or(prev) == ec)
                {
                    effects.push(e);
                }
            }
            ty = *inner;
            continue;
        }
        if let Ok(Some(shape)) = r.type_pool.error_shape(canon) {
            match shape.domain {
                ErrorDomain::Closed(errs) => {
                    for e in errs {
                        let ec = r.type_pool.canonical_type(e).unwrap_or(e);
                        if !errors
                            .iter()
                            .any(|&prev| r.type_pool.canonical_type(prev).unwrap_or(prev) == ec)
                        {
                            errors.push(e);
                        }
                    }
                }
                ErrorDomain::Open => {
                    open_error = true;
                }
            }
            ty = shape.inner;
            continue;
        }
        break;
    }
    (effects, errors, open_error, ty)
}

fn rebuild_qualified_return(
    r: &mut Resolver<'_>,
    call_node: NodeIndex,
    effects: Vec<TypeIndex>,
    errors: Vec<TypeIndex>,
    open_error: bool,
    core_ty: TypeIndex,
    assert_payload: bool,
) -> Option<TypeIndex> {
    let err_wrapped = if open_error {
        let open_ty = r
            .type_pool
            .normalize_error_type(vec![Intrinsic::Any.type_index()], core_ty)
            .ok()?;
        r.error_conversions
            .entry(call_node)
            .or_default()
            .push(ErrorConversionPlan {
                source: Intrinsic::Any.type_index(),
                target: open_ty,
                kind: ErrorConversionKind::CheckQualified,
            });
        open_ty
    } else if !errors.is_empty() {
        let closed_ty = r.type_pool.normalize_error_type(errors, core_ty).ok()?;
        r.error_conversions
            .entry(call_node)
            .or_default()
            .push(ErrorConversionPlan {
                source: Intrinsic::Any.type_index(),
                target: closed_ty,
                kind: ErrorConversionKind::CheckQualified,
            });
        closed_ty
    } else {
        if assert_payload {
            crate::task_combinators::emit_runtime_assert_on_expr(r, call_node, core_ty);
        }
        core_ty
    };
    Some(if effects.is_empty() {
        err_wrapped
    } else {
        crate::effect::normalize(r, effects, err_wrapped)
    })
}

fn remove_handled_effects(
    r: &Resolver<'_>,
    effects: &[TypeIndex],
    handled: &[Option<TypeIndex>],
) -> (Vec<TypeIndex>, Vec<TypeIndex>) {
    let mut kept = Vec::new();
    let mut removed = Vec::new();
    for &eff in effects {
        let ec = r.type_pool.canonical_type(eff).unwrap_or(eff);
        if handled
            .iter()
            .flatten()
            .any(|&h| r.type_pool.canonical_type(h).unwrap_or(h) == ec)
        {
            removed.push(ec);
        } else {
            kept.push(eff);
        }
    }
    (kept, removed)
}

fn subtree_may_use_error_or_effect(r: &Resolver<'_>, ast: &Ast, root: NodeIndex) -> bool {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_null() {
            continue;
        }
        let kind = ast.node(node).kind;
        if matches!(
            kind,
            NodeKind::FunctionDef
                | NodeKind::Lambda
                | NodeKind::CaseMap
                | NodeKind::CaseAlternative
        ) && node != root
        {
            continue;
        }
        if matches!(
            kind,
            NodeKind::ErrorConstruction | NodeKind::ErrorPropagation | NodeKind::EffectPropagation
        ) {
            return true;
        }
        if kind == NodeKind::Call {
            let callee = ast.fixed_children(node)[0];
            if let Some(&sym) = r.node_symbols.get(&callee)
                && r.symbols[sym.0 as usize].kind == SymbolKind::Effect
            {
                return true;
            }
        }
        stack.extend(ast.fixed_children(node));
        stack.extend(ast.multi_children(node));
    }
    false
}

fn receiver_iterator_item_type(
    r: &Resolver<'_>,
    ast: &Ast,
    receiver: NodeIndex,
) -> Option<TypeIndex> {
    if let TrackedElem::Known(t) = lookup_expr_elem(r, ast, receiver) {
        return Some(t);
    }
    let recv_ty = *r.node_types.get(&receiver)?;
    let canon = r.type_pool.canonical_type(recv_ty)?;
    let scope = r
        .node_scopes
        .get(&receiver)
        .copied()
        .unwrap_or(r.current_scope);
    let wk = r.type_pool.well_known;
    let record = r
        .type_pool
        .find_trait_impl_scoped(canon, wk.iterator, scope.0)
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
    r.type_pool.canonical_type(binding.value)
}

/// Inspect a call argument before resolving it. When `true` is returned, the caller
/// passes `expected = None` to the inline closure so its body can freely infer
/// `!Error`, `#Effect`, and concrete payload return types, while `inferred_lambda_params`
/// seeds parameter types on `NodeKind::Lambda`.
pub(crate) fn prepare_callback_inference(
    r: &mut Resolver<'_>,
    ast: &Ast,
    call_node: NodeIndex,
    callee: NodeIndex,
    parameter_index: usize,
    arg: NodeIndex,
) -> bool {
    if r.options.privileged_nodes.contains(&call_node) || arg.is_null() {
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
    if !r
        .options
        .privileged_nodes
        .contains(&r.symbols[sym.0 as usize].def_node)
    {
        return false;
    }

    // 1. Dedicated scope / capability / concurrency / stream combinators: ALWAYS infer unconstrained.
    let dedicated_params: Option<Vec<TypeIndex>> =
        if is_std_method(r, sym, "io", "File", "with_open") && parameter_index == 2 {
            crate::trusted::std_type(r, "io", "File").map(|(file_ty, _)| vec![file_ty])
        } else if ((is_std_fn(r, sym, "io", "with_stdio")
            || is_std_fn(r, sym, "io", "capture_output")
            || is_std_fn(r, sym, "time", "with_system_clock")
            || is_std_fn(r, sym, "stream", "collect")
            || is_std_method(r, sym, "sync", "Semaphore", "with_permit")
            || is_std_method(r, sym, "sync", "Once", "call_once"))
            && parameter_index == 0)
            || ((is_std_fn(r, sym, "time", "with_mock_clock")
                || is_std_fn(r, sym, "rand", "with_rng")
                || is_std_fn(r, sym, "rand", "with_seeded_rng"))
                && parameter_index == 1)
        {
            Some(Vec::new())
        } else if (is_std_method(r, sym, "sync", "Mutex", "with_lock")
            || is_std_method(r, sym, "sync", "Mutex", "update")
            || is_std_method(r, sym, "sync", "RwLock", "with_read")
            || is_std_method(r, sym, "sync", "RwLock", "with_write"))
            && parameter_index == 0
        {
            Some(vec![Intrinsic::Any.type_index()])
        } else if (is_std_fn(r, sym, "task", "par_map")
            || is_std_fn(r, sym, "task", "par_for_each"))
            && parameter_index == 1
        {
            let args = ast.multi_children(call_node);
            let elem_ty = bound_arg_node(r, ast, call_node, args, 0)
                .and_then(|items_node| match lookup_expr_elem(r, ast, items_node) {
                    TrackedElem::Known(t) => Some(t),
                    _ => None,
                })
                .unwrap_or(Intrinsic::Any.type_index());
            Some(vec![elem_ty])
        } else if is_std_fn(r, sym, "stream", "for_each") {
            match parameter_index {
                0 => Some(Vec::new()),
                1 => Some(vec![Intrinsic::Any.type_index()]),
                _ => None,
            }
        } else if is_std_fn(r, sym, "stream", "fold") {
            match parameter_index {
                0 => Some(Vec::new()),
                2 => {
                    let args = ast.multi_children(call_node);
                    let init_ty = bound_arg_node(r, ast, call_node, args, 1)
                        .and_then(|n| r.node_types.get(&n).copied())
                        .unwrap_or(Intrinsic::Any.type_index());
                    Some(vec![init_ty, Intrinsic::Any.type_index()])
                }
                _ => None,
            }
        } else if is_std_fn(r, sym, "pattern", "replace_with")
            && parameter_index == 2
            && subtree_may_use_error_or_effect(r, ast, val)
        {
            crate::trusted::std_type(r, "pattern", "PatternMatch").map(|(m_ty, _)| vec![m_ty])
        } else {
            None
        };

    if let Some(params) = dedicated_params {
        if ast.node(val).kind == NodeKind::Lambda && !params.is_empty() {
            r.collection_provenance
                .inferred_lambda_params
                .insert(val, params);
        }
        return true;
    }

    // 2. Fallible / effectful callbacks on `List`, `Map`, and `Iterator` methods.
    if ast.node(val).kind != NodeKind::Lambda || !subtree_may_use_error_or_effect(r, ast, val) {
        return false;
    }
    if ast.node(callee).kind != NodeKind::Projection {
        return false;
    }
    let Some(&method_sym) = r.instance_methods.get(&callee) else {
        return false;
    };
    let method_info = &r.symbols[method_sym.0 as usize];
    if !r.options.privileged_nodes.contains(&method_info.def_node) {
        return false;
    }
    let mname = str_interner::get(method_info.name);
    let receiver = ast.fixed_children(callee)[0];
    let Some(&recv_ty) = r.node_types.get(&receiver) else {
        return false;
    };

    if r.type_pool.collection_role(recv_ty) == Some(CollectionRole::List) {
        let elem_ty = match lookup_expr_elem(r, ast, receiver) {
            TrackedElem::Known(t) => t,
            _ => Intrinsic::Any.type_index(),
        };
        let params = match (mname.as_str(), parameter_index) {
            (
                "map" | "filter" | "filter_map" | "flat_map" | "each" | "foreach" | "any" | "all"
                | "find" | "find_map" | "position" | "rposition" | "partition" | "retain",
                0,
            ) => Some(vec![elem_ty]),
            ("reduce", 0) => Some(vec![elem_ty, elem_ty]),
            ("fold", 1) => {
                let args = ast.multi_children(call_node);
                let init_ty = bound_arg_node(r, ast, call_node, args, 0)
                    .and_then(|n| r.node_types.get(&n).copied())
                    .unwrap_or(Intrinsic::Any.type_index());
                Some(vec![init_ty, elem_ty])
            }
            _ => None,
        };
        if let Some(ps) = params {
            r.collection_provenance
                .inferred_lambda_params
                .insert(val, ps);
            return true;
        }
    } else if r.type_pool.collection_role(recv_ty) == Some(CollectionRole::Map) {
        let val_ty = match lookup_expr_elem(r, ast, receiver) {
            TrackedElem::Known(t) => t,
            _ => Intrinsic::Any.type_index(),
        };
        if matches!(mname.as_str(), "filter" | "map_values" | "retain") && parameter_index == 0 {
            r.collection_provenance
                .inferred_lambda_params
                .insert(val, vec![Intrinsic::Str.type_index(), val_ty]);
            return true;
        }
    } else if crate::iterator_defaults::is_default_template(r, method_sym) {
        let item_ty =
            receiver_iterator_item_type(r, ast, receiver).unwrap_or(Intrinsic::Any.type_index());
        let params = match (mname.as_str(), parameter_index) {
            (
                "map" | "filter" | "filter_map" | "flat_map" | "take_while" | "skip_while"
                | "inspect" | "for_each" | "any" | "all" | "find" | "find_map" | "position"
                | "partition",
                0,
            ) => Some(vec![item_ty]),
            ("reduce", 0) => Some(vec![item_ty, item_ty]),
            ("fold", 1) => {
                let args = ast.multi_children(call_node);
                let init_ty = bound_arg_node(r, ast, call_node, args, 0)
                    .and_then(|n| r.node_types.get(&n).copied())
                    .unwrap_or(Intrinsic::Any.type_index());
                Some(vec![init_ty, item_ty])
            }
            _ => None,
        };
        if let Some(ps) = params {
            r.collection_provenance
                .inferred_lambda_params
                .insert(val, ps);
            return true;
        }
    }

    false
}

fn callable_return_type(r: &Resolver<'_>, arg_node: NodeIndex) -> Option<TypeIndex> {
    let ty = *r.node_types.get(&arg_node)?;
    let canon = r.type_pool.canonical_type(ty)?;
    match &r.type_pool.get(canon).kind {
        TypeKind::Function { ret, .. } => Some(*ret),
        TypeKind::Intrinsic(Intrinsic::Any | Intrinsic::Closure) => {
            Some(Intrinsic::Any.type_index())
        }
        _ => None,
    }
}

pub(crate) fn refine_call(
    r: &mut Resolver<'_>,
    ast: &Ast,
    call_node: NodeIndex,
    callee: NodeIndex,
    args: &[NodeIndex],
) -> Option<TypeIndex> {
    if let Some(ffi_ty) =
        crate::ffi_bindings::check_and_refine_call(r, ast, call_node, callee, args)
    {
        return Some(ffi_ty);
    }

    let sym = callee_symbol(r, callee)?;
    let sym_info = &r.symbols[sym.0 as usize];
    if !r.options.privileged_nodes.contains(&sym_info.def_node) {
        return None;
    }
    let sym_name_id = sym_info.name;
    let assoc_type = r
        .scopes
        .get(sym_info.scope.0 as usize)
        .and_then(|s| s.assoc_type)
        .and_then(|t| r.type_pool.canonical_type(t));
    let is_privileged_def = true;

    // 1. First-class `Type` parsing: `String.parse` / `String.try_parse` / `conv.parse` / `conv.try_parse`.
    let is_parse_name = sym_name_id == str_interner::intern("parse")
        || sym_name_id == str_interner::intern("try_parse");
    let is_str_parse = is_parse_name
        && assoc_type == Some(Intrinsic::Str.type_index())
        && r.instance_methods.contains_key(&callee);
    let is_conv_parse = is_parse_name
        && (is_std_fn(r, sym, "conv", "parse") || is_std_fn(r, sym, "conv", "try_parse"));
    if is_str_parse || is_conv_parse {
        let sym_name = str_interner::get(sym_name_id);
        let is_try = sym_name == "try_parse";
        let target_arg = bound_arg_node(r, ast, call_node, args, 0)?;
        let &target_ty = r.node_type_values.get(&peel_expr(ast, target_arg))?;
        if !is_parseable_scalar(r, target_ty) {
            let bad = typing::type_display_name(&r.type_pool, target_ty);
            r.diag_ctx
                .error(format!(
                    "cannot parse `{bad}`: `{sym_name}` supports scalar numeric, `bool`, and `char` types"
                ))
                .with_primary_span(ast.node(target_arg).span)
                .emit(r.diag_ctx);
            return None;
        }
        if is_try {
            let opt_ty = r.register_type(TypeKind::Optional { inner: target_ty });
            crate::task_combinators::emit_runtime_assert_on_expr(r, call_node, opt_ty);
            return Some(opt_ty);
        } else {
            let (parse_err_ty, _) = crate::trusted::std_type(r, "conv", "ParseError")?;
            return rebuild_qualified_return(
                r,
                call_node,
                Vec::new(),
                vec![parse_err_ty],
                false,
                target_ty,
                true,
            );
        }
    }

    // 2. First-class `Type` JSON decoding: `std.json.decode(T, input, .allow_unknown_fields = ...)`.
    if is_std_fn(r, sym, "json", "decode") {
        let target_arg = bound_arg_node(r, ast, call_node, args, 0)?;
        let &target_ty = r.node_type_values.get(&peel_expr(ast, target_arg))?;
        let canon = r.type_pool.canonical_type(target_ty)?;
        let (json_err_ty, _) = crate::trusted::std_type(r, "json", "JsonError")?;
        if canon == r.type_pool.well_known.json
            || r.type_pool.as_intrinsic(canon) == Some(Intrinsic::Any)
        {
            return None;
        }
        if matches!(
            r.type_pool.collection_role(canon),
            Some(CollectionRole::Map | CollectionRole::List)
        ) {
            return rebuild_qualified_return(
                r,
                call_node,
                Vec::new(),
                vec![json_err_ty],
                false,
                target_ty,
                true,
            );
        }
        let derived_sym = r.derived_from_json_symbols.get(&canon).copied();
        let source_sym = derived_sym.or_else(|| {
            let fname = str_interner::intern("from_json");
            r.scopes.iter().find_map(|sc| {
                if sc.assoc_type.and_then(|t| r.type_pool.canonical_type(t)) == Some(canon)
                    && !r.imports.extension_scopes.contains_key(&sc.id)
                {
                    sc.bindings.get(&fname).copied()
                } else {
                    None
                }
            })
        });
        if let Some(from_json_symbol) = source_sym {
            let takes_allow_unknown = if derived_sym.is_some() {
                true
            } else {
                let fty = r.symbols[from_json_symbol.0 as usize].type_index;
                r.type_pool
                    .canonical_type(fty)
                    .and_then(|c| match &r.type_pool.get(c).kind {
                        TypeKind::Function { params, .. } => Some(params.len() >= 2),
                        _ => None,
                    })
                    .unwrap_or(false)
            };
            r.json_decode_calls.insert(
                call_node,
                JsonDecodeCallPlan {
                    from_json_symbol,
                    takes_allow_unknown,
                },
            );
            return r
                .type_pool
                .normalize_error_type(vec![json_err_ty], target_ty)
                .ok();
        }
        if !r.options.privileged_nodes.contains(&call_node) {
            let bad = typing::type_display_name(&r.type_pool, target_ty);
            r.diag_ctx
                .error(format!(
                    "type `{bad}` does not support `json.decode`; add `derive Json for {bad}` or define `{bad}.from_json`"
                ))
                .with_primary_span(ast.node(target_arg).span)
                .emit(r.diag_ctx);
        }
        return None;
    }

    if r.options.privileged_nodes.contains(&call_node) {
        return None;
    }

    let sym_name = str_interner::get(sym_name_id);

    // 3. `File.with_open(path, .mode = ..., body)`
    if is_std_method(r, sym, "io", "File", "with_open") {
        let (io_err_ty, _) = crate::trusted::std_type(r, "io", "IoError")?;
        let (effs, mut errs, open_err, core_ret) = if let Some(body_arg) =
            bound_arg_node(r, ast, call_node, args, 2)
            && let Some(ret) = callable_return_type(r, body_arg)
        {
            decompose_return_qualifiers(r, ret)
        } else {
            (Vec::new(), Vec::new(), false, Intrinsic::Any.type_index())
        };
        let io_canon = r.type_pool.canonical_type(io_err_ty).unwrap_or(io_err_ty);
        if !errs
            .iter()
            .any(|&e| r.type_pool.canonical_type(e).unwrap_or(e) == io_canon)
        {
            errs.insert(0, io_err_ty);
        }
        return rebuild_qualified_return(r, call_node, effs, errs, open_err, core_ret, true);
    }

    // 4. Console / Clock / Rng capability handler scopes
    if is_std_fn(r, sym, "io", "with_stdio") {
        let thunk = bound_arg_node(r, ast, call_node, args, 0)?;
        let ret = callable_return_type(r, thunk)?;
        let (effs, mut errs, open_err, core_ret) = decompose_return_qualifiers(r, ret);
        let c_print = std_effect_type(r, "io", "console_print");
        let c_println = std_effect_type(r, "io", "console_println");
        let c_read = std_effect_type(r, "io", "console_read_line");
        let (residual, removed) = remove_handled_effects(r, &effs, &[c_print, c_println, c_read]);
        if let Some(read_eff) = c_read
            && removed.contains(&read_eff)
            && let Some((io_err_ty, _)) = crate::trusted::std_type(r, "io", "IoError")
        {
            let io_canon = r.type_pool.canonical_type(io_err_ty).unwrap_or(io_err_ty);
            if !errs
                .iter()
                .any(|&e| r.type_pool.canonical_type(e).unwrap_or(e) == io_canon)
            {
                errs.push(io_err_ty);
            }
        }
        return rebuild_qualified_return(r, call_node, residual, errs, open_err, core_ret, true);
    }

    if is_std_fn(r, sym, "io", "capture_output") {
        let thunk = bound_arg_node(r, ast, call_node, args, 0)?;
        let ret = callable_return_type(r, thunk)?;
        let (effs, errs, open_err, core_ret) = decompose_return_qualifiers(r, ret);
        let c_print = std_effect_type(r, "io", "console_print");
        let c_println = std_effect_type(r, "io", "console_println");
        let (residual, _) = remove_handled_effects(r, &effs, &[c_print, c_println]);
        let pair_ty = r.register_type(TypeKind::Tuple {
            elements: vec![Intrinsic::Str.type_index(), core_ret],
        });
        return rebuild_qualified_return(r, call_node, residual, errs, open_err, pair_ty, true);
    }

    if is_std_fn(r, sym, "time", "with_system_clock") {
        let thunk = bound_arg_node(r, ast, call_node, args, 0)?;
        let ret = callable_return_type(r, thunk)?;
        let (effs, errs, open_err, core_ret) = decompose_return_qualifiers(r, ret);
        let c_now = std_effect_type(r, "time", "clock_now_ms");
        let c_sleep = std_effect_type(r, "time", "clock_sleep");
        let (residual, _) = remove_handled_effects(r, &effs, &[c_now, c_sleep]);
        return rebuild_qualified_return(r, call_node, residual, errs, open_err, core_ret, true);
    }

    if is_std_fn(r, sym, "time", "with_mock_clock") {
        let thunk = bound_arg_node(r, ast, call_node, args, 1)?;
        let ret = callable_return_type(r, thunk)?;
        let (effs, errs, open_err, core_ret) = decompose_return_qualifiers(r, ret);
        let c_now = std_effect_type(r, "time", "clock_now_ms");
        let c_sleep = std_effect_type(r, "time", "clock_sleep");
        let (residual, _) = remove_handled_effects(r, &effs, &[c_now, c_sleep]);
        let pair_ty = r.register_type(TypeKind::Tuple {
            elements: vec![Intrinsic::I64.type_index(), core_ret],
        });
        return rebuild_qualified_return(r, call_node, residual, errs, open_err, pair_ty, true);
    }

    if is_std_fn(r, sym, "rand", "with_rng") || is_std_fn(r, sym, "rand", "with_seeded_rng") {
        let thunk = bound_arg_node(r, ast, call_node, args, 1)?;
        let ret = callable_return_type(r, thunk)?;
        let (effs, errs, open_err, core_ret) = decompose_return_qualifiers(r, ret);
        let c_rand = std_effect_type(r, "rand", "random_u64");
        let (residual, _) = remove_handled_effects(r, &effs, &[c_rand]);
        return rebuild_qualified_return(r, call_node, residual, errs, open_err, core_ret, true);
    }

    // 5. Synchronization RAII wrappers (`Mutex`, `RwLock`, `Semaphore`, `Once`)
    if is_std_method(r, sym, "sync", "Mutex", "with_lock")
        || is_std_method(r, sym, "sync", "Mutex", "update")
        || is_std_method(r, sym, "sync", "RwLock", "with_read")
        || is_std_method(r, sym, "sync", "RwLock", "with_write")
        || is_std_method(r, sym, "sync", "Semaphore", "with_permit")
        || is_std_method(r, sym, "sync", "Once", "call_once")
    {
        let cb_idx = if r.instance_methods.contains_key(&callee) {
            0
        } else {
            1
        };
        let cb = bound_arg_node(r, ast, call_node, args, cb_idx)?;
        let ret = callable_return_type(r, cb)?;
        let (effs, errs, open_err, core_ret) = decompose_return_qualifiers(r, ret);
        return rebuild_qualified_return(r, call_node, effs, errs, open_err, core_ret, true);
    }

    // 6. Structured `std.task` combinators: `par_map`, `par_for_each`, `race`
    if is_std_fn(r, sym, "task", "par_map") {
        let mapper = bound_arg_node(r, ast, call_node, args, 1)?;
        let ret = callable_return_type(r, mapper)?;
        let (effs, errs, open_err, core_ret) = decompose_return_qualifiers(r, ret);
        if r.type_pool.as_intrinsic(core_ret) != Some(Intrinsic::Any)
            && r.type_pool.as_intrinsic(core_ret) != Some(Intrinsic::NoReturn)
        {
            r.collection_provenance
                .node_elements
                .insert(call_node, TrackedElem::Known(core_ret));
        }
        let list_ty = r.type_pool.list_type()?;
        return rebuild_qualified_return(r, call_node, effs, errs, open_err, list_ty, false);
    }

    if is_std_fn(r, sym, "task", "par_for_each") {
        let action = bound_arg_node(r, ast, call_node, args, 1)?;
        let ret = callable_return_type(r, action)?;
        let (effs, errs, open_err, _) = decompose_return_qualifiers(r, ret);
        return rebuild_qualified_return(
            r,
            call_node,
            effs,
            errs,
            open_err,
            Intrinsic::Unit.type_index(),
            false,
        );
    }

    if is_std_fn(r, sym, "task", "race") {
        let branches_node = peel_expr(ast, bound_arg_node(r, ast, call_node, args, 0)?);
        if ast.node(branches_node).kind == NodeKind::ListOf {
            let elems = ast.multi_children(branches_node);
            if !elems.is_empty() {
                let mut joined_ret: Option<TypeIndex> = None;
                for &elem in elems {
                    let branch_ret = callable_return_type(r, elem)?;
                    joined_ret = Some(match joined_ret {
                        None => branch_ret,
                        Some(prev) => crate::errors::join(r, prev, branch_ret),
                    });
                }
                if let Some(total) = joined_ret {
                    let (effs, errs, open_err, core_ret) = decompose_return_qualifiers(r, total);
                    return rebuild_qualified_return(
                        r, call_node, effs, errs, open_err, core_ret, true,
                    );
                }
            }
        }
        return None;
    }

    // 7. `std.stream` consumers: `collect`, `for_each`, `fold`
    if is_std_fn(r, sym, "stream", "collect") {
        let prod = bound_arg_node(r, ast, call_node, args, 0)?;
        let ret = callable_return_type(r, prod)?;
        let (effs, errs, open_err, _) = decompose_return_qualifiers(r, ret);
        let emit_eff = std_effect_type(r, "stream", "emit");
        let (residual, _) = remove_handled_effects(r, &effs, &[emit_eff]);
        let list_ty = r.type_pool.list_type()?;
        return rebuild_qualified_return(r, call_node, residual, errs, open_err, list_ty, false);
    }

    if is_std_fn(r, sym, "stream", "for_each") {
        let prod = bound_arg_node(r, ast, call_node, args, 0)?;
        let cons = bound_arg_node(r, ast, call_node, args, 1)?;
        let prod_ret = callable_return_type(r, prod)?;
        let cons_ret = callable_return_type(r, cons)?;
        let (p_effs, mut errs, mut open_err, _) = decompose_return_qualifiers(r, prod_ret);
        let emit_eff = std_effect_type(r, "stream", "emit");
        let (mut residual, _) = remove_handled_effects(r, &p_effs, &[emit_eff]);
        let (c_effs, c_errs, c_open, _) = decompose_return_qualifiers(r, cons_ret);
        open_err |= c_open;
        for e in c_effs {
            let ec = r.type_pool.canonical_type(e).unwrap_or(e);
            if !residual
                .iter()
                .any(|&prev| r.type_pool.canonical_type(prev).unwrap_or(prev) == ec)
            {
                residual.push(e);
            }
        }
        for err in c_errs {
            let ec = r.type_pool.canonical_type(err).unwrap_or(err);
            if !errs
                .iter()
                .any(|&prev| r.type_pool.canonical_type(prev).unwrap_or(prev) == ec)
            {
                errs.push(err);
            }
        }
        return rebuild_qualified_return(
            r,
            call_node,
            residual,
            errs,
            open_err,
            Intrinsic::Unit.type_index(),
            false,
        );
    }

    if is_std_fn(r, sym, "stream", "fold") {
        let prod = bound_arg_node(r, ast, call_node, args, 0)?;
        let init_node = bound_arg_node(r, ast, call_node, args, 1)?;
        let folder = bound_arg_node(r, ast, call_node, args, 2)?;
        let prod_ret = callable_return_type(r, prod)?;
        let fold_ret = callable_return_type(r, folder)?;
        let init_ty = r
            .node_types
            .get(&init_node)
            .copied()
            .unwrap_or(Intrinsic::Any.type_index());
        let (p_effs, mut errs, mut open_err, _) = decompose_return_qualifiers(r, prod_ret);
        let emit_eff = std_effect_type(r, "stream", "emit");
        let (mut residual, _) = remove_handled_effects(r, &p_effs, &[emit_eff]);
        let (f_effs, f_errs, f_open, f_core) = decompose_return_qualifiers(r, fold_ret);
        open_err |= f_open;
        for e in f_effs {
            let ec = r.type_pool.canonical_type(e).unwrap_or(e);
            if !residual
                .iter()
                .any(|&prev| r.type_pool.canonical_type(prev).unwrap_or(prev) == ec)
            {
                residual.push(e);
            }
        }
        for err in f_errs {
            let ec = r.type_pool.canonical_type(err).unwrap_or(err);
            if !errs
                .iter()
                .any(|&prev| r.type_pool.canonical_type(prev).unwrap_or(prev) == ec)
            {
                errs.push(err);
            }
        }
        let core_acc = if r.type_pool.is_subtype(f_core, init_ty) {
            init_ty
        } else {
            Intrinsic::Any.type_index()
        };
        return rebuild_qualified_return(r, call_node, residual, errs, open_err, core_acc, true);
    }

    // 8. `pattern.replace_with(text, pat, repl, ...)` with fallible/effectful `repl`
    if is_std_fn(r, sym, "pattern", "replace_with") {
        let repl = bound_arg_node(r, ast, call_node, args, 2)?;
        let ret = callable_return_type(r, repl)?;
        let (effs, mut errs, open_err, _) = decompose_return_qualifiers(r, ret);
        if effs.is_empty() && errs.is_empty() && !open_err {
            return None;
        }
        let (pat_err_ty, _) = crate::trusted::std_type(r, "pattern", "PatternError")?;
        let pat_canon = r.type_pool.canonical_type(pat_err_ty).unwrap_or(pat_err_ty);
        if !errs
            .iter()
            .any(|&e| r.type_pool.canonical_type(e).unwrap_or(e) == pat_canon)
        {
            errs.insert(0, pat_err_ty);
        }
        let pair_ty = r.register_type(TypeKind::Tuple {
            elements: vec![Intrinsic::Str.type_index(), Intrinsic::Usize.type_index()],
        });
        return rebuild_qualified_return(r, call_node, effs, errs, open_err, pair_ty, true);
    }

    // 9. Eager `List`, `Map`, and `Iterator` methods when the callback carries `!Error` or `#Effect`
    //    (plus element-type refinement on `Iterator.{first, last, nth, find, reduce}`).
    if ast.node(callee).kind == NodeKind::Projection && is_privileged_def {
        let receiver = ast.fixed_children(callee)[0];
        let recv_ty = *r.node_types.get(&receiver)?;
        let is_list = r.type_pool.collection_role(recv_ty) == Some(CollectionRole::List);
        let is_map = r.type_pool.collection_role(recv_ty) == Some(CollectionRole::Map);
        let is_iter_default = crate::iterator_defaults::is_default_template(r, sym);

        if is_iter_default && matches!(sym_name.as_str(), "first" | "last" | "nth") {
            if let TrackedElem::Known(elem_ty) = lookup_expr_elem(r, ast, receiver) {
                let opt_ty = r.register_type(TypeKind::Optional { inner: elem_ty });
                crate::task_combinators::emit_runtime_assert_on_expr(r, call_node, opt_ty);
                return Some(opt_ty);
            }
            return None;
        }

        let cb_idx = match sym_name.as_str() {
            "fold" if is_list || is_iter_default => Some(1usize),
            "map" | "filter" | "filter_map" | "flat_map" | "each" | "foreach" | "any" | "all"
            | "find" | "find_map" | "position" | "rposition" | "partition" | "retain"
            | "reduce"
                if is_list =>
            {
                Some(0)
            }
            "filter" | "map_values" | "retain" if is_map => Some(0),
            "fold" | "reduce" | "for_each" | "any" | "all" | "find" | "find_map" | "position"
            | "partition"
                if is_iter_default =>
            {
                Some(0)
            }
            _ => None,
        };

        if let Some(idx) = cb_idx
            && let Some(cb_node) = bound_arg_node(r, ast, call_node, args, idx)
            && let Some(cb_ret) = callable_return_type(r, cb_node)
        {
            let (effs, errs, open_err, cb_core) = decompose_return_qualifiers(r, cb_ret);
            let tracked_elem = match lookup_expr_elem(r, ast, receiver) {
                TrackedElem::Known(t) => Some(t),
                _ => None,
            };
            if effs.is_empty() && errs.is_empty() && !open_err {
                if is_iter_default
                    && matches!(sym_name.as_str(), "find" | "reduce")
                    && let Some(elem_ty) = tracked_elem
                {
                    let opt_ty = r.register_type(TypeKind::Optional { inner: elem_ty });
                    crate::task_combinators::emit_runtime_assert_on_expr(r, call_node, opt_ty);
                    return Some(opt_ty);
                }
                if is_iter_default && sym_name == "fold" {
                    let init_ty = bound_arg_node(r, ast, call_node, args, 0)
                        .and_then(|n| r.node_types.get(&n).copied())
                        .unwrap_or(Intrinsic::Any.type_index());
                    if r.type_pool.as_intrinsic(init_ty) != Some(Intrinsic::Any)
                        && r.type_pool.is_subtype(cb_core, init_ty)
                    {
                        crate::task_combinators::emit_runtime_assert_on_expr(r, call_node, init_ty);
                        return Some(init_ty);
                    }
                }
                return None;
            }

            let method_callee_ty = r.node_types.get(&callee).copied()?;
            let TypeKind::Function { ret: decl_ret, .. } = r
                .type_pool
                .get(r.type_pool.canonical_type(method_callee_ty)?)
                .kind
            else {
                return None;
            };
            let base_ret = if sym_name == "fold" {
                let init_ty = bound_arg_node(r, ast, call_node, args, 0)
                    .and_then(|n| r.node_types.get(&n).copied())
                    .unwrap_or(Intrinsic::Any.type_index());
                if r.type_pool.as_intrinsic(init_ty) != Some(Intrinsic::Any)
                    && r.type_pool.is_subtype(cb_core, init_ty)
                {
                    init_ty
                } else {
                    decl_ret
                }
            } else if is_iter_default
                && matches!(sym_name.as_str(), "find" | "reduce")
                && let Some(elem_ty) = tracked_elem
            {
                r.register_type(TypeKind::Optional { inner: elem_ty })
            } else {
                decl_ret
            };

            return rebuild_qualified_return(r, call_node, effs, errs, open_err, base_ret, true);
        }
    }

    None
}
