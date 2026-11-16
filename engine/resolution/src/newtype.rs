//! Nominal `newtype Name = InnerType` resolution, construction, casts, and patterns.

use std::collections::HashSet;

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{Intrinsic, TypeIndex, TypeKind, TypePool};

use crate::resolver::Resolver;
use crate::{ScopeId, SymbolId, SymbolKind};

pub(crate) fn prepare(r: &mut Resolver<'_>, ast: &Ast) {
    let declarations: Vec<(NodeIndex, TypeIndex, ScopeId, str_interner::StrId)> = r
        .symbols
        .iter()
        .filter(|sym| {
            sym.kind == SymbolKind::Type
                && !sym.def_node.is_null()
                && ast.node(sym.def_node).kind == NodeKind::Newtype
        })
        .filter_map(|sym| {
            let canon = r.type_pool.canonical_type(sym.type_index)?;
            Some((sym.def_node, canon, sym.scope, sym.name))
        })
        .collect();

    for &(def_node, canon_ty, scope, name) in &declarations {
        let ty_expr = ast.fixed_children(def_node)[1];
        let saved_scope = r.current_scope;
        r.current_scope = scope;
        let resolved_inner = crate::typing::resolve_type_expr(r, ast, ty_expr);
        r.current_scope = saved_scope;

        let inner = match resolved_inner {
            Some(ty) if ty != TypeIndex::INVALID => {
                if let Some(canon_inner) = r.type_pool.canonical_type(ty)
                    && matches!(
                        r.type_pool.get(canon_inner).kind,
                        TypeKind::Module { .. }
                            | TypeKind::Effect { .. }
                            | TypeKind::AssociatedType { .. }
                    )
                {
                    report(r, ast, ty_expr, "newtype requires a value type");
                    Intrinsic::Unit.type_index()
                } else {
                    ty
                }
            }
            _ => {
                report(
                    r,
                    ast,
                    if ty_expr.is_null() { def_node } else { ty_expr },
                    "newtype target must be a valid type",
                );
                Intrinsic::Unit.type_index()
            }
        };

        let info = r.type_pool.get_mut(canon_ty);
        info.kind = TypeKind::Newtype { name, inner };
        info.size = 8;
        info.align = 8;
    }

    // Reject transparent / unboxed newtype cycles such as `newtype A = A`,
    // `newtype A = B; newtype B = A`, or `newtype A = ?(i64, A)`.
    for &(def_node, canon_ty, _, name) in &declarations {
        let inner = match r.type_pool.get(canon_ty).kind {
            TypeKind::Newtype { inner, .. } => inner,
            _ => continue,
        };
        if reaches_newtype(&r.type_pool, inner, canon_ty, &mut HashSet::new()) {
            report(
                r,
                ast,
                def_node,
                format!(
                    "cyclic newtype definition for `{}`",
                    str_interner::get(name)
                ),
            );
            r.type_pool.get_mut(canon_ty).kind = TypeKind::Newtype {
                name,
                inner: Intrinsic::Unit.type_index(),
            };
        }
    }
}

fn reaches_newtype(
    pool: &TypePool,
    current: TypeIndex,
    target: TypeIndex,
    visited: &mut HashSet<TypeIndex>,
) -> bool {
    if current == TypeIndex::INVALID {
        return false;
    }
    let Some(canon) = pool.canonical_type(current) else {
        return true;
    };
    if canon == target {
        return true;
    }
    if !visited.insert(canon) {
        return false;
    }
    match &pool.get(canon).kind {
        TypeKind::Newtype { inner, .. }
        | TypeKind::Optional { inner }
        | TypeKind::Future { inner } => reaches_newtype(pool, *inner, target, visited),
        TypeKind::Tuple { elements } => elements
            .iter()
            .any(|&elem| reaches_newtype(pool, elem, target, visited)),
        TypeKind::ErrorQualified { errors, inner } => {
            reaches_newtype(pool, *inner, target, visited)
                || errors
                    .iter()
                    .any(|&err| reaches_newtype(pool, err, target, visited))
        }
        TypeKind::EffectQualified { inner, .. } => reaches_newtype(pool, *inner, target, visited),
        _ => false,
    }
}

pub(crate) fn inner_type(pool: &TypePool, ty: TypeIndex) -> Option<TypeIndex> {
    let canon = pool.canonical_type(ty)?;
    match pool.get(canon).kind {
        TypeKind::Newtype { inner, .. } => Some(inner),
        _ => None,
    }
}

pub(crate) fn denoted_newtype(
    r: &Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
) -> Option<(TypeIndex, Option<SymbolId>)> {
    let (raw_ty, sym_id) = denoted_type_symbol(r, ast, node)?;
    let canon = r.type_pool.canonical_type(raw_ty)?;
    matches!(r.type_pool.get(canon).kind, TypeKind::Newtype { .. }).then_some((canon, sym_id))
}

fn denoted_type_symbol(
    r: &Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
) -> Option<(TypeIndex, Option<SymbolId>)> {
    if node.is_null() {
        return None;
    }
    match ast.node(node).kind {
        NodeKind::Id => {
            let &sym_id = r.node_symbols.get(&node)?;
            let sym = &r.symbols[sym_id.0 as usize];
            (sym.kind == SymbolKind::Type && sym.type_index != TypeIndex::INVALID)
                .then_some((sym.type_index, Some(sym_id)))
        }
        NodeKind::SelfUpper => {
            if let Some(&sym_id) = r.node_symbols.get(&node) {
                let sym = &r.symbols[sym_id.0 as usize];
                if sym.kind == SymbolKind::Type && sym.type_index != TypeIndex::INVALID {
                    return Some((sym.type_index, Some(sym_id)));
                }
            }
            let scope = r.node_scopes.get(&node).copied().unwrap_or(r.current_scope);
            let mut cur = Some(scope);
            while let Some(sid) = cur {
                if let Some(assoc) = r.scopes[sid.0 as usize].assoc_type {
                    return Some((assoc, defining_symbol(r, ast, assoc)));
                }
                cur = r.scopes[sid.0 as usize].parent;
            }
            None
        }
        NodeKind::Projection => {
            if let Some(&sym_id) = r.node_symbols.get(&node) {
                let sym = &r.symbols[sym_id.0 as usize];
                if sym.kind == SymbolKind::Type && sym.type_index != TypeIndex::INVALID {
                    return Some((sym.type_index, Some(sym_id)));
                }
            }
            let children = ast.fixed_children(node);
            let name = ast.node(children[1]).str_id;
            if let Some(&base_sym_id) = r.node_symbols.get(&children[0]) {
                let base_sym = &r.symbols[base_sym_id.0 as usize];
                if base_sym.kind == SymbolKind::Module
                    && let Some(mod_scope) = r
                        .scopes
                        .iter()
                        .find(|s| s.node == base_sym.def_node)
                        .map(|s| s.id)
                    && let Some(&member_id) = r.scopes[mod_scope.0 as usize].bindings.get(&name)
                {
                    let member = &r.symbols[member_id.0 as usize];
                    if member.kind == SymbolKind::Type && member.type_index != TypeIndex::INVALID {
                        return Some((member.type_index, Some(member_id)));
                    }
                }
            }
            let (owner_ty, _) = denoted_type_symbol(r, ast, children[0])?;
            let member_id = crate::associated::member(r, owner_ty, name).ok()?;
            let member = &r.symbols[member_id.0 as usize];
            (member.kind == SymbolKind::Type && member.type_index != TypeIndex::INVALID)
                .then_some((member.type_index, Some(member_id)))
        }
        _ => None,
    }
}

pub(crate) fn defining_symbol(
    r: &Resolver<'_>,
    ast: &Ast,
    canon_ty: TypeIndex,
) -> Option<SymbolId> {
    let canon = r.type_pool.canonical_type(canon_ty)?;
    r.symbols
        .iter()
        .find(|sym| {
            sym.kind == SymbolKind::Type
                && !sym.def_node.is_null()
                && ast.node(sym.def_node).kind == NodeKind::Newtype
                && r.type_pool.canonical_type(sym.type_index) == Some(canon)
        })
        .map(|sym| sym.id)
}

pub(crate) fn construction(r: &mut Resolver<'_>, ast: &Ast, call: NodeIndex) -> Option<TypeIndex> {
    let callee = ast.fixed_children(call)[0];
    let (canon_ty, sym_hint) = denoted_newtype(r, ast, callee)?;
    let inner = inner_type(&r.type_pool, canon_ty)?;

    // Run normal callee resolution so visibility/access checks and type-value metadata fire.
    crate::typing::resolve_types(r, ast, callee);

    let args = ast.multi_children(call);
    let mut valid = true;
    if args.len() != 1 {
        valid = false;
        let ty_name = crate::typing::type_display_name(&r.type_pool, canon_ty);
        report(
            r,
            ast,
            call,
            format!(
                "newtype constructor `{ty_name}` expects 1 argument, found {}",
                args.len()
            ),
        );
    }
    for (idx, &arg) in args.iter().enumerate() {
        if arg.is_null() {
            continue;
        }
        match ast.node(arg).kind {
            NodeKind::NamedArg | NodeKind::SpreadArg => {
                valid = false;
                report(
                    r,
                    ast,
                    arg,
                    "newtype constructor accepts only a single positional argument",
                );
                let val = crate::argument_value_node(ast, arg);
                crate::typing::resolve_types(r, ast, val);
            }
            _ if idx == 0 => {
                crate::typing::resolve_types_expected(r, ast, arg, Some(inner));
                crate::typing::check_expected_type(
                    r,
                    ast,
                    arg,
                    inner,
                    "newtype constructor argument",
                );
            }
            _ => {
                crate::typing::resolve_types(r, ast, arg);
            }
        }
    }

    if valid {
        r.newtype_constructions.insert(call, canon_ty);
        if let Some(def_sym) = defining_symbol(r, ast, canon_ty).or(sym_hint) {
            r.constructor_types.insert(call, def_sym);
        }
    }
    Some(canon_ty)
}

/// Validate `.as(Target)` casts involving `Newtype` and record static wrapping coercions.
pub(crate) fn validate_cast(
    r: &mut Resolver<'_>,
    ast: &Ast,
    cast_node: NodeIndex,
    expr_node: NodeIndex,
    target: TypeIndex,
) {
    let Some(src_raw) = r.node_types.get(&expr_node).copied() else {
        return;
    };
    let (Some(src_canon), Some(target_canon)) = (
        r.type_pool.canonical_type(src_raw),
        r.type_pool.canonical_type(target),
    ) else {
        return;
    };
    if src_canon == target_canon {
        return;
    }
    let any_ty = Intrinsic::Any.type_index();
    let noret_ty = Intrinsic::NoReturn.type_index();
    if src_canon == any_ty || src_canon == noret_ty || target_canon == any_ty {
        return;
    }
    if matches!(r.type_pool.get(src_canon).kind, TypeKind::Trait { .. })
        || matches!(r.type_pool.get(target_canon).kind, TypeKind::Trait { .. })
    {
        return;
    }

    let src_nt_inner = inner_type(&r.type_pool, src_canon);
    let dst_nt_inner = inner_type(&r.type_pool, target_canon);
    if src_nt_inner.is_none() && dst_nt_inner.is_none() {
        return;
    }

    // Wrapping: `inner_val.as(Newtype)` where `src` is compatible with `dst_inner`.
    if let Some(dst_inner) = dst_nt_inner
        && let Some(dst_inner_canon) = r.type_pool.canonical_type(dst_inner)
    {
        let scope = r
            .node_scopes
            .get(&cast_node)
            .copied()
            .unwrap_or(r.current_scope);
        let compatible = r
            .type_pool
            .is_subtype_scoped(src_canon, dst_inner_canon, scope.0)
            .unwrap_or_else(|_| r.type_pool.is_subtype(src_canon, dst_inner_canon))
            || r.type_pool
                .is_gradually_consistent(src_canon, dst_inner_canon)
            || r.type_pool.numeric_lift(src_canon, dst_inner_canon) == Some(dst_inner_canon);
        if compatible {
            crate::typing::check_expected_type(r, ast, expr_node, dst_inner, "newtype cast");
            r.newtype_wraps.insert(cast_node, target_canon);
            if let Some(def_sym) = defining_symbol(r, ast, target_canon) {
                r.constructor_types.insert(cast_node, def_sym);
            }
            return;
        }
    }

    // Unwrapping: `nt_val.as(InnerType)` where `nt_val`'s canonical inner equals `target_canon`.
    if let Some(src_inner) = src_nt_inner
        && r.type_pool.canonical_type(src_inner) == Some(target_canon)
    {
        return;
    }

    let src_name = crate::typing::type_display_name(&r.type_pool, src_raw);
    let dst_name = crate::typing::type_display_name(&r.type_pool, target);
    report(
        r,
        ast,
        cast_node,
        format!(
            "cannot cast `{src_name}` to `{dst_name}`: newtype casts allow only wrapping from or unwrapping to the underlying type"
        ),
    );
}

pub(crate) fn is_irrefutable_pattern(r: &Resolver<'_>, ast: &Ast, node: NodeIndex) -> bool {
    if node.is_null() || ast.node(node).kind != NodeKind::PatternCall {
        return false;
    }
    let callee = ast.fixed_children(node)[0];
    if denoted_newtype(r, ast, callee).is_none() {
        return false;
    }
    let subpatterns = ast.multi_children(node);
    subpatterns.len() == 1 && is_irrefutable_inner(r, ast, subpatterns[0])
}

fn is_irrefutable_inner(r: &Resolver<'_>, ast: &Ast, node: NodeIndex) -> bool {
    if node.is_null() {
        return false;
    }
    match ast.node(node).kind {
        NodeKind::PatternCall => is_irrefutable_pattern(r, ast, node),
        NodeKind::PatternTuple => ast
            .multi_children(node)
            .iter()
            .all(|&child| is_irrefutable_inner(r, ast, child)),
        NodeKind::PatternTyped => is_irrefutable_inner(r, ast, ast.fixed_children(node)[0]),
        NodeKind::PatternExtendedCall => {
            ast.multi_children(node)
                .iter()
                .all(|&entry| match ast.node(entry).kind {
                    NodeKind::Id => true,
                    NodeKind::Property | NodeKind::PropertyPattern => {
                        is_irrefutable_inner(r, ast, ast.fixed_children(entry)[1])
                    }
                    NodeKind::PatternRestBind => {
                        let inner = ast.fixed_children(entry)[0];
                        inner.is_null() || ast.node(inner).kind == NodeKind::Underscore
                    }
                    _ => false,
                })
        }
        _ => crate::structs::is_irrefutable_subpattern(ast, node),
    }
}

pub(crate) fn resolve_pattern(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    expected: Option<TypeIndex>,
    irrefutable_only: bool,
    resolve_child: fn(&mut Resolver<'_>, &Ast, NodeIndex, Option<TypeIndex>),
) -> bool {
    if node.is_null() || ast.node(node).kind != NodeKind::PatternCall {
        return false;
    }
    let callee = ast.fixed_children(node)[0];
    let Some((canon_ty, _)) = denoted_newtype(r, ast, callee) else {
        return false;
    };
    let Some(inner) = inner_type(&r.type_pool, canon_ty) else {
        return false;
    };

    crate::typing::resolve_types(r, ast, callee);

    let expected_canon = expected.and_then(|exp| r.type_pool.canonical_type(exp));
    if irrefutable_only {
        if let Some(exp) = expected_canon
            && exp != canon_ty
        {
            let nt_name = crate::typing::type_display_name(&r.type_pool, canon_ty);
            report(
                r,
                ast,
                node,
                format!("newtype destructuring requires a statically known `{nt_name}` value"),
            );
        }
        let subpatterns = ast.multi_children(node);
        if subpatterns.len() == 1 && !is_irrefutable_inner(r, ast, subpatterns[0]) {
            report(
                r,
                ast,
                subpatterns[0],
                "refutable subpattern is not allowed in an irrefutable newtype binding",
            );
        }
    } else if let Some(exp) = expected_canon {
        let is_dynamic = r.type_pool.as_intrinsic(exp) == Some(Intrinsic::Any)
            || matches!(r.type_pool.get(exp).kind, TypeKind::Trait { .. });
        if exp != canon_ty && !is_dynamic {
            let nt_name = crate::typing::type_display_name(&r.type_pool, canon_ty);
            let found_name = crate::typing::type_display_name(&r.type_pool, exp);
            report(
                r,
                ast,
                node,
                format!("newtype pattern `{nt_name}` does not match expected type `{found_name}`"),
            );
        }
    }

    r.node_types.insert(node, canon_ty);
    r.newtype_patterns.insert(node, canon_ty);

    let subpatterns = ast.multi_children(node);
    if subpatterns.len() != 1 {
        let nt_name = crate::typing::type_display_name(&r.type_pool, canon_ty);
        report(
            r,
            ast,
            node,
            format!(
                "newtype pattern `{nt_name}` expects 1 subpattern, found {}",
                subpatterns.len()
            ),
        );
    }
    for (idx, &sub) in subpatterns.iter().enumerate() {
        let child_expected = (idx == 0).then_some(inner);
        resolve_child(r, ast, sub, child_expected);
    }
    true
}

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: impl Into<String>) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}
