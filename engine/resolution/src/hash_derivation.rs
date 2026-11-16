//! Resolution and prerequisite validation for `derive Hash`.

use std::collections::HashSet;

use ast::{Ast, NodeIndex, NodeKind};
use str_interner::StrId;
use type_pool::{DERIVE_FUNC_ID, Intrinsic, TypeIndex, TypeKind};

use crate::SymbolKind;
use crate::resolver::Resolver;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedHashPlan {
    pub node: NodeIndex,
    pub implementor: TypeIndex,
    pub trait_type: TypeIndex,
    pub method_name: StrId,
    pub signature: TypeIndex,
}

pub(crate) fn register_hash(r: &mut Resolver<'_>, node: NodeIndex, implementor: TypeIndex) {
    let signature = r.type_pool.intern_structural(TypeKind::Function {
        params: vec![implementor],
        ret: Intrinsic::U64.type_index(),
    });
    r.hash_derivations.push(DerivedHashPlan {
        node,
        implementor,
        trait_type: r.type_pool.well_known.hash,
        method_name: str_interner::intern("hash"),
        signature,
    });
}

pub(crate) fn declared_signature(
    r: &mut Resolver<'_>,
    ast: &Ast,
    receiver: TypeIndex,
    name: StrId,
) -> Option<TypeIndex> {
    if name != str_interner::intern("hash") {
        return None;
    }
    let receiver = r.type_pool.canonical_type(receiver)?;
    let denotes = |node: NodeIndex| {
        r.node_type_values
            .get(&node)
            .copied()
            .or_else(|| {
                r.node_symbols
                    .get(&node)
                    .map(|id| r.symbols[id.0 as usize].type_index)
            })
            .and_then(|ty| r.type_pool.canonical_type(ty))
    };
    let has_derive = (1..ast.nodes.len()).any(|raw| {
        let idx = NodeIndex(raw as u32);
        ast.node(idx).kind == NodeKind::DeriveDef
            && denotes(ast.fixed_children(idx)[0]) == Some(receiver)
            && ast
                .multi_children(idx)
                .iter()
                .any(|&node| denotes(node) == Some(r.type_pool.well_known.hash))
    });
    has_derive.then(|| {
        r.register_type(TypeKind::Function {
            params: Vec::new(),
            ret: Intrinsic::U64.type_index(),
        })
    })
}

pub(crate) fn validate(r: &mut Resolver<'_>, ast: &Ast) {
    let plans = r.hash_derivations.clone();
    for plan in plans {
        let mut seen = HashSet::from([plan.implementor]);
        match r.type_pool.get(plan.implementor).kind.clone() {
            TypeKind::Struct { fields, .. } => {
                for field in fields {
                    if !has_checked_hash(r, field.ty, &mut seen) {
                        let ty_name = crate::typing::type_display_name(&r.type_pool, field.ty);
                        report(
                            r,
                            ast,
                            plan.node,
                            format!(
                                "Hash derivation requires a checked global Hash implementation for field type `{ty_name}`"
                            ),
                        );
                    }
                }
            }
            TypeKind::Enum { variants, .. } => {
                for variant in variants {
                    for field in variant.fields {
                        if !has_checked_hash(r, field.ty, &mut seen) {
                            let ty_name = crate::typing::type_display_name(&r.type_pool, field.ty);
                            report(
                                r,
                                ast,
                                plan.node,
                                format!(
                                    "Hash derivation requires a checked global Hash implementation for variant field type `{ty_name}`"
                                ),
                            );
                        }
                    }
                }
            }
            TypeKind::Tuple { elements } => {
                for elem in elements {
                    if !has_checked_hash(r, elem, &mut seen) {
                        let ty_name = crate::typing::type_display_name(&r.type_pool, elem);
                        report(
                            r,
                            ast,
                            plan.node,
                            format!(
                                "Hash derivation requires a checked global Hash implementation for tuple element type `{ty_name}`"
                            ),
                        );
                    }
                }
            }
            TypeKind::Newtype { inner, .. } => {
                if !has_checked_hash(r, inner, &mut seen) {
                    let ty_name = crate::typing::type_display_name(&r.type_pool, inner);
                    report(
                        r,
                        ast,
                        plan.node,
                        format!(
                            "Hash derivation requires a checked global Hash implementation for newtype inner type `{ty_name}`"
                        ),
                    );
                }
            }
            _ => {
                report(
                    r,
                    ast,
                    plan.node,
                    "Hash derivation supports only struct, enum, tuple typealias, and newtype targets",
                );
            }
        }
    }
}

fn has_checked_hash(r: &Resolver<'_>, ty: TypeIndex, seen: &mut HashSet<TypeIndex>) -> bool {
    let Some(canon) = r.type_pool.canonical_type(ty) else {
        return false;
    };
    if r.type_pool.is_reserved_collection_role(canon)
        && r.type_pool.collection_role(canon) != Some(type_pool::CollectionRole::Bytes)
    {
        return false;
    }
    if let Some(intr) = r.type_pool.as_intrinsic(canon)
        && matches!(
            intr,
            Intrinsic::Any | Intrinsic::Closure | Intrinsic::Continuation | Intrinsic::NoReturn
        )
    {
        return false;
    }
    if seen.contains(&canon) {
        return true;
    }
    if checked_hash_method(r, canon) {
        return true;
    }
    match r.type_pool.get(canon).kind.clone() {
        TypeKind::Optional { inner } => {
            if r.type_pool.as_intrinsic(inner) == Some(Intrinsic::NoReturn) {
                return true;
            }
            let mut next = seen.clone();
            next.insert(canon);
            has_checked_hash(r, inner, &mut next)
        }
        TypeKind::Tuple { elements } => elements.iter().all(|&elem| {
            let mut next = seen.clone();
            next.insert(canon);
            has_checked_hash(r, elem, &mut next)
        }),
        _ => false,
    }
}

fn checked_hash_method(r: &Resolver<'_>, owner: TypeIndex) -> bool {
    let Some(slot) = r.type_pool.find_trait_method(
        owner,
        r.type_pool.well_known.hash,
        str_interner::intern("hash"),
    ) else {
        return false;
    };
    if slot.func_id == DERIVE_FUNC_ID {
        return r
            .hash_derivations
            .iter()
            .any(|plan| plan.implementor == owner);
    }
    let Some(sym) = r.symbols.get(slot.func_id as usize) else {
        return false;
    };
    if sym.kind != SymbolKind::Function || sym.type_index == TypeIndex::INVALID {
        return false;
    }
    let Some(sig) = r.type_pool.canonical_type(sym.type_index) else {
        return false;
    };
    match &r.type_pool.get(sig).kind {
        TypeKind::Function { params, ret } => {
            params.len() == 1
                && r.type_pool.canonical_type(params[0]) == Some(owner)
                && r.type_pool.canonical_type(*ret) == Some(Intrinsic::U64.type_index())
        }
        _ => false,
    }
}

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: impl Into<String>) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}
