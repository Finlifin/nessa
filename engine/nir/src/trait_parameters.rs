//! Bare trait parameters use separate proof and data locals at physical entry.

use ast::NodeIndex;
use resolution::ResolvedAst;
use type_pool::{TypeIndex, TypeKind};

use crate::builder::FunctionBuilder;
use crate::{BlockId, NirExpr, NirLocal, NirParam, NirParamRole, NirStmt, NirValue};

pub(crate) fn bare_trait(resolved: &ResolvedAst, ty: TypeIndex) -> Option<TypeIndex> {
    let ty = resolved.type_pool.canonical_type(ty)?;
    matches!(resolved.type_pool.get(ty).kind, TypeKind::Trait { .. }).then_some(ty)
}

pub(crate) fn prepare_parameters(resolved: &ResolvedAst, builder: &mut FunctionBuilder) {
    let parameters = std::mem::take(&mut builder.params);
    for parameter in parameters {
        if parameter.role == NirParamRole::User {
            bind_local_type(resolved, parameter.local, parameter.type_index, builder);
        }
        if parameter.role == NirParamRole::User
            && let Some(view) = bare_trait(resolved, parameter.type_index)
        {
            let proof = builder.alloc_local();
            builder.value_proofs.insert(parameter.local, (proof, view));
            builder.params.push(NirParam {
                local: proof,
                name: str_interner::intern("<trait-proof>"),
                type_index: TypeIndex::INVALID,
                role: NirParamRole::TraitProof { view },
            });
        }
        builder.params.push(parameter);
    }
}

pub(crate) fn bind_local_type(
    resolved: &ResolvedAst,
    local: NirLocal,
    ty: TypeIndex,
    builder: &mut FunctionBuilder,
) {
    if bare_trait(resolved, ty).is_none() {
        builder.proof_erased_locals.insert(local);
        builder.value_proofs.remove(&local);
    }
}

pub(crate) fn prepare_self_parameters(
    resolved: &ResolvedAst,
    view: TypeIndex,
    self_parameters: &[usize],
    builder: &mut FunctionBuilder,
) {
    let parameters = std::mem::take(&mut builder.params);
    let mut index = 0;
    for parameter in parameters {
        if parameter.role != NirParamRole::User {
            builder.params.push(parameter);
            continue;
        }
        let self_parameter = self_parameters.contains(&index);
        index += 1;
        if self_parameter {
            let proof = builder.alloc_local();
            builder.value_proofs.insert(parameter.local, (proof, view));
            builder.params.push(NirParam {
                local: proof,
                name: str_interner::intern("<default-self-proof>"),
                type_index: TypeIndex::INVALID,
                role: NirParamRole::TraitSelfProof { view },
            });
        } else if let Some(view) = bare_trait(resolved, parameter.type_index) {
            let proof = builder.alloc_local();
            builder.value_proofs.insert(parameter.local, (proof, view));
            builder.params.push(NirParam {
                local: proof,
                name: str_interner::intern("<trait-proof>"),
                type_index: TypeIndex::INVALID,
                role: NirParamRole::TraitProof { view },
            });
        } else {
            bind_local_type(resolved, parameter.local, parameter.type_index, builder);
        }
        builder.params.push(parameter);
    }
}

pub(crate) fn proof_for(
    resolved: &ResolvedAst,
    node: NodeIndex,
    value: NirValue,
    view: TypeIndex,
    builder: &mut FunctionBuilder,
    block: BlockId,
) -> NirValue {
    let previous = if let NirValue::Local(local) = value {
        builder.value_proofs.get(&local).copied()
    } else {
        None
    };
    if let Some((proof, old_view)) = previous
        && old_view == view
    {
        return NirValue::Local(proof);
    }
    let proof = builder.alloc_local();
    let expression = match previous {
        Some((old, old_view)) if resolved.type_pool.is_subtype(old_view, view) => {
            NirExpr::TraitProject(NirValue::Local(old), view)
        }
        _ => NirExpr::TraitProof(value, view).in_source_scope(resolved, node),
    };
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(proof, expression));
    NirValue::Local(proof)
}

pub(crate) fn copy_proof(
    destination: NirLocal,
    source: NirValue,
    builder: &mut FunctionBuilder,
    block: BlockId,
) {
    if builder.proof_erased_locals.contains(&destination) {
        builder.value_proofs.remove(&destination);
        return;
    }
    let proof = match source {
        NirValue::Local(local) => builder.value_proofs.get(&local).copied(),
        _ => None,
    };
    if let Some((source_proof, view)) = proof {
        let target = builder
            .value_proofs
            .get(&destination)
            .map(|&(proof, _)| proof)
            .unwrap_or_else(|| builder.alloc_local());
        builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
            target,
            NirExpr::Use(NirValue::Local(source_proof)),
        ));
        builder.value_proofs.insert(destination, (target, view));
    } else {
        builder.value_proofs.remove(&destination);
    }
}

pub(crate) fn expand_arguments(
    resolved: &ResolvedAst,
    node: NodeIndex,
    signature: TypeIndex,
    args: Vec<NirValue>,
    builder: &mut FunctionBuilder,
    block: BlockId,
) -> (Vec<NirValue>, bool) {
    let Some(signature) = resolved.type_pool.canonical_type(signature) else {
        return (args, false);
    };
    let TypeKind::Function { params, .. } = &resolved.type_pool.get(signature).kind else {
        return (args, false);
    };
    let mut expanded = Vec::new();
    let mut changed = false;
    for (index, data) in args.into_iter().enumerate() {
        if let Some(view) = params.get(index).and_then(|&ty| bare_trait(resolved, ty)) {
            expanded.push(proof_for(resolved, node, data, view, builder, block));
            changed = true;
        }
        expanded.push(data);
    }
    (expanded, changed)
}

/// Rewrite new direct calls once. The distinct variant prevents outer AST
/// traversal from expanding an already physical argument list a second time.
pub(crate) fn finish_statements(
    resolved: &ResolvedAst,
    node: NodeIndex,
    starts: &[usize],
    builder: &mut FunctionBuilder,
) {
    for index in 0..builder.blocks.len() {
        let block = BlockId(index as u32);
        let start = starts.get(index).copied().unwrap_or(0);
        let statements = builder.blocks[index].stmts.split_off(start);
        for mut statement in statements {
            if let NirStmt::Assign(local, expression) = &mut statement {
                if let NirExpr::Use(value) = expression {
                    copy_proof(*local, *value, builder, block);
                }
                if let NirExpr::Call(function, args) = expression {
                    let default = resolved
                        .default_methods
                        .iter()
                        .find(|plan| builder.func_map.get(&plan.function) == Some(function));
                    let signature = builder
                        .func_map
                        .iter()
                        .find(|(_, id)| *id == function)
                        .map(|(symbol, _)| builder.symbol_type(resolved, *symbol))
                        .or_else(|| {
                            builder
                                .lambda_functions
                                .iter()
                                .find(|candidate| candidate.func_id == *function)
                                .map(|candidate| candidate.function_type)
                        });
                    if let Some(signature) = signature {
                        let (expanded, changed) = if let Some(plan) = default {
                            let TypeKind::Function { params, .. } =
                                &resolved.type_pool.get(signature).kind
                            else {
                                unreachable!("default signature was checked")
                            };
                            let mut expanded = Vec::new();
                            for (index, data) in std::mem::take(args).into_iter().enumerate() {
                                let view = if plan.self_parameters.contains(&index) {
                                    Some(plan.trait_type)
                                } else {
                                    params.get(index).and_then(|&ty| bare_trait(resolved, ty))
                                };
                                if let Some(view) = view {
                                    expanded.push(proof_for(
                                        resolved, node, data, view, builder, block,
                                    ));
                                }
                                expanded.push(data);
                            }
                            (expanded, true)
                        } else {
                            expand_arguments(
                                resolved,
                                node,
                                signature,
                                std::mem::take(args),
                                builder,
                                block,
                            )
                        };
                        *expression = if changed {
                            NirExpr::CallWithProof(*function, expanded)
                        } else {
                            NirExpr::Call(*function, expanded)
                        };
                    }
                }
            }
            builder.blocks[index].stmts.push(statement);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nsbc::FuncId;
    use type_pool::{Intrinsic, TypeId, TypeInfo};

    #[test]
    fn specialized_self_and_explicit_trait_have_distinct_physical_roles() {
        let (resolved, _) = crate::literal::tests::resolved_integer("42", false, "i64");
        let view = resolved.type_pool.well_known.eq;
        let mut builder = FunctionBuilder::new(FuncId(0), str_interner::intern("default"));
        for (role, ty) in [
            (NirParamRole::Capture, Intrinsic::Any.type_index()),
            (NirParamRole::User, Intrinsic::I64.type_index()),
            (NirParamRole::User, view),
        ] {
            let local = builder.alloc_local();
            builder.params.push(NirParam {
                local,
                name: str_interner::intern("value"),
                type_index: ty,
                role,
            });
        }
        let self_data = builder.params[1].local;
        prepare_self_parameters(&resolved, view, &[0], &mut builder);
        assert_eq!(
            builder
                .params
                .iter()
                .map(|parameter| parameter.role)
                .collect::<Vec<_>>(),
            vec![
                NirParamRole::Capture,
                NirParamRole::TraitSelfProof { view },
                NirParamRole::User,
                NirParamRole::TraitProof { view },
                NirParamRole::User,
            ]
        );
        assert_eq!(builder.params[2].type_index, Intrinsic::I64.type_index());
        assert_eq!(
            builder.value_proofs[&self_data],
            (builder.params[1].local, view)
        );
        let function = builder.build();
        assert_eq!(function.entry_abi().captures, vec![nsbc::CaptureAbi::Value]);
        assert_eq!(
            function.entry_abi().parameters,
            vec![
                nsbc::ParameterAbi::TraitSelf { view },
                nsbc::ParameterAbi::Trait { view }
            ]
        );
        assert_eq!(function.entry_abi().physical_parameter_count(), 5);
    }

    #[test]
    fn bare_trait_entry_and_forwarding_keep_separate_proof_and_data() {
        let (mut resolved, node) = crate::literal::tests::resolved_integer("42", false, "i64");
        let view = resolved.type_pool.well_known.eq;
        let mut builder = FunctionBuilder::new(FuncId(0), str_interner::intern("consume"));
        let data = builder.alloc_local();
        builder.params.push(NirParam {
            local: data,
            name: str_interner::intern("x"),
            type_index: view,
            role: NirParamRole::User,
        });
        prepare_parameters(&resolved, &mut builder);
        assert_eq!(
            builder
                .params
                .iter()
                .map(|parameter| parameter.role)
                .collect::<Vec<_>>(),
            vec![NirParamRole::TraitProof { view }, NirParamRole::User]
        );
        let block = builder.new_block();
        let signature = resolved.type_pool.intern_structural(TypeKind::Function {
            params: vec![view],
            ret: Intrinsic::I64.type_index(),
        });
        let (arguments, changed) = expand_arguments(
            &resolved,
            node,
            signature,
            vec![NirValue::Local(data)],
            &mut builder,
            block,
        );
        assert!(changed);
        let proof = builder.params[0].local;
        assert_eq!(
            arguments,
            vec![NirValue::Local(proof), NirValue::Local(data)]
        );
        assert!(
            builder.blocks[0].stmts.is_empty(),
            "forwarding does not reacquire in callee scope"
        );
        let copy = builder.alloc_local();
        copy_proof(copy, NirValue::Local(data), &mut builder, block);
        assert_eq!(builder.value_proofs[&copy].1, view);
        let child = resolved.type_pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("ChildProof"),
                parents: vec![view],
                assoc_types: vec![],
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        builder.value_proofs.insert(data, (proof, child));
        proof_for(
            &resolved,
            node,
            NirValue::Local(data),
            view,
            &mut builder,
            block,
        );
        assert!(
            matches!(builder.blocks[0].stmts.last(),Some(NirStmt::Assign(_,NirExpr::TraitProject(NirValue::Local(source),parent))) if *source == proof && *parent == view)
        );
        let erased = builder.alloc_local();
        bind_local_type(&resolved, erased, Intrinsic::Any.type_index(), &mut builder);
        copy_proof(erased, NirValue::Local(data), &mut builder, block);
        assert!(!builder.value_proofs.contains_key(&erased));
        let start = builder.blocks[0].stmts.len();
        builder.blocks[0]
            .stmts
            .push(NirStmt::Assign(erased, NirExpr::Use(NirValue::Local(data))));
        finish_statements(&resolved, node, &[start], &mut builder);
        assert!(!builder.value_proofs.contains_key(&erased));
        assert_eq!(builder.blocks[0].stmts.len(), start + 1);
        let function = builder.build();
        assert_eq!(
            function.entry_abi().parameters,
            vec![nsbc::ParameterAbi::Trait { view }]
        );
        assert_eq!(function.entry_abi().physical_parameter_count(), 2);
    }
}
