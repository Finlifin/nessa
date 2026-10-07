//! Shared bounds for inlined declaration defaults.

use crate::CallArgumentValue;
use crate::resolver::Resolver;
use ast::{Ast, NodeIndex, NodeKind};
use std::collections::{HashMap, HashSet};

const MAX_DEFAULT_EXPANSION_DEPTH: usize = 256;
const MAX_DEFAULT_EXPANSIONS: usize = 262_144;

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: impl Into<String>) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}

pub(crate) fn validate_default_control(r: &Resolver<'_>, ast: &Ast, expression: NodeIndex) {
    let mut pending = vec![(expression, 0usize)];
    while let Some((node, loop_depth)) = pending.pop() {
        if node.is_null() {
            continue;
        }
        let kind = ast.node(node).kind;
        // Nested callable and handler bodies have their own control context.
        if matches!(
            kind,
            NodeKind::FunctionDef | NodeKind::TraitDeriveFn | NodeKind::Lambda
        ) || (kind == NodeKind::CaseArm && r.handler_arms.contains(&node))
        {
            continue;
        }
        if matches!(
            kind,
            NodeKind::ReturnStatement
                | NodeKind::ResumeStatement
                | NodeKind::OptionPropagation
                | NodeKind::ErrorPropagation
        ) {
            report(
                r,
                ast,
                node,
                if kind == NodeKind::ErrorPropagation {
                    "Error propagation cannot cross a parameter default boundary"
                } else if kind == NodeKind::OptionPropagation {
                    "Optional propagation cannot cross a parameter default boundary"
                } else {
                    "return/resume cannot cross a parameter default boundary"
                },
            );
        }
        if matches!(kind, NodeKind::BreakStatement | NodeKind::ContinueStatement) && loop_depth == 0
        {
            report(
                r,
                ast,
                node,
                "break/continue in a parameter default require a loop inside that default",
            );
        }
        let loop_depth =
            loop_depth + usize::from(matches!(kind, NodeKind::WhileLoop | NodeKind::ForLoop));
        pending.extend(
            ast.fixed_children(node)
                .iter()
                .map(|&child| (child, loop_depth)),
        );
        pending.extend(
            ast.multi_children(node)
                .iter()
                .map(|&child| (child, loop_depth)),
        );
    }
}

pub(crate) fn validate_default_expansions(r: &Resolver<'_>, ast: &Ast) {
    let expressions: HashSet<_> = r
        .call_arguments
        .values()
        .flat_map(|plan| &plan.parameters)
        .filter_map(|binding| match binding.value {
            CallArgumentValue::Default { expression, .. } => Some(expression),
            CallArgumentValue::Explicit { .. }
            | CallArgumentValue::Variadic { .. }
            | CallArgumentValue::MapVariadic { .. } => None,
        })
        .chain(
            r.struct_constructions
                .values()
                .flat_map(|plan| &plan.fields)
                .filter_map(|binding| match binding.value {
                    crate::StructFieldValue::Default { expression } => Some(expression),
                    _ => None,
                }),
        )
        .collect();
    let mut graph = HashMap::new();
    for &expression in &expressions {
        let mut edges = Vec::new();
        let mut pending = vec![expression];
        while let Some(node) = pending.pop() {
            if node.is_null() {
                continue;
            }
            if let Some(plan) = r.call_arguments.get(&node) {
                for binding in &plan.parameters {
                    if let CallArgumentValue::Default { expression, .. } = binding.value {
                        edges.push(expression);
                    }
                }
            }
            if let Some(plan) = r.struct_constructions.get(&node) {
                for binding in &plan.fields {
                    if let crate::StructFieldValue::Default { expression } = binding.value {
                        edges.push(expression);
                    }
                }
            }
            pending.extend(ast.fixed_children(node));
            pending.extend(ast.multi_children(node));
        }
        graph.insert(expression, edges);
    }
    let mut states = HashMap::new();
    let mut heights = HashMap::new();
    let mut costs = HashMap::new();
    for &expression in &expressions {
        if states.contains_key(&expression) {
            continue;
        }
        let mut pending = vec![(expression, false)];
        while let Some((node, exiting)) = pending.pop() {
            if exiting {
                let mut cost = 1usize;
                for child in &graph[&node] {
                    let Some(next) = cost
                        .checked_add(costs[child])
                        .filter(|&next| next <= MAX_DEFAULT_EXPANSIONS)
                    else {
                        report(
                            r,
                            ast,
                            node,
                            "default argument expansion exceeds the compiler size limit",
                        );
                        return;
                    };
                    cost = next;
                }
                let height = graph[&node]
                    .iter()
                    .map(|child| heights[child] + 1usize)
                    .max()
                    .unwrap_or(0);
                if height > MAX_DEFAULT_EXPANSION_DEPTH {
                    report(
                        r,
                        ast,
                        node,
                        "default argument expansion exceeds the compiler nesting limit",
                    );
                    return;
                }
                heights.insert(node, height);
                costs.insert(node, cost);
                states.insert(node, 2);
            } else {
                match states.get(&node) {
                    Some(2) => continue,
                    Some(1) => {
                        report(
                            r,
                            ast,
                            node,
                            "recursive default argument expansion is not supported",
                        );
                        return;
                    }
                    _ => {}
                }
                states.insert(node, 1);
                pending.push((node, true));
                pending.extend(graph[&node].iter().map(|&child| (child, false)));
            }
        }
    }
}
