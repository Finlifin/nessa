//! Lowering for `.use(...)` (`HandlerApplication`), `handles` block statements
//! (`HandlesStatement`), and `handles(eff) let` (`LetHandler`).

use std::collections::HashSet;

use ast::{NodeIndex, NodeKind};
use resolution::{HandlerBindingKind, ResolvedAst, SymbolKind};
use type_pool::{Intrinsic, TypeIndex};

use crate::arguments::snapshot;
use crate::builder::{CleanupAction, FunctionBuilder};
use crate::expr::{collect_free_vars, lower_closure, lower_expr};
use crate::{BlockId, NirExpr, NirLocal, NirParam, NirParamRole, NirStmt, NirValue, Terminator};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockExecMode {
    /// Function/lambda body block: tail statement returns from the function.
    TailReturn,
    /// Non-tail block (`if`/`while`/`for`/`defer` body): returns nothing.
    NonTail,
    /// Expression block (`{ s1; s2; tail }`): produces the tail value.
    ExprValue { block_node: NodeIndex },
}

fn ensure_local_preserving_proof(
    value: NirValue,
    builder: &mut FunctionBuilder,
    block: BlockId,
) -> NirLocal {
    if let NirValue::Local(_) = value {
        let snap = snapshot(value, builder, block);
        let NirValue::Local(local) = snap else {
            unreachable!("snapshot of a local produces a local");
        };
        local
    } else {
        let local = builder.alloc_local();
        builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::Assign(local, NirExpr::Use(value)));
        local
    }
}

fn build_capability_wrapper_closure(
    resolved: &ResolvedAst,
    site_node: NodeIndex,
    recv_local: NirLocal,
    binding: &HandlerBindingKind,
    builder: &mut FunctionBuilder,
    block: BlockId,
) -> (TypeIndex, NirValue, Option<u8>) {
    let HandlerBindingKind::CapabilityMethod {
        call_scope,
        effect_type,
        continuation_param,
        receiver_type,
        method_name,
        concrete_method,
        trait_view,
        param_types,
        return_type,
        ..
    } = binding
    else {
        unreachable!("capability wrapper requires CapabilityMethod");
    };

    let wrapper_id = builder.alloc_func_id();
    let mut wrapper =
        FunctionBuilder::new(wrapper_id, str_interner::intern("<capability-handler>"));
    wrapper.entry_scope = Some(call_scope.0);
    wrapper.body_facts = builder.body_facts.clone();
    wrapper.specialized_node_types = builder.specialized_node_types.clone();
    wrapper.specialized_symbol_types = builder.specialized_symbol_types.clone();
    wrapper.specialized_coercion_targets = builder.specialized_coercion_targets.clone();
    wrapper.self_type_values = builder.self_type_values.clone();
    wrapper.self_function_types = builder.self_function_types.clone();
    wrapper.self_function_parameters = builder.self_function_parameters.clone();
    wrapper.default_trait_view = builder.default_trait_view;
    wrapper.func_map = builder.func_map.clone();
    wrapper.derived_functions = builder.derived_functions.clone();
    wrapper.global_map = builder.global_map.clone();
    wrapper.native_adapters = builder.native_adapters.clone();
    wrapper.next_func_id = builder.next_func_id;
    wrapper.is_closure = true;
    wrapper.return_type = *return_type;

    let captured_recv = wrapper.alloc_local();
    wrapper.params.push(NirParam {
        local: captured_recv,
        name: str_interner::intern("<receiver>"),
        type_index: *receiver_type,
        role: NirParamRole::Capture,
    });
    let mut captures = vec![NirValue::Local(recv_local)];
    if let Some(&(parent_proof, view)) = builder.value_proofs.get(&recv_local) {
        let captured_proof = wrapper.alloc_local();
        wrapper.params.push(NirParam {
            local: captured_proof,
            name: str_interner::intern("<captured-proof>"),
            type_index: TypeIndex::INVALID,
            role: NirParamRole::CaptureProof { view },
        });
        wrapper
            .value_proofs
            .insert(captured_recv, (captured_proof, view));
        captures.push(NirValue::Local(parent_proof));
    } else {
        crate::trait_parameters::bind_local_type(
            resolved,
            captured_recv,
            *receiver_type,
            &mut wrapper,
        );
    }

    let mut user_locals = Vec::with_capacity(param_types.len());
    for (idx, &param_ty) in param_types.iter().enumerate() {
        let arg_local = wrapper.alloc_local();
        user_locals.push(NirValue::Local(arg_local));
        wrapper.params.push(NirParam {
            local: arg_local,
            name: str_interner::intern(&format!("<arg{idx}>")),
            type_index: param_ty,
            role: NirParamRole::User,
        });
    }
    crate::trait_parameters::prepare_parameters(resolved, &mut wrapper);

    let entry = wrapper.new_block();
    wrapper.entry_block = entry;
    let res_local = wrapper.alloc_local();

    if let Some(view) = *trait_view {
        let call_expr = crate::trait_dispatch::call(
            resolved,
            site_node,
            crate::trait_dispatch::TraitMethodTarget {
                owner: view,
                view,
                name: &str_interner::get(*method_name),
            },
            NirValue::Local(captured_recv),
            user_locals,
            &mut wrapper,
            entry,
        );
        wrapper.blocks[entry.0 as usize]
            .stmts
            .push(NirStmt::Assign(res_local, call_expr));
    } else if let Some(method_sym) = *concrete_method
        && let Some(&func_id) = wrapper.func_map.get(&method_sym)
    {
        let mut call_args = vec![NirValue::Local(captured_recv)];
        call_args.extend(user_locals);
        wrapper.blocks[entry.0 as usize].stmts.push(NirStmt::Assign(
            res_local,
            NirExpr::Call(func_id, call_args),
        ));
        crate::trait_parameters::finish_statements(resolved, site_node, &[0], &mut wrapper);
    } else {
        wrapper.blocks[entry.0 as usize].stmts.push(NirStmt::Assign(
            res_local,
            NirExpr::MethodCall(NirValue::Local(captured_recv), *method_name, user_locals)
                .in_source_scope(resolved, site_node),
        ));
    }

    if resolved.type_pool.as_intrinsic(*return_type) != Some(Intrinsic::Any) {
        let asserted = match wrapper.entry_scope {
            Some(scope) => {
                NirExpr::TypeAssert(NirValue::Local(res_local), *return_type).in_scope(scope)
            }
            None => NirExpr::TypeAssert(NirValue::Local(res_local), *return_type),
        };
        wrapper.blocks[entry.0 as usize]
            .stmts
            .push(NirStmt::Assign(res_local, asserted));
    }
    wrapper.blocks[entry.0 as usize].terminator = Terminator::Return(NirValue::Local(res_local));

    builder.next_func_id = wrapper.next_func_id;
    builder.lambda_functions.push(wrapper.build());

    let closure_local = builder.alloc_local();
    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
        closure_local,
        NirExpr::NewClosure(wrapper_id, captures),
    ));
    (
        *effect_type,
        NirValue::Local(closure_local),
        *continuation_param,
    )
}

pub(crate) fn lower_handler_application(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let ast = &resolved.ast;
    let children = ast.fixed_children(node);
    let expr_node = children[0];
    let handler_node = children[1];

    let Some(plan) = builder.handler_application(resolved, node) else {
        return lower_expr(resolved, expr_node, builder, block);
    };

    if plan.handlers.is_empty() {
        lower_expr(resolved, handler_node, builder, block);
        return lower_expr(resolved, expr_node, builder, block);
    }

    let mut prepared = Vec::with_capacity(plan.handlers.len());
    let shared_recv = if plan.handlers.iter().any(|h| {
        matches!(
            h,
            HandlerBindingKind::CapabilityMethod {
                receiver_expr: None,
                ..
            }
        )
    }) {
        let raw = lower_expr(resolved, handler_node, builder, block);
        Some(ensure_local_preserving_proof(raw, builder, *block))
    } else {
        None
    };

    let mut captures = false;
    for binding in &plan.handlers {
        let entry = match binding {
            HandlerBindingKind::Callable {
                effect_type,
                continuation_param,
            } => {
                let closure = lower_expr(resolved, handler_node, builder, block);
                (*effect_type, closure, *continuation_param)
            }
            HandlerBindingKind::CapabilityMethod { receiver_expr, .. } => {
                let recv_local = if let Some(recv_expr) = *receiver_expr {
                    let raw = lower_expr(resolved, recv_expr, builder, block);
                    ensure_local_preserving_proof(raw, builder, *block)
                } else {
                    shared_recv.expect("shared capability receiver evaluated above")
                };
                build_capability_wrapper_closure(
                    resolved, node, recv_local, binding, builder, *block,
                )
            }
        };
        captures |= entry.2.is_some();
        prepared.push(entry);
    }

    for &(effect, closure, continuation_param) in &prepared {
        builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::PushHandler {
                effect,
                closure,
                continuation_param,
            });
    }

    crate::stmt::finish_handler_scope_expr(
        resolved,
        crate::stmt::HandlerScopeSpec {
            scope_node: node,
            expr_node,
            count: prepared.len(),
            captures,
            answer_type: Some(plan.answer_type),
        },
        builder,
        block,
    )
}

fn bind_let_handler_pattern(
    resolved: &ResolvedAst,
    let_node: NodeIndex,
    pattern_node: NodeIndex,
    value: NirValue,
    builder: &mut FunctionBuilder,
    block: BlockId,
) {
    let ast = &resolved.ast;
    if matches!(
        ast.node(pattern_node).kind,
        NodeKind::PatternTuple
            | NodeKind::PatternExtendedCall
            | NodeKind::PatternTyped
            | NodeKind::PatternCall
    ) {
        crate::tuples::bind_pattern(resolved, pattern_node, value, builder, block);
        return;
    }
    if let Some(global) = builder
        .node_symbol(resolved, pattern_node)
        .and_then(|symbol| builder.global_map.get(&symbol))
        .copied()
    {
        builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::StoreGlobal(global, value).in_source_scope(resolved, let_node));
        return;
    }
    if let Some(sym_id) = builder.node_symbol(resolved, pattern_node) {
        let local = builder.local_for_symbol(sym_id);
        crate::trait_parameters::bind_local_type(
            resolved,
            local,
            builder.symbol_type(resolved, sym_id),
            builder,
        );
        crate::trait_parameters::copy_proof(local, value, builder, block);
        builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::Assign(local, NirExpr::Use(value)));
    }
}

fn build_delimited_rest_closure(
    resolved: &ResolvedAst,
    rest_stmts: &[NodeIndex],
    mode: BlockExecMode,
    return_type: TypeIndex,
    parent_builder: &mut FunctionBuilder,
    parent_block: BlockId,
) -> NirValue {
    let ast = &resolved.ast;
    let mut free_vars: Vec<(resolution::SymbolId, NirLocal)> = Vec::new();
    for &stmt in rest_stmts {
        collect_free_vars(resolved, ast, stmt, parent_builder, &mut free_vars);
    }
    let mut seen = HashSet::new();
    free_vars.retain(|(sym, _)| seen.insert(*sym));
    free_vars.retain(|(sym, _)| {
        matches!(
            resolved.symbols[sym.0 as usize].kind,
            SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::Constant
        )
    });

    let func_id = parent_builder.alloc_func_id();
    let mut delim_builder =
        FunctionBuilder::new(func_id, str_interner::intern("<delimited-block>"));
    delim_builder.entry_scope = rest_stmts
        .first()
        .and_then(|n| resolved.node_scopes.get(n).map(|s| s.0))
        .or(parent_builder.entry_scope);
    delim_builder.body_facts = parent_builder.body_facts.clone();
    delim_builder.specialized_node_types = parent_builder.specialized_node_types.clone();
    delim_builder.specialized_symbol_types = parent_builder.specialized_symbol_types.clone();
    delim_builder.specialized_coercion_targets =
        parent_builder.specialized_coercion_targets.clone();
    delim_builder.self_type_values = parent_builder.self_type_values.clone();
    delim_builder.self_function_types = parent_builder.self_function_types.clone();
    delim_builder.self_function_parameters = parent_builder.self_function_parameters.clone();
    delim_builder.default_trait_view = parent_builder.default_trait_view;
    delim_builder.func_map = parent_builder.func_map.clone();
    delim_builder.derived_functions = parent_builder.derived_functions.clone();
    delim_builder.global_map = parent_builder.global_map.clone();
    delim_builder.native_adapters = parent_builder.native_adapters.clone();
    delim_builder.next_func_id = parent_builder.next_func_id;
    delim_builder.is_closure = true;
    delim_builder.return_type = return_type;

    let mut captures = Vec::new();
    for (sym_id, parent_local) in &free_vars {
        let local = delim_builder.alloc_local();
        delim_builder.symbol_to_local.insert(*sym_id, local);
        delim_builder.params.push(NirParam {
            local,
            name: resolved.symbols[sym_id.0 as usize].name,
            type_index: parent_builder.symbol_type(resolved, *sym_id),
            role: NirParamRole::Capture,
        });
        captures.push(NirValue::Local(*parent_local));
        if let Some(&(parent_proof, view)) = parent_builder.value_proofs.get(parent_local) {
            let proof = delim_builder.alloc_local();
            delim_builder.params.push(NirParam {
                local: proof,
                name: str_interner::intern("<captured-proof>"),
                type_index: TypeIndex::INVALID,
                role: NirParamRole::CaptureProof { view },
            });
            delim_builder.value_proofs.insert(local, (proof, view));
            captures.push(NirValue::Local(parent_proof));
        } else {
            crate::trait_parameters::bind_local_type(
                resolved,
                local,
                parent_builder.symbol_type(resolved, *sym_id),
                &mut delim_builder,
            );
        }
    }

    let mut entry = delim_builder.new_block();
    delim_builder.entry_block = entry;
    let val = lower_block_statements(resolved, rest_stmts, mode, &mut delim_builder, &mut entry);
    if matches!(
        delim_builder.blocks[entry.0 as usize].terminator,
        Terminator::Unreachable
    ) {
        let ret_val = match mode {
            BlockExecMode::ExprValue { .. } => val,
            BlockExecMode::TailReturn | BlockExecMode::NonTail => NirValue::Unit,
        };
        crate::stmt::emit_return(resolved, ret_val, &mut delim_builder, &mut entry);
    }

    parent_builder.next_func_id = delim_builder.next_func_id;
    let nested = std::mem::take(&mut delim_builder.lambda_functions);
    parent_builder.lambda_functions.push(delim_builder.build());
    parent_builder.lambda_functions.extend(nested);

    let closure_local = parent_builder.alloc_local();
    parent_builder.blocks[parent_block.0 as usize]
        .stmts
        .push(NirStmt::Assign(
            closure_local,
            NirExpr::NewClosure(func_id, captures),
        ));
    NirValue::Local(closure_local)
}

fn unwrap_stmt_modifiers(ast: &ast::Ast, mut node: NodeIndex) -> NodeIndex {
    while !node.is_null()
        && matches!(
            ast.node(node).kind,
            NodeKind::PubDef | NodeKind::PrivateDef | NodeKind::GlobalDecl
        )
    {
        node = ast.fixed_children(node)[0];
    }
    node
}

/// Shared statement-list lowering for `Block` in all 3 execution modes,
/// supporting scoped `HandlesStatement` and `LetHandler` (both non-capturing and capturing).
pub(crate) fn lower_block_statements(
    resolved: &ResolvedAst,
    stmts: &[NodeIndex],
    mode: BlockExecMode,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let ast = &resolved.ast;
    builder.defer_scopes.push(Vec::new());
    let mut last_val = NirValue::Unit;
    let last_idx = stmts.len().saturating_sub(1);

    for (i, &raw_stmt) in stmts.iter().enumerate() {
        if !matches!(
            builder.blocks[block.0 as usize].terminator,
            Terminator::Unreachable
        ) {
            break;
        }
        let stmt = unwrap_stmt_modifiers(ast, raw_stmt);
        let kind = ast.node(stmt).kind;

        if kind == NodeKind::HandlesStatement
            && let Some(plan) = builder.handles_statement(resolved, stmt)
        {
            let children = ast.fixed_children(stmt);
            let (closure, _) = lower_closure(
                resolved,
                children[2],
                ast.multi_children(stmt),
                plan.fresh_continuation,
                Some(plan.answer_type),
                builder,
                block,
            );
            builder.blocks[block.0 as usize]
                .stmts
                .push(NirStmt::PushHandler {
                    effect: plan.effect_type,
                    closure,
                    continuation_param: plan.continuation_param,
                });
            if plan.continuation_param.is_some() {
                let rest = &stmts[i + 1..];
                let delim_ret = match mode {
                    BlockExecMode::TailReturn => builder.return_type,
                    BlockExecMode::NonTail => Intrinsic::Unit.type_index(),
                    BlockExecMode::ExprValue { block_node } => builder
                        .node_type(resolved, block_node)
                        .unwrap_or(Intrinsic::Unit.type_index()),
                };
                let val = if rest.is_empty() {
                    NirValue::Unit
                } else {
                    let body = build_delimited_rest_closure(
                        resolved, rest, mode, delim_ret, builder, *block,
                    );
                    let local = builder.alloc_local();
                    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                        local,
                        NirExpr::DelimitedCall {
                            body,
                            handler_count: 1,
                        },
                    ));
                    NirValue::Local(local)
                };
                builder.blocks[block.0 as usize]
                    .stmts
                    .push(NirStmt::AwaitPopHandler);
                match mode {
                    BlockExecMode::TailReturn => {
                        if matches!(
                            builder.blocks[block.0 as usize].terminator,
                            Terminator::Unreachable
                        ) {
                            crate::stmt::emit_normal_return(resolved, stmt, val, builder, block);
                        }
                        let _ = builder.defer_scopes.pop();
                        return NirValue::Unit;
                    }
                    BlockExecMode::NonTail => {
                        last_val = NirValue::Unit;
                        break;
                    }
                    BlockExecMode::ExprValue { .. } => {
                        last_val = val;
                        break;
                    }
                }
            } else {
                if let Some(scope) = builder.defer_scopes.last_mut() {
                    scope.push(CleanupAction::PopHandler);
                }
                if mode == BlockExecMode::TailReturn && i == last_idx {
                    if matches!(
                        builder.blocks[block.0 as usize].terminator,
                        Terminator::Unreachable
                    ) {
                        crate::stmt::emit_normal_return(
                            resolved,
                            stmt,
                            NirValue::Unit,
                            builder,
                            block,
                        );
                    }
                    let _ = builder.defer_scopes.pop();
                    return NirValue::Unit;
                }
                continue;
            }
        }

        if kind == NodeKind::LetHandler {
            let children = ast.fixed_children(stmt);
            // [0] effect_node  [1] pattern  [2] type_ann  [3] value
            let raw_val = lower_expr(resolved, children[3], builder, block);
            let recv_local = ensure_local_preserving_proof(raw_val, builder, *block);
            bind_let_handler_pattern(
                resolved,
                stmt,
                children[1],
                NirValue::Local(recv_local),
                builder,
                *block,
            );

            if let Some(plan) = builder.let_handler(resolved, stmt)
                && !plan.handlers.is_empty()
            {
                let mut prepared = Vec::with_capacity(plan.handlers.len());
                let mut captures = false;
                for binding in &plan.handlers {
                    let entry = build_capability_wrapper_closure(
                        resolved, stmt, recv_local, binding, builder, *block,
                    );
                    captures |= entry.2.is_some();
                    prepared.push(entry);
                }
                for &(effect, closure, continuation_param) in &prepared {
                    builder.blocks[block.0 as usize]
                        .stmts
                        .push(NirStmt::PushHandler {
                            effect,
                            closure,
                            continuation_param,
                        });
                }
                if captures {
                    let rest = &stmts[i + 1..];
                    let delim_ret = match mode {
                        BlockExecMode::TailReturn => builder.return_type,
                        BlockExecMode::NonTail => Intrinsic::Unit.type_index(),
                        BlockExecMode::ExprValue { block_node } => builder
                            .node_type(resolved, block_node)
                            .unwrap_or(Intrinsic::Unit.type_index()),
                    };
                    let val = if rest.is_empty() {
                        NirValue::Unit
                    } else {
                        let body = build_delimited_rest_closure(
                            resolved, rest, mode, delim_ret, builder, *block,
                        );
                        let local = builder.alloc_local();
                        builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                            local,
                            NirExpr::DelimitedCall {
                                body,
                                handler_count: prepared.len() as u8,
                            },
                        ));
                        NirValue::Local(local)
                    };
                    for _ in &prepared {
                        builder.blocks[block.0 as usize]
                            .stmts
                            .push(NirStmt::AwaitPopHandler);
                    }
                    match mode {
                        BlockExecMode::TailReturn => {
                            if matches!(
                                builder.blocks[block.0 as usize].terminator,
                                Terminator::Unreachable
                            ) {
                                crate::stmt::emit_normal_return(
                                    resolved, stmt, val, builder, block,
                                );
                            }
                            let _ = builder.defer_scopes.pop();
                            return NirValue::Unit;
                        }
                        BlockExecMode::NonTail => {
                            last_val = NirValue::Unit;
                            break;
                        }
                        BlockExecMode::ExprValue { .. } => {
                            last_val = val;
                            break;
                        }
                    }
                } else {
                    if let Some(scope) = builder.defer_scopes.last_mut() {
                        for _ in &prepared {
                            scope.push(CleanupAction::PopHandler);
                        }
                    }
                    if mode == BlockExecMode::TailReturn && i == last_idx {
                        if matches!(
                            builder.blocks[block.0 as usize].terminator,
                            Terminator::Unreachable
                        ) {
                            crate::stmt::emit_normal_return(
                                resolved,
                                stmt,
                                NirValue::Unit,
                                builder,
                                block,
                            );
                        }
                        let _ = builder.defer_scopes.pop();
                        return NirValue::Unit;
                    }
                    continue;
                }
            }
            continue;
        }

        match mode {
            BlockExecMode::TailReturn => {
                crate::stmt::lower_stmt(resolved, raw_stmt, builder, block, i == last_idx);
            }
            BlockExecMode::NonTail => {
                crate::stmt::lower_stmt(resolved, raw_stmt, builder, block, false);
            }
            BlockExecMode::ExprValue { .. } => {
                if i == last_idx
                    && !matches!(
                        kind,
                        NodeKind::LetDecl
                            | NodeKind::ConstDecl
                            | NodeKind::VarDecl
                            | NodeKind::LetHandler
                            | NodeKind::HandlesStatement
                            | NodeKind::DeferStatement
                            | NodeKind::ReturnStatement
                            | NodeKind::ResumeStatement
                            | NodeKind::BreakStatement
                            | NodeKind::ContinueStatement
                    )
                {
                    last_val = lower_expr(resolved, raw_stmt, builder, block);
                } else {
                    crate::stmt::lower_stmt(resolved, raw_stmt, builder, block, false);
                }
            }
        }
    }

    match mode {
        BlockExecMode::TailReturn => {
            if matches!(
                builder.blocks[block.0 as usize].terminator,
                Terminator::Unreachable
            ) {
                let scope_defers = builder.defer_scopes.pop().unwrap_or_default();
                crate::stmt::emit_scope_defers(resolved, &scope_defers, builder, block);
                if matches!(
                    builder.blocks[block.0 as usize].terminator,
                    Terminator::Unreachable
                ) {
                    crate::stmt::emit_normal_return(
                        resolved,
                        NodeIndex::NULL,
                        NirValue::Unit,
                        builder,
                        block,
                    );
                }
            } else {
                let _ = builder.defer_scopes.pop();
            }
        }
        BlockExecMode::NonTail => {
            let scope_defers = builder.defer_scopes.pop().unwrap_or_default();
            if matches!(
                builder.blocks[block.0 as usize].terminator,
                Terminator::Unreachable
            ) && !scope_defers.is_empty()
            {
                crate::stmt::emit_scope_defers(resolved, &scope_defers, builder, block);
            }
        }
        BlockExecMode::ExprValue { block_node } => {
            let scope_defers = builder.defer_scopes.pop().unwrap_or_default();
            last_val = crate::stmt::emit_scope_defers_for_value(
                resolved,
                block_node,
                last_val,
                builder.node_type(resolved, block_node),
                &scope_defers,
                builder,
                block,
            );
        }
    }
    last_val
}
