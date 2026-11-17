//! Static capability gating, C-ABI signature checking, and bound symbol projection for `std.ffi`.

use std::collections::{HashMap, HashSet};

use ast::{Ast, NodeIndex, NodeKind};
use str_interner::StrId;
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::resolver::Resolver;
use crate::{
    CallArgumentValue, ErrorConversionKind, ErrorConversionPlan, SymbolId, SymbolKind, typing,
};

const MAX_FFI_C_WORDS: usize = 12;

#[derive(Default)]
pub(crate) struct FfiBindingState {
    pub(crate) node_modules: HashMap<NodeIndex, HashMap<StrId, TypeIndex>>,
    pub(crate) symbol_modules: HashMap<SymbolId, HashMap<StrId, TypeIndex>>,
    pub(crate) symbol_projections: HashMap<NodeIndex, (SymbolId, StrId)>,
    pub(crate) called_ffi_callees: HashSet<NodeIndex>,
    pub(crate) resolving_consts: HashSet<SymbolId>,
}

fn peel_expr(ast: &Ast, mut expr: NodeIndex) -> NodeIndex {
    while !expr.is_null() && ast.node(expr).kind == NodeKind::ExprStatement {
        expr = ast.fixed_children(expr)[0];
    }
    expr
}

fn is_privileged_site(r: &Resolver<'_>, node: NodeIndex) -> bool {
    r.options.builtin_access || r.options.privileged_nodes.contains(&node)
}

fn unescape_str_literal(ast: &Ast, node: NodeIndex) -> Option<String> {
    let node = peel_expr(ast, node);
    if node.is_null() || ast.node(node).kind != NodeKind::Str {
        return None;
    }
    let raw = str_interner::get(ast.node(node).str_id);
    let inner = if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
        &raw[1..raw.len() - 1]
    } else {
        raw.as_str()
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if matches!(c, '{' | '}') && chars.peek() == Some(&c) {
            chars.next();
            out.push(c);
        } else if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('0') => out.push('\0'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some('x') => {
                    let h1 = chars.next()?;
                    let h2 = chars.next()?;
                    let v = u8::from_str_radix(&format!("{h1}{h2}"), 16).ok()?;
                    out.push(v as char);
                }
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if hex.len() != 4 {
                        return None;
                    }
                    let v = u32::from_str_radix(&hex, 16).ok()?;
                    out.push(char::from_u32(v)?);
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

fn std_ffi_type(r: &Resolver<'_>, name: &str) -> Option<TypeIndex> {
    crate::trusted::std_type(r, "ffi", name).and_then(|(ty, _)| r.type_pool.canonical_type(ty))
}

fn std_ffi_method(r: &Resolver<'_>, type_name: &str, method_name: &str) -> Option<SymbolId> {
    let owner_ty = std_ffi_type(r, type_name)?;
    let target = str_interner::intern(method_name);
    for sc in &r.scopes {
        if sc.assoc_type.and_then(|t| r.type_pool.canonical_type(t)) == Some(owner_ty)
            && let Some(&sym_id) = sc.bindings.get(&target)
        {
            let sym = &r.symbols[sym_id.0 as usize];
            if sym.kind == SymbolKind::Function
                && r.options.privileged_nodes.contains(&sym.def_node)
            {
                return Some(sym_id);
            }
        }
    }
    None
}

fn is_std_ffi_fn(r: &Resolver<'_>, sym: SymbolId, fn_name: &str) -> bool {
    let sym_info = &r.symbols[sym.0 as usize];
    sym_info.kind == SymbolKind::Function
        && sym_info.name == str_interner::intern(fn_name)
        && r.options.privileged_nodes.contains(&sym_info.def_node)
        && crate::trusted::std_function(r, "ffi", fn_name) == Some(sym)
}

fn is_std_ffi_method(r: &Resolver<'_>, sym: SymbolId, type_name: &str, method_name: &str) -> bool {
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
    std_ffi_type(r, type_name) == Some(assoc)
}

pub(crate) fn library_get_symbol_fn(r: &Resolver<'_>) -> Option<SymbolId> {
    std_ffi_method(r, "Library", "get_symbol")
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

fn is_c_scalar_intrinsic(intr: Intrinsic) -> bool {
    matches!(
        intr,
        Intrinsic::I8
            | Intrinsic::U8
            | Intrinsic::I16
            | Intrinsic::U16
            | Intrinsic::I32
            | Intrinsic::U32
            | Intrinsic::I64
            | Intrinsic::U64
            | Intrinsic::Isize
            | Intrinsic::Usize
            | Intrinsic::F32
            | Intrinsic::F64
            | Intrinsic::Bool
    )
}

fn ffi_param_c_words(r: &Resolver<'_>, ty: TypeIndex) -> Option<usize> {
    let canon = r.type_pool.canonical_type(ty)?;
    if let Some(intr) = r.type_pool.as_intrinsic(canon)
        && is_c_scalar_intrinsic(intr)
    {
        return Some(1);
    }
    for single in ["Ptr", "CStr", "PtrRo", "PtrMut"] {
        if std_ffi_type(r, single) == Some(canon) {
            return Some(1);
        }
    }
    for pair in ["SliceRo", "BufMut"] {
        if std_ffi_type(r, pair) == Some(canon) {
            return Some(2);
        }
    }
    None
}

fn is_valid_ffi_return(r: &Resolver<'_>, ty: TypeIndex) -> bool {
    let Some(canon) = r.type_pool.canonical_type(ty) else {
        return false;
    };
    if let Some(intr) = r.type_pool.as_intrinsic(canon) {
        return intr == Intrinsic::Unit || is_c_scalar_intrinsic(intr);
    }
    std_ffi_type(r, "Ptr") == Some(canon) || std_ffi_type(r, "CStr") == Some(canon)
}

pub(crate) fn validate_ffi_fn_signature(
    r: &Resolver<'_>,
    ast: &Ast,
    span_node: NodeIndex,
    sig_ty: TypeIndex,
) -> bool {
    let Some(canon) = r.type_pool.canonical_type(sig_ty) else {
        return false;
    };
    let TypeKind::Function { params, ret } = r.type_pool.get(canon).kind.clone() else {
        let found = typing::type_display_name(&r.type_pool, sig_ty);
        r.diag_ctx
            .error(format!(
                "FFI symbol binding requires a pure `fn(...) -> ...` signature type, found `{found}`"
            ))
            .with_primary_span(ast.node(span_node).span)
            .emit(r.diag_ctx);
        return false;
    };
    if params.len() > MAX_FFI_C_WORDS {
        r.diag_ctx
            .error(format!(
                "FFI signature has {} parameters, exceeding the maximum of {MAX_FFI_C_WORDS}",
                params.len()
            ))
            .with_primary_span(ast.node(span_node).span)
            .emit(r.diag_ctx);
        return false;
    }
    let mut total_words = 0usize;
    for (idx, param_ty) in params.iter().copied().enumerate() {
        let Some(words) = ffi_param_c_words(r, param_ty) else {
            let bad = typing::type_display_name(&r.type_pool, param_ty);
            r.diag_ctx
                .error(format!(
                    "unsupported FFI parameter {} type `{bad}`; allowed types are C-width scalars (`i8..i64`, `u8..u64`, `isize`, `usize`, `f32`, `f64`, `bool`) and `std.ffi` pointer/slice markers (`Ptr`, `CStr`, `PtrRo`, `PtrMut`, `SliceRo`, `BufMut`)",
                    idx + 1
                ))
                .with_primary_span(ast.node(span_node).span)
                .emit(r.diag_ctx);
            return false;
        };
        total_words += words;
    }
    if total_words > MAX_FFI_C_WORDS {
        r.diag_ctx
            .error(format!(
                "FFI signature expands to {total_words} C argument words, exceeding the maximum of {MAX_FFI_C_WORDS}"
            ))
            .with_primary_span(ast.node(span_node).span)
            .emit(r.diag_ctx);
        return false;
    }
    if !is_valid_ffi_return(r, ret) {
        let bad = typing::type_display_name(&r.type_pool, ret);
        r.diag_ctx
            .error(format!(
                "unsupported FFI return type `{bad}`; allowed return types are `Unit`, C-width scalars (`i8..i64`, `u8..u64`, `isize`, `usize`, `f32`, `f64`, `bool`), `Ptr`, and `CStr`"
            ))
            .with_primary_span(ast.node(span_node).span)
            .emit(r.diag_ctx);
        return false;
    }
    true
}

fn check_ffi_capability(r: &Resolver<'_>, ast: &Ast, node: NodeIndex) -> bool {
    if is_privileged_site(r, node) || r.options.ffi_nodes.contains(&node) {
        return true;
    }
    r.diag_ctx
        .error("FFI requires `[ffi] allow = true` in package.toml".to_string())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
    false
}

fn check_library_allowlist(
    r: &Resolver<'_>,
    ast: &Ast,
    call_node: NodeIndex,
    lib_arg: Option<NodeIndex>,
) {
    if is_privileged_site(r, call_node) {
        return;
    }
    let Some(allowlist) = r.options.ffi_library_allowlists.get(&call_node) else {
        return;
    };
    let literal_lib = lib_arg.and_then(|arg| unescape_str_literal(ast, arg));
    match literal_lib {
        Some(lib_name) if allowlist.contains(&lib_name) => {}
        Some(lib_name) => {
            let span_node = lib_arg.unwrap_or(call_node);
            r.diag_ctx
                .error(format!(
                    "FFI library `{lib_name}` is not permitted by `[ffi] libraries` allowlist in package.toml"
                ))
                .with_primary_span(ast.node(span_node).span)
                .emit(r.diag_ctx);
        }
        None => {
            r.diag_ctx
                .error(
                    "restricted `[ffi] libraries` policy requires a string literal library name"
                        .to_string(),
                )
                .with_primary_span(ast.node(call_node).span)
                .emit(r.diag_ctx);
        }
    }
}

pub(crate) fn check_and_refine_call(
    r: &mut Resolver<'_>,
    ast: &Ast,
    call_node: NodeIndex,
    callee: NodeIndex,
    args: &[NodeIndex],
) -> Option<TypeIndex> {
    let sym = callee_symbol(r, callee)?;
    if !r
        .options
        .privileged_nodes
        .contains(&r.symbols[sym.0 as usize].def_node)
    {
        return None;
    }

    let is_load = is_std_ffi_fn(r, sym, "load");
    let is_must_load = is_std_ffi_fn(r, sym, "must_load");
    let is_lib_open = is_std_ffi_method(r, sym, "Library", "open");
    let is_read_cstr = is_std_ffi_fn(r, sym, "read_cstr");
    let is_ptr_read_cstr = is_std_ffi_method(r, sym, "Ptr", "read_cstr");
    let is_lib_bind = is_std_ffi_method(r, sym, "Library", "bind");
    let is_func_ctor = is_std_ffi_fn(r, sym, "func") || is_std_ffi_method(r, sym, "Func", "new");

    if is_load || is_must_load || is_lib_open || is_read_cstr || is_ptr_read_cstr {
        r.ffi_bindings.called_ffi_callees.insert(callee);
        if check_ffi_capability(r, ast, call_node) && (is_load || is_must_load || is_lib_open) {
            let lib_arg = bound_arg_node(r, ast, call_node, args, 0);
            check_library_allowlist(r, ast, call_node, lib_arg);
        }
        return None;
    }

    if is_func_ctor {
        if let Some(sig_arg) = bound_arg_node(r, ast, call_node, args, 0)
            && let Some(&sig_ty) = r.node_type_values.get(&sig_arg)
        {
            let _ = validate_ffi_fn_signature(r, ast, sig_arg, sig_ty);
        }
        return None;
    }

    if is_lib_bind {
        r.ffi_bindings.called_ffi_callees.insert(callee);
        let _ = check_ffi_capability(r, ast, call_node);
        let param_idx = if r.instance_methods.contains_key(&callee) {
            1
        } else {
            2
        };
        let sig_arg = bound_arg_node(r, ast, call_node, args, param_idx)?;
        let &sig_ty = r.node_type_values.get(&sig_arg)?;
        if !validate_ffi_fn_signature(r, ast, sig_arg, sig_ty) {
            return None;
        }
        let ffi_err_ty = std_ffi_type(r, "FfiError")?;
        let refined = r
            .type_pool
            .normalize_error_type(vec![ffi_err_ty], sig_ty)
            .ok()?;
        r.error_conversions
            .entry(call_node)
            .or_default()
            .push(ErrorConversionPlan {
                source: Intrinsic::Any.type_index(),
                target: refined,
                kind: ErrorConversionKind::CheckQualified,
            });
        return Some(refined);
    }

    None
}

fn extract_spec_signature(r: &Resolver<'_>, ast: &Ast, spec_node: NodeIndex) -> Option<TypeIndex> {
    let spec_node = peel_expr(ast, spec_node);
    if spec_node.is_null() {
        return None;
    }
    if let Some(&ty) = r.node_type_values.get(&spec_node) {
        return Some(ty);
    }
    if ast.node(spec_node).kind == NodeKind::Call {
        let callee = ast.fixed_children(spec_node)[0];
        let args = ast.multi_children(spec_node);
        if let Some(sym) = callee_symbol(r, callee)
            && (crate::trusted::std_function(r, "ffi", "func") == Some(sym)
                || std_ffi_method(r, "Func", "new") == Some(sym))
            && let Some(sig_arg) = bound_arg_node(r, ast, spec_node, args, 0)
        {
            return r.node_type_values.get(&peel_expr(ast, sig_arg)).copied();
        }
    }
    if ast.node(spec_node).kind == NodeKind::ExtendedCall
        && let Some(plan) = r.struct_constructions.get(&spec_node)
        && std_ffi_type(r, "Func") == r.type_pool.canonical_type(plan.type_index)
    {
        for &arg in ast.multi_children(spec_node) {
            if ast.node(arg).kind == NodeKind::Property {
                let ch = ast.fixed_children(arg);
                if str_interner::get(ast.node(ch[0]).str_id) == "sig" {
                    return r.node_type_values.get(&peel_expr(ast, ch[1])).copied();
                }
            }
        }
    }
    None
}

pub(crate) fn check_extended_load(
    r: &mut Resolver<'_>,
    ast: &Ast,
    ext_node: NodeIndex,
    apply_fn_sym: SymbolId,
) {
    let is_loader_apply = is_std_ffi_method(r, apply_fn_sym, "Loader", "apply")
        || is_std_ffi_method(r, apply_fn_sym, "MustLoader", "apply");
    if !is_loader_apply {
        return;
    }
    let _ = check_ffi_capability(r, ast, ext_node);

    let func_ty = std_ffi_type(r, "Func");
    let mut bound_map: HashMap<StrId, TypeIndex> = HashMap::new();
    let mut seen_props: HashSet<StrId> = HashSet::new();

    for &arg in ast.multi_children(ext_node) {
        if ast.node(arg).kind != NodeKind::Property {
            r.diag_ctx
                .error(
                    "`ffi.load` / `ffi.must_load` block only accepts named `symbol: signature` properties"
                        .to_string(),
                )
                .with_primary_span(ast.node(arg).span)
                .emit(r.diag_ctx);
            continue;
        }
        let ch = ast.fixed_children(arg);
        let prop_name = ast.node(ch[0]).str_id;
        let val_node = ch[1];
        if !seen_props.insert(prop_name) {
            r.diag_ctx
                .error(format!(
                    "duplicate FFI symbol binding `{}`",
                    str_interner::get(prop_name)
                ))
                .with_primary_span(ast.node(arg).span)
                .emit(r.diag_ctx);
            continue;
        }
        if let Some(sig_ty) = extract_spec_signature(r, ast, val_node) {
            if validate_ffi_fn_signature(r, ast, val_node, sig_ty) {
                bound_map.insert(prop_name, sig_ty);
            }
        } else {
            let val_ty = r.node_types.get(&val_node).copied();
            let val_canon = val_ty.and_then(|t| r.type_pool.canonical_type(t));
            let is_dynamic_spec = val_canon.is_some_and(|c| {
                r.type_pool.as_intrinsic(c) == Some(Intrinsic::Type)
                    || r.type_pool.as_intrinsic(c) == Some(Intrinsic::Any)
                    || Some(c) == func_ty
            });
            if is_dynamic_spec {
                bound_map.insert(prop_name, Intrinsic::Any.type_index());
            } else if let Some(found_ty) = val_ty {
                let found = typing::type_display_name(&r.type_pool, found_ty);
                r.diag_ctx
                    .error(format!(
                        "FFI binding `{}` expects a function `Type` or `ffi.func(...)` descriptor, found `{found}`",
                        str_interner::get(prop_name)
                    ))
                    .with_primary_span(ast.node(val_node).span)
                    .emit(r.diag_ctx);
            }
        }
    }

    r.ffi_bindings.node_modules.insert(ext_node, bound_map);
}

pub(crate) fn lookup_ffi_module(
    r: &mut Resolver<'_>,
    ast: &Ast,
    expr: NodeIndex,
) -> Option<HashMap<StrId, TypeIndex>> {
    let expr = peel_expr(ast, expr);
    if expr.is_null() {
        return None;
    }
    if let Some(map) = r.ffi_bindings.node_modules.get(&expr) {
        return Some(map.clone());
    }
    match ast.node(expr).kind {
        NodeKind::ErrorPropagation => {
            let inner = ast.fixed_children(expr)[0];
            lookup_ffi_module(r, ast, inner)
        }
        NodeKind::ErrorElimination => {
            let inner = ast.fixed_children(expr)[0];
            let arms = ast.multi_children(expr);
            let all_diverge = !arms.is_empty()
                && arms
                    .iter()
                    .all(|&arm| typing::definitely_diverges(r, ast, ast.fixed_children(arm)[1]));
            if all_diverge {
                lookup_ffi_module(r, ast, inner)
            } else {
                None
            }
        }
        NodeKind::Id | NodeKind::Projection => {
            let &sym = r.node_symbols.get(&expr)?;
            if r.assigned_symbols.contains(&sym) {
                return None;
            }
            if let Some(map) = r.ffi_bindings.symbol_modules.get(&sym) {
                return Some(map.clone());
            }
            let sym_info = r.symbols[sym.0 as usize].clone();
            if sym_info.kind == SymbolKind::Constant
                && !sym_info.def_node.is_null()
                && ast.node(sym_info.def_node).kind == NodeKind::ConstDecl
            {
                if !r.ffi_bindings.resolving_consts.insert(sym) {
                    return None;
                }
                typing::resolve_decl(r, ast, sym_info.def_node);
                r.ffi_bindings.resolving_consts.remove(&sym);
                return r.ffi_bindings.symbol_modules.get(&sym).cloned();
            }
            None
        }
        _ => None,
    }
}

pub(crate) fn record_decl(
    r: &mut Resolver<'_>,
    ast: &Ast,
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
    if r.assigned_symbols.contains(&sym_id) {
        return;
    }
    if let Some(map) = lookup_ffi_module(r, ast, value_node) {
        r.ffi_bindings.symbol_modules.insert(sym_id, map);
    }
}

fn is_inherent_library_member(name: &str) -> bool {
    matches!(
        name,
        "name"
            | "is_open"
            | "close"
            | "bind"
            | "attach_finalizer"
            | "has_symbol"
            | "get_symbol"
            | "to_string"
    )
}

/// Resolve `c.member` when `c` is a `std.ffi.Library` produced by `ffi.load(...) { ... }`
/// or `ffi.must_load(...) { ... }`.
///
/// Returns:
/// - `None`: `lhs_ti` is not a tracked bound `Library` (or `member` is an unshadowed `Library`
///   inherent field/method); caller should fall through to normal projection resolution.
/// - `Some(Some(ty))`: resolved to bound FFI function signature `ty` and recorded in
///   `ffi_symbol_projections`.
/// - `Some(None)`: unknown FFI symbol diagnostic was emitted; caller should return `None`.
pub(crate) fn resolve_bound_library_projection(
    r: &mut Resolver<'_>,
    ast: &Ast,
    proj_node: NodeIndex,
    recv_node: NodeIndex,
    lhs_ti: TypeIndex,
    member_node: NodeIndex,
) -> Option<Option<TypeIndex>> {
    if member_node.is_null() {
        return None;
    }
    let canon = r.type_pool.canonical_type(lhs_ti)?;
    if std_ffi_type(r, "Library") != Some(canon) {
        return None;
    }
    let bound_map = lookup_ffi_module(r, ast, recv_node)?;
    let member_id = ast.node(member_node).str_id;
    let member_str = str_interner::get(member_id);

    if let Some(&fn_ty) = bound_map.get(&member_id) {
        let get_sym_fn = library_get_symbol_fn(r)?;
        r.ffi_bindings
            .symbol_projections
            .insert(proj_node, (get_sym_fn, member_id));
        return Some(Some(fn_ty));
    }

    if is_inherent_library_member(&member_str) {
        return None;
    }

    let mut builder = r
        .diag_ctx
        .error(format!(
            "unknown FFI symbol `{member_str}` on bound `Library`"
        ))
        .with_code(diagnostic::codes::UNKNOWN_MEMBER)
        .with_primary_span(ast.node(proj_node).span);
    if !bound_map.is_empty() {
        let mut names: Vec<String> = bound_map.keys().map(|&k| str_interner::get(k)).collect();
        names.sort();
        builder = builder.with_help(format!("bound FFI symbols are: {}", names.join(", ")));
    }
    builder.emit(r.diag_ctx);
    Some(None)
}

/// Validate that non-privileged references to `std.ffi.{load, must_load, read_cstr}` or
/// `Library.open` as uncalled first-class values do not bypass `[ffi]` capability or
/// `[ffi] libraries` allowlist checks.
pub(crate) fn validate_ffi_value_references(r: &Resolver<'_>, ast: &Ast) {
    let load_sym = crate::trusted::std_function(r, "ffi", "load");
    let must_load_sym = crate::trusted::std_function(r, "ffi", "must_load");
    let read_cstr_sym = crate::trusted::std_function(r, "ffi", "read_cstr");
    let lib_open_sym = std_ffi_method(r, "Library", "open");

    let mut entries: Vec<(NodeIndex, SymbolId)> = r
        .node_symbols
        .iter()
        .filter_map(|(&node, &sym)| {
            let is_guarded = Some(sym) == load_sym
                || Some(sym) == must_load_sym
                || Some(sym) == read_cstr_sym
                || Some(sym) == lib_open_sym;
            (is_guarded
                && !is_privileged_site(r, node)
                && !r.ffi_bindings.called_ffi_callees.contains(&node)
                && matches!(ast.node(node).kind, NodeKind::Id | NodeKind::Projection))
            .then_some((node, sym))
        })
        .collect();
    if entries.is_empty() {
        return;
    }
    entries.sort_by_key(|(n, _)| n.0);

    for (node, sym) in entries {
        let is_child_of_projection = ast.nodes.iter().enumerate().any(|(p_idx, p_node)| {
            p_node.kind == NodeKind::Projection
                && p_node.children.get(1).copied() == Some(node)
                && r.node_symbols.get(&NodeIndex(p_idx as u32)).copied() == Some(sym)
        });
        if is_child_of_projection {
            continue;
        }
        if !r.options.ffi_nodes.contains(&node) {
            r.diag_ctx
                .error("FFI requires `[ffi] allow = true` in package.toml".to_string())
                .with_primary_span(ast.node(node).span)
                .emit(r.diag_ctx);
            continue;
        }
        let is_loader =
            Some(sym) == load_sym || Some(sym) == must_load_sym || Some(sym) == lib_open_sym;
        if is_loader && r.options.ffi_library_allowlists.contains_key(&node) {
            r.diag_ctx
                .error(
                    "restricted FFI loader cannot escape as a function value; call it directly with a string literal library name from `[ffi] libraries`"
                        .to_string(),
                )
                .with_primary_span(ast.node(node).span)
                .emit(r.diag_ctx);
        }
    }
}
