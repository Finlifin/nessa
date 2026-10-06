//! Phase 3b: Type Resolution
//!
//! Walks the AST (post name-resolution) and infers types for expression nodes.
//! Literal types are assigned directly; operators propagate from their operands;
//! identifier types are read from the symbol table populated in Phase 3a.
//! Type annotations on function signatures and variable declarations are
//! resolved and checked against inferred types.

use ast::{Ast, NodeIndex, NodeKind};
use diagnostic::Level;
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::SymbolKind;
use crate::resolver::Resolver;

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

/// Walk `node_idx` and descendants, inferring types for expression nodes.
pub(crate) fn resolve_types(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    // Forward pass: resolve all function signatures so that call-site type
    // checking works regardless of definition order.
    forward_resolve_signatures(r, ast, node_idx);

    resolve_types_expected(r, ast, node_idx, None);
}

// ---------------------------------------------------------------------------
// Forward signature resolution
// ---------------------------------------------------------------------------

/// Pre-scan the AST for all function definitions and resolve their parameter +
/// return types so that `Function` TypeKinds are registered on symbols before
/// any function body is type-checked. This eliminates definition-order
/// sensitivity for call-site type checking.
fn forward_resolve_signatures(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    if node_idx.is_null() {
        return;
    }
    let kind = ast.node(node_idx).kind;
    if kind == NodeKind::FunctionDef {
        resolve_function_signature(r, ast, node_idx);
    } else {
        for &child in ast.fixed_children(node_idx) {
            forward_resolve_signatures(r, ast, child);
        }
        for &child in ast.multi_children(node_idx) {
            forward_resolve_signatures(r, ast, child);
        }
    }
}

/// Resolve only the signature (params + return type) of a function definition,
/// registering the Function type on the function's symbol. Does NOT recurse
/// into the function body.
fn resolve_function_signature(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] name  [1] ret_type  [2] body  [3] capability

    // Resolve parameter types.
    for &param in ast.multi_children(node_idx) {
        resolve_param_types(r, ast, param);
    }

    // Resolve return type annotation.
    let ret_type = resolve_type_expr(r, ast, children[1]);

    // Build a Function type and assign to the symbol.
    let name_node = children[0];
    if let Some(ret_ti) = ret_type {
        if let Some(&sym_id) = r.node_symbols.get(&name_node) {
            let param_types: Vec<TypeIndex> = ast
                .multi_children(node_idx)
                .iter()
                .filter_map(|&p| param_type_index(r, ast, p))
                .collect();
            let func_ti = r.register_type(TypeKind::Function {
                params: param_types,
                ret: ret_ti,
            });
            r.symbols[sym_id.0 as usize].type_index = func_ti;
        }
    }
}

/// Walk `node_idx` with an optional expected type for bidirectional inference.
/// When `expected` is `Some`, numeric literals adopt the expected type instead
/// of defaulting to i64/f64.
fn resolve_types_expected(
    r: &mut Resolver,
    ast: &Ast,
    node_idx: NodeIndex,
    expected: Option<TypeIndex>,
) {
    if node_idx.is_null() {
        return;
    }
    let kind = ast.node(node_idx).kind;

    let inferred = match kind {
        // ── Literal types ──────────────────────────────────────────
        NodeKind::Int => Some(contextual_int_type(&r.type_pool, expected)),
        NodeKind::Real => Some(contextual_float_type(&r.type_pool, expected)),
        NodeKind::Str => Some(Intrinsic::Str.type_index()),
        NodeKind::FStringConcat => {
            // Resolve all children (literal segments + interpolated expressions).
            for &child in ast.multi_children(node_idx) {
                resolve_types(r, ast, child);
            }
            Some(Intrinsic::Str.type_index())
        }
        NodeKind::Char => Some(Intrinsic::Char.type_index()),
        NodeKind::Bool => Some(Intrinsic::Bool.type_index()),
        NodeKind::Null | NodeKind::Unit => Some(Intrinsic::Unit.type_index()),

        // ── Arithmetic — propagate type from lhs ───────────────────
        NodeKind::Add | NodeKind::Sub | NodeKind::Mul | NodeKind::Div | NodeKind::Mod => {
            let children = ast.fixed_children(node_idx);
            resolve_types_expected(r, ast, children[0], expected);
            resolve_types_expected(r, ast, children[1], expected);
            r.node_types.get(&children[0]).copied()
        }

        // ── Comparison / boolean — always Bool ─────────────────────
        NodeKind::BoolEq
        | NodeKind::BoolNotEq
        | NodeKind::BoolGt
        | NodeKind::BoolGtEq
        | NodeKind::BoolLt
        | NodeKind::BoolLtEq
        | NodeKind::BoolAnd
        | NodeKind::BoolOr
        | NodeKind::BoolNot
        | NodeKind::BoolMatches => {
            for &child in ast.fixed_children(node_idx) {
                resolve_types(r, ast, child);
            }
            Some(Intrinsic::Bool.type_index())
        }

        // ── Unary negation — propagate operand type ────────────────
        NodeKind::Negative => {
            let children = ast.fixed_children(node_idx);
            resolve_types_expected(r, ast, children[0], expected);
            r.node_types.get(&children[0]).copied()
        }

        // ── Identifier — read type from symbol table ───────────────
        NodeKind::Id => {
            if let Some(&sym_id) = r.node_symbols.get(&node_idx) {
                let ti = r.symbols[sym_id.0 as usize].type_index;
                if ti != TypeIndex::INVALID {
                    Some(ti)
                } else {
                    None
                }
            } else {
                None
            }
        }

        // ── Let / Const / Var — propagate and check types ──────────
        NodeKind::LetDecl | NodeKind::ConstDecl | NodeKind::VarDecl => {
            resolve_decl(r, ast, node_idx);
            None
        }

        // ── Block — type is the type of the last expression ────────
        NodeKind::Block => {
            let mc = ast.multi_children(node_idx);
            let last_idx = mc.len().saturating_sub(1);
            for (i, &child) in mc.iter().enumerate() {
                if i == last_idx {
                    resolve_types_expected(r, ast, child, expected);
                } else {
                    resolve_types(r, ast, child);
                }
            }
            mc.last().and_then(|&last| r.node_types.get(&last).copied())
        }

        // ── Function definition — resolve signatures and check body ─
        NodeKind::FunctionDef => {
            resolve_function_def_types(r, ast, node_idx);
            None
        }

        // ── Call — infer return type from callee ───────────────────
        NodeKind::Call => resolve_call_types(r, ast, node_idx),

        // ── PostMatch — type is the type of the first arm body ─────
        NodeKind::PostMatch => resolve_match_types(r, ast, node_idx, expected),

        // ── Projection (a.b) — infer from enum variant or field ────
        NodeKind::Projection => resolve_projection_types(r, ast, node_idx),

        // ── Tuple — build a Tuple type from element types ──────────
        NodeKind::Tuple => resolve_tuple_types(r, ast, node_idx, expected),

        // ── Error construction — produces ErrorQualified type ──────
        NodeKind::ErrorConstruction => {
            let children = ast.fixed_children(node_idx);
            resolve_types(r, ast, children[0]);
            let err_ti = r.node_types.get(&children[0]).copied();
            // Wrap in ErrorQualified to match expected !E T return types.
            if let Some(eti) = err_ti {
                // If there's an expected ErrorQualified type, use its inner type.
                let inner = expected
                    .and_then(|exp| {
                        if let TypeKind::ErrorQualified { inner, .. } = &r.type_pool.get(exp).kind {
                            Some(*inner)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(Intrinsic::Unit.type_index());
                Some(r.register_type(TypeKind::ErrorQualified {
                    errors: vec![eti],
                    inner,
                }))
            } else {
                None
            }
        }

        // ── Error/effect/option propagation — passthrough ──────────
        NodeKind::ErrorPropagation | NodeKind::EffectPropagation | NodeKind::OptionPropagation => {
            let children = ast.fixed_children(node_idx);
            resolve_types(r, ast, children[0]);
            r.node_types.get(&children[0]).copied()
        }

        // ── Lambda — resolve params, body, build Function type ─────
        NodeKind::Lambda => resolve_lambda_types(r, ast, node_idx, expected),

        // ── Named argument — resolve value only ────────────────────
        NodeKind::NamedArg => {
            let children = ast.fixed_children(node_idx);
            // children[0] = parameter name (Id) — skip
            // children[1] = value expression — resolve with expected type
            resolve_types_expected(r, ast, children[1], expected);
            r.node_types.get(&children[1]).copied()
        }

        // ── Struct construction: TypeName { field: val, ... } ──────
        NodeKind::ExtendedCall => {
            let children = ast.fixed_children(node_idx);
            let callee = children[0];
            // Resolve callee type (should be a struct/enum type symbol).
            resolve_types(r, ast, callee);
            let struct_ti = r.node_types.get(&callee).copied().or_else(|| {
                r.node_symbols
                    .get(&callee)
                    .map(|&sym_id| r.symbols[sym_id.0 as usize].type_index)
            });
            // Resolve each property value (skip field name keys).
            for &arg in ast.multi_children(node_idx) {
                if ast.node(arg).kind == NodeKind::Property {
                    let prop_children = ast.fixed_children(arg);
                    if prop_children.len() > 1 {
                        resolve_types(r, ast, prop_children[1]);
                    }
                } else {
                    resolve_types(r, ast, arg);
                }
            }
            struct_ti
        }

        // ── Struct definition — populate FieldInfo in the type pool ─
        NodeKind::StructDef => {
            let children = ast.fixed_children(node_idx);
            let name_node = children[0];
            // Get the already-registered struct TypeIndex from the symbol.
            let type_idx = r
                .node_symbols
                .get(&name_node)
                .map(|&sym_id| r.symbols[sym_id.0 as usize].type_index)
                .unwrap_or(TypeIndex::INVALID);
            // Collect fields in declaration order.
            let fields: Vec<(str_interner::StrId, type_pool::TypeIndex, NodeIndex)> = ast
                .multi_children(node_idx)
                .iter()
                .filter(|&&m| ast.node(m).kind == NodeKind::StructField)
                .map(|&field_node| {
                    let fc = ast.fixed_children(field_node);
                    let field_name = ast.node(fc[0]).str_id;
                    let type_node = fc[1];
                    resolve_types(r, ast, type_node);
                    let field_ti =
                        resolve_type_expr(r, ast, type_node).unwrap_or(TypeIndex::INVALID);
                    (field_name, field_ti, field_node)
                })
                .collect();
            // Populate the struct's FieldInfo in the type pool.
            if type_idx != TypeIndex::INVALID {
                let field_infos: Vec<type_pool::FieldInfo> = fields
                    .iter()
                    .enumerate()
                    .map(|(i, &(name, ty, _))| type_pool::FieldInfo {
                        name,
                        ty,
                        has_default: false,
                        offset: (i * 8) as u32,
                    })
                    .collect();
                if let TypeKind::Struct {
                    fields: ref mut f, ..
                } = r.type_pool.get_mut(type_idx).kind
                {
                    *f = field_infos;
                }
            }
            // Also resolve non-field members (methods, etc.)
            for &member in ast.multi_children(node_idx) {
                if ast.node(member).kind != NodeKind::StructField {
                    resolve_types(r, ast, member);
                }
            }
            None
        }

        // ── Default: recurse ───────────────────────────────────────
        _ => {
            for &child in ast.fixed_children(node_idx) {
                resolve_types(r, ast, child);
            }
            for &child in ast.multi_children(node_idx) {
                resolve_types(r, ast, child);
            }
            None
        }
    };

    if let Some(ti) = inferred {
        r.node_types.insert(node_idx, ti);
    }
}

// ---------------------------------------------------------------------------
// Type annotation resolution
// ---------------------------------------------------------------------------

/// Resolve a type-expression AST node to a TypeIndex.
/// Handles: Id (named types), OptionalType, FnType, etc.
fn resolve_type_expr(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) -> Option<TypeIndex> {
    if node_idx.is_null() {
        return None;
    }
    let kind = ast.node(node_idx).kind;
    match kind {
        // Named type: look up the identifier's symbol, which should be a Type or Trait.
        NodeKind::Id => {
            if let Some(&sym_id) = r.node_symbols.get(&node_idx) {
                let sym = &r.symbols[sym_id.0 as usize];
                if sym.kind == SymbolKind::Type || sym.kind == SymbolKind::Trait {
                    let ti = sym.type_index;
                    if ti != TypeIndex::INVALID {
                        return Some(ti);
                    }
                }
            }
            None
        }
        // `.Name'builtin` type view
        NodeKind::View => {
            if let Some(&sym_id) = r.node_symbols.get(&node_idx) {
                let sym = &r.symbols[sym_id.0 as usize];
                if sym.kind == SymbolKind::Type && sym.type_index != TypeIndex::INVALID {
                    return Some(sym.type_index);
                }
            }
            None
        }
        NodeKind::OptionalType => {
            let children = ast.fixed_children(node_idx);
            resolve_type_expr(r, ast, children[0])
                .map(|inner| r.register_type(TypeKind::Optional { inner }))
        }
        // Tuple type: (T1, T2, ...)
        NodeKind::Tuple => {
            let elems: Vec<TypeIndex> = ast
                .multi_children(node_idx)
                .iter()
                .filter_map(|&child| resolve_type_expr(r, ast, child))
                .collect();
            if elems.is_empty() {
                None
            } else {
                Some(r.register_type(TypeKind::Tuple { elements: elems }))
            }
        }
        // Error qualified type: !ErrType InnerType  — children: [0] err_set [1] inner
        NodeKind::ErrorQualifiedType => {
            let children = ast.fixed_children(node_idx);
            if children.len() >= 2 {
                let err_ti = resolve_type_expr(r, ast, children[0]);
                let inner_ti = resolve_type_expr(r, ast, children[1]);
                if let (Some(err), Some(inner)) = (err_ti, inner_ti) {
                    Some(r.register_type(TypeKind::ErrorQualified {
                        errors: vec![err],
                        inner,
                    }))
                } else {
                    inner_ti.or(err_ti)
                }
            } else {
                resolve_type_expr(r, ast, children[0])
            }
        }
        // Effect qualified type: #EffType InnerType — children: [0] eff_set [1] inner
        NodeKind::EffectQualifiedType => {
            let children = ast.fixed_children(node_idx);
            if children.len() >= 2 {
                let eff_ti = resolve_type_expr(r, ast, children[0]);
                let inner_ti = resolve_type_expr(r, ast, children[1]);
                if let (Some(eff), Some(inner)) = (eff_ti, inner_ti) {
                    Some(r.register_type(TypeKind::EffectQualified {
                        effects: vec![eff],
                        inner,
                    }))
                } else {
                    inner_ti.or(eff_ti)
                }
            } else {
                resolve_type_expr(r, ast, children[0])
            }
        }
        // Unit type literal
        NodeKind::Unit => Some(Intrinsic::Unit.type_index()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Function definition type resolution
// ---------------------------------------------------------------------------

fn resolve_function_def_types(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] name  [1] ret_type  [2] body  [3] capability
    // multi: params

    // Signature (params + return type) was already resolved in the forward
    // pass (`forward_resolve_signatures`). Read the return type back.
    let name_node = children[0];
    let ret_type = if let Some(&sym_id) = r.node_symbols.get(&name_node) {
        let ti = r.symbols[sym_id.0 as usize].type_index;
        if ti != TypeIndex::INVALID {
            if let TypeKind::Function { ret, .. } = &r.type_pool.get(ti).kind {
                Some(*ret)
            } else {
                None
            }
        } else {
            // Fallback: no forward signature (e.g. no return type annotation).
            resolve_type_expr(r, ast, children[1])
        }
    } else {
        resolve_type_expr(r, ast, children[1])
    };

    // Resolve body with expected return type for bidirectional inference.
    resolve_types_expected(r, ast, children[2], ret_type);

    // Check: body type vs declared return type.
    if let Some(ret_ti) = ret_type {
        if let Some(&body_ti) = r.node_types.get(&children[2]) {
            if !r.type_pool.is_subtype(body_ti, ret_ti)
                && !r.type_pool.is_gradually_consistent(body_ti, ret_ti)
            {
                let fn_name = str_interner::get(ast.node(name_node).str_id);
                let expected = type_display_name(&r.type_pool, ret_ti);
                let found = type_display_name(&r.type_pool, body_ti);
                let body_span = ast.node(children[2]).span;
                let ret_type_span = ast.node(children[1]).span;
                r.diag_ctx
                    .error(format!(
                        "type mismatch in function `{fn_name}`: expected return type `{expected}`, found `{found}`"
                    ))
                    .with_primary_span(body_span)
                    .with_label(body_span, format!("this has type `{found}`"), Level::Error)
                    .with_label(ret_type_span, format!("expected `{expected}` because of return type"), Level::Note)
                    .emit(r.diag_ctx);
            }
        }
    }

    // Resolve capability.
    resolve_types(r, ast, children[3]);
}

// ---------------------------------------------------------------------------
// Lambda type resolution
// ---------------------------------------------------------------------------

fn resolve_lambda_types(
    r: &mut Resolver,
    ast: &Ast,
    node_idx: NodeIndex,
    expected: Option<TypeIndex>,
) -> Option<TypeIndex> {
    let children = ast.fixed_children(node_idx);
    // Lambda: fixed=[0] body, [1] ret_type   multi=params
    let body_node = children[0];
    let ret_type_node = children[1];
    let params = ast.multi_children(node_idx).to_vec();

    // Extract expected parameter/return types from context (if expected is a Function type).
    let (expected_param_types, expected_ret) = expected
        .and_then(|exp| {
            if let TypeKind::Function { params, ret } = &r.type_pool.get(exp).kind {
                Some((Some(params.clone()), Some(*ret)))
            } else {
                None
            }
        })
        .unwrap_or((None, None));

    // Resolve parameter types: use annotations if present, fall back to expected types.
    let mut param_type_list = Vec::with_capacity(params.len());
    for (i, &param) in params.iter().enumerate() {
        resolve_param_types(r, ast, param);
        let from_annotation = param_type_index(r, ast, param);
        let from_expected = expected_param_types
            .as_ref()
            .and_then(|pts| pts.get(i).copied());
        let ti = from_annotation
            .or(from_expected)
            .unwrap_or(TypeIndex::INVALID);
        // Propagate resolved type to the parameter symbol.
        if ti != TypeIndex::INVALID {
            if let Some(pat_node) = param_pattern_node(ast, param) {
                if let Some(&sym_id) = r.node_symbols.get(&pat_node) {
                    r.symbols[sym_id.0 as usize].type_index = ti;
                }
            }
        }
        param_type_list.push(ti);
    }

    // Resolve explicit return type annotation.
    let explicit_ret = resolve_type_expr(r, ast, ret_type_node);

    // Resolve body with expected return type for bidirectional inference.
    let body_expected = explicit_ret.or(expected_ret);
    resolve_types_expected(r, ast, body_node, body_expected);
    let body_type = r.node_types.get(&body_node).copied();

    // Determine return type: explicit annotation > body type > expected > Unit.
    let ret_type = explicit_ret
        .or(body_type)
        .or(expected_ret)
        .unwrap_or(Intrinsic::Unit.type_index());

    // Check explicit return type vs body type.
    if let (Some(ann_ret), Some(body_ti)) = (explicit_ret, body_type) {
        if !r.type_pool.is_subtype(body_ti, ann_ret)
            && !r.type_pool.is_gradually_consistent(body_ti, ann_ret)
        {
            let expected_name = type_display_name(&r.type_pool, ann_ret);
            let found_name = type_display_name(&r.type_pool, body_ti);
            let body_span = ast.node(body_node).span;
            let ret_span = ast.node(ret_type_node).span;
            r.diag_ctx
                .error(format!(
                    "type mismatch in lambda: expected return type `{expected_name}`, found `{found_name}`"
                ))
                .with_primary_span(body_span)
                .with_label(body_span, format!("this has type `{found_name}`"), Level::Error)
                .with_label(ret_span, format!("expected `{expected_name}` because of return type"), Level::Note)
                .emit(r.diag_ctx);
        }
    }

    // Build the Function type.
    let func_ti = r.register_type(TypeKind::Function {
        params: param_type_list,
        ret: ret_type,
    });
    Some(func_ti)
}

/// Get the pattern node of a parameter (the identifier binding).
fn param_pattern_node(ast: &Ast, param_idx: NodeIndex) -> Option<NodeIndex> {
    if param_idx.is_null() {
        return None;
    }
    match ast.node(param_idx).kind {
        NodeKind::ParamTyped | NodeKind::ParamLambda => {
            let children = ast.fixed_children(param_idx);
            Some(children[0])
        }
        NodeKind::ParamSelf => Some(param_idx),
        _ => None,
    }
}

/// Resolve the type annotation on a typed parameter and set its symbol's type_index.
fn resolve_param_types(r: &mut Resolver, ast: &Ast, param_idx: NodeIndex) {
    if param_idx.is_null() {
        return;
    }
    let kind = ast.node(param_idx).kind;
    match kind {
        NodeKind::ParamTyped | NodeKind::ParamLambda => {
            let children = ast.fixed_children(param_idx);
            // [0] pattern  [1] type
            let type_node = children[1];
            if let Some(ti) = resolve_type_expr(r, ast, type_node) {
                // Set the parameter symbol's type.
                let pat_node = children[0];
                if let Some(&sym_id) = r.node_symbols.get(&pat_node) {
                    r.symbols[sym_id.0 as usize].type_index = ti;
                }
            }
        }
        NodeKind::ParamOptional => {
            let children = ast.fixed_children(param_idx);
            // [0] id  [1] type  [2] default
            if children.len() > 1 {
                if let Some(ti) = resolve_type_expr(r, ast, children[1]) {
                    if let Some(&sym_id) = r.node_symbols.get(&children[0]) {
                        r.symbols[sym_id.0 as usize].type_index = ti;
                    }
                }
            }
            // Also resolve the default value expression.
            if children.len() > 2 {
                resolve_types(r, ast, children[2]);
            }
        }
        NodeKind::ParamVarargs => {
            let children = ast.fixed_children(param_idx);
            // [0] id  [1] type
            if children.len() > 1 {
                if let Some(ti) = resolve_type_expr(r, ast, children[1]) {
                    if let Some(&sym_id) = r.node_symbols.get(&children[0]) {
                        r.symbols[sym_id.0 as usize].type_index = ti;
                    }
                }
            }
        }
        _ => {}
    }
}

/// Extract the TypeIndex of a parameter (if resolved).
fn param_type_index(r: &Resolver, ast: &Ast, param_idx: NodeIndex) -> Option<TypeIndex> {
    if param_idx.is_null() {
        return None;
    }
    let kind = ast.node(param_idx).kind;
    let pat_node = match kind {
        NodeKind::ParamTyped | NodeKind::ParamLambda => ast.fixed_children(param_idx)[0],
        NodeKind::ParamOptional | NodeKind::ParamVarargs => ast.fixed_children(param_idx)[0],
        NodeKind::ParamSelf => {
            return None; // Self type resolved elsewhere.
        }
        _ => return None,
    };
    if let Some(&sym_id) = r.node_symbols.get(&pat_node) {
        let ti = r.symbols[sym_id.0 as usize].type_index;
        if ti != TypeIndex::INVALID {
            return Some(ti);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Variable declaration type resolution
// ---------------------------------------------------------------------------

fn resolve_decl(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // [0] pattern  [1] type_annotation  [2] value  ([3] else for some)

    // Resolve value first.
    if children.len() > 2 {
        resolve_types(r, ast, children[2]);
    }

    // Resolve type annotation.
    let annotation_ti = if children.len() > 1 {
        resolve_type_expr(r, ast, children[1])
    } else {
        None
    };

    // Determine the effective type: annotation takes priority, fallback to value type.
    let val_ti = if children.len() > 2 {
        r.node_types.get(&children[2]).copied()
    } else {
        None
    };

    let effective_ti = annotation_ti.or(val_ti);

    // Propagate type to the pattern symbol.
    if let Some(ti) = effective_ti {
        propagate_type_to_pattern(r, ast, children[0], ti);
    }

    // Check: if both annotation and value type are known, verify compatibility.
    if let (Some(ann_ti), Some(v_ti)) = (annotation_ti, val_ti) {
        if !r.type_pool.is_subtype(v_ti, ann_ti)
            && !r.type_pool.is_gradually_consistent(v_ti, ann_ti)
        {
            let expected = type_display_name(&r.type_pool, ann_ti);
            let found = type_display_name(&r.type_pool, v_ti);
            let val_span = ast.node(children[2]).span;
            let ann_span = ast.node(children[1]).span;
            r.diag_ctx
                .error(format!(
                    "type mismatch: expected `{expected}`, found `{found}`"
                ))
                .with_primary_span(val_span)
                .with_label(
                    val_span,
                    format!("expected `{expected}`, found `{found}`"),
                    Level::Error,
                )
                .with_label(
                    ann_span,
                    format!("expected due to this type annotation"),
                    Level::Note,
                )
                .emit(r.diag_ctx);
        }
    }

    // Recurse into remaining children.
    if children.len() > 3 {
        resolve_types(r, ast, children[3]);
    }
}

// ---------------------------------------------------------------------------
// Call type inference
// ---------------------------------------------------------------------------

fn resolve_call_types(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) -> Option<TypeIndex> {
    let children = ast.fixed_children(node_idx);
    // [0] callee  multi = args
    let callee = children[0];
    resolve_types(r, ast, callee);

    // Retrieve callee's parameter types to propagate expected types to arguments.
    let param_types = get_callee_param_types(r, callee);

    // Resolve argument types with expected parameter types for bidirectional inference.
    let args = ast.multi_children(node_idx);
    for (i, &arg) in args.iter().enumerate() {
        let expected = param_types.as_ref().and_then(|pts| {
            let t = pts.get(i).copied()?;
            if t != TypeIndex::INVALID {
                Some(t)
            } else {
                None
            }
        });
        resolve_types_expected(r, ast, arg, expected);
    }

    // Back-propagate: if callee has INVALID param types, update them from argument types.
    if let Some(&callee_sym) = r.node_symbols.get(&callee) {
        let ti = r.symbols[callee_sym.0 as usize].type_index;
        if ti != TypeIndex::INVALID {
            let kind = r.type_pool.get(ti).kind.clone();
            if let TypeKind::Function { params, ret } = kind {
                let has_invalid = params.iter().any(|p| *p == TypeIndex::INVALID);
                if has_invalid && args.len() == params.len() {
                    let updated_params: Vec<TypeIndex> = params
                        .iter()
                        .enumerate()
                        .map(|(i, &pt)| {
                            if pt == TypeIndex::INVALID {
                                args.get(i)
                                    .and_then(|&arg| r.node_types.get(&arg).copied())
                                    .unwrap_or(TypeIndex::INVALID)
                            } else {
                                pt
                            }
                        })
                        .collect();
                    let new_func_ti = r.register_type(TypeKind::Function {
                        params: updated_params,
                        ret,
                    });
                    r.symbols[callee_sym.0 as usize].type_index = new_func_ti;
                }
            }
        }
    }

    // Validate argument count and types against callee signature.
    if let Some(ref pts) = param_types {
        let call_span = ast.node(node_idx).span;
        let callee_span = ast.node(callee).span;

        // Arity check.
        if args.len() != pts.len() {
            let callee_name = callee_display_name(r, ast, callee);
            r.diag_ctx
                .error(format!(
                    "`{callee_name}` expects {} argument{}, but {} {} supplied",
                    pts.len(),
                    if pts.len() == 1 { "" } else { "s" },
                    args.len(),
                    if args.len() == 1 { "was" } else { "were" },
                ))
                .with_primary_span(call_span)
                .with_label(
                    callee_span,
                    format!(
                        "expects {} argument{}",
                        pts.len(),
                        if pts.len() == 1 { "" } else { "s" }
                    ),
                    Level::Note,
                )
                .emit(r.diag_ctx);
        } else {
            // Type check each argument.
            for (i, &arg) in args.iter().enumerate() {
                if let Some(&arg_ti) = r.node_types.get(&arg) {
                    let param_ti = pts[i];
                    if param_ti != TypeIndex::INVALID
                        && !r.type_pool.is_subtype(arg_ti, param_ti)
                        && !r.type_pool.is_gradually_consistent(arg_ti, param_ti)
                    {
                        let callee_name = callee_display_name(r, ast, callee);
                        let expected = type_display_name(&r.type_pool, param_ti);
                        let found = type_display_name(&r.type_pool, arg_ti);
                        let arg_span = ast.node(arg).span;
                        r.diag_ctx
                            .error(format!(
                                "type mismatch in argument {} of `{callee_name}`: expected `{expected}`, found `{found}`",
                                i + 1
                            ))
                            .with_primary_span(arg_span)
                            .with_label(arg_span, format!("expected `{expected}`, found `{found}`"), Level::Error)
                            .emit(r.diag_ctx);
                    }
                }
            }
        }
    }

    // Infer return type from callee.
    if let Some(&callee_sym) = r.node_symbols.get(&callee) {
        let sym = &r.symbols[callee_sym.0 as usize];
        if let SymbolKind::BuiltinFunction(id) = sym.kind {
            return Some(builtin_return_type(id));
        }
        let ti = sym.type_index;
        if ti != TypeIndex::INVALID {
            if let TypeKind::Function { ret, .. } = &r.type_pool.get(ti).kind {
                return Some(*ret);
            }
        }
    }
    None
}

/// Get the parameter types for a callee, if known.
fn get_callee_param_types(r: &Resolver, callee: NodeIndex) -> Option<Vec<TypeIndex>> {
    if let Some(&sym_id) = r.node_symbols.get(&callee) {
        let sym = &r.symbols[sym_id.0 as usize];
        let ti = sym.type_index;
        if ti != TypeIndex::INVALID {
            if let TypeKind::Function { params, .. } = &r.type_pool.get(ti).kind {
                return Some(params.clone());
            }
        }
    }
    None
}

/// Get a display name for a callee expression (for diagnostics).
fn callee_display_name(_r: &Resolver, ast: &Ast, callee: NodeIndex) -> String {
    if !callee.is_null() && ast.node(callee).kind == NodeKind::Id {
        return str_interner::get(ast.node(callee).str_id);
    }
    "<callee>".to_string()
}

/// Approximate return type for known builtins (dynamic; std wrappers refine).
fn builtin_return_type(id: runtime::BuiltinFnId) -> TypeIndex {
    use runtime::ids;
    match id {
        ids::PRINT | ids::PRINTLN | ids::EXIT | ids::PANIC | ids::LIST_INIT => {
            Intrinsic::Unit.type_index()
        }
        ids::TYPE_OF | ids::TO_STRING | ids::STR_CONCAT => Intrinsic::Str.type_index(),
        ids::TO_I64 | ids::STR_LEN => Intrinsic::I64.type_index(),
        ids::TO_F64
        | ids::ABS
        | ids::SIN
        | ids::COS
        | ids::SQRT
        | ids::FLOOR
        | ids::CEIL
        | ids::ROUND
        | ids::POW
        | ids::LOG => Intrinsic::F64.type_index(),
        _ => Intrinsic::Any.type_index(),
    }
}

// ---------------------------------------------------------------------------
// Match expression type inference
// ---------------------------------------------------------------------------

fn resolve_match_types(
    r: &mut Resolver,
    ast: &Ast,
    node_idx: NodeIndex,
    expected: Option<TypeIndex>,
) -> Option<TypeIndex> {
    let children = ast.fixed_children(node_idx);
    // [0] scrutinee  multi = arms
    resolve_types(r, ast, children[0]);

    let mut result_type: Option<TypeIndex> = None;
    for &arm in ast.multi_children(node_idx) {
        let arm_children = ast.fixed_children(arm);
        // CaseArm: [0] pattern  [1] body
        resolve_types_expected(r, ast, arm_children[1], expected);
        if result_type.is_none() {
            result_type = r.node_types.get(&arm_children[1]).copied();
        }
    }
    result_type
}

// ---------------------------------------------------------------------------
// Projection type inference (a.b)
// ---------------------------------------------------------------------------

fn resolve_projection_types(r: &mut Resolver, ast: &Ast, node_idx: NodeIndex) -> Option<TypeIndex> {
    let children = ast.fixed_children(node_idx);
    resolve_types(r, ast, children[0]);

    // If the member resolves to an enum variant, the type is the parent enum type.
    let member_node = children[1];
    if !member_node.is_null() {
        if let Some(&sym_id) = r.node_symbols.get(&member_node) {
            let sym = &r.symbols[sym_id.0 as usize];
            if sym.kind == SymbolKind::EnumVariant {
                return r.node_types.get(&children[0]).copied();
            }
        }
    }

    // Try struct field access: look up field by name in the LHS struct type.
    let lhs_type = r.node_types.get(&children[0]).copied();
    if let Some(lhs_ti) = lhs_type {
        if let type_pool::TypeKind::Struct { fields, .. } = &r.type_pool.get(lhs_ti).kind.clone() {
            if !member_node.is_null() {
                let field_name = ast.node(member_node).str_id;
                for (idx, field) in fields.iter().enumerate() {
                    if field.name == field_name {
                        r.node_field_indices.insert(node_idx, idx as u32);
                        return Some(field.ty);
                    }
                }
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Tuple type inference
// ---------------------------------------------------------------------------

fn resolve_tuple_types(
    r: &mut Resolver,
    ast: &Ast,
    node_idx: NodeIndex,
    expected: Option<TypeIndex>,
) -> Option<TypeIndex> {
    let elems = ast.multi_children(node_idx);

    // Extract expected element types from a Tuple expected type.
    let expected_elems = expected.and_then(|exp| {
        if let TypeKind::Tuple { elements } = &r.type_pool.get(exp).kind {
            Some(elements.clone())
        } else {
            None
        }
    });

    // Resolve each element with its expected type.
    let mut elem_types = Vec::with_capacity(elems.len());
    for (i, &elem) in elems.iter().enumerate() {
        let exp = expected_elems.as_ref().and_then(|e| e.get(i).copied());
        resolve_types_expected(r, ast, elem, exp);
        if let Some(&ti) = r.node_types.get(&elem) {
            elem_types.push(ti);
        } else {
            elem_types.push(TypeIndex::INVALID);
        }
    }

    // Check tuple arity against expected.
    if let Some(exp) = expected {
        if let TypeKind::Tuple {
            elements: ref exp_elems,
        } = r.type_pool.get(exp).kind.clone()
        {
            if elems.len() != exp_elems.len() {
                let span = ast.node(node_idx).span;
                r.diag_ctx
                    .error(format!(
                        "tuple has {} element{}, but expected {}",
                        elems.len(),
                        if elems.len() == 1 { "" } else { "s" },
                        exp_elems.len(),
                    ))
                    .with_primary_span(span)
                    .with_label(
                        span,
                        format!(
                            "has {} element{}",
                            elems.len(),
                            if elems.len() == 1 { "" } else { "s" }
                        ),
                        Level::Error,
                    )
                    .emit(r.diag_ctx);
            } else {
                // Check each element type.
                for (i, &elem) in elems.iter().enumerate() {
                    if let Some(&elem_ti) = r.node_types.get(&elem) {
                        let exp_ti = exp_elems[i];
                        if exp_ti != TypeIndex::INVALID
                            && !r.type_pool.is_subtype(elem_ti, exp_ti)
                            && !r.type_pool.is_gradually_consistent(elem_ti, exp_ti)
                        {
                            let expected_name = type_display_name(&r.type_pool, exp_ti);
                            let found_name = type_display_name(&r.type_pool, elem_ti);
                            let elem_span = ast.node(elem).span;
                            r.diag_ctx
                                .error(format!(
                                    "type mismatch in tuple element {}: expected `{expected_name}`, found `{found_name}`",
                                    i + 1
                                ))
                                .with_primary_span(elem_span)
                                .with_label(elem_span, format!("expected `{expected_name}`, found `{found_name}`"), Level::Error)
                                .emit(r.diag_ctx);
                        }
                    }
                }
            }
        }
    }

    Some(r.register_type(TypeKind::Tuple {
        elements: elem_types,
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// If a pattern is a simple identifier, set its symbol's type_index.
fn propagate_type_to_pattern(r: &mut Resolver, ast: &Ast, pat: NodeIndex, ty: TypeIndex) {
    if pat.is_null() {
        return;
    }
    if ast.node(pat).kind == NodeKind::Id {
        if let Some(&sym_id) = r.node_symbols.get(&pat) {
            r.symbols[sym_id.0 as usize].type_index = ty;
        }
    }
}

/// Get a human-readable name for a type, for use in diagnostic messages.
fn type_display_name(pool: &type_pool::TypePool, ti: TypeIndex) -> String {
    if ti == TypeIndex::INVALID {
        return "<unknown>".to_string();
    }
    let info = pool.get(ti);
    match &info.kind {
        TypeKind::Intrinsic(intr) => intr.name().to_string(),
        TypeKind::Struct { name, .. } => str_interner::get(*name),
        TypeKind::Enum { name, .. } => str_interner::get(*name),
        TypeKind::Typealias { name, .. } => str_interner::get(*name),
        TypeKind::Newtype { name, .. } => str_interner::get(*name),
        TypeKind::Optional { inner } => format!("?{}", type_display_name(pool, *inner)),
        TypeKind::Tuple { elements } => {
            let parts: Vec<String> = elements
                .iter()
                .map(|e| type_display_name(pool, *e))
                .collect();
            format!("({})", parts.join(", "))
        }
        TypeKind::Function { params, ret } => {
            let ps: Vec<String> = params.iter().map(|p| type_display_name(pool, *p)).collect();
            format!("fn({}) -> {}", ps.join(", "), type_display_name(pool, *ret))
        }
        TypeKind::Module { name } => format!("module {}", str_interner::get(*name)),
        TypeKind::Trait { name, .. } => format!("trait {}", str_interner::get(*name)),
        TypeKind::ErrorQualified { errors, inner } => {
            let errs: Vec<String> = errors.iter().map(|e| type_display_name(pool, *e)).collect();
            if errs.len() == 1 {
                format!("!{} {}", errs[0], type_display_name(pool, *inner))
            } else {
                format!("![{}] {}", errs.join(", "), type_display_name(pool, *inner))
            }
        }
        TypeKind::EffectQualified { effects, inner } => {
            let effs: Vec<String> = effects
                .iter()
                .map(|e| type_display_name(pool, *e))
                .collect();
            if effs.len() == 1 {
                format!("#{} {}", effs[0], type_display_name(pool, *inner))
            } else {
                format!("#[{}] {}", effs.join(", "), type_display_name(pool, *inner))
            }
        }
        _ => format!("TypeIndex({})", ti.as_u32()),
    }
}

/// Choose the type for an unsuffixed integer literal.
/// If the expected type is an integer type, use it; otherwise default to i64.
fn contextual_int_type(pool: &type_pool::TypePool, expected: Option<TypeIndex>) -> TypeIndex {
    if let Some(exp) = expected {
        if let Some(intr) = pool.as_intrinsic(exp) {
            if intr.is_integer() {
                return exp;
            }
        }
    }
    Intrinsic::I64.type_index()
}

/// Choose the type for an unsuffixed float literal.
/// If the expected type is f32, use it; otherwise default to f64.
fn contextual_float_type(pool: &type_pool::TypePool, expected: Option<TypeIndex>) -> TypeIndex {
    if let Some(exp) = expected {
        if let Some(intr) = pool.as_intrinsic(exp) {
            if matches!(intr, Intrinsic::F32 | Intrinsic::F64) {
                return exp;
            }
        }
    }
    Intrinsic::F64.type_index()
}
