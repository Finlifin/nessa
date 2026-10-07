//! Static input refinements for stable continuation bindings.
//!
//! Bare Continuation remains an erased type. Proven catch provenance supplies
//! a resume input expectation; writes anywhere invalidate that provenance.

use std::collections::HashSet;

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::TypeIndex;

use crate::{SymbolId, resolver::Resolver};

pub(crate) fn input(r: &Resolver<'_>, ast: &Ast, node: NodeIndex) -> Option<TypeIndex> {
    if node.is_null() {
        return None;
    }
    match ast.node(node).kind {
        NodeKind::Id => r
            .continuation_inputs
            .get(r.node_symbols.get(&node)?)
            .copied(),
        NodeKind::Call if ast.multi_children(node).is_empty() => {
            let callee = ast.fixed_children(node)[0];
            if ast.node(callee).kind != NodeKind::Projection {
                return None;
            }
            let children = ast.fixed_children(callee);
            (str_interner::get(ast.node(children[1]).str_id) == "clone")
                .then(|| input(r, ast, children[0]))
                .flatten()
        }
        _ => None,
    }
}

pub(crate) fn prepare_inputs(r: &mut Resolver<'_>, ast: &Ast) {
    let mut assigned = HashSet::<SymbolId>::new();
    for (index, node) in ast.nodes.iter().enumerate() {
        if matches!(
            node.kind,
            NodeKind::Assign
                | NodeKind::AddAssign
                | NodeKind::SubAssign
                | NodeKind::MulAssign
                | NodeKind::DivAssign
                | NodeKind::ModAssign
        ) {
            let target = ast.fixed_children(NodeIndex(index as u32))[0];
            if let Some(&symbol) = r.node_symbols.get(&target) {
                assigned.insert(symbol);
            }
        }
    }
    for &arm in &r.handler_arms {
        let pattern = ast.fixed_children(arm)[0];
        if ast.node(pattern).kind != NodeKind::PatternCall {
            continue;
        }
        let callee = ast.fixed_children(pattern)[0];
        let Some(symbol) = r.node_symbols.get(&callee) else {
            continue;
        };
        let effect = r.symbols[symbol.0 as usize].type_index;
        let Some(operation) = r
            .effects
            .iter()
            .find(|info| info.type_index == effect)
            .and_then(|info| info.operations.first())
        else {
            continue;
        };
        let Some(position) = operation.continuation_param else {
            continue;
        };
        let Some(binding) = ast.multi_children(pattern).get(position) else {
            continue;
        };
        if let Some(&symbol) = r.node_symbols.get(binding)
            && !assigned.contains(&symbol)
        {
            r.continuation_inputs.insert(symbol, operation.return_type);
        }
    }
    // Aliases copy values, not variable identity. Require the source and target
    // to be stable, so inference cannot depend on traversal order or a branch.
    loop {
        let before = r.continuation_inputs.len();
        for (index, node) in ast.nodes.iter().enumerate() {
            if !matches!(
                node.kind,
                NodeKind::LetDecl | NodeKind::ConstDecl | NodeKind::VarDecl
            ) {
                continue;
            }
            let children = ast.fixed_children(NodeIndex(index as u32));
            if ast.node(children[0]).kind != NodeKind::Id {
                continue;
            }
            let Some(&symbol) = r.node_symbols.get(&children[0]) else {
                continue;
            };
            if assigned.contains(&symbol) {
                continue;
            }
            if let Some(input) = input(r, ast, children[2]) {
                r.continuation_inputs.insert(symbol, input);
            }
        }
        if r.continuation_inputs.len() == before {
            break;
        }
    }
}
