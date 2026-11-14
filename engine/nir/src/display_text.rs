//! Display text of a value for string interpolation. Statically typed values
//! use their checked Display implementation; `Any` values acquire a runtime
//! Display proof, so a value without one raises a type error.

use ast::NodeIndex;
use resolution::ResolvedAst;
use type_pool::{Intrinsic, TypeIndex};

use crate::builder::FunctionBuilder;
use crate::trait_dispatch::{TraitMethodTarget, schema_slot};
use crate::{BlockId, NirExpr, NirStmt, NirValue};

/// Return a String value holding the Display text of `value`, whose static
/// type is `ty`. `node` supplies the lexical scope of the conversion.
pub(crate) fn lower(
    resolved: &ResolvedAst,
    node: NodeIndex,
    value: NirValue,
    ty: Option<TypeIndex>,
    builder: &mut FunctionBuilder,
    block: BlockId,
) -> NirValue {
    let display = resolved.type_pool.well_known.display;
    let ty = ty.and_then(|ty| resolved.type_pool.canonical_type(ty));
    let intrinsic = ty.and_then(|ty| resolved.type_pool.as_intrinsic(ty));
    if intrinsic == Some(Intrinsic::Str) {
        return value;
    }
    let expression = match ty {
        Some(owner) if intrinsic != Some(Intrinsic::Any) => crate::trait_dispatch::call(
            resolved,
            node,
            TraitMethodTarget {
                owner,
                view: display,
                name: "to_string",
            },
            value,
            Vec::new(),
            builder,
            block,
        ),
        _ => {
            let scope = resolved.node_scopes[&node].0;
            let value = crate::arguments::snapshot(value, builder, block);
            let proof = builder.alloc_local();
            builder.blocks[block.0 as usize].stmts.push(NirStmt::Assign(
                proof,
                NirExpr::TraitProof(value, display).in_scope(scope),
            ));
            NirExpr::TraitCall {
                receiver: value,
                proof: NirValue::Local(proof),
                view: display,
                slot: schema_slot(resolved, display, "to_string"),
                args: Vec::new(),
            }
            .in_scope(scope)
        }
    };
    let text = builder.alloc_local();
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(text, expression));
    NirValue::Local(text)
}
