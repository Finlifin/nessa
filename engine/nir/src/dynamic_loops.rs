//! `for` over an `Any` input selects the iterator protocol from the runtime
//! type. `IntoIterator` takes precedence; otherwise the value itself must be
//! an `Iterator`. Both selections use trait proofs in the loop's lexical
//! query scope, so a value implementing neither raises a type error.

use crate::arguments::snapshot;
use crate::builder::FunctionBuilder;
use crate::trait_dispatch::schema_slot as slot;
use crate::{BlockId, NirExpr, NirLocal, NirStmt, NirValue, Terminator};

fn assign(builder: &mut FunctionBuilder, block: BlockId, local: NirLocal, expression: NirExpr) {
    builder.blocks[block.0 as usize]
        .stmts
        .push(NirStmt::Assign(local, expression));
}

/// Store the selected iterator in `iterator` and return the expression that
/// produces each step. `block` continues after the selection.
pub(crate) fn prepare(
    resolved: &resolution::ResolvedAst,
    iterable: NirValue,
    iterator: NirLocal,
    scope: u32,
    builder: &mut FunctionBuilder,
    block: &mut BlockId,
) -> NirExpr {
    let known = resolved.type_pool.well_known;
    let iterable = snapshot(iterable, builder, *block);
    let convertible = builder.alloc_local();
    assign(
        builder,
        *block,
        convertible,
        NirExpr::TypeCheck(iterable, known.into_iterator).in_scope(scope),
    );
    let convert_block = builder.new_block();
    let direct_block = builder.new_block();
    let ready_block = builder.new_block();
    builder.blocks[block.0 as usize].terminator =
        Terminator::Branch(NirValue::Local(convertible), convert_block, direct_block);

    let proof = builder.alloc_local();
    assign(
        builder,
        convert_block,
        proof,
        NirExpr::TraitProof(iterable, known.into_iterator).in_scope(scope),
    );
    assign(
        builder,
        convert_block,
        iterator,
        NirExpr::TraitCall {
            receiver: iterable,
            proof: NirValue::Local(proof),
            view: known.into_iterator,
            slot: slot(resolved, known.into_iterator, "into_iter"),
            args: Vec::new(),
        }
        .in_scope(scope),
    );
    builder.blocks[convert_block.0 as usize].terminator = Terminator::Goto(ready_block);

    assign(builder, direct_block, iterator, NirExpr::Use(iterable));
    builder.blocks[direct_block.0 as usize].terminator = Terminator::Goto(ready_block);

    let iterator_proof = builder.alloc_local();
    assign(
        builder,
        ready_block,
        iterator_proof,
        NirExpr::TraitProof(NirValue::Local(iterator), known.iterator).in_scope(scope),
    );
    *block = ready_block;
    NirExpr::TraitCall {
        receiver: NirValue::Local(iterator),
        proof: NirValue::Local(iterator_proof),
        view: known.iterator,
        slot: slot(resolved, known.iterator, "next"),
        args: Vec::new(),
    }
    .in_scope(scope)
}
