//! Alternative patterns share one lexical identity for every successful binding.

use std::collections::HashMap;

use ast::{Ast, NodeIndex, NodeKind};
use str_interner::StrId;

use crate::{SymbolKind, resolver::Resolver};

type Bindings = HashMap<StrId, Vec<NodeIndex>>;

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: &str) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}

fn merge(r: &Resolver<'_>, ast: &Ast, target: &mut Bindings, source: Bindings) {
    for (name, nodes) in source {
        if target.contains_key(&name) {
            report(
                r,
                ast,
                nodes[0],
                "pattern binds the same name more than once",
            );
        }
        target.entry(name).or_default().extend(nodes);
    }
}

/// Guards and constructor targets are expressions, never binding declarations.
fn collect(r: &Resolver<'_>, ast: &Ast, node: NodeIndex) -> Bindings {
    if node.is_null() {
        return Bindings::new();
    }
    let children = ast.fixed_children(node);
    match ast.node(node).kind {
        NodeKind::Id => {
            let name = ast.node(node).str_id;
            if r.node_symbols
                .get(&node)
                .copied()
                .or_else(|| r.lookup(name))
                .is_some_and(|symbol| r.symbols[symbol.0 as usize].kind == SymbolKind::EnumVariant)
            {
                Bindings::new()
            } else {
                HashMap::from([(name, vec![node])])
            }
        }
        NodeKind::PatternOr => {
            let mut left = collect(r, ast, children[0]);
            let right = collect(r, ast, children[1]);
            if left.len() != right.len() || left.keys().any(|name| !right.contains_key(name)) {
                report(
                    r,
                    ast,
                    node,
                    "or pattern alternatives must bind the same names",
                );
            }
            // Repeated names across alternatives denote the same binding.
            for (name, nodes) in right {
                left.entry(name).or_default().extend(nodes);
            }
            left
        }
        NodeKind::PatternAsBind => {
            let mut bindings = collect(r, ast, children[0]);
            let alias = children[1];
            if ast.node(alias).kind != NodeKind::Id {
                report(r, ast, alias, "as pattern requires an identifier binding");
            } else {
                merge(
                    r,
                    ast,
                    &mut bindings,
                    HashMap::from([(ast.node(alias).str_id, vec![alias])]),
                );
            }
            bindings
        }
        NodeKind::PatternIfGuard
        | NodeKind::PatternOptionSome
        | NodeKind::PatternError
        | NodeKind::PatternErrorOk => collect(r, ast, children[0]),
        NodeKind::PatternAndIs => {
            let mut bindings = collect(r, ast, children[0]);
            merge(r, ast, &mut bindings, collect(r, ast, children[2]));
            bindings
        }
        NodeKind::PatternRestBind => {
            let binding = children[0];
            if binding.is_null() || ast.node(binding).kind != NodeKind::Id {
                Bindings::new()
            } else {
                HashMap::from([(ast.node(binding).str_id, vec![binding])])
            }
        }
        NodeKind::PatternTuple | NodeKind::PatternCall | NodeKind::PatternList => {
            let mut bindings = Bindings::new();
            for &child in ast.multi_children(node) {
                merge(r, ast, &mut bindings, collect(r, ast, child));
            }
            bindings
        }
        // Negated predicates have their own private binding preparation in
        // name resolution; none of their names belongs to the outer path.
        NodeKind::PatternNot => Bindings::new(),
        _ => Bindings::new(),
    }
}

pub(crate) fn prepare(r: &mut Resolver<'_>, ast: &Ast, pattern: NodeIndex) {
    // Allocate before resolving either guard so both paths and nested captures
    // use the same SymbolId; no AST references or closure facts need remapping.
    let mut bindings: Vec<_> = collect(r, ast, pattern).into_iter().collect();
    bindings.sort_by_key(|(name, _)| name.as_u32());
    for (name, nodes) in bindings {
        let symbol = r.alloc_unbound_symbol(name, SymbolKind::Variable, nodes[0]);
        for node in nodes {
            r.node_symbols.insert(node, symbol);
        }
    }
}

pub(crate) fn check_alternative_types(r: &Resolver<'_>, ast: &Ast, pattern: NodeIndex) {
    let children = ast.fixed_children(pattern);
    let left = collect(r, ast, children[0]);
    let right = collect(r, ast, children[1]);
    for (name, left_nodes) in left {
        let Some(right_nodes) = right.get(&name) else {
            continue;
        };
        let left_type = r
            .node_types
            .get(&left_nodes[0])
            .and_then(|&ty| r.type_pool.canonical_type(ty));
        let right_type = r
            .node_types
            .get(&right_nodes[0])
            .and_then(|&ty| r.type_pool.canonical_type(ty));
        if left_type != right_type {
            report(
                r,
                ast,
                pattern,
                "or pattern alternatives must bind each name with the same type",
            );
        }
    }
}
