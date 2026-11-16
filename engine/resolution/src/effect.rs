//! Phase 3c: Effect Resolution
//!
//! After name resolution has registered every `EffectDef` / `AsyncEffectDef`
//! symbol, this phase collects effect metadata (parameters, return type) and
//! stores it in [`EffectInfo`] entries on the resolver.

use std::collections::HashSet;

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::resolver::Resolver;
use crate::{EffectInfo, EffectOperation, SymbolKind};

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

/// Unwrap `async eff(args)` (`PatternAsync`) in an effect handler arm.
pub(crate) fn unwrap_handler_pattern(ast: &Ast, pattern: NodeIndex) -> (NodeIndex, bool) {
    if !pattern.is_null() && ast.node(pattern).kind == NodeKind::PatternAsync {
        (ast.fixed_children(pattern)[0], true)
    } else {
        (pattern, false)
    }
}

/// Collect [`EffectInfo`] entries from all effect symbols found by Phase 3a.
pub(crate) fn resolve_effects(r: &mut Resolver, ast: &Ast) {
    // Gather (name, type_index, def_node, is_async) from all Effect symbols.
    let effect_defs: Vec<_> = r
        .scopes
        .iter()
        .flat_map(|scope| scope.bindings.values())
        .filter_map(|&sym_id| {
            let sym = &r.symbols[sym_id.0 as usize];
            if sym.kind == SymbolKind::Effect {
                // Determine async-ness from the definition node kind.
                let is_async = ast.node(sym.def_node).kind == NodeKind::AsyncEffectDef;
                Some((sym.name, sym.type_index, sym.def_node, is_async))
            } else {
                None
            }
        })
        .collect();

    for (name, type_index, def_node, is_async) in effect_defs {
        // EffectDef / AsyncEffectDef layout:
        //   fixed: [0] name  [1] ret_type
        //   multi: params (ParamTyped / ParamOptional / etc.)
        //
        // Effects do NOT have body members — their multi_children are the
        // *parameters* of the effect, not operations.  The effect itself is a
        // single "callable" with those params and a return type.

        let children = ast.fixed_children(def_node);
        let return_type = crate::typing::resolve_type_expr(r, ast, children[1])
            .unwrap_or(Intrinsic::Unit.type_index());

        let mut params = Vec::new();
        let mut continuation_param = None;
        for (position, &param_node) in ast.multi_children(def_node).iter().enumerate() {
            if ast.node(param_node).kind == NodeKind::ParamCatch {
                let span = ast.node(param_node).span;
                if is_async {
                    r.diag_ctx
                        .error(
                            "`async effect` cannot capture a synchronous continuation with `catch`; async handlers execute in child tasks and return values via `Future`"
                                .into(),
                        )
                        .with_primary_span(span)
                        .emit(r.diag_ctx);
                }
                if continuation_param.replace(position).is_some() {
                    r.diag_ctx
                        .error("an effect can capture only one continuation".into())
                        .with_primary_span(span)
                        .emit(r.diag_ctx);
                }
                let children = ast.fixed_children(param_node);
                let continuation_type = Intrinsic::Continuation.type_index();
                if !children[1].is_null()
                    && crate::typing::resolve_type_expr(r, ast, children[1])
                        .and_then(|ty| r.type_pool.as_intrinsic(ty))
                        != Some(Intrinsic::Continuation)
                {
                    r.diag_ctx
                        .error("a `catch` parameter must have type `Continuation`".into())
                        .with_primary_span(span)
                        .emit(r.diag_ctx);
                }
                if let Some(&symbol) = r.node_symbols.get(&children[0]) {
                    r.symbol_mut(symbol).type_index = continuation_type;
                }
                r.node_types.insert(param_node, continuation_type);
                continue;
            }
            // Complete all effect headers before checking any default's calls.
            crate::typing::resolve_param_annotation(r, ast, param_node);
            params.push(
                crate::typing::param_type_index(r, ast, param_node)
                    .unwrap_or(Intrinsic::Any.type_index()),
            );
        }

        let caller_parameters = crate::arguments::caller_parameters(ast, def_node);
        crate::arguments::validate_variadic_parameters(r, ast, def_node, &caller_parameters);

        if params.len() > 31 {
            r.diag_ctx
                .error("effect calls currently support at most 31 arguments".into())
                .with_primary_span(ast.node(def_node).span)
                .emit(r.diag_ctx);
        }
        if continuation_param.is_some_and(|position| position >= 32) {
            r.diag_ctx
                .error("a catch binding must be within the first 32 effect parameters".into())
                .with_primary_span(ast.node(def_node).span)
                .emit(r.diag_ctx);
        }

        r.type_pool.get_mut(type_index).kind = TypeKind::Effect {
            params: params.clone(),
            ret: return_type,
            is_async,
        };
        if is_async {
            r.type_pool.intern_future(return_type);
        }

        // The effect is modelled as a single operation bearing the effect's
        // own name, with the collected parameter types and return type.
        let op = EffectOperation {
            name,
            param_types: params,
            return_type,
            continuation_param,
        };

        r.effects.push(EffectInfo {
            name,
            type_index,
            operations: vec![op],
            is_async,
        });
    }
}

/// Check declaration defaults after function bodies have refined return types.
/// Keeping this separate from header collection also checks unused effects.
pub(crate) fn resolve_defaults(r: &mut Resolver<'_>, ast: &Ast) {
    let defaults: Vec<_> = ast
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| matches!(node.kind, NodeKind::EffectDef | NodeKind::AsyncEffectDef))
        .flat_map(|(index, _)| ast.multi_children(NodeIndex(index as u32)))
        .copied()
        .filter(|&parameter| ast.node(parameter).kind == NodeKind::ParamOptional)
        .collect();
    for parameter in defaults {
        crate::typing::resolve_param_types(r, ast, parameter);
    }
}

/// Bind handler parameters to the declared effect signature before body inference.
pub(crate) fn resolve_handler_parameters(r: &mut Resolver, ast: &Ast, node: ast::NodeIndex) {
    if node.is_null() {
        return;
    }
    if ast.node(node).kind == NodeKind::EffectElimination {
        for &arm in ast.multi_children(node) {
            let (pattern, _) = unwrap_handler_pattern(ast, ast.fixed_children(arm)[0]);
            if ast.node(pattern).kind != NodeKind::PatternCall {
                continue;
            }
            let callee = ast.fixed_children(pattern)[0];
            let Some(&symbol) = r.node_symbols.get(&callee) else {
                continue;
            };
            let effect_type = r.symbols[symbol.0 as usize].type_index;
            let Some(operation) = r
                .effects
                .iter()
                .find(|effect| effect.type_index == effect_type)
                .and_then(|effect| effect.operations.first())
                .cloned()
            else {
                continue;
            };
            let mut argument = 0;
            for (index, &param) in ast.multi_children(pattern).iter().enumerate() {
                let ty = if operation.continuation_param == Some(index) {
                    Intrinsic::Continuation.type_index()
                } else {
                    let ty = operation
                        .param_types
                        .get(argument)
                        .copied()
                        .unwrap_or(Intrinsic::Any.type_index());
                    argument += 1;
                    ty
                };
                if ast.node(param).kind == NodeKind::Id
                    && crate::enums::descriptor(r, ast, param).is_none()
                    && let Some(&symbol) = r.node_symbols.get(&param)
                {
                    r.symbol_mut(symbol).type_index = ty;
                }
            }
        }
    }
    for &child in ast
        .fixed_children(node)
        .iter()
        .chain(ast.multi_children(node))
    {
        resolve_handler_parameters(r, ast, child);
    }
}

/// Type-check destructured or literal handler parameters once struct and enum layouts exist.
pub(crate) fn resolve_handler_pattern_types(r: &mut Resolver, ast: &Ast, node: ast::NodeIndex) {
    if node.is_null() {
        return;
    }
    if ast.node(node).kind == NodeKind::EffectElimination {
        for &arm in ast.multi_children(node) {
            let (pattern, _) = unwrap_handler_pattern(ast, ast.fixed_children(arm)[0]);
            if ast.node(pattern).kind != NodeKind::PatternCall {
                continue;
            }
            let callee = ast.fixed_children(pattern)[0];
            let Some(&symbol) = r.node_symbols.get(&callee) else {
                continue;
            };
            let effect_type = r.symbols[symbol.0 as usize].type_index;
            let Some(operation) = r
                .effects
                .iter()
                .find(|effect| effect.type_index == effect_type)
                .and_then(|effect| effect.operations.first())
                .cloned()
            else {
                continue;
            };
            let mut argument = 0;
            for (index, &param) in ast.multi_children(pattern).iter().enumerate() {
                if operation.continuation_param == Some(index) {
                    continue;
                }
                let ty = operation
                    .param_types
                    .get(argument)
                    .copied()
                    .unwrap_or(Intrinsic::Any.type_index());
                argument += 1;
                if matches!(ast.node(param).kind, NodeKind::Underscore) {
                    r.node_types.insert(param, ty);
                } else if ast.node(param).kind == NodeKind::Id
                    && crate::enums::descriptor(r, ast, param).is_none()
                {
                    r.node_types.insert(param, ty);
                    if let Some(&sym) = r.node_symbols.get(&param) {
                        r.symbol_mut(sym).type_index = ty;
                    }
                } else {
                    crate::enums::pattern(r, ast, param, Some(ty));
                }
            }
        }
    }
    for &child in ast
        .fixed_children(node)
        .iter()
        .chain(ast.multi_children(node))
    {
        resolve_handler_pattern_types(r, ast, child);
    }
}

/// Evaluate a static effect-set expression (`eff`, `[e1, e2]`, `[]`, `es1 ++ es2`, or `const` alias).
pub(crate) fn set(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) -> Option<Vec<TypeIndex>> {
    fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: impl Into<String>) {
        r.diag_ctx
            .error(message.into())
            .with_primary_span(ast.node(node).span)
            .emit(r.diag_ctx);
    }

    fn walk(
        r: &mut Resolver<'_>,
        ast: &Ast,
        node: NodeIndex,
        active: &mut HashSet<NodeIndex>,
    ) -> Option<Vec<TypeIndex>> {
        if active.len() >= 256 {
            report(
                r,
                ast,
                node,
                "effect set expression exceeds 256 nested expressions",
            );
            return None;
        }
        if node.is_null() || !active.insert(node) {
            report(r, ast, node, "cyclic or missing effect set expression");
            return None;
        }
        let result = match ast.node(node).kind {
            NodeKind::ListOf => {
                let mut effects = Vec::new();
                for &child in ast.multi_children(node) {
                    let members = walk(r, ast, child, active)?;
                    effects.extend(members);
                }
                Some(effects)
            }
            NodeKind::Concat => {
                let children = ast.fixed_children(node);
                let mut effects = walk(r, ast, children[0], active)?;
                let rhs = walk(r, ast, children[1], active)?;
                effects.extend(rhs);
                Some(effects)
            }
            NodeKind::Id | NodeKind::Projection | NodeKind::View | NodeKind::SelfUpper => {
                let sym_id = r.node_symbols.get(&node).copied().or_else(|| {
                    if ast.node(node).kind == NodeKind::Projection {
                        let ch = ast.fixed_children(node);
                        r.node_symbols.get(&ch[0]).and_then(|&base| {
                            crate::imports::member(r, base, ast.node(ch[1]).str_id).ok()
                        })
                    } else {
                        None
                    }
                });
                let definition = sym_id.map(|s| {
                    (
                        r.symbols[s.0 as usize].def_node,
                        r.symbols[s.0 as usize].scope,
                    )
                });
                if let Some((def_node, def_scope)) = definition
                    && !def_node.is_null()
                    && ast.node(def_node).kind == NodeKind::ConstDecl
                {
                    let saved = r.current_scope;
                    r.current_scope = def_scope;
                    let res = walk(r, ast, ast.fixed_children(def_node)[2], active);
                    r.current_scope = saved;
                    res
                } else if let Some(sym_id) = sym_id
                    && r.symbols[sym_id.0 as usize].kind == SymbolKind::Effect
                    && r.symbols[sym_id.0 as usize].type_index != TypeIndex::INVALID
                {
                    let eff_ty = r
                        .type_pool
                        .canonical_type(r.symbols[sym_id.0 as usize].type_index)?;
                    r.node_type_values.insert(node, eff_ty);
                    Some(vec![eff_ty])
                } else if let Some(ty) = crate::typing::resolve_type_expr_inner(r, ast, node) {
                    let canon = r.type_pool.canonical_type(ty)?;
                    r.node_type_values.insert(node, canon);
                    Some(vec![canon])
                } else {
                    report(
                        r,
                        ast,
                        node,
                        "effect set requires an effect path, `[e1, e2]` list, `++` union, or `const` alias",
                    );
                    None
                }
            }
            _ => {
                report(
                    r,
                    ast,
                    node,
                    "effect set supports only effect paths, lists, `const` aliases, and `++` union",
                );
                None
            }
        };
        active.remove(&node);
        result
    }

    walk(r, ast, node, &mut HashSet::new())
}

/// Normalize an effect-qualified type: flatten nested `EffectQualified`, canonicalize,
/// sort + deduplicate effect `TypeIndex`es, and return `inner` directly when empty (`#[] T == T`).
pub(crate) fn normalize(
    r: &mut Resolver<'_>,
    mut effects: Vec<TypeIndex>,
    mut inner: TypeIndex,
) -> TypeIndex {
    while let Some(canon_inner) = r.type_pool.canonical_type(inner) {
        if let TypeKind::EffectQualified {
            effects: nested,
            inner: next_inner,
        } = r.type_pool.get(canon_inner).kind.clone()
        {
            effects.extend(nested);
            inner = next_inner;
        } else {
            break;
        }
    }
    let mut normalized: Vec<TypeIndex> = effects
        .into_iter()
        .filter_map(|e| r.type_pool.canonical_type(e))
        .collect();
    normalized.sort_by_key(|e| e.as_u32());
    normalized.dedup();
    if normalized.is_empty() {
        inner
    } else {
        r.register_type(TypeKind::EffectQualified {
            effects: normalized,
            inner,
        })
    }
}
