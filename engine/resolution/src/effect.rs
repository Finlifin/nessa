//! Phase 3c: Effect Resolution
//!
//! After name resolution has registered every `EffectDef` / `AsyncEffectDef`
//! symbol, this phase collects effect metadata (parameters, return type) and
//! stores it in [`EffectInfo`] entries on the resolver.

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{Intrinsic, TypeKind};

use crate::resolver::Resolver;
use crate::{EffectInfo, EffectOperation, SymbolKind};

// ---------------------------------------------------------------------------
// Top-level entry point
// ---------------------------------------------------------------------------

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
            let pattern = ast.fixed_children(arm)[0];
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
                if let Some(&symbol) = r.node_symbols.get(&param) {
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
