//! Capability methods (`handles(eff)`), `.use()` handler application,
//! `handles` block statements (`HandlesStatement`), and `handles(eff) let` (`LetHandler`).

use std::collections::HashSet;

use ast::{Ast, NodeIndex, NodeKind};
use str_interner::StrId;
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::EffectOperation;
use crate::resolver::Resolver;
use crate::{ScopeId, SymbolId, SymbolKind, Visibility};

#[derive(Debug, Clone)]
pub struct CapabilityDecl {
    pub owner_type: TypeIndex,
    pub trait_type: Option<TypeIndex>,
    pub owner_scope: ScopeId,
    pub visible_scope: Option<ScopeId>,
    pub method_name: StrId,
    pub method_symbol: SymbolId,
    pub def_node: NodeIndex,
    pub effect_type: TypeIndex,
    pub continuation_param: Option<u8>,
    pub param_types: Vec<TypeIndex>,
    pub return_type: TypeIndex,
}

#[derive(Debug, Clone)]
pub enum HandlerBindingKind {
    Callable {
        effect_type: TypeIndex,
        continuation_param: Option<u8>,
    },
    CapabilityMethod {
        call_scope: ScopeId,
        effect_type: TypeIndex,
        continuation_param: Option<u8>,
        receiver_type: TypeIndex,
        receiver_expr: Option<NodeIndex>,
        method_name: StrId,
        concrete_method: Option<SymbolId>,
        trait_view: Option<TypeIndex>,
        param_types: Vec<TypeIndex>,
        return_type: TypeIndex,
    },
}

#[derive(Debug, Clone)]
pub struct HandlerApplicationPlan {
    pub handlers: Vec<HandlerBindingKind>,
    pub answer_type: TypeIndex,
}

#[derive(Debug, Clone)]
pub struct HandlesStmtPlan {
    pub effect_type: TypeIndex,
    pub continuation_param: Option<u8>,
    pub fresh_continuation: Option<SymbolId>,
    pub answer_type: TypeIndex,
}

#[derive(Debug, Clone)]
pub struct LetHandlerPlan {
    pub receiver_type: TypeIndex,
    pub handlers: Vec<HandlerBindingKind>,
}

pub(crate) fn validate_lambda_params(r: &Resolver<'_>, ast: &Ast, params: &[NodeIndex]) {
    for &param in params {
        if !param.is_null() && ast.node(param).kind == NodeKind::ParamCatch {
            report(
                r,
                ast,
                param,
                "continuation capture parameter `catch` is not allowed in lambdas",
            );
        }
    }
}

fn effect_display_name(r: &Resolver<'_>, eff_ty: TypeIndex) -> String {
    let canon = r.type_pool.canonical_type(eff_ty).unwrap_or(eff_ty);
    if let Some(info) = r.effects.iter().find(|e| e.type_index == canon) {
        return str_interner::get(info.name).to_string();
    }
    crate::typing::type_display_name(&r.type_pool, eff_ty)
}

pub(crate) fn resolve_signature_capability(r: &mut Resolver<'_>, ast: &Ast, node_idx: NodeIndex) {
    let children = ast.fixed_children(node_idx);
    // FunctionDef / TraitDeriveFn: [0] name  [1] ret_type  [2] body  [3] capability
    // TraitDefFn:                  [0] name  [1] ret_type  [2] capability
    let cap_idx = if ast.node(node_idx).kind == NodeKind::TraitDefFn {
        2
    } else {
        3
    };
    let params = ast.multi_children(node_idx);
    if children.len() <= cap_idx || children[cap_idx].is_null() {
        for &param in params {
            if !param.is_null() && ast.node(param).kind == NodeKind::ParamCatch {
                report(
                    r,
                    ast,
                    param,
                    "continuation capture parameter `catch` is only allowed in effect declarations, `handles` statements, and `handles(...)` capability methods",
                );
            }
        }
        return;
    }

    let cap_node = children[cap_idx];
    r.handler_arms.insert(node_idx);

    let fn_scope = r
        .node_scopes
        .get(&node_idx)
        .copied()
        .unwrap_or(r.current_scope);
    let Some((assoc_scope, owner_ty, trait_type, visible_scope)) =
        enclosing_capability_owner(r, ast, fn_scope)
    else {
        report(
            r,
            ast,
            cap_node,
            "capability `handles(...)` requires an associated method inside a type or trait scope",
        );
        return;
    };

    if params
        .first()
        .is_none_or(|&p| p.is_null() || ast.node(p).kind != NodeKind::ParamSelf)
    {
        report(
            r,
            ast,
            node_idx,
            "capability method requires `self` as its first parameter",
        );
        return;
    }

    let Some((eff_ty, eff_op)) = resolve_single_effect(r, ast, cap_node) else {
        return;
    };

    let non_self = &params[1..];
    for &param in non_self {
        if !param.is_null()
            && matches!(
                ast.node(param).kind,
                NodeKind::ParamOptional | NodeKind::ParamVarargs
            )
        {
            report(
                r,
                ast,
                param,
                "capability method cannot declare optional or variadic parameters",
            );
            return;
        }
    }

    let expected_non_self =
        eff_op.param_types.len() + usize::from(eff_op.continuation_param.is_some());
    let catch_positions: Vec<usize> = non_self
        .iter()
        .enumerate()
        .filter_map(|(i, &p)| {
            (!p.is_null() && ast.node(p).kind == NodeKind::ParamCatch).then_some(i)
        })
        .collect();
    if catch_positions.len() > 1 {
        report(
            r,
            ast,
            non_self[catch_positions[1]],
            "capability method may declare at most one `catch` continuation parameter",
        );
    }
    if let Some(&bad_pos) = catch_positions.first() {
        match eff_op.continuation_param {
            None => {
                let eff_name = effect_display_name(r, eff_ty);
                report(
                    r,
                    ast,
                    non_self[bad_pos],
                    format!(
                        "effect `{eff_name}` does not declare a `catch` continuation parameter"
                    ),
                );
            }
            Some(expected_pos) if bad_pos != expected_pos => {
                report(
                    r,
                    ast,
                    non_self[bad_pos],
                    format!(
                        "`catch` continuation parameter must appear at parameter position {} to match effect signature",
                        expected_pos + 1
                    ),
                );
            }
            _ => {}
        }
    }

    if non_self.len() != expected_non_self {
        let eff_name = effect_display_name(r, eff_ty);
        report(
            r,
            ast,
            node_idx,
            format!(
                "capability method parameter count does not match effect `{eff_name}`: expected {expected_non_self} non-self parameter(s), found {}",
                non_self.len()
            ),
        );
    }

    let mut resolved_non_self = Vec::with_capacity(non_self.len());
    for (idx, &param) in non_self.iter().enumerate() {
        let is_cont = eff_op.continuation_param == Some(idx);
        let expected_param_ty = if is_cont {
            Intrinsic::Continuation.type_index()
        } else {
            let caller_idx = match eff_op.continuation_param {
                Some(cp) if idx > cp => idx - 1,
                _ => idx,
            };
            eff_op
                .param_types
                .get(caller_idx)
                .copied()
                .unwrap_or(Intrinsic::Any.type_index())
        };
        bind_and_check_handler_param(
            r,
            ast,
            param,
            expected_param_ty,
            is_cont,
            eff_op.return_type,
        );
        resolved_non_self.push(expected_param_ty);
    }

    let explicit_ret = crate::typing::resolve_type_expr(r, ast, children[1]);
    let captures = eff_op.continuation_param.is_some();
    let final_ret = if !captures {
        if let Some(ann_ret) = explicit_ret {
            let payload_ret = strip_effect_qualified(&r.type_pool, ann_ret);
            if r.type_pool.canonical_type(payload_ret)
                != r.type_pool.canonical_type(eff_op.return_type)
                && !r
                    .type_pool
                    .is_gradually_consistent(payload_ret, eff_op.return_type)
            {
                let exp_name = crate::typing::type_display_name(&r.type_pool, eff_op.return_type);
                let found_name = crate::typing::type_display_name(&r.type_pool, payload_ret);
                report(
                    r,
                    ast,
                    children[1],
                    format!(
                        "capability method return type mismatch: expected `{exp_name}` for handled effect, found `{found_name}`"
                    ),
                );
            }
            ann_ret
        } else {
            eff_op.return_type
        }
    } else {
        explicit_ret.unwrap_or(Intrinsic::Any.type_index())
    };

    let self_ty = crate::typing::param_type_index(r, ast, params[0]).unwrap_or(owner_ty);
    let mut full_params = Vec::with_capacity(1 + resolved_non_self.len());
    full_params.push(self_ty);
    full_params.extend(resolved_non_self.iter().copied());

    let name_node = children[0];
    let Some(&fn_sym) = r.node_symbols.get(&name_node) else {
        return;
    };
    let func_ti = r.register_type(TypeKind::Function {
        params: full_params,
        ret: final_ret,
    });
    r.symbols[fn_sym.0 as usize].type_index = func_ti;

    if r.default_body_context.is_none() {
        let method_name = r.symbols[fn_sym.0 as usize].name;
        let decl = CapabilityDecl {
            owner_type: owner_ty,
            trait_type,
            owner_scope: assoc_scope,
            visible_scope,
            method_name,
            method_symbol: fn_sym,
            def_node: node_idx,
            effect_type: eff_ty,
            continuation_param: eff_op.continuation_param.map(|p| p as u8),
            param_types: resolved_non_self,
            return_type: final_ret,
        };
        if let Some(existing) = r
            .capabilities
            .iter_mut()
            .find(|c| c.method_symbol == fn_sym)
        {
            *existing = decl;
        } else {
            r.capabilities.push(decl);
        }
    }
}

pub(crate) fn capability_expected_return(
    r: &Resolver<'_>,
    node_idx: NodeIndex,
) -> Option<TypeIndex> {
    let decl = r.capabilities.iter().find(|c| c.def_node == node_idx)?;
    if decl.continuation_param.is_none() {
        Some(strip_effect_qualified(&r.type_pool, decl.return_type))
    } else {
        (r.type_pool.as_intrinsic(decl.return_type) != Some(Intrinsic::Any))
            .then(|| strip_effect_qualified(&r.type_pool, decl.return_type))
    }
}

fn enclosing_capability_owner(
    r: &Resolver<'_>,
    ast: &Ast,
    mut scope: ScopeId,
) -> Option<(ScopeId, TypeIndex, Option<TypeIndex>, Option<ScopeId>)> {
    loop {
        let entry = &r.scopes[scope.0 as usize];
        if let Some(raw_owner) = entry.assoc_type
            && let Some(owner_ty) = r.type_pool.canonical_type(raw_owner)
        {
            let kind = if entry.node.is_null() {
                NodeKind::Invalid
            } else {
                ast.node(entry.node).kind
            };
            let trait_type = match kind {
                NodeKind::TraitDef => Some(owner_ty),
                NodeKind::ImplTraitDef | NodeKind::ExtendTraitDef => {
                    let tr_expr = ast.fixed_children(entry.node)[0];
                    denoted_trait_type(r, ast, tr_expr)
                }
                _ => None,
            };
            let visible_scope = match kind {
                NodeKind::ExtendDef | NodeKind::ExtendTraitDef => entry.parent,
                _ => None,
            };
            return Some((scope, owner_ty, trait_type, visible_scope));
        }
        scope = entry.parent?;
    }
}

fn denoted_trait_type(r: &Resolver<'_>, ast: &Ast, node: NodeIndex) -> Option<TypeIndex> {
    if node.is_null() {
        return None;
    }
    if let Some(&sym_id) = r.node_symbols.get(&node) {
        let sym = &r.symbols[sym_id.0 as usize];
        if sym.kind == SymbolKind::Trait && sym.type_index != TypeIndex::INVALID {
            return r.type_pool.canonical_type(sym.type_index);
        }
    }
    if ast.node(node).kind == NodeKind::Projection {
        let children = ast.fixed_children(node);
        let base_sym = *r.node_symbols.get(&children[0])?;
        if let Ok(member_id) = crate::imports::member(r, base_sym, ast.node(children[1]).str_id) {
            let sym = &r.symbols[member_id.0 as usize];
            if sym.kind == SymbolKind::Trait && sym.type_index != TypeIndex::INVALID {
                return r.type_pool.canonical_type(sym.type_index);
            }
        }
    }
    None
}

fn resolve_single_effect(
    r: &mut Resolver<'_>,
    ast: &Ast,
    eff_node: NodeIndex,
) -> Option<(TypeIndex, EffectOperation)> {
    crate::typing::resolve_types(r, ast, eff_node);
    let sym_id = match r.node_symbols.get(&eff_node).copied() {
        Some(sym) => Some(sym),
        None if ast.node(eff_node).kind == NodeKind::Projection => {
            let children = ast.fixed_children(eff_node);
            r.node_symbols.get(&children[0]).and_then(|&base| {
                crate::imports::member(r, base, ast.node(children[1]).str_id).ok()
            })
        }
        None => None,
    };
    let Some(sym_id) = sym_id else {
        report(r, ast, eff_node, "expected an effect declaration");
        return None;
    };
    let sym = &r.symbols[sym_id.0 as usize];
    if sym.kind != SymbolKind::Effect || sym.type_index == TypeIndex::INVALID {
        report(r, ast, eff_node, "expected an effect declaration");
        return None;
    }
    let Some(eff_ty) = r.type_pool.canonical_type(sym.type_index) else {
        report(r, ast, eff_node, "invalid effect type");
        return None;
    };
    let Some(info) = r.effects.iter().find(|e| e.type_index == eff_ty) else {
        report(r, ast, eff_node, "unknown effect signature");
        return None;
    };
    let Some(op) = info.operations.first().cloned() else {
        report(r, ast, eff_node, "effect has no operation signature");
        return None;
    };
    Some((eff_ty, op))
}

fn bind_and_check_handler_param(
    r: &mut Resolver<'_>,
    ast: &Ast,
    param: NodeIndex,
    expected_ty: TypeIndex,
    is_continuation: bool,
    effect_return_ty: TypeIndex,
) -> Option<SymbolId> {
    if param.is_null() {
        return None;
    }
    let cont_ty = Intrinsic::Continuation.type_index();
    match ast.node(param).kind {
        NodeKind::ParamCatch => {
            let children = ast.fixed_children(param);
            if !children[1].is_null()
                && let Some(ann_ty) = crate::typing::resolve_type_expr(r, ast, children[1])
                && r.type_pool.as_intrinsic(ann_ty) != Some(Intrinsic::Continuation)
            {
                report(
                    r,
                    ast,
                    children[1],
                    "captured continuation parameter must have type `Continuation`",
                );
            }
            r.node_types.insert(param, cont_ty);
            if !children[0].is_null() {
                r.node_types.insert(children[0], cont_ty);
                if let Some(&sym) = r.node_symbols.get(&children[0]) {
                    r.symbols[sym.0 as usize].type_index = cont_ty;
                    if is_continuation && !r.assigned_symbols.contains(&sym) {
                        r.continuation_inputs.insert(sym, effect_return_ty);
                    }
                    return Some(sym);
                }
            }
            None
        }
        NodeKind::ParamTyped | NodeKind::ParamLambda => {
            let children = ast.fixed_children(param);
            let (pat_node, type_node) = if children[1].is_null()
                && !children[0].is_null()
                && ast.node(children[0]).kind == NodeKind::PatternTyped
            {
                let inner = ast.fixed_children(children[0]);
                r.annotation_typed_patterns.insert(children[0]);
                (inner[0], inner[1])
            } else {
                (children[0], children[1])
            };
            let annotated = crate::typing::resolve_type_expr(r, ast, type_node);
            if let Some(ann_ty) = annotated
                && r.type_pool.canonical_type(ann_ty) != r.type_pool.canonical_type(expected_ty)
                && !r.type_pool.is_gradually_consistent(ann_ty, expected_ty)
            {
                let exp_name = crate::typing::type_display_name(&r.type_pool, expected_ty);
                let found_name = crate::typing::type_display_name(&r.type_pool, ann_ty);
                report(
                    r,
                    ast,
                    type_node,
                    format!(
                        "handler parameter type mismatch: expected `{exp_name}`, found `{found_name}`"
                    ),
                );
            }
            let effective = annotated.unwrap_or(expected_ty);
            r.node_types.insert(param, effective);
            if !pat_node.is_null() {
                r.node_types.insert(pat_node, effective);
                if let Some(&sym) = r.node_symbols.get(&pat_node) {
                    r.symbols[sym.0 as usize].type_index = effective;
                    if is_continuation && !r.assigned_symbols.contains(&sym) {
                        r.continuation_inputs.insert(sym, effect_return_ty);
                    }
                    return Some(sym);
                }
                if !matches!(ast.node(pat_node).kind, NodeKind::Id | NodeKind::Underscore) {
                    crate::typing::resolve_irrefutable_pattern(r, ast, pat_node, effective);
                }
            }
            None
        }
        NodeKind::Id => {
            r.node_types.insert(param, expected_ty);
            if let Some(&sym) = r.node_symbols.get(&param) {
                r.symbols[sym.0 as usize].type_index = expected_ty;
                if is_continuation && !r.assigned_symbols.contains(&sym) {
                    r.continuation_inputs.insert(sym, effect_return_ty);
                }
                return Some(sym);
            }
            None
        }
        NodeKind::Underscore => {
            r.node_types.insert(param, expected_ty);
            None
        }
        _ => {
            r.node_types.insert(param, expected_ty);
            crate::enums::pattern(r, ast, param, Some(expected_ty));
            None
        }
    }
}

pub(crate) fn strip_effect_qualified(pool: &type_pool::TypePool, ty: TypeIndex) -> TypeIndex {
    if ty == TypeIndex::INVALID {
        return ty;
    }
    let Some(canon) = pool.canonical_type(ty) else {
        return ty;
    };
    match pool.get(canon).kind {
        TypeKind::EffectQualified { inner, .. } => inner,
        _ => ty,
    }
}

pub(crate) fn decompose_effects(
    pool: &type_pool::TypePool,
    ty: TypeIndex,
) -> (Vec<TypeIndex>, TypeIndex) {
    if ty == TypeIndex::INVALID {
        return (Vec::new(), ty);
    }
    let Some(canon) = pool.canonical_type(ty) else {
        return (Vec::new(), ty);
    };
    match &pool.get(canon).kind {
        TypeKind::EffectQualified { effects, inner } => (effects.clone(), *inner),
        _ => (Vec::new(), ty),
    }
}

pub(crate) fn subtract_effects(
    r: &mut Resolver<'_>,
    ty: TypeIndex,
    handled: &[TypeIndex],
) -> TypeIndex {
    if ty == TypeIndex::INVALID || handled.is_empty() {
        return ty;
    }
    let (effects, inner) = decompose_effects(&r.type_pool, ty);
    if effects.is_empty() {
        return ty;
    }
    let remaining: Vec<TypeIndex> = effects
        .into_iter()
        .filter(|eff| {
            let eff_canon = r.type_pool.canonical_type(*eff).unwrap_or(*eff);
            !handled
                .iter()
                .any(|h| r.type_pool.canonical_type(*h).unwrap_or(*h) == eff_canon)
        })
        .collect();
    crate::effect::normalize(r, remaining, inner)
}

fn within_scope(r: &Resolver<'_>, ancestor: ScopeId, mut scope: ScopeId) -> bool {
    loop {
        if scope == ancestor {
            return true;
        }
        let Some(parent) = r.scopes[scope.0 as usize].parent else {
            return false;
        };
        scope = parent;
    }
}

fn decl_visible_from(r: &Resolver<'_>, decl: &CapabilityDecl, from: ScopeId) -> bool {
    if let Some(vis_scope) = decl.visible_scope
        && !within_scope(r, vis_scope, from)
    {
        return false;
    }
    if let Some(&ext_scope) = r.imports.extension_scopes.get(&decl.owner_scope)
        && !within_scope(r, ext_scope, from)
    {
        return false;
    }
    match crate::imports::binding_visibility(
        r,
        decl.owner_scope,
        decl.method_name,
        decl.method_symbol,
    ) {
        Visibility::Public => true,
        Visibility::Package => crate::imports::same_package(r, decl.owner_scope, from),
        Visibility::Private => {
            let target_ty = r.scopes[decl.owner_scope.0 as usize].assoc_type;
            let mut cur = Some(from);
            while let Some(s) = cur {
                if s == decl.owner_scope
                    || (target_ty.is_some() && r.scopes[s.0 as usize].assoc_type == target_ty)
                {
                    return true;
                }
                cur = r.scopes[s.0 as usize].parent;
            }
            false
        }
    }
}

/// Collect all visible capability declarations applicable to `receiver_ty` at `from`,
/// including capabilities declared on traits implemented by `receiver_ty`.
fn visible_capabilities_for_type(
    r: &Resolver<'_>,
    ast: &Ast,
    receiver_ty: TypeIndex,
    from: ScopeId,
) -> Vec<(CapabilityDecl, Option<TypeIndex>)> {
    let Some(recv_canon) = r.type_pool.canonical_type(receiver_ty) else {
        return Vec::new();
    };
    let is_trait_recv = matches!(r.type_pool.get(recv_canon).kind, TypeKind::Trait { .. });
    if is_trait_recv {
        let mut results = Vec::new();
        let mut pending = vec![recv_canon];
        let mut visited = HashSet::new();
        while let Some(tr) = pending.pop() {
            let Some(tr_canon) = r.type_pool.canonical_type(tr) else {
                continue;
            };
            if !visited.insert(tr_canon) {
                continue;
            }
            for decl in &r.capabilities {
                if decl.owner_type == tr_canon && decl_visible_from(r, decl, from) {
                    results.push((decl.clone(), Some(recv_canon)));
                }
            }
            if let TypeKind::Trait { parents, .. } = &r.type_pool.get(tr_canon).kind {
                pending.extend(parents.iter().copied());
            }
        }
        return results;
    }

    let mut results: Vec<(CapabilityDecl, Option<TypeIndex>)> = Vec::new();
    for decl in &r.capabilities {
        if decl.owner_type == recv_canon && decl_visible_from(r, decl, from) {
            results.push((decl.clone(), None));
        }
    }

    // Also inherit capability contracts from traits implemented by `recv_canon`
    // (via `impl Trait for T`, visible `extend Trait for T`, or `derive Trait for T`).
    let implemented_traits = implemented_traits_in_scope(r, ast, recv_canon, from);
    for tr_canon in implemented_traits {
        for decl in &r.capabilities {
            if decl.owner_type == tr_canon && decl_visible_from(r, decl, from) {
                let already_covered = results.iter().any(|(existing, _)| {
                    existing.method_name == decl.method_name
                        && existing.effect_type == decl.effect_type
                        && (existing.trait_type == Some(tr_canon) || existing.trait_type.is_none())
                });
                if !already_covered {
                    results.push((decl.clone(), None));
                }
            }
        }
    }
    results
}

fn implemented_traits_in_scope(
    r: &Resolver<'_>,
    ast: &Ast,
    recv_canon: TypeIndex,
    from: ScopeId,
) -> Vec<TypeIndex> {
    let mut traits = Vec::new();
    let mut visited = HashSet::new();

    for scope in &r.scopes {
        if scope.node.is_null()
            || scope.assoc_type.and_then(|t| r.type_pool.canonical_type(t)) != Some(recv_canon)
        {
            continue;
        }
        let kind = ast.node(scope.node).kind;
        if !matches!(kind, NodeKind::ImplTraitDef | NodeKind::ExtendTraitDef) {
            continue;
        }
        if kind == NodeKind::ExtendTraitDef
            && let Some(parent) = scope.parent
            && !within_scope(r, parent, from)
        {
            continue;
        }
        if let Some(tr) = denoted_trait_type(r, ast, ast.fixed_children(scope.node)[0]) {
            collect_trait_and_parents(r, tr, &mut visited, &mut traits);
        }
    }

    for raw in 1..ast.nodes.len() {
        let idx = NodeIndex(raw as u32);
        if ast.node(idx).kind != NodeKind::DeriveDef {
            continue;
        }
        let target_expr = ast.fixed_children(idx)[0];
        let matches_recv = r.node_symbols.get(&target_expr).and_then(|&s| {
            r.type_pool
                .canonical_type(r.symbols[s.0 as usize].type_index)
        }) == Some(recv_canon);
        if !matches_recv {
            continue;
        }
        for &tr_expr in ast.multi_children(idx) {
            if let Some(tr) = denoted_trait_type(r, ast, tr_expr) {
                collect_trait_and_parents(r, tr, &mut visited, &mut traits);
            }
        }
    }
    traits
}

fn collect_trait_and_parents(
    r: &Resolver<'_>,
    tr: TypeIndex,
    visited: &mut HashSet<TypeIndex>,
    out: &mut Vec<TypeIndex>,
) {
    let Some(canon) = r.type_pool.canonical_type(tr) else {
        return;
    };
    if !visited.insert(canon) {
        return;
    }
    out.push(canon);
    if let TypeKind::Trait { parents, .. } = &r.type_pool.get(canon).kind {
        for &parent in parents {
            collect_trait_and_parents(r, parent, visited, out);
        }
    }
}

fn handler_params_for_operation(op: &EffectOperation) -> Vec<TypeIndex> {
    let total = op.param_types.len() + usize::from(op.continuation_param.is_some());
    let mut params = Vec::with_capacity(total);
    for idx in 0..total {
        if op.continuation_param == Some(idx) {
            params.push(Intrinsic::Continuation.type_index());
        } else {
            let caller_idx = match op.continuation_param {
                Some(cp) if idx > cp => idx - 1,
                _ => idx,
            };
            params.push(op.param_types[caller_idx]);
        }
    }
    params
}

pub(crate) fn resolve_handler_application(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    expected: Option<TypeIndex>,
) -> Option<TypeIndex> {
    let children = ast.fixed_children(node);
    let expr_node = children[0];
    let handler_node = children[1];
    let call_scope = r.node_scopes.get(&node).copied().unwrap_or(r.current_scope);

    crate::typing::resolve_types(r, ast, expr_node);
    let expr_ty = r
        .node_types
        .get(&expr_node)
        .copied()
        .unwrap_or(Intrinsic::Any.type_index());
    let (mut expr_effects, expr_inner) = decompose_effects(&r.type_pool, expr_ty);
    if expr_effects.is_empty() && !expr_node.is_null() && ast.node(expr_node).kind == NodeKind::Call
    {
        let callee = ast.fixed_children(expr_node)[0];
        if let Some(&sym_id) = r.node_symbols.get(&callee) {
            let sym = &r.symbols[sym_id.0 as usize];
            if sym.kind == SymbolKind::Effect && sym.type_index != TypeIndex::INVALID {
                expr_effects.push(sym.type_index);
            }
        }
    }

    if handler_node.is_null() {
        return Some(expr_inner);
    }

    // Case 1: `expr.use(recv.cap_method)` — bound capability method on a concrete or trait receiver.
    if ast.node(handler_node).kind == NodeKind::Projection
        && !r.node_symbols.contains_key(&handler_node)
    {
        let proj = ast.fixed_children(handler_node);
        let recv_node = proj[0];
        let member_node = proj[1];
        if !member_node.is_null() && ast.node(member_node).kind != NodeKind::Int {
            crate::typing::resolve_types(r, ast, recv_node);
            if let Some(recv_ty) = r.node_types.get(&recv_node).copied()
                && let Some(recv_canon) = r.type_pool.canonical_type(recv_ty)
            {
                let method_name = ast.node(member_node).str_id;
                let candidates = visible_capabilities_for_type(r, ast, recv_canon, call_scope);
                if let Some((decl, trait_view)) = candidates
                    .into_iter()
                    .find(|(d, _)| d.method_name == method_name)
                {
                    let fn_ty = r.register_type(TypeKind::Function {
                        params: decl.param_types.clone(),
                        ret: decl.return_type,
                    });
                    r.node_types.insert(handler_node, fn_ty);
                    let binding = HandlerBindingKind::CapabilityMethod {
                        call_scope,
                        effect_type: decl.effect_type,
                        continuation_param: decl.continuation_param,
                        receiver_type: recv_canon,
                        receiver_expr: Some(recv_node),
                        method_name: decl.method_name,
                        concrete_method: trait_view.is_none().then_some(decl.method_symbol),
                        trait_view,
                        param_types: decl.param_types.clone(),
                        return_type: decl.return_type,
                    };
                    let answer_inner = if decl.continuation_param.is_some()
                        && r.type_pool.as_intrinsic(decl.return_type) != Some(Intrinsic::Any)
                    {
                        crate::errors::join(
                            r,
                            expr_inner,
                            strip_effect_qualified(&r.type_pool, decl.return_type),
                        )
                    } else {
                        expr_inner
                    };
                    let result_ty = subtract_effects_with_inner(
                        r,
                        &expr_effects,
                        &[decl.effect_type],
                        answer_inner,
                    );
                    r.handler_applications.insert(
                        node,
                        HandlerApplicationPlan {
                            handlers: vec![binding],
                            answer_type: result_ty,
                        },
                    );
                    return Some(result_ty);
                }
            }
        }
    }

    // Case 2: `expr.use(|...| ...)` — literal closure handler.
    if matches!(
        ast.node(handler_node).kind,
        NodeKind::Lambda | NodeKind::CaseMap | NodeKind::CaseAlternative
    ) {
        if expr_effects.len() != 1 {
            let msg = if expr_effects.is_empty() {
                "cannot infer handled effect for closure in `.use()` because receiver has no `#effect` annotation"
            } else {
                "ambiguous closure handler in `.use()`: receiver has multiple effects; use a capability or `# { ... }`"
            };
            report(r, ast, handler_node, msg);
            crate::typing::resolve_types(r, ast, handler_node);
            return Some(expr_inner);
        }
        let eff_ty = r
            .type_pool
            .canonical_type(expr_effects[0])
            .unwrap_or(expr_effects[0]);
        let Some(eff_op) = r
            .effects
            .iter()
            .find(|e| e.type_index == eff_ty)
            .and_then(|e| e.operations.first().cloned())
        else {
            crate::typing::resolve_types(r, ast, handler_node);
            return Some(expr_inner);
        };
        let captures = eff_op.continuation_param.is_some();
        let expected_ret = if captures {
            expected
                .map(|ty| strip_effect_qualified(&r.type_pool, ty))
                .unwrap_or(expr_inner)
        } else {
            eff_op.return_type
        };
        let expected_fn = r.register_type(TypeKind::Function {
            params: handler_params_for_operation(&eff_op),
            ret: expected_ret,
        });
        crate::typing::resolve_types_expected(r, ast, handler_node, Some(expected_fn));
        crate::typing::check_expected_type(r, ast, handler_node, expected_fn, "`.use()` handler");
        let answer_inner = if captures
            && let Some(&h_ty) = r.node_types.get(&handler_node)
            && let Some(h_canon) = r.type_pool.canonical_type(h_ty)
            && let TypeKind::Function { ret, .. } = r.type_pool.get(h_canon).kind
        {
            crate::errors::join(r, expr_inner, strip_effect_qualified(&r.type_pool, ret))
        } else {
            expr_inner
        };
        let result_ty = subtract_effects_with_inner(r, &expr_effects, &[eff_ty], answer_inner);
        r.handler_applications.insert(
            node,
            HandlerApplicationPlan {
                handlers: vec![HandlerBindingKind::Callable {
                    effect_type: eff_ty,
                    continuation_param: eff_op.continuation_param.map(|p| p as u8),
                }],
                answer_type: result_ty,
            },
        );
        return Some(result_ty);
    }

    // Case 3: general expression — either a function/bound-method value or a capability instance.
    crate::typing::resolve_types(r, ast, handler_node);
    let Some(h_ty) = r.node_types.get(&handler_node).copied() else {
        return Some(expr_inner);
    };
    let Some(h_canon) = r.type_pool.canonical_type(h_ty) else {
        return Some(expr_inner);
    };

    if let TypeKind::Function { params, ret } = r.type_pool.get(h_canon).kind.clone() {
        // Check if this function value came from an `instance_methods` capability method.
        if let Some(&method_sym) = r.instance_methods.get(&handler_node)
            && let Some(decl) = r
                .capabilities
                .iter()
                .find(|c| c.method_symbol == method_sym)
                .cloned()
        {
            let answer_inner = if decl.continuation_param.is_some()
                && r.type_pool.as_intrinsic(ret) != Some(Intrinsic::Any)
            {
                crate::errors::join(r, expr_inner, strip_effect_qualified(&r.type_pool, ret))
            } else {
                expr_inner
            };
            let result_ty =
                subtract_effects_with_inner(r, &expr_effects, &[decl.effect_type], answer_inner);
            r.handler_applications.insert(
                node,
                HandlerApplicationPlan {
                    handlers: vec![HandlerBindingKind::Callable {
                        effect_type: decl.effect_type,
                        continuation_param: decl.continuation_param,
                    }],
                    answer_type: result_ty,
                },
            );
            return Some(result_ty);
        }

        if expr_effects.len() != 1 {
            let msg = if expr_effects.is_empty() {
                "cannot infer handled effect for function in `.use()` because receiver has no `#effect` annotation"
            } else {
                "ambiguous function handler in `.use()`: receiver has multiple effects; use a capability or `# { ... }`"
            };
            report(r, ast, handler_node, msg);
            return Some(expr_inner);
        }
        let eff_ty = r
            .type_pool
            .canonical_type(expr_effects[0])
            .unwrap_or(expr_effects[0]);
        if let Some(eff_op) = r
            .effects
            .iter()
            .find(|e| e.type_index == eff_ty)
            .and_then(|e| e.operations.first().cloned())
        {
            let captures = eff_op.continuation_param.is_some();
            let expected_ret = if captures { ret } else { eff_op.return_type };
            let expected_fn = r.register_type(TypeKind::Function {
                params: handler_params_for_operation(&eff_op),
                ret: expected_ret,
            });
            crate::typing::check_expected_type(
                r,
                ast,
                handler_node,
                expected_fn,
                "`.use()` handler",
            );
            let answer_inner =
                if captures && params.len() == handler_params_for_operation(&eff_op).len() {
                    crate::errors::join(r, expr_inner, strip_effect_qualified(&r.type_pool, ret))
                } else {
                    expr_inner
                };
            let result_ty = subtract_effects_with_inner(r, &expr_effects, &[eff_ty], answer_inner);
            r.handler_applications.insert(
                node,
                HandlerApplicationPlan {
                    handlers: vec![HandlerBindingKind::Callable {
                        effect_type: eff_ty,
                        continuation_param: eff_op.continuation_param.map(|p| p as u8),
                    }],
                    answer_type: result_ty,
                },
            );
            return Some(result_ty);
        }
        return Some(expr_inner);
    }

    // Capability instance (`Struct`, `Newtype`, `Enum`, `Trait`, etc.).
    let all_caps = visible_capabilities_for_type(r, ast, h_canon, call_scope);
    let selected: Vec<(CapabilityDecl, Option<TypeIndex>)> = if expr_effects.is_empty() {
        all_caps
    } else {
        all_caps
            .into_iter()
            .filter(|(decl, _)| {
                expr_effects
                    .iter()
                    .any(|&e| r.type_pool.canonical_type(e) == Some(decl.effect_type))
            })
            .collect()
    };

    if selected.is_empty() {
        let ty_name = crate::typing::type_display_name(&r.type_pool, h_ty);
        report(
            r,
            ast,
            handler_node,
            format!(
                "type `{ty_name}` has no visible `handles(...)` capability matching the expression's effects"
            ),
        );
        return Some(expr_inner);
    }

    // Ensure no two selected methods handle the same effect ambiguously.
    let mut seen_effects = HashSet::new();
    for (decl, _) in &selected {
        if !seen_effects.insert(decl.effect_type) {
            let ty_name = crate::typing::type_display_name(&r.type_pool, h_ty);
            let eff_name = effect_display_name(r, decl.effect_type);
            report(
                r,
                ast,
                handler_node,
                format!(
                    "ambiguous `handles({eff_name})` capability on `{ty_name}`: multiple methods handle `{eff_name}`"
                ),
            );
            return Some(expr_inner);
        }
    }

    let mut handled_effects = Vec::with_capacity(selected.len());
    let mut bindings = Vec::with_capacity(selected.len());
    let mut answer_inner = expr_inner;
    for (decl, trait_view) in selected {
        handled_effects.push(decl.effect_type);
        if decl.continuation_param.is_some()
            && r.type_pool.as_intrinsic(decl.return_type) != Some(Intrinsic::Any)
        {
            answer_inner = crate::errors::join(
                r,
                answer_inner,
                strip_effect_qualified(&r.type_pool, decl.return_type),
            );
        }
        bindings.push(HandlerBindingKind::CapabilityMethod {
            call_scope,
            effect_type: decl.effect_type,
            continuation_param: decl.continuation_param,
            receiver_type: h_canon,
            receiver_expr: None,
            method_name: decl.method_name,
            concrete_method: trait_view.is_none().then_some(decl.method_symbol),
            trait_view,
            param_types: decl.param_types,
            return_type: decl.return_type,
        });
    }
    let result_ty = subtract_effects_with_inner(r, &expr_effects, &handled_effects, answer_inner);
    r.handler_applications.insert(
        node,
        HandlerApplicationPlan {
            handlers: bindings,
            answer_type: result_ty,
        },
    );
    Some(result_ty)
}

fn subtract_effects_with_inner(
    r: &mut Resolver<'_>,
    expr_effects: &[TypeIndex],
    handled: &[TypeIndex],
    inner: TypeIndex,
) -> TypeIndex {
    let remaining: Vec<TypeIndex> = expr_effects
        .iter()
        .copied()
        .filter(|eff| {
            let eff_canon = r.type_pool.canonical_type(*eff).unwrap_or(*eff);
            !handled
                .iter()
                .any(|h| r.type_pool.canonical_type(*h).unwrap_or(*h) == eff_canon)
        })
        .collect();
    crate::effect::normalize(r, remaining, inner)
}

pub(crate) fn resolve_handles_statement(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    block_expected: Option<TypeIndex>,
) -> Option<TypeIndex> {
    let children = ast.fixed_children(node);
    // [0] effect_expr  [1] ret_type  [2] body   multi = params
    let params = ast.multi_children(node);
    let Some((eff_ty, eff_op)) = resolve_single_effect(r, ast, children[0]) else {
        if !children[2].is_null() {
            crate::typing::resolve_types(r, ast, children[2]);
        }
        return Some(Intrinsic::Unit.type_index());
    };

    let expected_param_count =
        eff_op.param_types.len() + usize::from(eff_op.continuation_param.is_some());
    let catch_positions: Vec<usize> = params
        .iter()
        .enumerate()
        .filter_map(|(i, &p)| {
            (!p.is_null() && ast.node(p).kind == NodeKind::ParamCatch).then_some(i)
        })
        .collect();
    if catch_positions.len() > 1 {
        report(
            r,
            ast,
            params[catch_positions[1]],
            "`handles` statement may declare at most one `catch` continuation parameter",
        );
    }
    if let Some(&bad_pos) = catch_positions.first() {
        match eff_op.continuation_param {
            None => {
                let eff_name = crate::typing::type_display_name(&r.type_pool, eff_ty);
                report(
                    r,
                    ast,
                    params[bad_pos],
                    format!(
                        "effect `{eff_name}` does not declare a `catch` continuation parameter"
                    ),
                );
            }
            Some(expected_pos) if bad_pos != expected_pos => {
                report(
                    r,
                    ast,
                    params[bad_pos],
                    format!(
                        "`catch` continuation parameter must appear at parameter position {} to match effect signature",
                        expected_pos + 1
                    ),
                );
            }
            _ => {}
        }
    }
    if params.len() != expected_param_count {
        let eff_name = crate::typing::type_display_name(&r.type_pool, eff_ty);
        report(
            r,
            ast,
            node,
            format!(
                "`handles` parameter count does not match effect `{eff_name}`: expected {expected_param_count}, found {}",
                params.len()
            ),
        );
    }

    let mut fresh_continuation = None;
    for (idx, &param) in params.iter().enumerate() {
        let is_cont = eff_op.continuation_param == Some(idx);
        let expected_param_ty = if is_cont {
            Intrinsic::Continuation.type_index()
        } else {
            let caller_idx = match eff_op.continuation_param {
                Some(cp) if idx > cp => idx - 1,
                _ => idx,
            };
            eff_op
                .param_types
                .get(caller_idx)
                .copied()
                .unwrap_or(Intrinsic::Any.type_index())
        };
        let sym = bind_and_check_handler_param(
            r,
            ast,
            param,
            expected_param_ty,
            is_cont,
            eff_op.return_type,
        );
        if is_cont {
            fresh_continuation = sym;
        }
    }

    let explicit_ret = crate::typing::resolve_type_expr(r, ast, children[1]);
    let captures = eff_op.continuation_param.is_some();
    let expected_return = if !captures {
        if let Some(ann_ret) = explicit_ret {
            let payload_ret = strip_effect_qualified(&r.type_pool, ann_ret);
            if r.type_pool.canonical_type(payload_ret)
                != r.type_pool.canonical_type(eff_op.return_type)
                && !r
                    .type_pool
                    .is_gradually_consistent(payload_ret, eff_op.return_type)
            {
                let exp_name = crate::typing::type_display_name(&r.type_pool, eff_op.return_type);
                let found_name = crate::typing::type_display_name(&r.type_pool, payload_ret);
                report(
                    r,
                    ast,
                    children[1],
                    format!(
                        "`handles` return type mismatch: expected `{exp_name}`, found `{found_name}`"
                    ),
                );
            }
        }
        Some(eff_op.return_type)
    } else {
        explicit_ret.or_else(|| block_expected.map(|ty| strip_effect_qualified(&r.type_pool, ty)))
    };

    let body = children[2];
    r.return_types.push(expected_return);
    r.inferred_returns.push(Vec::new());
    crate::typing::resolve_types_expected(r, ast, body, expected_return);
    r.return_types.pop();
    let returns = r
        .inferred_returns
        .pop()
        .expect("handles return context was pushed");

    let body_ty = r
        .node_types
        .get(&body)
        .copied()
        .unwrap_or(Intrinsic::Unit.type_index());
    let inferred = if returns.is_empty() {
        body_ty
    } else {
        let mut all = returns;
        all.push(body_ty);
        all.into_iter()
            .reduce(|a, b| crate::errors::join(r, a, b))
            .unwrap_or(body_ty)
    };
    if let Some(exp) = expected_return
        && let Some(&actual) = r.node_types.get(&body)
        && !crate::typing::body_definitely_returns(ast, body)
        && !crate::trait_typing::is_subtype(r, ast, body, actual, exp)
        && !r.type_pool.is_gradually_consistent(actual, exp)
    {
        let exp_name = crate::typing::type_display_name(&r.type_pool, exp);
        let found_name = crate::typing::type_display_name(&r.type_pool, actual);
        report(
            r,
            ast,
            body,
            format!("handler result type mismatch: expected `{exp_name}`, found `{found_name}`"),
        );
    }
    let handler_result = expected_return.unwrap_or(inferred);
    crate::errors::finish_callable(r, node, handler_result);
    r.node_types.insert(body, handler_result);

    r.handles_statements.insert(
        node,
        HandlesStmtPlan {
            effect_type: eff_ty,
            continuation_param: eff_op.continuation_param.map(|p| p as u8),
            fresh_continuation,
            answer_type: handler_result,
        },
    );
    Some(Intrinsic::Unit.type_index())
}

pub(crate) fn resolve_let_handler(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
) -> Option<TypeIndex> {
    if r.function_inference.values.contains(&node) {
        return Some(Intrinsic::Unit.type_index());
    }
    let children = ast.fixed_children(node);
    // [0] effect_node  [1] pattern  [2] type_ann  [3] value
    let call_scope = r.node_scopes.get(&node).copied().unwrap_or(r.current_scope);
    let effects = crate::effect::set(r, ast, children[0]).unwrap_or_default();

    let annotation_ti = crate::typing::resolve_type_expr(r, ast, children[2]);
    crate::typing::resolve_types_expected(r, ast, children[3], annotation_ti);
    let val_ti = r.node_types.get(&children[3]).copied();

    if let (Some(ann_ti), Some(v_ti)) = (annotation_ti, val_ti)
        && !crate::trait_typing::is_subtype(r, ast, children[3], v_ti, ann_ti)
        && !r.type_pool.is_gradually_consistent(v_ti, ann_ti)
    {
        let exp_name = crate::typing::type_display_name(&r.type_pool, ann_ti);
        let found_name = crate::typing::type_display_name(&r.type_pool, v_ti);
        report(
            r,
            ast,
            children[3],
            format!("type mismatch: expected `{exp_name}`, found `{found_name}`"),
        );
    }

    let binding_annotation = annotation_ti
        .and_then(|ty| crate::default_methods::parameter_body_type(r, ast, children[2], ty));
    let Some(effective_ti) = binding_annotation.or(val_ti) else {
        return Some(Intrinsic::Unit.type_index());
    };

    crate::typing::resolve_irrefutable_pattern(r, ast, children[1], effective_ti);

    let Some(recv_canon) = r.type_pool.canonical_type(effective_ti) else {
        return Some(Intrinsic::Unit.type_index());
    };

    let all_caps = visible_capabilities_for_type(r, ast, recv_canon, call_scope);
    let mut seen_target_effects = HashSet::new();
    let mut handlers = Vec::with_capacity(effects.len());

    for raw_eff in effects {
        let Some(eff_canon) = r.type_pool.canonical_type(raw_eff) else {
            continue;
        };
        if !seen_target_effects.insert(eff_canon) {
            continue;
        }
        let matches: Vec<_> = all_caps
            .iter()
            .filter(|(decl, _)| decl.effect_type == eff_canon)
            .collect();
        if matches.is_empty() {
            let ty_name = crate::typing::type_display_name(&r.type_pool, effective_ti);
            let eff_name = effect_display_name(r, eff_canon);
            report(
                r,
                ast,
                node,
                format!(
                    "type `{ty_name}` does not implement a visible `handles({eff_name})` capability"
                ),
            );
            continue;
        }
        if matches.len() > 1 {
            let ty_name = crate::typing::type_display_name(&r.type_pool, effective_ti);
            let eff_name = effect_display_name(r, eff_canon);
            report(
                r,
                ast,
                node,
                format!(
                    "ambiguous `handles({eff_name})` capability on `{ty_name}`: multiple visible methods handle `{eff_name}`"
                ),
            );
            continue;
        }
        let (decl, trait_view) = matches[0];
        handlers.push(HandlerBindingKind::CapabilityMethod {
            call_scope,
            effect_type: decl.effect_type,
            continuation_param: decl.continuation_param,
            receiver_type: recv_canon,
            receiver_expr: None,
            method_name: decl.method_name,
            concrete_method: trait_view.is_none().then_some(decl.method_symbol),
            trait_view: *trait_view,
            param_types: decl.param_types.clone(),
            return_type: decl.return_type,
        });
    }

    r.let_handlers.insert(
        node,
        LetHandlerPlan {
            receiver_type: recv_canon,
            handlers,
        },
    );
    Some(Intrinsic::Unit.type_index())
}

fn update_binding(
    r: &Resolver<'_>,
    ast: &Ast,
    default_methods: &[crate::DefaultMethodPlan],
    binding: &mut HandlerBindingKind,
) {
    let HandlerBindingKind::CapabilityMethod {
        call_scope,
        receiver_type,
        method_name,
        concrete_method,
        trait_view,
        ..
    } = binding
    else {
        return;
    };
    if trait_view.is_some() {
        return;
    }
    let Some(sym) = *concrete_method else {
        return;
    };
    let def_node = r.symbols[sym.0 as usize].def_node;
    if def_node.is_null()
        || !matches!(
            ast.node(def_node).kind,
            NodeKind::TraitDefFn | NodeKind::TraitDeriveFn
        )
    {
        return;
    }
    // 1. Check instantiated default method adapters (`derive fn` on `impl` or `derive`).
    if let Some(dm) = default_methods.iter().find(|dm| {
        r.type_pool.canonical_type(dm.implementor) == Some(*receiver_type)
            && dm.method_name == *method_name
            && dm
                .visible_scope
                .is_none_or(|s| within_scope(r, ScopeId(s), *call_scope))
    }) {
        *concrete_method = Some(dm.function);
        return;
    }
    // 2. Check explicit trait impl method on `receiver_type`.
    if let Some(trait_owner) = r.scopes[r.symbols[sym.0 as usize].scope.0 as usize]
        .assoc_type
        .and_then(|t| r.type_pool.canonical_type(t))
        && let Ok(Some(record)) =
            r.type_pool
                .find_trait_impl_scoped(*receiver_type, trait_owner, call_scope.0)
        && let Some(slot) = record.methods.iter().find(|m| m.name == *method_name)
        && slot.func_id != type_pool::DERIVE_FUNC_ID
    {
        *concrete_method = Some(SymbolId(slot.func_id));
        return;
    }
    // 3. Fallback to associated member lookup on `receiver_type`.
    if let Ok(found) = crate::associated::member_from(r, *receiver_type, *method_name, *call_scope)
    {
        *concrete_method = Some(found);
    }
}

/// Bind concrete method symbols (including trait impl overrides and instantiated `derive fn`
/// default methods) after Phase 3d trait resolution completes.
pub(crate) fn finalize(r: &mut Resolver<'_>, ast: &Ast) {
    let default_methods = r.default_methods.clone();
    let mut app_keys: Vec<NodeIndex> = r.handler_applications.keys().copied().collect();
    app_keys.sort_by_key(|n| n.0);
    for key in app_keys {
        if let Some(mut plan) = r.handler_applications.remove(&key) {
            for binding in &mut plan.handlers {
                update_binding(r, ast, &default_methods, binding);
            }
            r.handler_applications.insert(key, plan);
        }
    }

    let mut lh_keys: Vec<NodeIndex> = r.let_handlers.keys().copied().collect();
    lh_keys.sort_by_key(|n| n.0);
    for key in lh_keys {
        if let Some(mut plan) = r.let_handlers.remove(&key) {
            for binding in &mut plan.handlers {
                update_binding(r, ast, &default_methods, binding);
            }
            r.let_handlers.insert(key, plan);
        }
    }
}

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: impl Into<String>) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}
