//! Conservative ownership proof for fresh continuation handler bindings.

use ast::{NodeIndex, NodeKind};
use resolution::{ResolvedAst, SymbolId};

/// A direct tail call can consume a freshly captured continuation if its
/// binding occurs exactly once in the whole handler body. Aliasing, storage,
/// cloning, nested closures and repeated calls retain the multi-shot path.
/// Other calls or implicit dispatch can suspend the handler and duplicate its
/// sole lexical reference, so only an explicitly non-suspending body qualifies.
pub(crate) fn single_use_handler_call(
    resolved: &ResolvedAst,
    body: NodeIndex,
    fresh_binding: Option<SymbolId>,
) -> Option<NodeIndex> {
    if body.is_null() {
        return None;
    }
    let ast = &resolved.ast;
    let mut tail = body;
    if ast.node(tail).kind == NodeKind::Block {
        tail = *ast.multi_children(tail).last()?;
    }
    if ast.node(tail).kind == NodeKind::ExprStatement {
        tail = ast.fixed_children(tail)[0];
    }
    if ast.node(tail).kind != NodeKind::Call {
        return None;
    }
    let callee = ast.fixed_children(tail)[0];
    if ast.node(callee).kind != NodeKind::Id {
        return None;
    }
    let symbol = *resolved.node_symbols.get(&callee)?;
    // Only the declared catch position provides fresh ownership. A regular
    // Continuation argument (even in a handler) may already have aliases.
    (fresh_binding == Some(symbol) && has_unique_unaliased_use(resolved, body, tail, symbol))
        .then_some(tail)
}

fn has_unique_unaliased_use(
    resolved: &ResolvedAst,
    body: NodeIndex,
    tail: NodeIndex,
    symbol: SymbolId,
) -> bool {
    let ast = &resolved.ast;
    let mut pending = vec![body];
    let mut references = 0;
    while let Some(node) = pending.pop() {
        if node.is_null() {
            continue;
        }
        let kind = ast.node(node).kind;
        if node != tail && !non_suspending_node(kind) {
            return false;
        }
        if kind == NodeKind::Id && resolved.node_symbols.get(&node) == Some(&symbol) {
            references += 1;
            if references > 1 {
                return false;
            }
        }
        pending.extend(ast.fixed_children(node));
        pending.extend(ast.multi_children(node));
    }
    references == 1
}

/// A closed whitelist avoids assuming new syntax or dispatched operators are
/// pure. Allocations and checked scalar conversions do not capture task stacks;
/// ordinary, native and indirect calls require a separate suspension proof.
/// Arithmetic and unary nodes below lower directly to built-in NIR operations
/// (expr::lower_arith/lower_unary), and TypeCast uses checked VM conversion.
/// Comparisons can dispatch source methods and are deliberately not included.
fn non_suspending_node(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Id
            | NodeKind::SelfLower
            | NodeKind::SelfUpper
            | NodeKind::Int
            | NodeKind::Real
            | NodeKind::Bool
            | NodeKind::Str
            | NodeKind::Char
            | NodeKind::Null
            | NodeKind::Unit
            | NodeKind::Tuple
            | NodeKind::Block
            | NodeKind::ExprStatement
            | NodeKind::TypeCast
            | NodeKind::Negative
            | NodeKind::BoolNot
            | NodeKind::Add
            | NodeKind::Sub
            | NodeKind::Mul
            | NodeKind::Div
            | NodeKind::Mod
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ast::Ast;
    use rustc_span::{
        DUMMY_SP,
        source_map::{FilePathMapping, SourceMap},
    };

    #[test]
    fn lexical_single_use_is_not_linear_after_direct_or_indirect_suspension() {
        let source_map = SourceMap::new(FilePathMapping::empty());
        let diagnostics = diagnostic::DiagnosticContext::new(&source_map);
        let mut ast = Ast::new();
        ast.root = ast.builder(NodeKind::FileScope, DUMMY_SP).build();
        let mut resolved = resolution::resolve(ast, &diagnostics);
        let binding = SymbolId(0);
        let callee = resolved
            .ast
            .builder(NodeKind::Id, DUMMY_SP)
            .set_str_id(str_interner::intern("k"))
            .build();
        resolved.node_symbols.insert(callee, binding);
        let value = resolved
            .ast
            .builder(NodeKind::Int, DUMMY_SP)
            .set_str_id(str_interner::intern("40"))
            .build();
        let tail = resolved
            .ast
            .builder(NodeKind::Call, DUMMY_SP)
            .add_child(callee)
            .add_multi_children(&[value])
            .build();
        assert_eq!(
            single_use_handler_call(&resolved, tail, Some(binding)),
            Some(tail)
        );
        let sum = resolved
            .ast
            .builder(NodeKind::Add, DUMMY_SP)
            .add_child(value)
            .add_child(value)
            .build();
        let arithmetic_tail = resolved
            .ast
            .builder(NodeKind::Call, DUMMY_SP)
            .add_child(callee)
            .add_multi_children(&[sum])
            .build();
        assert_eq!(
            single_use_handler_call(&resolved, arithmetic_tail, Some(binding)),
            Some(arithmetic_tail)
        );
        let inner = resolved
            .ast
            .builder(NodeKind::Id, DUMMY_SP)
            .set_str_id(str_interner::intern("inner"))
            .build();
        let call = resolved
            .ast
            .builder(NodeKind::Call, DUMMY_SP)
            .add_child(inner)
            .build();
        let suspension = resolved
            .ast
            .builder(NodeKind::EffectPropagation, DUMMY_SP)
            .add_child(call)
            .build();
        let body = resolved
            .ast
            .builder(NodeKind::Block, DUMMY_SP)
            .add_multi_children(&[suspension, tail])
            .build();
        assert_eq!(
            single_use_handler_call(&resolved, body, Some(binding)),
            None
        );
        let indirect = resolved
            .ast
            .builder(NodeKind::Block, DUMMY_SP)
            .add_multi_children(&[call, tail])
            .build();
        assert_eq!(
            single_use_handler_call(&resolved, indirect, Some(binding)),
            None
        );
        let comparison = resolved
            .ast
            .builder(NodeKind::BoolEq, DUMMY_SP)
            .add_child(value)
            .add_child(value)
            .build();
        let dispatched = resolved
            .ast
            .builder(NodeKind::Call, DUMMY_SP)
            .add_child(callee)
            .add_multi_children(&[comparison])
            .build();
        assert_eq!(
            single_use_handler_call(&resolved, dispatched, Some(binding)),
            None
        );
        // Even the resume value can suspend before the original continuation is consumed.
        let argument = resolved
            .ast
            .builder(NodeKind::Call, DUMMY_SP)
            .add_child(callee)
            .add_multi_children(&[call])
            .build();
        assert_eq!(
            single_use_handler_call(&resolved, argument, Some(binding)),
            None
        );
    }
}
