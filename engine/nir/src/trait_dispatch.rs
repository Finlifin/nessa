//! Consume resolution's checked lexical trait evidence without name dispatch.

use ast::NodeIndex;
use nsbc::FuncId;
use resolution::{ResolvedAst, SymbolId};
use type_pool::{TraitTypeStep, TypeIndex, TypeKind};

use crate::builder::FunctionBuilder;
use crate::{BlockId, NirExpr, NirValue};

pub(crate) fn implements(
    resolved: &ResolvedAst,
    node: NodeIndex,
    owner: TypeIndex,
    trait_type: TypeIndex,
) -> bool {
    if crate::trait_parameters::bare_trait(resolved, owner).is_some() {
        return resolved.type_pool.is_subtype(owner, trait_type);
    }
    resolved
        .type_pool
        .has_trait_impl_scoped(owner, trait_type, resolved.node_scopes[&node].0)
        .expect("resolution checked lexical trait evidence")
}

pub(crate) struct TraitMethodTarget<'a> {
    pub owner: TypeIndex,
    pub view: TypeIndex,
    pub name: &'a str,
}

/// Interface receivers dispatch through the proof frozen at their call site;
/// concrete receivers retain lexical selection of a source function.
pub(crate) fn call(
    resolved: &ResolvedAst,
    node: NodeIndex,
    target: TraitMethodTarget<'_>,
    receiver: NirValue,
    args: Vec<NirValue>,
    builder: &mut FunctionBuilder,
    block: BlockId,
) -> NirExpr {
    let TraitMethodTarget { owner, view, name } = target;
    if crate::trait_parameters::bare_trait(resolved, owner).is_none() {
        let function = function(resolved, node, builder, owner, view, name)
            .expect("resolution checked the source trait method");
        let mut values = vec![receiver];
        values.extend(args);
        return NirExpr::Call(function, values);
    }
    interface_call(
        resolved,
        node,
        TraitMethodTarget { owner, view, name },
        receiver,
        args,
        builder,
        block,
    )
}

fn interface_call(
    resolved: &ResolvedAst,
    node: NodeIndex,
    target: TraitMethodTarget<'_>,
    receiver: NirValue,
    args: Vec<NirValue>,
    builder: &mut FunctionBuilder,
    block: BlockId,
) -> NirExpr {
    let TraitMethodTarget { view, name, .. } = target;
    let view = resolved
        .type_pool
        .canonical_type(view)
        .expect("checked trait view");
    let proof = crate::trait_parameters::proof_for(resolved, node, receiver, view, builder, block);
    let schema = resolved
        .type_pool
        .trait_schema(view)
        .expect("checked trait schema");
    let slot = schema
        .slots
        .iter()
        .position(|key| key.name == str_interner::intern(name))
        .expect("checked interface method");
    let key = &schema.slots[slot];
    let mut physical = Vec::new();
    if let Some(signature) = &key.signature
        && let Some(declaration) = resolved.type_pool.canonical_type(signature.declaration)
        && let TypeKind::Function { params, .. } = &resolved.type_pool.get(declaration).kind
    {
        for (index, data) in args.into_iter().enumerate() {
            let parameter_view = if signature
                .self_paths
                .contains(&vec![TraitTypeStep::Parameter((index + 1) as u32)])
            {
                Some(view)
            } else {
                params
                    .get(index + 1)
                    .and_then(|&ty| crate::trait_parameters::bare_trait(resolved, ty))
            };
            if let Some(parameter_view) = parameter_view {
                physical.push(crate::trait_parameters::proof_for(
                    resolved,
                    node,
                    data,
                    parameter_view,
                    builder,
                    block,
                ));
            }
            physical.push(data);
        }
    } else {
        physical = args;
    }
    NirExpr::TraitCall {
        receiver,
        proof,
        view,
        slot: slot as u32,
        args: physical,
    }
    .in_source_scope(resolved, node)
}

pub(crate) fn function(
    resolved: &ResolvedAst,
    node: NodeIndex,
    builder: &FunctionBuilder,
    owner: TypeIndex,
    trait_type: TypeIndex,
    name: &str,
) -> Option<FuncId> {
    let name = str_interner::intern(name);
    let method = resolved
        .type_pool
        .find_trait_method_scoped(owner, trait_type, name, resolved.node_scopes[&node].0)
        .expect("resolution checked lexical trait evidence")?;
    if method.func_id == type_pool::DERIVE_FUNC_ID {
        // Derivation plans only carry global evidence; extensions cannot be
        // synthesized by overwriting a global comparator with the same name.
        assert!(
            method.visible_scope.is_none(),
            "scoped derivation is rejected during resolution"
        );
        builder
            .derived_functions
            .get(&(owner, trait_type, name))
            .copied()
    } else {
        Some(
            *builder
                .func_map
                .get(&SymbolId(method.func_id))
                .expect("all source trait methods were collected"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NirParam, NirParamRole};
    use type_pool::{
        Intrinsic, TraitDispatchSchema, TraitMethodKey, TraitMethodSignature, TraitParameterKind,
    };

    #[test]
    fn interface_comparison_reuses_both_parameter_proofs() {
        let (mut resolved, node) = crate::literal::tests::resolved_integer("42", false, "i64");
        let view = resolved.type_pool.register(type_pool::TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("ProofComparison"),
                parents: vec![],
                assoc_types: vec![],
            },
            type_id: type_pool::TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let declaration = resolved.type_pool.intern_structural(TypeKind::Function {
            params: vec![view, view],
            ret: Intrinsic::Bool.type_index(),
        });
        resolved
            .type_pool
            .register_trait_schema(TraitDispatchSchema {
                trait_type: view,
                slots: vec![TraitMethodKey {
                    trait_owner: view,
                    name: str_interner::intern("eq"),
                    signature: Some(TraitMethodSignature {
                        associated_paths: Vec::new(),
                        declaration,
                        self_paths: vec![
                            vec![TraitTypeStep::Parameter(0)],
                            vec![TraitTypeStep::Parameter(1)],
                        ],
                        parameter_kinds: vec![
                            TraitParameterKind::Receiver,
                            TraitParameterKind::Required,
                        ],
                    }),
                }],
            })
            .unwrap();
        let mut builder = FunctionBuilder::new(FuncId(0), str_interner::intern("compare"));
        let left = builder.alloc_local();
        let right = builder.alloc_local();
        for local in [left, right] {
            builder.params.push(NirParam {
                local,
                name: str_interner::intern("value"),
                type_index: view,
                role: NirParamRole::User,
            });
        }
        crate::trait_parameters::prepare_parameters(&resolved, &mut builder);
        let block = builder.new_block();
        assert!(implements(&resolved, node, view, view));
        let expression = call(
            &resolved,
            node,
            TraitMethodTarget {
                owner: view,
                view,
                name: "eq",
            },
            NirValue::Local(left),
            vec![NirValue::Local(right)],
            &mut builder,
            block,
        );
        assert!(
            matches!(expression.unscoped(), NirExpr::TraitCall { receiver, proof, view: selected, slot: 0, args } if *receiver == NirValue::Local(left) && *proof == NirValue::Local(builder.value_proofs[&left].0) && *selected == view && *args == vec![NirValue::Local(builder.value_proofs[&right].0), NirValue::Local(right)])
        );
        assert!(
            builder.blocks[0].stmts.is_empty(),
            "dispatch must not reacquire in the callee scope"
        );
    }
}
