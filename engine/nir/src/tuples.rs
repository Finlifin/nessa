//! Tuple values and recursive bindings share the checked object field layout.

use ast::{NodeIndex, NodeKind};
use resolution::ResolvedAst;

use crate::arguments::snapshot;
use crate::builder::FunctionBuilder;
use crate::expr::lower_expr;
use crate::{BlockId, NirExpr, NirStmt, NirValue};

pub(crate) fn lower_tuple(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirValue {
    let elements = resolved.ast.multi_children(node);
    if elements.is_empty() {
        return NirValue::Unit;
    }
    let fields = elements
        .iter()
        .map(|&element| {
            let value = lower_expr(resolved, element, builder, block);
            snapshot(value, builder, *block)
        })
        .collect();
    let ty = builder
        .node_type(resolved, node)
        .expect("checked tuple type");
    let local = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(local, NirExpr::NewObject(ty, fields)));
    NirValue::Local(local)
}

pub(crate) fn is_irrefutable_binding(resolved: &ResolvedAst, pattern: NodeIndex) -> bool {
    if pattern.is_null() {
        return false;
    }
    match resolved.ast.node(pattern).kind {
        NodeKind::Id => !resolved.enum_variants.contains_key(&pattern),
        NodeKind::Underscore => true,
        NodeKind::PatternTuple => resolved
            .ast
            .multi_children(pattern)
            .iter()
            .all(|&child| is_irrefutable_binding(resolved, child)),
        NodeKind::PatternTyped => {
            resolved.annotation_typed_patterns.contains(&pattern)
                && is_irrefutable_binding(resolved, resolved.ast.fixed_children(pattern)[0])
        }
        NodeKind::PatternExtendedCall => {
            resolved.struct_patterns.get(&pattern).is_some_and(|plan| {
                plan.fields
                    .iter()
                    .all(|&(_, subpat)| is_irrefutable_binding(resolved, subpat))
            })
        }
        NodeKind::PatternCall => {
            resolved.newtype_patterns.contains_key(&pattern)
                && resolved.ast.multi_children(pattern).len() == 1
                && is_irrefutable_binding(resolved, resolved.ast.multi_children(pattern)[0])
        }
        _ => false,
    }
}

/// The resolver checks tuple/struct shape and annotates every leaf before lowering.
/// Values are already evaluated; projections never evaluate the source again.
pub(crate) fn bind_pattern(
    resolved: &ResolvedAst,
    pattern: NodeIndex,
    value: NirValue,
    builder: &mut FunctionBuilder,
    block: BlockId,
) {
    match resolved.ast.node(pattern).kind {
        NodeKind::PatternTuple => {
            let value = snapshot(value, builder, block);
            for (index, &element) in resolved.ast.multi_children(pattern).iter().enumerate() {
                let local = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    local,
                    NirExpr::FieldAccess(value, index as u32).in_source_scope(resolved, pattern),
                ));
                bind_pattern(resolved, element, NirValue::Local(local), builder, block);
            }
        }
        NodeKind::PatternExtendedCall => {
            let value = snapshot(value, builder, block);
            let fields = builder
                .struct_pattern(resolved, pattern)
                .map(|plan| plan.fields.clone())
                .unwrap_or_default();
            for (slot, subpat) in fields {
                let local = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    local,
                    NirExpr::FieldAccess(value, slot).in_source_scope(resolved, pattern),
                ));
                bind_pattern(resolved, subpat, NirValue::Local(local), builder, block);
            }
        }
        NodeKind::PatternCall => {
            let value = snapshot(value, builder, block);
            if let Some(&subpat) = resolved.ast.multi_children(pattern).first() {
                let local = builder.alloc_local();
                builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                    local,
                    NirExpr::FieldAccess(value, 0).in_source_scope(resolved, pattern),
                ));
                bind_pattern(resolved, subpat, NirValue::Local(local), builder, block);
            }
        }
        NodeKind::PatternTyped => {
            let children = resolved.ast.fixed_children(pattern);
            let value = snapshot(value, builder, block);
            if let Some(target) = builder.node_type(resolved, children[1])
                && resolved.type_pool.as_intrinsic(target) != Some(type_pool::Intrinsic::Any)
                && ast::NodeKind::Underscore != resolved.ast.node(children[0]).kind
            {
                let converted =
                    crate::expr::lower_cast(resolved, pattern, value, target, builder, block);
                bind_pattern(
                    resolved,
                    children[0],
                    NirValue::Local(converted),
                    builder,
                    block,
                );
            } else {
                bind_pattern(resolved, children[0], value, builder, block);
            }
        }
        NodeKind::Id => {
            if let Some(symbol) = builder.node_symbol(resolved, pattern) {
                let value = if let Some(coercion) = builder.coercion(resolved, pattern) {
                    let target = builder.coercion_target(pattern, coercion.target);
                    let coerced = builder.alloc_local();
                    if let Some(view) = crate::trait_parameters::bare_trait(resolved, target) {
                        let proof = crate::trait_parameters::proof_for(
                            resolved, pattern, value, view, builder, block,
                        );
                        let NirValue::Local(proof) = proof else {
                            unreachable!("proof construction returns a local")
                        };
                        builder.value_proofs.insert(coerced, (proof, view));
                        builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                            coerced,
                            NirExpr::TraitAssert(value, NirValue::Local(proof), view)
                                .in_source_scope(resolved, pattern),
                        ));
                    } else {
                        let expression = match coercion.kind {
                            resolution::CoercionKind::Assert => NirExpr::TypeAssert(value, target),
                            resolution::CoercionKind::Convert => NirExpr::TypeCast(value, target),
                        };
                        builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                            coerced,
                            expression.in_source_scope(resolved, pattern),
                        ));
                    }
                    NirValue::Local(coerced)
                } else {
                    value
                };
                if let Some(&global) = builder.global_map.get(&symbol) {
                    builder.blocks[block.0 as usize].stmts.push(
                        NirStmt::StoreGlobal(global, value).in_source_scope(resolved, pattern),
                    );
                } else {
                    let local = builder.local_for_symbol(symbol);
                    crate::trait_parameters::bind_local_type(
                        resolved,
                        local,
                        builder.symbol_type(resolved, symbol),
                        builder,
                    );
                    crate::trait_parameters::copy_proof(local, value, builder, block);
                    builder.blocks[block.0 as usize]
                        .stmts
                        .push(NirStmt::Assign(local, NirExpr::Use(value)));
                }
            }
        }
        NodeKind::Underscore => {}
        _ => unreachable!("resolver admits only irrefutable destructuring bindings"),
    }
}
