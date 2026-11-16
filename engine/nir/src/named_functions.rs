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
                    let fn_name_node = resolved.ast.fixed_children(function_scope.node)[0];
                    let fn_name = if fn_name_node.is_null() {
                        "f".to_string()
                    } else {
                        str_interner::get(resolved.ast.node(fn_name_node).str_id)
                    };
                    let captured_name = str_interner::get(definition.name);
                    let note_node = (!definition.def_node.is_null()).then(|| {
                        (
                            definition.def_node,
                            format!("outer local `{captured_name}` declared here"),
                        )
                    });
                    errors.push(LoweringError {
                        node,
                        message:
                            "named functions cannot capture enclosing local values; use a lambda"
                                .into(),
                        code: Some(5002),
                        note_node,
                        help: Some(format!(
                            "replace `fn {fn_name}(...)` with a closure binding: `let {fn_name} = |...| {{ ... }}`"
                        )),
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
