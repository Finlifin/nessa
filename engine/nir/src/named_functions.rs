//! Named functions use standalone frames; lexical capture requires a lambda.

use std::collections::HashMap;

use ast::NodeKind;
use nsbc::GlobalId;
use resolution::{ResolvedAst, ScopeId, SymbolId, SymbolKind};

use crate::LoweringError;

pub(crate) fn validate(
    resolved: &ResolvedAst,
    globals: &HashMap<SymbolId, GlobalId>,
) -> Result<(), Vec<LoweringError>> {
    let mut errors = Vec::new();
    for function_scope in &resolved.scopes {
        if function_scope.node.is_null()
            || resolved.ast.node(function_scope.node).kind != NodeKind::FunctionDef
        {
            continue;
        }
        let body = resolved.ast.fixed_children(function_scope.node)[2];
        let mut pending = vec![body];
        while let Some(node) = pending.pop() {
            if node.is_null() || resolved.ast.node(node).kind == NodeKind::FunctionDef {
                continue;
            }
            if resolved.ast.node(node).kind == NodeKind::NamedArg {
                pending.push(resolution::argument_value_node(&resolved.ast, node));
                continue;
            }
            if matches!(
                resolved.ast.node(node).kind,
                NodeKind::Id | NodeKind::SelfLower
            ) && let Some(symbol) = resolved.node_symbols.get(&node)
                && !globals.contains_key(symbol)
            {
                let definition = &resolved.symbols[symbol.0 as usize];
                if matches!(
                    definition.kind,
                    SymbolKind::Variable | SymbolKind::Parameter | SymbolKind::Constant
                ) && !within(resolved, function_scope.node, definition.scope)
                {
                    errors.push(LoweringError {
                        node,
                        message:
                            "named functions cannot capture enclosing local values; use a lambda"
                                .into(),
                    });
                }
            }
            pending.extend(
                resolved
                    .ast
                    .fixed_children(node)
                    .iter()
                    .chain(resolved.ast.multi_children(node))
                    .copied(),
            );
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn within(resolved: &ResolvedAst, owner: ast::NodeIndex, mut scope: ScopeId) -> bool {
    loop {
        if resolved.scopes[scope.0 as usize].node == owner {
            return true;
        }
        let Some(parent) = resolved.scopes[scope.0 as usize].parent else {
            return false;
        };
        scope = parent;
    }
}
