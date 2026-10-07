//! Expression lowering — converts AST expression nodes to NIR values.

use ast::{Ast, NodeIndex, NodeKind};
use nsbc::FuncId;
use resolution::{ResolvedAst, SymbolKind};

use crate::builder::FunctionBuilder;
use crate::{BinOp, BlockId, NirExpr, NirLocal, NirParam, NirStmt, NirValue, Terminator, UnaryOp};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Lower an AST expression node into a [`NirValue`], emitting any necessary
/// statements into `block`.
pub(crate) fn lower_expr(
    resolved: &ResolvedAst,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let starts: Vec<_> = builder
        .blocks
        .iter()
        .map(|block| block.stmts.len())
        .collect();
    let value = lower_expr_uncoerced(resolved, node_idx, builder, block);
    let value = crate::errors::conversions(resolved, node_idx, value, builder, *block);
    crate::trait_parameters::finish_statements(resolved, node_idx, &starts, builder);
    if let Some(scope) = resolved.node_scopes.get(&node_idx) {
        for (index, block) in builder.blocks.iter_mut().enumerate() {
            for statement in block
                .stmts
                .iter_mut()
                .skip(starts.get(index).copied().unwrap_or(0))
            {
                if statement.requires_scope() {
                    let store = std::mem::replace(statement, NirStmt::Nop);
                    *statement = store.in_scope(scope.0);
                    continue;
                }
                let NirStmt::Assign(_, expression) = statement else {
                    continue;
                };
                if expression.requires_scope() {
                    let call = std::mem::replace(expression, NirExpr::Use(NirValue::Unit));
                    *expression = call.in_scope(scope.0);
                }
            }
        }
    }
    let Some(coercion) = builder.coercion(resolved, node_idx) else {
        return value;
    };
    let target = builder.coercion_target(node_idx, coercion.target);
    let local = builder.alloc_local();
    if let Some(view) = crate::trait_parameters::bare_trait(resolved, target) {
        let proof =
            crate::trait_parameters::proof_for(resolved, node_idx, value, view, builder, *block);
        let NirValue::Local(proof) = proof else {
            unreachable!("proof construction returns a local")
        };
        builder.value_proofs.insert(local, (proof, view));
        builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
            local,
            NirExpr::TraitAssert(value, NirValue::Local(proof), view)
                .in_source_scope(resolved, node_idx),
        ));
        return NirValue::Local(local);
    }
    let expression = match coercion.kind {
        resolution::CoercionKind::Assert => NirExpr::TypeAssert(value, target),
        resolution::CoercionKind::Convert => NirExpr::TypeCast(value, target),
    };
    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
        local,
        expression.in_source_scope(resolved, node_idx),
    ));
    NirValue::Local(local)
}

fn lower_expr_uncoerced(
    resolved: &ResolvedAst,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    if node_idx.is_null() {
        return NirValue::Unit;
    }
    let ast = &resolved.ast;
    let node = ast.node(node_idx);
    if let Some(ty) = builder
        .self_type_values
        .get(&node_idx)
        .copied()
        .or_else(|| builder.type_value(resolved, node_idx))
    {
        return NirValue::ConstType(ty);
    }
    match node.kind {
        // ── Literals ───────────────────────────────────────────────
        NodeKind::Int => crate::literal::lower_integer_as(
            resolved,
            node_idx,
            builder.node_type(resolved, node_idx),
        ),
        NodeKind::Real => lower_real_literal(node),
        NodeKind::Bool => lower_bool_literal(node),
        NodeKind::Str => lower_str_literal(node),
        NodeKind::FStringConcat => lower_fstring_concat(resolved, ast, node_idx, builder, block),
        NodeKind::Char => lower_char_literal(node),
        NodeKind::Null => NirValue::Null,
        NodeKind::Unit => NirValue::Unit,
        NodeKind::ExprStatement => {
            lower_expr(resolved, ast.fixed_children(node_idx)[0], builder, block)
        }

        // ── Identifier ─────────────────────────────────────────────
        NodeKind::Id | NodeKind::SelfLower | NodeKind::SelfUpper => {
            lower_id(resolved, node_idx, builder, block)
        }

        // ── Arithmetic ─────────────────────────────────────────────
        NodeKind::Add | NodeKind::Sub | NodeKind::Mul | NodeKind::Div | NodeKind::Mod => {
            lower_arith(resolved, ast, node_idx, node.kind, builder, block)
        }

        // ── String concat ──────────────────────────────────────────
        NodeKind::Concat => crate::concat::lower(resolved, node_idx, builder, block),

        // ── Comparison / Boolean ───────────────────────────────────
        NodeKind::BoolEq
        | NodeKind::BoolNotEq
        | NodeKind::BoolGt
        | NodeKind::BoolGtEq
        | NodeKind::BoolLt
        | NodeKind::BoolLtEq => {
            lower_comparison(resolved, ast, node_idx, node.kind, builder, block)
        }
        NodeKind::BoolAnd | NodeKind::BoolOr => {
            lower_logical(resolved, ast, node_idx, node.kind, builder, block)
        }

        // ── Unary ──────────────────────────────────────────────────
        NodeKind::Negative if ast.node(ast.fixed_children(node_idx)[0]).kind == NodeKind::Int => {
            crate::literal::lower_integer_as(
                resolved,
                node_idx,
                builder.node_type(resolved, node_idx),
            )
        }
        NodeKind::Negative => lower_unary(resolved, ast, node_idx, UnaryOp::Neg, builder, block),
        NodeKind::BoolNot => lower_unary(resolved, ast, node_idx, UnaryOp::Not, builder, block),

        NodeKind::TypeCast => {
            let children = ast.fixed_children(node_idx);
            let source = lower_expr(resolved, children[0], builder, block);
            // Successful resolution establishes the destination type. Driver
            // diagnostics prevent invalid casts from reaching NIR lowering.
            let specialized_self = builder.self_type_values.get(&children[1]).copied();
            let target = specialized_self.unwrap_or_else(|| {
                builder
                    .node_type(resolved, node_idx)
                    .expect("resolved cast type")
            });
            let local = builder.alloc_local();
            if let Some(view) = crate::trait_parameters::bare_trait(resolved, target) {
                let proof = crate::trait_parameters::proof_for(
                    resolved, node_idx, source, view, builder, *block,
                );
                let NirValue::Local(proof_local) = proof else {
                    unreachable!("proof construction returns a local")
                };
                builder.value_proofs.insert(local, (proof_local, view));
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    local,
                    NirExpr::TraitAssert(source, proof, view).in_source_scope(resolved, node_idx),
                ));
                return NirValue::Local(local);
            }
            builder.blocks[block.0 as usize]
                .stmts
                .push(NirStmt::Assign(local, NirExpr::TypeCast(source, target)));
            if specialized_self.is_some() {
                crate::trait_parameters::copy_proof(local, source, builder, *block);
            }
            NirValue::Local(local)
        }

        NodeKind::View
            if str_interner::get(ast.node(ast.fixed_children(node_idx)[1]).str_id) == "type" =>
        {
            let source = lower_expr(resolved, ast.fixed_children(node_idx)[0], builder, block);
            let local = builder.alloc_local();
            builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                local,
                NirExpr::CallBuiltin(runtime::ids::TYPE_OF, vec![source]),
            ));
            NirValue::Local(local)
        }
        NodeKind::View => lower_id(resolved, node_idx, builder, block),

        // ── Call ───────────────────────────────────────────────────
        NodeKind::Call => lower_call(resolved, ast, node_idx, builder, block),
        NodeKind::EffectPropagation => {
            lower_expr(resolved, ast.fixed_children(node_idx)[0], builder, block)
        }
        NodeKind::EffectElimination => {
            lower_effect_elimination(resolved, ast, node_idx, builder, block)
        }

        // ── Struct / enum construction: TypeName { field: val, ... }
        NodeKind::ExtendedCall => {
            if builder.call_plan(resolved, node_idx).is_some() {
                lower_call(resolved, ast, node_idx, builder, block)
            } else {
                crate::construction::lower_construction(resolved, node_idx, builder, block)
            }
        }

        // ── Projection (field access / enum variant construction) ──
        NodeKind::Projection => lower_projection(resolved, ast, node_idx, builder, block),

        // ── Block ──────────────────────────────────────────────────
        NodeKind::Block => lower_block_expr(resolved, ast, node_idx, builder, block),

        NodeKind::ListOf => crate::lists::lower_list(resolved, node_idx, builder, block),

        // ── Assignment ─────────────────────────────────────────────
        NodeKind::Assign => {
            let children = ast.fixed_children(node_idx);
            let lhs_node = ast.node(children[0]);

            if lhs_node.kind == NodeKind::Projection
                && let Some(index) = builder.field_index(resolved, children[0])
            {
                let receiver_node = ast.fixed_children(children[0])[0];
                let receiver = lower_expr(resolved, receiver_node, builder, block);
                let receiver = crate::arguments::snapshot(receiver, builder, *block);
                let value = lower_expr(resolved, children[1], builder, block);
                builder.blocks[block.0 as usize]
                    .stmts
                    .push(NirStmt::StoreField(receiver, index, value));
                return NirValue::Unit;
            }

            // obj(args) = val → obj.update(args..., val) desugaring.
            if lhs_node.kind == NodeKind::Call {
                let call_children = ast.fixed_children(children[0]);
                let callee = call_children[0];
                let call_args = ast.multi_children(children[0]);
                let obj = lower_expr(resolved, callee, builder, block);
                let obj = crate::arguments::snapshot(obj, builder, *block);
                if let Some(function) = builder.update_call(resolved, node_idx) {
                    let function = builder.func_map[&function];
                    let mut args = vec![obj];
                    args.extend(crate::arguments::lower_method_arguments(
                        resolved, node_idx, obj, builder, block,
                    ));
                    let result = builder.alloc_local();
                    builder.blocks[block.0 as usize]
                        .stmts
                        .push(NirStmt::Assign(result, NirExpr::Call(function, args)));
                    return NirValue::Unit;
                }
                let mut args: Vec<NirValue> = call_args
                    .iter()
                    .map(|&a| {
                        let value = lower_expr(resolved, a, builder, block);
                        crate::arguments::snapshot(value, builder, *block)
                    })
                    .collect();
                let val = lower_expr(resolved, children[1], builder, block);
                if crate::collections::store_index(
                    resolved, callee, obj, &args, val, builder, *block,
                ) {
                    return NirValue::Unit;
                }
                args.push(val);

                let update_str = str_interner::intern("update");
                let local = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    local,
                    NirExpr::MethodCall(obj, update_str, args),
                ));
                return NirValue::Unit;
            }

            let val = lower_expr(resolved, children[1], builder, block);
            // If LHS is a resolved local, assign to it.
            if let Some(global) = resolved_symbol_in(resolved, children[0], builder)
                .and_then(|symbol| builder.global_map.get(&symbol))
                .copied()
            {
                builder.blocks[block.0 as usize]
                    .stmts
                    .push(NirStmt::StoreGlobal(global, val));
            } else if let Some(local) = resolve_local(resolved, children[0], builder) {
                builder.blocks[block.0 as usize]
                    .stmts
                    .push(NirStmt::Assign(local, NirExpr::Use(val)));
            }
            NirValue::Unit
        }

        NodeKind::AddAssign
        | NodeKind::SubAssign
        | NodeKind::MulAssign
        | NodeKind::DivAssign
        | NodeKind::ModAssign => {
            let children = ast.fixed_children(node_idx);
            let kind = match node.kind {
                NodeKind::AddAssign => NodeKind::Add,
                NodeKind::SubAssign => NodeKind::Sub,
                NodeKind::MulAssign => NodeKind::Mul,
                NodeKind::DivAssign => NodeKind::Div,
                NodeKind::ModAssign => NodeKind::Mod,
                _ => unreachable!("compound assignment branch"),
            };
            let mut value = lower_arith(resolved, ast, node_idx, kind, builder, block);
            if let Some(target) = builder.node_type(resolved, children[0])
                && resolved.type_pool.as_intrinsic(target) != Some(type_pool::Intrinsic::Any)
            {
                let local = builder.alloc_local();
                builder.blocks[block.0 as usize]
                    .stmts
                    .push(NirStmt::Assign(local, NirExpr::TypeCast(value, target)));
                value = NirValue::Local(local);
            }
            if let Some(global) = resolved_symbol_in(resolved, children[0], builder)
                .and_then(|symbol| builder.global_map.get(&symbol))
                .copied()
            {
                builder.blocks[block.0 as usize]
                    .stmts
                    .push(NirStmt::StoreGlobal(global, value));
            } else if let Some(local) = resolve_local(resolved, children[0], builder) {
                builder.blocks[block.0 as usize]
                    .stmts
                    .push(NirStmt::Assign(local, NirExpr::Use(value)));
            }
            NirValue::Unit
        }

        NodeKind::ErrorConstruction => crate::errors::construct(resolved, node_idx, builder, block),
        NodeKind::ErrorPropagation => crate::errors::propagate(resolved, node_idx, builder, block),
        NodeKind::ErrorElimination => crate::errors::eliminate(resolved, node_idx, builder, block),
        NodeKind::OptionPropagation => {
            crate::optional::propagate(resolved, node_idx, builder, block)
        }
        NodeKind::IfStatement => lower_if_value(resolved, ast, node_idx, builder, block),

        // ── Match expression ────────────────────────────────────────
        NodeKind::PostMatch => crate::patterns::lower_match(resolved, node_idx, builder, block),
        NodeKind::BoolMatches => crate::patterns::lower_matches(resolved, node_idx, builder, block),

        // ── Lambda ─────────────────────────────────────────────────
        NodeKind::Lambda => lower_lambda(resolved, ast, node_idx, builder, block),

        // ── Named argument — lower value only ──────────────────────
        NodeKind::NamedArg => {
            let children = ast.fixed_children(node_idx);
            // children[0] = parameter name (Id) — skip
            // children[1] = value expression — lower
            lower_expr(resolved, children[1], builder, block)
        }

        NodeKind::Tuple => crate::tuples::lower_tuple(resolved, node_idx, builder, block),

        // ── Default: produce unit ──────────────────────────────────
        _ => NirValue::Unit,
    }
}

// ---------------------------------------------------------------------------
// Literal lowering
// ---------------------------------------------------------------------------

/// Check if an AST node has a meaningful str_id set by the parser.
/// Since the parser now interns all token text, leaf nodes always have valid str_ids.
fn has_str_id(_node: &ast::Node) -> bool {
    true
}

fn lower_real_literal(node: &ast::Node) -> NirValue {
    if has_str_id(node) {
        let text = str_interner::get(node.str_id);
        let val: f64 = text.parse().unwrap_or(0.0);
        NirValue::ConstFloat(val)
    } else {
        NirValue::ConstFloat(0.0)
    }
}

fn lower_bool_literal(node: &ast::Node) -> NirValue {
    if has_str_id(node) {
        let text = str_interner::get(node.str_id);
        NirValue::ConstBool(text == "true")
    } else {
        NirValue::ConstBool(true)
    }
}

fn lower_str_literal(node: &ast::Node) -> NirValue {
    if has_str_id(node) {
        NirValue::ConstStr(node.str_id)
    } else {
        NirValue::Unit
    }
}

/// Lower an f-string (FStringConcat) node.
///
/// Strategy:
///   - `Str` parts → lowered as string constants (already have type String)
///   - non-`Str` parts → call `to_string()` method on the value
///   - chain all parts with `StrConcat` intrinsic calls left-to-right
///   - if there are zero parts, produce an empty string constant
fn lower_fstring_concat(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    use runtime::ids;

    let parts = ast.multi_children(node_idx);
    if parts.is_empty() {
        // Empty string constant.
        let empty_id = str_interner::intern("\"\"");
        return NirValue::ConstStr(empty_id);
    }

    // Lower each part to a String-typed NirValue.
    let to_string_name = str_interner::intern("to_string");
    let mut string_vals: Vec<NirValue> = Vec::with_capacity(parts.len());

    for &part in parts {
        if part.is_null() {
            continue;
        }
        let part_node = ast.node(part);
        if part_node.kind == NodeKind::Str {
            // Literal segment — already a String constant.
            string_vals.push(lower_str_literal(part_node));
        } else {
            // Interpolated expression — lower then call to_string().
            let val = lower_expr(resolved, part, builder, block);
            let local = builder.alloc_local();
            builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                local,
                NirExpr::MethodCall(val, to_string_name, Vec::new()),
            ));
            string_vals.push(NirValue::Local(local));
        }
    }

    // Fold left with str_concat: result = concat(concat(a, b), c) ...
    let mut acc = string_vals[0];
    for part_val in string_vals.into_iter().skip(1) {
        let local = builder.alloc_local();
        builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
            local,
            NirExpr::CallBuiltin(ids::STR_CONCAT, vec![acc, part_val]),
        ));
        acc = NirValue::Local(local);
    }
    acc
}

fn lower_char_literal(node: &ast::Node) -> NirValue {
    // The lexer validates the scalar/escape and parser retains the complete
    // quoted token. Driver diagnostics prevent invalid tokens reaching NIR.
    NirValue::ConstChar(
        crate::characters::parse_literal(&str_interner::get(node.str_id))
            .expect("character AST nodes come from validated character tokens"),
    )
}

// ---------------------------------------------------------------------------
// Identifier lowering
// ---------------------------------------------------------------------------

fn lower_id(
    resolved: &ResolvedAst,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    if let Some(variant) = builder.enum_variant(resolved, node_idx) {
        return NirValue::ConstEnum(variant.type_index, variant.variant_tag);
    }
    if let Some(value) = load_global(resolved, node_idx, builder, block) {
        return value;
    }
    if let Some(symbol) = resolved_symbol_in(resolved, node_idx, builder)
        && let Some(function) = callable_function(resolved, node_idx, symbol, builder)
    {
        return create_function_closure(function, builder, block);
    }
    if let Some(local) = resolve_local(resolved, node_idx, builder) {
        NirValue::Local(local)
    } else {
        // Unresolved identifier — produce unit to avoid crashes.
        NirValue::Unit
    }
}

fn load_global(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> Option<NirValue> {
    let symbol = resolved_symbol_in(resolved, node, builder)?;
    let global = *builder.global_map.get(&symbol)?;
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(local, NirExpr::LoadGlobal(global)));
    Some(NirValue::Local(local))
}

/// Qualified namespace members retain the defining symbol, rather than becoming
/// instance method calls or fresh, uninitialized locals.
fn resolved_symbol_in(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &FunctionBuilder,
) -> Option<resolution::SymbolId> {
    if builder.body_facts.is_none() {
        return resolved_symbol(resolved, node);
    }
    builder.node_symbol(resolved, node).or_else(|| {
        (resolved.ast.node(node).kind == NodeKind::Projection)
            .then(|| {
                resolved
                    .ast
                    .fixed_children(node)
                    .get(1)
                    .and_then(|member| builder.node_symbol(resolved, *member))
            })
            .flatten()
    })
}

fn resolved_symbol(resolved: &ResolvedAst, node: NodeIndex) -> Option<resolution::SymbolId> {
    resolved.node_symbols.get(&node).copied().or_else(|| {
        if !node.is_null() && resolved.ast.node(node).kind == NodeKind::Projection {
            resolved
                .ast
                .fixed_children(node)
                .get(1)
                .and_then(|member| resolved.node_symbols.get(member))
                .copied()
        } else {
            None
        }
    })
}

fn callable_function(
    resolved: &ResolvedAst,
    node: NodeIndex,
    symbol: resolution::SymbolId,
    builder: &mut FunctionBuilder,
) -> Option<FuncId> {
    let definition = &resolved.symbols[symbol.0 as usize];
    if definition.kind == SymbolKind::Function {
        return builder.func_map.get(&symbol).copied();
    }
    let native = resolved.builtin_fns.get(&symbol).copied()?;
    let signature = declared_function_type(resolved, node, symbol, builder);
    // Resolution diagnoses unannotated native values before lowering. Direct
    // native calls do not require an adapter when no signature was declared.
    let signature = signature.expect("native function values require a resolved signature");
    if let Some(&function) = builder.native_adapters.get(&(native, signature)) {
        return Some(function);
    }
    let type_pool::TypeKind::Function { params, ret } = &resolved.type_pool.get(signature).kind
    else {
        unreachable!("signature was checked above");
    };
    let function = builder.alloc_func_id();
    let name = str_interner::intern(&format!("<native:{}>", str_interner::get(definition.name)));
    let mut adapter = FunctionBuilder::new(function, name);
    adapter.entry_scope = if definition.def_node.is_null() {
        resolved.node_scopes.get(&node).map(|scope| scope.0)
    } else {
        Some(definition.scope.0)
    };
    adapter.function_type = signature;
    adapter.return_type = *ret;
    adapter.is_closure = true;
    for (index, &parameter_type) in params.iter().enumerate() {
        let local = adapter.alloc_local();
        adapter.params.push(NirParam {
            local,
            name: str_interner::intern(&format!("arg{index}")),
            type_index: parameter_type,
            role: crate::NirParamRole::User,
        });
    }
    let entry = adapter.new_block();
    let args = adapter
        .params
        .iter()
        .map(|parameter| NirValue::Local(parameter.local))
        .collect();
    crate::trait_parameters::prepare_parameters(resolved, &mut adapter);
    let result = adapter.alloc_local();
    adapter.blocks[entry.0 as usize]
        .stmts
        .push(NirStmt::Assign(result, NirExpr::CallBuiltin(native, args)));
    if resolved.type_pool.as_intrinsic(*ret) != Some(type_pool::Intrinsic::Any) {
        adapter.blocks[entry.0 as usize].stmts.push(NirStmt::Assign(
            result,
            match adapter.entry_scope {
                Some(scope) => NirExpr::TypeAssert(NirValue::Local(result), *ret).in_scope(scope),
                None => NirExpr::TypeAssert(NirValue::Local(result), *ret),
            },
        ));
    }
    adapter.blocks[entry.0 as usize].terminator = Terminator::Return(NirValue::Local(result));
    builder.lambda_functions.push(adapter.build());
    builder
        .native_adapters
        .insert((native, signature), function);
    Some(function)
}

fn declared_function_type(
    resolved: &ResolvedAst,
    node: NodeIndex,
    symbol: resolution::SymbolId,
    builder: &FunctionBuilder,
) -> Option<type_pool::TypeIndex> {
    let ty = builder
        .node_type(resolved, node)
        .unwrap_or_else(|| builder.symbol_type(resolved, symbol));
    resolved.type_pool.canonical_type(ty).filter(|&ty| {
        matches!(
            resolved.type_pool.get(ty).kind,
            type_pool::TypeKind::Function { .. }
        )
    })
}

#[cfg(test)]
mod function_value_tests {
    use ast::{NodeIndex, NodeKind};
    use nsbc::FuncId;
    use resolution::ResolvedAst;
    use rustc_span::DUMMY_SP;
    use type_pool::{Intrinsic, TypeKind};

    use super::{lower_expr, resolved_symbol};
    use crate::builder::FunctionBuilder;
    use crate::{NirExpr, NirStmt, NirValue};

    fn native_reference() -> (ResolvedAst, NodeIndex, type_pool::TypeIndex) {
        let (mut resolved, _) = crate::literal::tests::resolved_integer("42", false, "i64");
        let signature = resolved.type_pool.intern_structural(TypeKind::Function {
            params: vec![Intrinsic::I64.type_index()],
            ret: Intrinsic::I64.type_index(),
        });
        let symbol = resolved
            .symbols
            .iter()
            .find(|symbol| {
                symbol.kind == resolution::SymbolKind::BuiltinFunction(runtime::ids::TO_I64)
            })
            .unwrap()
            .id;
        resolved.symbols[symbol.0 as usize].type_index = signature;
        let reference = resolved
            .ast
            .builder(NodeKind::Id, DUMMY_SP)
            .set_str_id(str_interner::intern("to_i64"))
            .build();
        resolved.node_symbols.insert(reference, symbol);
        resolved.node_types.insert(reference, signature);
        (resolved, reference, signature)
    }

    #[test]
    fn typed_native_values_create_checked_adapters_and_real_closures() {
        let (mut resolved, reference, signature) = native_reference();
        let native_symbol = resolved.node_symbols[&reference];
        resolved.symbols[native_symbol.0 as usize].def_node = reference;
        resolved.symbols[native_symbol.0 as usize].scope = resolution::ScopeId(13);
        resolved
            .node_scopes
            .insert(reference, resolution::ScopeId(7));
        let mut builder = FunctionBuilder::new(FuncId(0), str_interner::intern("main"));
        builder.next_func_id = 1;
        let mut block = builder.new_block();
        let value = lower_expr(&resolved, reference, &mut builder, &mut block);
        assert!(matches!(value, NirValue::Local(_)));
        assert!(
            matches!(builder.blocks[block.0 as usize].stmts.last(), Some(NirStmt::Assign(_, NirExpr::NewClosure(FuncId(1), captures))) if captures.is_empty())
        );
        let adapter = &builder.lambda_functions[0];
        assert_eq!(adapter.function_type, signature);
        assert_eq!(adapter.return_type, Intrinsic::I64.type_index());
        assert_eq!(adapter.params.len(), 1);
        assert_eq!(adapter.params[0].role, crate::NirParamRole::User);
        assert_eq!(adapter.entry_abi().capture_count(), 0);
        assert_eq!(adapter.entry_abi().logical_parameter_count(), 1);
        let parameter = adapter.params[0].local;
        assert_eq!(adapter.params[0].type_index, Intrinsic::I64.type_index());
        assert_eq!(
            adapter.entry_scope,
            Some(
                resolved.symbols[resolved.node_symbols[&reference].0 as usize]
                    .scope
                    .0
            )
        );
        assert!(
            matches!(adapter.blocks[0].stmts.last(),Some(NirStmt::Assign(_,NirExpr::ScopedCall {scope,..})) if Some(*scope)==adapter.entry_scope)
        );
        assert!(adapter.blocks[0].stmts.iter().any(|statement| matches!(statement, NirStmt::Assign(_, NirExpr::CallBuiltin(runtime::ids::TO_I64, args)) if args == &[NirValue::Local(parameter)])));
        assert!(
            matches!(adapter.blocks[0].stmts.last(), Some(NirStmt::Assign(_, expression)) if matches!(expression.unscoped(), NirExpr::TypeAssert(_,ty) if *ty == Intrinsic::I64.type_index()))
        );
        lower_expr(&resolved, reference, &mut builder, &mut block);
        assert_eq!(
            builder.lambda_functions.len(),
            1,
            "same signature shares adapter code"
        );
    }

    #[test]
    fn lambda_capture_layout_distinguishes_capture_and_explicit_parameters() {
        let (mut resolved, reference, _) = native_reference();
        let symbol = resolved.node_symbols[&reference];
        resolved.symbols[symbol.0 as usize].kind = resolution::SymbolKind::Variable;
        resolved.builtin_fns.remove(&symbol);
        resolved.symbols[symbol.0 as usize].type_index = Intrinsic::I64.type_index();
        resolved
            .node_types
            .insert(reference, Intrinsic::I64.type_index());
        let parameter = resolved.ast.builder(NodeKind::Underscore, DUMMY_SP).build();
        let mut builder = FunctionBuilder::new(FuncId(0), str_interner::intern("outer"));
        builder.next_func_id = 1;
        let capture = builder.local_for_symbol(symbol);
        let mut block = builder.new_block();
        super::lower_closure(
            &resolved,
            &resolved.ast,
            reference,
            &[parameter],
            None,
            &mut builder,
            &mut block,
        );
        let lambda = &builder.lambda_functions[0];
        assert_eq!(
            lambda
                .params
                .iter()
                .map(|param| param.role)
                .collect::<Vec<_>>(),
            vec![crate::NirParamRole::Capture, crate::NirParamRole::User]
        );
        let abi = lambda.entry_abi();
        assert_eq!(abi.captures, vec![nsbc::CaptureAbi::Value]);
        assert_eq!(abi.parameters, vec![nsbc::ParameterAbi::Value]);
        assert!(!abi.has_trait_proofs());
        assert!(matches!(builder.blocks[block.0 as usize].stmts.last(),
            Some(NirStmt::Assign(_, NirExpr::NewClosure(_, captures))) if captures == &[NirValue::Local(capture)]));
    }

    #[test]
    fn qualified_native_call_retains_its_declared_signature() {
        let (mut resolved, member, signature) = native_reference();
        let owner = resolved
            .ast
            .builder(NodeKind::Id, DUMMY_SP)
            .set_str_id(str_interner::intern("io"))
            .build();
        let projection = resolved
            .ast
            .builder(NodeKind::Projection, DUMMY_SP)
            .add_child(owner)
            .add_child(member)
            .build();
        resolved.node_types.insert(projection, signature);
        assert_eq!(
            resolved_symbol(&resolved, projection),
            resolved_symbol(&resolved, member)
        );
        let argument = resolved
            .ast
            .builder(NodeKind::Int, DUMMY_SP)
            .set_str_id(str_interner::intern("42"))
            .build();
        let call = resolved
            .ast
            .builder(NodeKind::Call, DUMMY_SP)
            .add_child(projection)
            .add_multi_children(&[argument])
            .build();
        let mut builder = FunctionBuilder::new(FuncId(0), str_interner::intern("main"));
        builder.next_func_id = 1;
        let mut block = builder.new_block();
        lower_expr(&resolved, call, &mut builder, &mut block);
        assert!(
            matches!(builder.blocks[0].stmts.last(), Some(NirStmt::Assign(_, NirExpr::Call(FuncId(1), args))) if args == &[NirValue::ConstInt(42)])
        );
        assert_eq!(builder.lambda_functions[0].function_type, signature);
    }

    #[test]
    fn intrinsic_aliases_keep_continuation_and_dynamic_call_semantics() {
        for intrinsic in [Intrinsic::Continuation, Intrinsic::Any] {
            let (mut resolved, reference, _) = native_reference();
            let symbol = resolved.node_symbols[&reference];
            let alias = resolved.type_pool.intern_structural(TypeKind::Typealias {
                name: str_interner::intern("CallableAlias"),
                target: intrinsic.type_index(),
            });
            resolved.symbols[symbol.0 as usize].kind = resolution::SymbolKind::Variable;
            resolved.symbols[symbol.0 as usize].type_index = alias;
            resolved.builtin_fns.remove(&symbol);
            resolved.node_types.insert(reference, alias);
            let argument = resolved
                .ast
                .builder(NodeKind::Int, DUMMY_SP)
                .set_str_id(str_interner::intern("42"))
                .build();
            let call = resolved
                .ast
                .builder(NodeKind::Call, DUMMY_SP)
                .add_child(reference)
                .add_multi_children(&[argument])
                .build();
            resolved.node_scopes.insert(call, resolution::ScopeId(7));
            let mut builder = FunctionBuilder::new(FuncId(0), str_interner::intern("main"));
            let callee = builder.local_for_symbol(symbol);
            let mut block = builder.new_block();
            lower_expr(&resolved, call, &mut builder, &mut block);
            let Some(NirStmt::Assign(_, expression)) = builder.blocks[0].stmts.last() else {
                panic!("call must produce an expression");
            };
            let Some(NirStmt::Assign(snapshot, NirExpr::Use(NirValue::Local(source)))) =
                builder.blocks[0].stmts.first()
            else {
                panic!("call must save its callee before evaluating arguments");
            };
            assert_eq!(*source, callee);
            assert_ne!(*snapshot, callee);
            let callee = *snapshot;
            match intrinsic {
                Intrinsic::Continuation => assert!(matches!(expression,
                    NirExpr::ResumeContinuation(NirValue::Local(local), NirValue::ConstInt(42)) if *local == callee)),
                Intrinsic::Any => {
                    assert!(matches!(expression, NirExpr::ScopedCall { scope: 7, .. }));
                    assert!(matches!(expression.unscoped(),
                    NirExpr::CallIndirect(NirValue::Local(local), args) if *local == callee && args == &[NirValue::ConstInt(42)]));
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn nested_dynamic_calls_keep_their_original_expression_scopes() {
        let (mut resolved, reference, _) = native_reference();
        let symbol = resolved.node_symbols[&reference];
        resolved.symbols[symbol.0 as usize].kind = resolution::SymbolKind::Variable;
        resolved.symbols[symbol.0 as usize].type_index = Intrinsic::Any.type_index();
        resolved.builtin_fns.remove(&symbol);
        resolved
            .node_types
            .insert(reference, Intrinsic::Any.type_index());
        let inner = resolved
            .ast
            .builder(NodeKind::Call, DUMMY_SP)
            .add_child(reference)
            .add_multi_children(&[])
            .build();
        let outer = resolved
            .ast
            .builder(NodeKind::Call, DUMMY_SP)
            .add_child(inner)
            .add_multi_children(&[])
            .build();
        resolved
            .node_types
            .insert(inner, Intrinsic::Any.type_index());
        resolved.node_scopes.insert(inner, resolution::ScopeId(7));
        resolved.node_scopes.insert(outer, resolution::ScopeId(8));
        let mut builder = FunctionBuilder::new(FuncId(0), str_interner::intern("main"));
        builder.local_for_symbol(symbol);
        let mut block = builder.new_block();
        lower_expr(&resolved, outer, &mut builder, &mut block);
        let scopes: Vec<_> = builder
            .blocks
            .iter()
            .flat_map(|block| &block.stmts)
            .filter_map(|statement| match statement {
                NirStmt::Assign(_, NirExpr::ScopedCall { scope, call }) => {
                    assert!(matches!(call.as_ref(), NirExpr::CallIndirect(..)));
                    Some(*scope)
                }
                _ => None,
            })
            .collect();
        assert_eq!(scopes, vec![7, 8]);
    }

    #[test]
    fn named_source_function_values_do_not_allocate_uninitialized_symbol_slots() {
        let (mut resolved, _) = crate::literal::tests::resolved_integer("42", false, "i64");
        let symbol = resolved
            .symbols
            .iter()
            .find(|symbol| symbol.kind == resolution::SymbolKind::Function)
            .unwrap()
            .id;
        let reference = resolved
            .ast
            .builder(NodeKind::Id, DUMMY_SP)
            .set_str_id(resolved.symbols[symbol.0 as usize].name)
            .build();
        resolved.node_symbols.insert(reference, symbol);
        let mut builder = FunctionBuilder::new(FuncId(0), str_interner::intern("main"));
        builder.func_map.insert(symbol, FuncId(7));
        let mut block = builder.new_block();
        lower_expr(&resolved, reference, &mut builder, &mut block);
        assert!(!builder.symbol_to_local.contains_key(&symbol));
        assert!(
            matches!(builder.blocks[0].stmts.last(), Some(NirStmt::Assign(_, NirExpr::NewClosure(FuncId(7), captures))) if captures.is_empty())
        );
    }
}

fn create_function_closure(
    function: FuncId,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
        local,
        NirExpr::NewClosure(function, vec![]),
    ));
    NirValue::Local(local)
}

/// Map a resolved symbol to a NirLocal, allocating if needed.
fn resolve_local(
    resolved: &ResolvedAst,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
) -> Option<NirLocal> {
    builder
        .node_symbol(resolved, node_idx)
        .map(|sym_id| builder.local_for_symbol(sym_id))
}

// ---------------------------------------------------------------------------
// Arithmetic
// ---------------------------------------------------------------------------

fn lower_arith(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    kind: NodeKind,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let children = ast.fixed_children(node_idx);
    let lhs = lower_expr(resolved, children[0], builder, block);
    let lhs = crate::arguments::snapshot(lhs, builder, *block);
    let rhs = lower_expr(resolved, children[1], builder, block);
    let op = match kind {
        NodeKind::Add => BinOp::Add,
        NodeKind::Sub => BinOp::Sub,
        NodeKind::Mul => BinOp::Mul,
        NodeKind::Div => BinOp::Div,
        NodeKind::Mod => BinOp::Mod,
        _ => unreachable!(),
    };
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(local, NirExpr::BinOp(op, lhs, rhs)));
    NirValue::Local(local)
}

// ---------------------------------------------------------------------------
// Comparison
// ---------------------------------------------------------------------------

fn lower_comparison(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    kind: NodeKind,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let children = ast.fixed_children(node_idx);
    let lhs = lower_expr(resolved, children[0], builder, block);
    let lhs = crate::arguments::snapshot(lhs, builder, *block);
    let rhs = lower_expr(resolved, children[1], builder, block);

    // Check if the LHS operand's type has a trait impl for this comparison.
    let lhs_ti = builder
        .coercion(resolved, children[0])
        .map(|coercion| builder.coercion_target(children[0], coercion.target))
        .or_else(|| builder.node_type(resolved, children[0]))
        .and_then(|ty| resolved.type_pool.canonical_type(ty));
    let rhs_ti = builder
        .coercion(resolved, children[1])
        .map(|coercion| builder.coercion_target(children[1], coercion.target))
        .or_else(|| builder.node_type(resolved, children[1]))
        .and_then(|ty| resolved.type_pool.canonical_type(ty));
    let mixed_numeric = lhs_ti.zip(rhs_ti).is_some_and(|(left, right)| {
        left != right
            && resolved
                .type_pool
                .as_intrinsic(left)
                .is_some_and(|ty| ty.is_numeric())
            && resolved
                .type_pool
                .as_intrinsic(right)
                .is_some_and(|ty| ty.is_numeric())
    });
    let wk = &resolved.type_pool.well_known;

    // For == and !=, check for Eq trait impl.
    // For <, <=, >, >=, check for Ord trait impl.
    let trait_dispatch = match kind {
        NodeKind::BoolEq | NodeKind::BoolNotEq => lhs_ti.and_then(|ti| {
            if crate::trait_dispatch::implements(resolved, node_idx, ti, wk.eq) {
                Some((ti, wk.eq, "eq"))
            } else if crate::trait_dispatch::implements(resolved, node_idx, ti, wk.partial_eq) {
                Some((ti, wk.partial_eq, "eq"))
            } else {
                None
            }
        }),
        NodeKind::BoolLt | NodeKind::BoolLtEq | NodeKind::BoolGt | NodeKind::BoolGtEq => lhs_ti
            .and_then(|ti| {
                if crate::trait_dispatch::implements(resolved, node_idx, ti, wk.ord) {
                    Some((ti, wk.ord, "cmp"))
                } else if crate::trait_dispatch::implements(resolved, node_idx, ti, wk.partial_ord)
                {
                    Some((ti, wk.partial_ord, "partial_cmp"))
                } else {
                    None
                }
            }),
        _ => None,
    };

    // Mixed numeric operators retain the exact cross-kind numeric kernel;
    // converting both sides to float can erase large integer distinctions.
    // An explicitly selected lexical implementation retains its prior dispatch.
    let trait_dispatch = trait_dispatch.filter(|&(owner, view, _)| {
        !mixed_numeric
            || resolved
                .type_pool
                .find_trait_impl_scoped(owner, view, resolved.node_scopes[&node_idx].0)
                .expect("resolution checked comparison implementation visibility")
                .is_some_and(|record| record.visible_scope.is_some())
    });
    if let Some((type_idx, trait_idx, method_name)) = trait_dispatch {
        let expression = crate::trait_dispatch::call(
            resolved,
            node_idx,
            crate::trait_dispatch::TraitMethodTarget {
                owner: type_idx,
                view: trait_idx,
                name: method_name,
            },
            lhs,
            vec![rhs],
            builder,
            *block,
        );
        let result = builder.alloc_local();
        builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::Assign(result, expression));
        if kind == NodeKind::BoolEq {
            return NirValue::Local(result);
        }
        if kind != NodeKind::BoolNotEq {
            let contract = resolved
                .type_pool
                .trait_schema(trait_idx)
                .and_then(|schema| {
                    schema
                        .slots
                        .iter()
                        .find(|key| key.name == str_interner::intern(method_name))
                })
                .and_then(|key| key.signature.as_ref())
                .and_then(
                    |signature| match resolved.type_pool.get(signature.declaration).kind {
                        type_pool::TypeKind::Function { ret, .. } => {
                            crate::ordering::OrderingLayout::from_result(&resolved.type_pool, ret)
                                .map(|layout| (ret, layout))
                        }
                        _ => None,
                    },
                );
            if let Some((result_type, ordering)) = contract {
                let checked = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    checked,
                    NirExpr::TypeAssert(NirValue::Local(result), result_type)
                        .in_source_scope(resolved, node_idx),
                ));
                let tag = match kind {
                    NodeKind::BoolLt | NodeKind::BoolLtEq => ordering.less,
                    NodeKind::BoolGt | NodeKind::BoolGtEq => ordering.greater,
                    _ => unreachable!("ordering comparison"),
                };
                let ordered = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    ordered,
                    NirExpr::EnumIs(NirValue::Local(checked), ordering.ty, tag),
                ));
                if matches!(kind, NodeKind::BoolLtEq | NodeKind::BoolGtEq) {
                    let equal = builder.alloc_local();
                    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                        equal,
                        NirExpr::EnumIs(NirValue::Local(checked), ordering.ty, ordering.equal),
                    ));
                    let combined = builder.alloc_local();
                    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                        combined,
                        NirExpr::BinOp(BinOp::Or, NirValue::Local(ordered), NirValue::Local(equal)),
                    ));
                    return NirValue::Local(combined);
                }
                return NirValue::Local(ordered);
            }
        }
        let local = builder.alloc_local();
        let expression = if kind == NodeKind::BoolNotEq {
            NirExpr::UnaryOp(UnaryOp::Not, NirValue::Local(result))
        } else {
            let op = match kind {
                NodeKind::BoolLt => BinOp::Lt,
                NodeKind::BoolLtEq => BinOp::Le,
                NodeKind::BoolGt => BinOp::Gt,
                NodeKind::BoolGtEq => BinOp::Ge,
                _ => unreachable!("comparison kind selects a checked ordering operator"),
            };
            NirExpr::BinOp(op, NirValue::Local(result), NirValue::ConstInt(0))
        };
        builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::Assign(local, expression));
        return NirValue::Local(local);
    }

    // Fallback: use intrinsic comparison.
    let op = match kind {
        NodeKind::BoolEq => BinOp::Eq,
        NodeKind::BoolNotEq => BinOp::Ne,
        NodeKind::BoolGt => BinOp::Gt,
        NodeKind::BoolGtEq => BinOp::Ge,
        NodeKind::BoolLt => BinOp::Lt,
        NodeKind::BoolLtEq => BinOp::Le,
        _ => unreachable!(),
    };
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(local, NirExpr::BinOp(op, lhs, rhs)));
    NirValue::Local(local)
}

// ---------------------------------------------------------------------------
// Logical operators
// ---------------------------------------------------------------------------

fn lower_logical(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    kind: NodeKind,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let children = ast.fixed_children(node_idx);
    let lhs = lower_expr(resolved, children[0], builder, block);
    let result = builder.alloc_local();
    let mut right_block = builder.new_block();
    let short_block = builder.new_block();
    let merge_block = builder.new_block();
    let conjunction = kind == NodeKind::BoolAnd;
    let (when_true, when_false) = if conjunction {
        (right_block, short_block)
    } else {
        (short_block, right_block)
    };
    builder.blocks[block.0 as usize].terminator = Terminator::Branch(lhs, when_true, when_false);

    builder.blocks[short_block.0 as usize]
        .stmts
        .push(NirStmt::Assign(
            result,
            NirExpr::Use(NirValue::ConstBool(!conjunction)),
        ));
    builder.blocks[short_block.0 as usize].terminator = Terminator::Goto(merge_block);

    // Lower the right operand only on the edge that needs it. Nested logical
    // expressions may advance right_block to their own merge block.
    let rhs = lower_expr(resolved, children[1], builder, &mut right_block);
    if matches!(
        builder.blocks[right_block.0 as usize].terminator,
        Terminator::Unreachable
    ) {
        builder.blocks[right_block.0 as usize]
            .stmts
            .push(NirStmt::Assign(result, NirExpr::Use(rhs)));
        builder.blocks[right_block.0 as usize].terminator = Terminator::Goto(merge_block);
    }
    *block = merge_block;
    NirValue::Local(result)
}

// ---------------------------------------------------------------------------
// Unary operators
// ---------------------------------------------------------------------------

fn lower_unary(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    op: UnaryOp,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let children = ast.fixed_children(node_idx);
    let inner = lower_expr(resolved, children[0], builder, block);
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(local, NirExpr::UnaryOp(op, inner)));
    NirValue::Local(local)
}

// ---------------------------------------------------------------------------
// Call expression
// ---------------------------------------------------------------------------

fn lower_call(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    if let Some(value) = crate::optional::unwrap_call(resolved, node_idx, builder, block) {
        return value;
    }
    if builder.enum_plan(resolved, node_idx).is_some() {
        return crate::enums::lower_construction(resolved, node_idx, builder, block);
    }
    let children = ast.fixed_children(node_idx);
    // Materialize the receiver/callable before arguments, and freeze its value
    // before a later expression can assign to the same source variable.
    let callee_node = children[0];
    if let Some(function) = builder.application_call(resolved, node_idx) {
        let function = builder.func_map[&function];
        let receiver = lower_expr(resolved, callee_node, builder, block);
        let receiver = crate::arguments::snapshot(receiver, builder, *block);
        let mut args = vec![receiver];
        args.extend(crate::arguments::lower_method_arguments(
            resolved, node_idx, receiver, builder, block,
        ));
        let result = builder.alloc_local();
        builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::Assign(result, NirExpr::Call(function, args)));
        return NirValue::Local(result);
    }
    let symbol = resolved_symbol_in(resolved, callee_node, builder);
    let instance_receiver = if ast.node(callee_node).kind == NodeKind::Projection {
        let projection = ast.fixed_children(callee_node);
        if builder.node_symbol(resolved, callee_node).is_none()
            && builder.node_symbol(resolved, projection[1]).is_none()
            && builder.field_index(resolved, callee_node).is_none()
        {
            Some((projection[0], ast.node(projection[1]).str_id))
        } else {
            None
        }
    } else {
        None
    };
    let dynamic_node = instance_receiver.map(|(receiver, _)| receiver).or_else(|| {
        let static_target = symbol.is_some_and(|id| {
            resolved.builtin_fns.contains_key(&id)
                || matches!(
                    resolved.symbols[id.0 as usize].kind,
                    SymbolKind::Effect | SymbolKind::Type
                )
                || (resolved.symbols[id.0 as usize].kind == SymbolKind::Function
                    && builder.func_map.contains_key(&id))
        });
        (!static_target).then_some(callee_node)
    });
    let callable = dynamic_node.map(|node| {
        let value = lower_expr(resolved, node, builder, block);
        crate::arguments::snapshot(value, builder, *block)
    });
    let args = if instance_receiver.is_some() {
        crate::arguments::lower_method_arguments(
            resolved,
            node_idx,
            callable.expect("instance receiver was evaluated"),
            builder,
            block,
        )
    } else {
        crate::arguments::lower_arguments(resolved, node_idx, builder, block)
    };
    if let Some(expression) = callable.and_then(|receiver| {
        crate::collections::index_access(resolved, callee_node, receiver, &args, builder)
    }) {
        let local = builder.alloc_local();
        builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::Assign(local, expression));
        return NirValue::Local(local);
    }
    if let Some((receiver_node, method)) = instance_receiver {
        let local = builder.alloc_local();
        // An interface declaration supplies binding and type information, not
        // an executable body. Trait receivers still require virtual dispatch.
        let virtual_interface = builder
            .node_type(resolved, receiver_node)
            .and_then(|ty| resolved.type_pool.canonical_type(ty))
            .is_some_and(|ty| {
                matches!(
                    resolved.type_pool.get(ty).kind,
                    type_pool::TypeKind::Trait { .. }
                )
            });
        if virtual_interface {
            let view = resolved
                .type_pool
                .canonical_type(
                    builder
                        .node_type(resolved, receiver_node)
                        .expect("checked receiver type"),
                )
                .expect("checked receiver view");
            let receiver = callable.expect("trait receiver was evaluated");
            let expression = crate::trait_dispatch::call(
                resolved,
                node_idx,
                crate::trait_dispatch::TraitMethodTarget {
                    owner: view,
                    view,
                    name: &str_interner::get(method),
                },
                receiver,
                args,
                builder,
                *block,
            );
            builder.blocks[block.0 as usize]
                .stmts
                .push(NirStmt::Assign(local, expression));
            return NirValue::Local(local);
        }
        if !virtual_interface && let Some(function) = builder.instance_method(resolved, callee_node)
        {
            let function = builder.func_map[&function];
            let mut arguments = vec![callable.expect("instance receiver was evaluated")];
            arguments.extend(args);
            builder.blocks[block.0 as usize]
                .stmts
                .push(NirStmt::Assign(local, NirExpr::Call(function, arguments)));
            return NirValue::Local(local);
        }
        builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
            local,
            NirExpr::MethodCall(
                callable.expect("instance receiver was evaluated"),
                method,
                args,
            ),
        ));
        return NirValue::Local(local);
    }
    if !callee_node.is_null()
        && let Some(sym_id) = resolved_symbol_in(resolved, callee_node, builder)
    {
        // Check if this symbol is a builtin function.
        if let Some(&builtin_id) = resolved.builtin_fns.get(&sym_id) {
            let expression =
                if declared_function_type(resolved, callee_node, sym_id, builder).is_some() {
                    let function = callable_function(resolved, callee_node, sym_id, builder)
                        .expect("typed native symbol produces an adapter");
                    NirExpr::Call(function, args)
                } else {
                    NirExpr::CallBuiltin(builtin_id, args)
                };
            let local = builder.alloc_local();
            builder.blocks[block.0 as usize]
                .stmts
                .push(NirStmt::Assign(local, expression));
            return NirValue::Local(local);
        }
        // Check if this is a user-defined function referenced by symbol.
        let sym = &resolved.symbols[sym_id.0 as usize];
        if sym.kind == SymbolKind::Effect {
            let local = builder.alloc_local();
            builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                local,
                NirExpr::EffectCall(sym.type_index, args),
            ));
            return NirValue::Local(local);
        }
        if sym.kind == SymbolKind::Function
            && let Some(&func_id) = builder.func_map.get(&sym_id)
        {
            let local = builder.alloc_local();
            builder.blocks[block.0 as usize]
                .stmts
                .push(NirStmt::Assign(local, NirExpr::Call(func_id, args)));
            return NirValue::Local(local);
        }
        // Check if callee is a variable/parameter — indirect call through closure
        // or apply desugaring for non-function types.
        if matches!(
            sym.kind,
            SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::Constant
        ) {
            // Check the type to determine call strategy.
            let callee_ti = builder
                .node_type(resolved, callee_node)
                .unwrap_or_else(|| builder.symbol_type(resolved, sym_id));
            let is_function_type = resolved
                .type_pool
                .canonical_type(callee_ti)
                .is_some_and(|ty| {
                    matches!(
                        resolved.type_pool.get(ty).kind,
                        type_pool::TypeKind::Function { .. }
                    )
                });

            let callee_intrinsic = resolved
                .type_pool
                .canonical_type(callee_ti)
                .and_then(|ty| resolved.type_pool.as_intrinsic(ty));
            if callee_intrinsic == Some(type_pool::Intrinsic::Continuation) {
                let callee = callable.expect("dynamic callable was evaluated");
                let local = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    local,
                    if builder.consume_continuation_at == Some(node_idx) {
                        NirExpr::ResumeContinuationOnce(callee, args[0])
                    } else {
                        NirExpr::ResumeContinuation(callee, args[0])
                    },
                ));
                return NirValue::Local(local);
            } else if is_function_type
                || callee_ti == type_pool::TypeIndex::INVALID
                || callee_intrinsic == Some(type_pool::Intrinsic::Any)
            {
                // Function type or unknown type → indirect call.
                let callee_val = callable.expect("dynamic callable was evaluated");
                let local = builder.alloc_local();
                let (args, changed) = crate::trait_parameters::expand_arguments(
                    resolved, node_idx, callee_ti, args, builder, *block,
                );
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    local,
                    if changed {
                        NirExpr::CallIndirectProof(callee_val, args)
                    } else {
                        NirExpr::CallIndirect(callee_val, args)
                    },
                ));
                check_indirect_return(resolved, node_idx, local, builder, *block);
                return NirValue::Local(local);
            } else {
                // Non-function type → desugar obj(args) to obj.apply(args).
                let callee_val = callable.expect("dynamic callable was evaluated");
                let apply_str = str_interner::intern("apply");
                let local = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    local,
                    NirExpr::MethodCall(callee_val, apply_str, args),
                ));
                return NirValue::Local(local);
            }
        }
    }

    // Expressions such as `make_function()(value)` have no direct callee symbol.
    // Materialize their callable value and preserve the indirect-call semantics.
    let callee = callable.expect("dynamic callable was evaluated");
    let local = builder.alloc_local();
    let signature = builder
        .node_type(resolved, callee_node)
        .unwrap_or(type_pool::TypeIndex::INVALID);
    let (args, changed) = crate::trait_parameters::expand_arguments(
        resolved, node_idx, signature, args, builder, *block,
    );
    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
        local,
        if changed {
            NirExpr::CallIndirectProof(callee, args)
        } else {
            NirExpr::CallIndirect(callee, args)
        },
    ));
    check_indirect_return(resolved, node_idx, local, builder, *block);
    NirValue::Local(local)
}

fn check_indirect_return(
    resolved: &ResolvedAst,
    call: NodeIndex,
    result: crate::NirLocal,
    builder: &mut FunctionBuilder,
    block: BlockId,
) {
    if let Some(expected) = builder.node_type(resolved, call)
        && resolved.type_pool.as_intrinsic(expected) != Some(type_pool::Intrinsic::Any)
    {
        // A gradually compatible function may have an Any return descriptor.
        // Establish the caller's actual contract even if its result is ignored.
        builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
            result,
            NirExpr::TypeAssert(NirValue::Local(result), expected).in_source_scope(resolved, call),
        ));
    }
}

// ---------------------------------------------------------------------------
// Block expression
// ---------------------------------------------------------------------------

fn lower_block_expr(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let stmts = ast.multi_children(node_idx);
    let mut last_val = NirValue::Unit;
    for (i, &stmt) in stmts.iter().enumerate() {
        if !matches!(
            builder.blocks[block.0 as usize].terminator,
            Terminator::Unreachable
        ) {
            break;
        }
        if i == stmts.len() - 1
            && !matches!(
                ast.node(stmt).kind,
                NodeKind::LetDecl
                    | NodeKind::ConstDecl
                    | NodeKind::VarDecl
                    | NodeKind::ReturnStatement
                    | NodeKind::ResumeStatement
                    | NodeKind::BreakStatement
                    | NodeKind::ContinueStatement
            )
        {
            last_val = lower_expr(resolved, stmt, builder, block);
        } else {
            crate::stmt::lower_stmt(resolved, stmt, builder, block, false);
        }
    }
    last_val
}

fn lower_if_value(
    resolved: &ResolvedAst,
    ast: &Ast,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let children = ast.fixed_children(node);
    let condition = lower_expr(resolved, children[0], builder, block);
    let result = builder.alloc_local();
    let then_block = builder.new_block();
    let else_block = builder.new_block();
    let merge = builder.new_block();
    builder.blocks[block.0 as usize].terminator =
        Terminator::Branch(condition, then_block, else_block);
    for (branch, mut current) in [(children[1], then_block), (children[2], else_block)] {
        let value = lower_expr(resolved, branch, builder, &mut current);
        if matches!(
            builder.blocks[current.0 as usize].terminator,
            Terminator::Unreachable
        ) {
            builder.blocks[current.0 as usize]
                .stmts
                .push(NirStmt::Assign(result, NirExpr::Use(value)));
            builder.blocks[current.0 as usize].terminator = Terminator::Goto(merge);
        }
    }
    *block = merge;
    NirValue::Local(result)
}

// ---------------------------------------------------------------------------
// Projection lowering (field access or enum variant construction)
// ---------------------------------------------------------------------------

fn lower_projection(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    if let Some(variant) = builder.enum_variant(resolved, node_idx) {
        return NirValue::ConstEnum(variant.type_index, variant.variant_tag);
    }
    let children = ast.fixed_children(node_idx);
    let member_node = children[1];
    if let Some(value) = load_global(resolved, node_idx, builder, block) {
        return value;
    }
    if let Some(symbol) = resolved_symbol_in(resolved, node_idx, builder)
        && let Some(function) = callable_function(resolved, node_idx, symbol, builder)
    {
        return create_function_closure(function, builder, block);
    }

    // Fallback: regular field access / method call.
    let object = lower_expr(resolved, children[0], builder, block);

    // If type resolution populated a field index for this projection, emit FieldAccess.
    if let Some(field_idx) = builder.field_index(resolved, node_idx) {
        let local = builder.alloc_local();
        builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
            local,
            NirExpr::FieldAccess(object, field_idx),
        ));
        return NirValue::Local(local);
    }

    let field_name = ast.node(member_node).str_id;
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
        local,
        NirExpr::MethodCall(object, field_name, Vec::new()),
    ));
    NirValue::Local(local)
}

// ---------------------------------------------------------------------------
// Lambda lowering — capture analysis + closure creation
// ---------------------------------------------------------------------------

/// Lower a Lambda AST node into a `NewClosure` NIR expression.
///
/// 1. Create a new NirFunction for the lambda body with an extra parameter
///    per captured variable (captures come before user params).
/// 2. Walk the lambda body to discover free variables — any symbol that is
///    already bound in the *parent* builder's symbol_to_local map.
/// 3. Emit `NewClosure(lambda_func_id, [captured_values])` in the parent.
fn lower_lambda(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    // Lambda AST: fixed: [0] body  [1] return_type   multi: params
    let children = ast.fixed_children(node_idx);
    let body_node = children[0];
    let params = ast.multi_children(node_idx);
    let (value, function) = lower_closure(resolved, ast, body_node, params, None, builder, block);
    if let Some(signature) = builder
        .self_function_types
        .get(&node_idx)
        .copied()
        .or_else(|| builder.node_type(resolved, node_idx))
        && let Some(signature) = resolved.type_pool.canonical_type(signature)
        && let type_pool::TypeKind::Function { ret, .. } = resolved.type_pool.get(signature).kind
        && let Some(generated) = builder
            .lambda_functions
            .iter_mut()
            .find(|generated| generated.func_id == function)
    {
        generated.function_type = signature;
        generated.return_type = ret;
    }
    value
}

fn lower_closure(
    resolved: &ResolvedAst,
    ast: &Ast,
    body_node: NodeIndex,
    params: &[NodeIndex],
    fresh_continuation: Option<resolution::SymbolId>,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> (NirValue, FuncId) {
    // Phase 1: discover free variables by collecting all referenced symbols
    // in the body that are already known in the parent's scope.
    let mut free_vars: Vec<(resolution::SymbolId, NirLocal)> = Vec::new();
    collect_free_vars(resolved, ast, body_node, builder, &mut free_vars);
    // Deduplicate preserving order.
    let mut seen = std::collections::HashSet::new();
    free_vars.retain(|(sym, _)| seen.insert(*sym));

    // Phase 2: build the lambda's NirFunction.
    let lambda_name = str_interner::intern("<lambda>");
    let lambda_func_id = builder.alloc_func_id();
    let mut lambda_builder = FunctionBuilder::new(lambda_func_id, lambda_name);
    lambda_builder.entry_scope = resolved.node_scopes.get(&body_node).map(|scope| scope.0);
    lambda_builder.body_facts = builder.body_facts.clone();
    lambda_builder.specialized_node_types = builder.specialized_node_types.clone();
    lambda_builder.specialized_symbol_types = builder.specialized_symbol_types.clone();
    lambda_builder.specialized_coercion_targets = builder.specialized_coercion_targets.clone();
    lambda_builder.self_type_values = builder.self_type_values.clone();
    lambda_builder.self_function_types = builder.self_function_types.clone();
    lambda_builder.self_function_parameters = builder.self_function_parameters.clone();
    lambda_builder.default_trait_view = builder.default_trait_view;
    lambda_builder.func_map = builder.func_map.clone();
    lambda_builder.derived_functions = builder.derived_functions.clone();
    lambda_builder.global_map = builder.global_map.clone();
    lambda_builder.native_adapters = builder.native_adapters.clone();
    lambda_builder.next_func_id = builder.next_func_id;
    lambda_builder.is_closure = true;
    lambda_builder.consume_continuation_at =
        crate::continuation::single_use_handler_call(resolved, body_node, fresh_continuation);

    // Add capture parameters first (LoadCapture will access these).
    // In the lambda body, each captured variable gets a fresh local that
    // is bound to the corresponding capture parameter.
    for (sym_id, parent_local) in &free_vars {
        let local = lambda_builder.alloc_local();
        lambda_builder.symbol_to_local.insert(*sym_id, local);
        lambda_builder.params.push(NirParam {
            local,
            name: resolved.symbols[sym_id.0 as usize].name,
            type_index: builder.symbol_type(resolved, *sym_id),
            role: crate::NirParamRole::Capture,
        });
        if let Some(&(_, view)) = builder.value_proofs.get(parent_local) {
            let proof = lambda_builder.alloc_local();
            lambda_builder.params.push(NirParam {
                local: proof,
                name: str_interner::intern("<captured-proof>"),
                type_index: type_pool::TypeIndex::INVALID,
                role: crate::NirParamRole::CaptureProof { view },
            });
            lambda_builder.value_proofs.insert(local, (proof, view));
        }
    }

    // Add explicit lambda parameters.
    let mut tuple_parameters = Vec::new();
    for &param in params {
        let param_node = ast.node(param);
        match param_node.kind {
            NodeKind::Id | NodeKind::Underscore => {
                let local = if let Some(&symbol) = resolved.node_symbols.get(&param) {
                    lambda_builder.local_for_symbol(symbol)
                } else {
                    lambda_builder.alloc_local()
                };
                lambda_builder.params.push(NirParam {
                    local,
                    name: param_node.str_id,
                    type_index: resolved
                        .node_symbols
                        .get(&param)
                        .map(|symbol| builder.symbol_type(resolved, *symbol))
                        .unwrap_or(type_pool::Intrinsic::Any.type_index()),
                    role: crate::NirParamRole::User,
                });
            }
            NodeKind::ParamTyped | NodeKind::ParamLambda => {
                let param_children = ast.fixed_children(param);
                let pat_node_idx = param_children[0];
                let pat_node = ast.node(pat_node_idx);
                let local = if let Some(&sym_id) = resolved.node_symbols.get(&pat_node_idx) {
                    lambda_builder.local_for_symbol(sym_id)
                } else {
                    lambda_builder.alloc_local()
                };
                lambda_builder.params.push(NirParam {
                    local,
                    name: pat_node.str_id,
                    type_index: resolved
                        .node_symbols
                        .get(&pat_node_idx)
                        .map(|symbol| builder.symbol_type(resolved, *symbol))
                        .or_else(|| builder.node_type(resolved, pat_node_idx))
                        .unwrap_or(type_pool::TypeIndex::INVALID),
                    role: crate::NirParamRole::User,
                });
                if pat_node.kind == NodeKind::PatternTuple {
                    tuple_parameters.push((pat_node_idx, local));
                }
            }
            NodeKind::PatternTuple => {
                let local = lambda_builder.alloc_local();
                lambda_builder.params.push(NirParam {
                    local,
                    name: str_interner::intern("<tuple>"),
                    type_index: builder
                        .node_type(resolved, param)
                        .unwrap_or(type_pool::TypeIndex::INVALID),
                    role: crate::NirParamRole::User,
                });
                tuple_parameters.push((param, local));
            }
            NodeKind::ParamSelf => {
                let local = lambda_builder.alloc_local();
                lambda_builder.params.push(NirParam {
                    local,
                    name: param_node.str_id,
                    type_index: type_pool::TypeIndex::INVALID,
                    role: crate::NirParamRole::User,
                });
            }
            NodeKind::ParamOptional => {
                let param_children = ast.fixed_children(param);
                let id_node_idx = param_children[0];
                let id_node = ast.node(id_node_idx);
                let local = if let Some(&sym_id) = resolved.node_symbols.get(&id_node_idx) {
                    lambda_builder.local_for_symbol(sym_id)
                } else {
                    lambda_builder.alloc_local()
                };
                lambda_builder.params.push(NirParam {
                    local,
                    name: id_node.str_id,
                    type_index: type_pool::TypeIndex::INVALID,
                    role: crate::NirParamRole::User,
                });
            }
            NodeKind::ParamVarargs => {
                let param_children = ast.fixed_children(param);
                let id_node_idx = param_children[0];
                let id_node = ast.node(id_node_idx);
                let local = if let Some(&sym_id) = resolved.node_symbols.get(&id_node_idx) {
                    lambda_builder.local_for_symbol(sym_id)
                } else {
                    lambda_builder.alloc_local()
                };
                lambda_builder.params.push(NirParam {
                    local,
                    name: id_node.str_id,
                    type_index: builder
                        .node_type(resolved, id_node_idx)
                        .or_else(|| {
                            builder
                                .node_symbol(resolved, id_node_idx)
                                .map(|symbol| builder.symbol_type(resolved, symbol))
                        })
                        .unwrap_or(type_pool::TypeIndex::INVALID),
                    role: crate::NirParamRole::User,
                });
            }
            _ => {}
        }
    }

    let specialization = builder
        .self_function_types
        .iter()
        .find(|(lambda, _)| ast.fixed_children(**lambda).first() == Some(&body_node));
    if let Some((&lambda, &signature)) = specialization {
        let type_pool::TypeKind::Function { params, .. } = &resolved.type_pool.get(signature).kind
        else {
            unreachable!("checked lambda specialization is a function")
        };
        for (parameter, &ty) in lambda_builder
            .params
            .iter_mut()
            .filter(|parameter| parameter.role == crate::NirParamRole::User)
            .zip(params)
        {
            parameter.type_index = ty;
        }
        crate::trait_parameters::prepare_self_parameters(
            resolved,
            builder
                .default_trait_view
                .expect("Self specialization belongs to a default adapter"),
            builder
                .self_function_parameters
                .get(&lambda)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            &mut lambda_builder,
        );
    } else {
        crate::trait_parameters::prepare_parameters(resolved, &mut lambda_builder);
    }
    // Create the entry block and lower the body.
    let mut entry = lambda_builder.new_block();
    lambda_builder.entry_block = entry;
    for (pattern, local) in tuple_parameters {
        crate::tuples::bind_pattern(
            resolved,
            pattern,
            NirValue::Local(local),
            &mut lambda_builder,
            entry,
        );
    }
    lambda_builder.return_type = builder
        .node_type(resolved, body_node)
        .unwrap_or(type_pool::Intrinsic::Unit.type_index());
    if let Some((_, &signature)) = specialization
        && let type_pool::TypeKind::Function { ret, .. } = resolved.type_pool.get(signature).kind
    {
        lambda_builder.return_type = ret;
    }

    if !body_node.is_null() {
        crate::stmt::lower_body_into(resolved, body_node, &mut lambda_builder, &mut entry);
    } else {
        lambda_builder.blocks[entry.0 as usize].terminator = Terminator::Return(NirValue::Unit);
    }
    let source_return = resolved.node_types.get(&body_node).copied();
    if specialization.is_some()
        && (source_return != Some(lambda_builder.return_type)
            || !lambda_builder.specialized_node_types.is_empty())
    {
        crate::self_specialization::check_returns(&mut lambda_builder);
    }

    // Reclaim any nested lambda func ids.
    builder.next_func_id = lambda_builder.next_func_id;

    // Collect nested lambda functions.
    let nested_lambdas = std::mem::take(&mut lambda_builder.lambda_functions);
    let lambda_fn = lambda_builder.build();
    builder.lambda_functions.push(lambda_fn);
    builder.lambda_functions.extend(nested_lambdas);

    // Phase 3: emit NewClosure in the parent function.
    let mut captures = Vec::new();
    for (_, parent_local) in &free_vars {
        captures.push(NirValue::Local(*parent_local));
        if let Some(&(proof, _)) = builder.value_proofs.get(parent_local) {
            captures.push(NirValue::Local(proof));
        }
    }

    let result = builder.alloc_local();
    builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
        result,
        NirExpr::NewClosure(lambda_func_id, captures),
    ));
    (NirValue::Local(result), lambda_func_id)
}

fn lower_effect_elimination(
    resolved: &ResolvedAst,
    ast: &Ast,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let arms = ast.multi_children(node);
    let mut captures = false;
    for &arm in arms {
        let children = ast.fixed_children(arm);
        let pattern = children[0];
        let callee = ast.fixed_children(pattern)[0];
        let symbol = resolved.node_symbols[&callee];
        let effect = resolved.symbols[symbol.0 as usize].type_index;
        let continuation_param = resolved
            .effects
            .iter()
            .find(|info| info.type_index == effect)
            .and_then(|info| info.operations[0].continuation_param)
            .map(|position| position as u8);
        captures |= continuation_param.is_some();
        let fresh_continuation = continuation_param
            .and_then(|position| ast.multi_children(pattern).get(position as usize))
            .and_then(|binding| resolved.node_symbols.get(binding))
            .copied();
        let (closure, _) = lower_closure(
            resolved,
            ast,
            children[1],
            ast.multi_children(pattern),
            fresh_continuation,
            builder,
            block,
        );
        builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::PushHandler {
                effect,
                closure,
                continuation_param,
            });
    }
    let result = if captures {
        let (body, _) = lower_closure(
            resolved,
            ast,
            ast.fixed_children(node)[0],
            &[],
            None,
            builder,
            block,
        );
        let local = builder.alloc_local();
        builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
            local,
            NirExpr::DelimitedCall {
                body,
                handler_count: arms.len() as u8,
            },
        ));
        NirValue::Local(local)
    } else {
        lower_expr(resolved, ast.fixed_children(node)[0], builder, block)
    };
    for _ in arms {
        builder.blocks[block.0 as usize]
            .stmts
            .push(NirStmt::PopHandler);
    }
    result
}

/// Recursively collect free variables referenced in a subtree.
///
/// A "free variable" is any identifier whose resolved SymbolId is already
/// mapped to a NirLocal in the *parent* builder — i.e., it was defined in
/// an enclosing scope and needs to be captured.
fn collect_free_vars(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    parent_builder: &FunctionBuilder,
    out: &mut Vec<(resolution::SymbolId, NirLocal)>,
) {
    if node_idx.is_null() {
        return;
    }
    let node = ast.node(node_idx);

    // If this is an identifier that resolves to a symbol the parent knows about,
    // it's a free variable that needs capturing.
    if matches!(
        node.kind,
        NodeKind::Id | NodeKind::SelfLower | NodeKind::SelfUpper
    ) && let Some(&sym_id) = resolved.node_symbols.get(&node_idx)
        && !parent_builder.global_map.contains_key(&sym_id)
    {
        let sym = &resolved.symbols[sym_id.0 as usize];
        match sym.kind {
            SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::Constant => {
                if let Some(&parent_local) = parent_builder.symbol_to_local.get(&sym_id) {
                    out.push((sym_id, parent_local));
                }
            }
            _ => {}
        }
    }

    // Include nested lambdas: the enclosing closure must carry any outer values
    // needed when it constructs a nested closure. The nested lambda's own
    // analysis then captures those values from its immediate enclosing scope.
    for &child in ast.fixed_children(node_idx) {
        collect_free_vars(resolved, ast, child, parent_builder, out);
    }
    for &child in ast.multi_children(node_idx) {
        collect_free_vars(resolved, ast, child, parent_builder, out);
    }
}

// ---------------------------------------------------------------------------
// Match expression lowering
// ---------------------------------------------------------------------------

/// Lower a pattern into a value for comparison.
pub(crate) fn lower_pattern_value(
    resolved: &ResolvedAst,
    ast: &Ast,
    node_idx: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    if let Some(variant) = builder.enum_variant(resolved, node_idx) {
        return NirValue::ConstEnum(variant.type_index, variant.variant_tag);
    }
    if node_idx.is_null() {
        return NirValue::Unit;
    }
    let node = ast.node(node_idx);
    match node.kind {
        NodeKind::Int => crate::literal::lower_integer(resolved, node_idx),
        NodeKind::Negative if ast.node(ast.fixed_children(node_idx)[0]).kind == NodeKind::Int => {
            crate::literal::lower_integer(resolved, node_idx)
        }
        NodeKind::Real => lower_real_literal(node),
        NodeKind::Negative => lower_expr(resolved, node_idx, builder, block),
        NodeKind::Bool => lower_bool_literal(node),
        NodeKind::Str => lower_str_literal(node),
        NodeKind::Char => lower_char_literal(node),
        NodeKind::Null => NirValue::Null,
        NodeKind::Unit => NirValue::Unit,
        NodeKind::Id | NodeKind::SelfLower | NodeKind::SelfUpper => {
            lower_id(resolved, node_idx, builder, block)
        }
        _ => unreachable!("resolver admits only implemented literal patterns"),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod integer_pattern_tests {
    use super::*;

    #[test]
    fn expression_patterns_keep_radix_and_signed_minimum() {
        for (text, negative, ty, expected) in [
            ("0x2a", false, "u64", NirValue::ConstUInt(42)),
            (
                "170141183460469231731687303715884105728",
                true,
                "i128",
                NirValue::ConstI128(i128::MIN),
            ),
            (
                "0xffffffffffffffffffffffffffffffff",
                false,
                "u128",
                NirValue::ConstU128(u128::MAX),
            ),
        ] {
            let (resolved, pattern) = crate::literal::tests::resolved_integer(text, negative, ty);
            let mut builder = FunctionBuilder::new(FuncId(0), str_interner::intern("pattern"));
            let mut block = builder.new_block();
            assert_eq!(
                lower_pattern_value(&resolved, &resolved.ast, pattern, &mut builder, &mut block),
                expected
            );
        }
    }
}

#[cfg(test)]
mod cast_tests {
    use super::*;
    use diagnostic::DiagnosticContext;
    use rustc_span::{
        DUMMY_SP,
        source_map::{FilePathMapping, SourceMap},
    };
    use type_pool::Intrinsic;

    #[test]
    fn explicit_cast_lowers_source_and_destination_to_nir() {
        for (text, destination) in [
            ("42", Intrinsic::F64),
            ("300", Intrinsic::I8),
            ("42", Intrinsic::U128),
        ] {
            let mut ast = Ast::new();
            ast.source = Some(format!("fn cast()->{0}={text} as {0}", destination.name()));
            let source = ast
                .builder(NodeKind::Int, DUMMY_SP)
                .set_str_id(str_interner::intern(text))
                .build();
            let target = ast
                .builder(NodeKind::Id, DUMMY_SP)
                .set_str_id(str_interner::intern(destination.name()))
                .build();
            let cast = ast
                .builder(NodeKind::TypeCast, DUMMY_SP)
                .add_child(source)
                .add_child(target)
                .build();
            let name = ast
                .builder(NodeKind::Id, DUMMY_SP)
                .set_str_id(str_interner::intern("cast"))
                .build();
            let function = ast
                .builder(NodeKind::FunctionDef, DUMMY_SP)
                .add_child(name)
                .add_child(target)
                .add_child(cast)
                .add_child(NodeIndex::NULL)
                .build();
            ast.root = ast
                .builder(NodeKind::FileScope, DUMMY_SP)
                .add_multi_children(&[function])
                .build();
            let source_map = SourceMap::new(FilePathMapping::empty());
            let diagnostics = DiagnosticContext::new(&source_map);
            let resolved = resolution::resolve(ast, &diagnostics);
            assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
            let module = crate::lower(&resolved);
            let function = &module.functions[0];
            let entry = &function.blocks[function.entry_block.0 as usize];
            let NirStmt::Assign(result, expression) = &entry.stmts[0] else {
                panic!("cast produces a statement");
            };
            let NirExpr::TypeCast(value, ty) = expression.unscoped() else {
                panic!("cast must remain an explicit checked conversion in NIR");
            };
            assert_eq!(*value, NirValue::ConstInt(text.parse().unwrap()));
            assert_eq!(*ty, destination.type_index());
            assert!(
                matches!(entry.terminator, Terminator::Return(NirValue::Local(local)) if local == *result)
            );
        }
    }
}

#[cfg(test)]
mod logical_tests {
    use ast::{Ast, NodeIndex, NodeKind};
    use diagnostic::DiagnosticContext;
    use rustc_span::{
        DUMMY_SP,
        source_map::{FilePathMapping, SourceMap},
    };

    use crate::{NirExpr, NirStmt, NirValue, Terminator};

    fn logical_function(kind: NodeKind, left: bool) -> crate::NirFunction {
        let mut ast = Ast::new();
        let operator = match kind {
            NodeKind::BoolAnd => "and",
            NodeKind::BoolOr => "or",
            _ => panic!("logical fixture requires and/or"),
        };
        ast.source = Some(format!(
            "fn probe()->bool=true;fn main()->bool={left} {operator} probe()"
        ));
        let bool_type = ast
            .builder(NodeKind::Id, DUMMY_SP)
            .set_str_id(str_interner::intern("bool"))
            .build();
        let probe_name = ast
            .builder(NodeKind::Id, DUMMY_SP)
            .set_str_id(str_interner::intern("probe"))
            .build();
        let probe_body = ast
            .builder(NodeKind::Bool, DUMMY_SP)
            .set_str_id(str_interner::intern("true"))
            .build();
        let probe = ast
            .builder(NodeKind::FunctionDef, DUMMY_SP)
            .add_child(probe_name)
            .add_child(bool_type)
            .add_child(probe_body)
            .add_child(NodeIndex::NULL)
            .build();
        let left = ast
            .builder(NodeKind::Bool, DUMMY_SP)
            .set_str_id(str_interner::intern(if left { "true" } else { "false" }))
            .build();
        let callee = ast
            .builder(NodeKind::Id, DUMMY_SP)
            .set_str_id(str_interner::intern("probe"))
            .build();
        let right = ast
            .builder(NodeKind::Call, DUMMY_SP)
            .add_child(callee)
            .build();
        let logical = ast
            .builder(kind, DUMMY_SP)
            .add_child(left)
            .add_child(right)
            .build();
        let main_name = ast
            .builder(NodeKind::Id, DUMMY_SP)
            .set_str_id(str_interner::intern("main"))
            .build();
        let main = ast
            .builder(NodeKind::FunctionDef, DUMMY_SP)
            .add_child(main_name)
            .add_child(bool_type)
            .add_child(logical)
            .add_child(NodeIndex::NULL)
            .build();
        ast.root = ast
            .builder(NodeKind::FileScope, DUMMY_SP)
            .add_multi_children(&[probe, main])
            .build();
        let source_map = SourceMap::new(FilePathMapping::empty());
        let diagnostics = DiagnosticContext::new(&source_map);
        let resolved = resolution::resolve(ast, &diagnostics);
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
        crate::lower(&resolved)
            .functions
            .into_iter()
            .find(|function| str_interner::get(function.name) == "main")
            .unwrap()
    }

    #[test]
    fn logical_control_flow_evaluates_the_right_call_only_when_needed() {
        for (kind, left, needs_right) in [
            (NodeKind::BoolAnd, false, false),
            (NodeKind::BoolAnd, true, true),
            (NodeKind::BoolOr, false, true),
            (NodeKind::BoolOr, true, false),
        ] {
            let function = logical_function(kind, left);
            let entry = &function.blocks[function.entry_block.0 as usize];
            assert!(
                entry.stmts.is_empty(),
                "the right call must not precede the branch"
            );
            let Terminator::Branch(NirValue::ConstBool(condition), when_true, when_false) =
                entry.terminator
            else {
                panic!("logical expression must branch on its left operand");
            };
            assert_eq!(condition, left);
            let chosen = if condition { when_true } else { when_false };
            let chosen = &function.blocks[chosen.0 as usize];
            assert_eq!(
                chosen
                    .stmts
                    .iter()
                    .any(|statement| matches!(statement, NirStmt::Assign(_, NirExpr::Call(_, _)))),
                needs_right
            );
            let Terminator::Goto(merge) = chosen.terminator else {
                panic!("both value paths merge");
            };
            let Terminator::Return(NirValue::Local(result)) =
                function.blocks[merge.0 as usize].terminator
            else {
                panic!("the merged result must be returned");
            };
            assert!(chosen.stmts.iter().any(|statement| matches!(statement,
                NirStmt::Assign(local, NirExpr::Use(_)) if *local == result)));
            if !needs_right {
                assert!(chosen.stmts.iter().any(|statement| matches!(statement,
                    NirStmt::Assign(_, NirExpr::Use(NirValue::ConstBool(value))) if *value == left)));
            }
        }
    }
}

#[cfg(test)]
mod comparison_coercion_tests {
    use ast::NodeKind;
    use nsbc::FuncId;
    use rustc_span::DUMMY_SP;
    use type_pool::{Intrinsic, MethodAccess, MethodSlot, TraitImplRecord};

    use super::lower_expr;
    use crate::builder::FunctionBuilder;
    use crate::{NirExpr, NirStmt};

    #[test]
    fn mixed_numeric_comparison_selects_the_converted_receiver_implementation() {
        let (mut resolved, left) = crate::literal::tests::resolved_integer("1", false, "i64");
        let right = resolved
            .ast
            .builder(NodeKind::Real, DUMMY_SP)
            .set_str_id(str_interner::intern("1.0"))
            .build();
        let comparison = resolved
            .ast
            .builder(NodeKind::BoolEq, DUMMY_SP)
            .add_child(left)
            .add_child(right)
            .build();
        let scope = resolved.node_scopes[&left];
        resolved.node_scopes.insert(right, scope);
        resolved.node_scopes.insert(comparison, scope);
        resolved
            .node_types
            .insert(right, Intrinsic::F64.type_index());
        resolved
            .node_types
            .insert(comparison, Intrinsic::Bool.type_index());
        resolved.node_coercions.insert(
            left,
            resolution::Coercion {
                target: Intrinsic::F64.type_index(),
                kind: resolution::CoercionKind::Convert,
            },
        );
        let eq = resolved.type_pool.well_known.eq;
        let name = str_interner::intern("eq");
        let mut builder = FunctionBuilder::new(FuncId(0), str_interner::intern("compare"));
        for (owner, function) in [
            (Intrinsic::I64.type_index(), FuncId(7)),
            (Intrinsic::F64.type_index(), FuncId(8)),
        ] {
            resolved.type_pool.add_trait_impl(TraitImplRecord {
                trait_type: eq,
                implementor: owner,
                visible_scope: None,
                methods: vec![MethodSlot {
                    access: MethodAccess::Public,
                    name,
                    func_id: type_pool::DERIVE_FUNC_ID,
                    trait_impl: Some(eq),
                    visible_scope: None,
                }],
            });
            builder
                .derived_functions
                .insert((owner, eq, name), function);
        }
        let mut block = builder.new_block();
        lower_expr(&resolved, comparison, &mut builder, &mut block);
        assert!(builder.blocks[0].stmts.iter().any(|statement| matches!(statement,
            NirStmt::Assign(_, expression) if matches!(expression.unscoped(), NirExpr::TypeCast(_, ty) if *ty == Intrinsic::F64.type_index()))));
        let calls: Vec<_> = builder.blocks[0]
            .stmts
            .iter()
            .filter_map(|statement| match statement {
                NirStmt::Assign(_, expression) => match expression.unscoped() {
                    NirExpr::Call(function, _) => Some(*function),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(calls, vec![FuncId(8)]);

        // Without a conversion these are distinct numeric representations.
        // The homogeneous Eq bridge cannot accept both; retain the numeric
        // kernel instead of narrowing the real operand or rounding the integer.
        resolved.node_coercions.remove(&left);
        let mut mixed_builder =
            FunctionBuilder::new(FuncId(0), str_interner::intern("mixed_compare"));
        mixed_builder.derived_functions = builder.derived_functions.clone();
        let mut mixed_block = mixed_builder.new_block();
        lower_expr(&resolved, comparison, &mut mixed_builder, &mut mixed_block);
        assert!(mixed_builder.blocks[0].stmts.iter().any(|statement| matches!(statement,
            NirStmt::Assign(_, expression) if matches!(expression.unscoped(), NirExpr::BinOp(crate::BinOp::Eq, _, _)))));
        assert!(!mixed_builder.blocks[0].stmts.iter().any(|statement| matches!(statement,
            NirStmt::Assign(_, expression) if matches!(expression.unscoped(), NirExpr::Call(_, _) | NirExpr::TypeCast(_, _)))));
    }
}
