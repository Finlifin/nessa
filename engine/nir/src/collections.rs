//! Checked indexing selects the collection ABI after evaluating receiver and key.

use ast::NodeIndex;
use resolution::ResolvedAst;
use type_pool::CollectionRole;

use crate::builder::FunctionBuilder;
use crate::{BlockId, NirExpr, NirStmt, NirValue};

fn role(
    resolved: &ResolvedAst,
    callee: NodeIndex,
    builder: &FunctionBuilder,
) -> Option<CollectionRole> {
    builder
        .node_type(resolved, callee)
        .and_then(|ty| resolved.type_pool.collection_role(ty))
}

pub(crate) fn index_access(
    resolved: &ResolvedAst,
    callee: NodeIndex,
    receiver: NirValue,
    arguments: &[NirValue],
    builder: &FunctionBuilder,
) -> Option<NirExpr> {
    match role(resolved, callee, builder)? {
        // Resolution checks the single positional key before lowering starts.
        CollectionRole::List => Some(NirExpr::IndexAccess(receiver, arguments[0])),
        CollectionRole::Map => Some(NirExpr::CallBuiltin(
            runtime::ids::MAP_GET,
            vec![receiver, arguments[0]],
        )),
        _ => None,
    }
}

pub(crate) fn store_index(
    resolved: &ResolvedAst,
    callee: NodeIndex,
    receiver: NirValue,
    arguments: &[NirValue],
    value: NirValue,
    builder: &mut FunctionBuilder,
    block: BlockId,
) -> bool {
    let statement = match role(resolved, callee, builder) {
        Some(CollectionRole::List) => NirStmt::StoreIndex(receiver, arguments[0], value),
        Some(CollectionRole::Map) => {
            let result = builder.alloc_local();
            NirStmt::Assign(
                result,
                NirExpr::CallBuiltin(runtime::ids::MAP_SET, vec![receiver, arguments[0], value]),
            )
        }
        _ => return false,
    };
    builder.blocks[block.0 as usize].stmts.push(statement);
    true
}
