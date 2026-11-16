//! Normalize trailing callbacks before any scope or call facts are created.
//!
//! The parser preserves the source PostDo form. Resolution uses ordinary
//! Call/Lambda nodes so argument plans, contextual typing, control boundaries,
//! default adapters and initialization all observe the same expression.

use std::collections::HashSet;

use ast::{Ast, NodeIndex, NodeKind};

pub(crate) fn normalize(ast: &mut Ast) -> HashSet<NodeIndex> {
    let mut pipeline_calls = HashSet::new();
    let mut pending = vec![(ast.root, false)];
    while let Some((node, expanded)) = pending.pop() {
        if node.is_null() {
            continue;
        }
        if !expanded {
            pending.push((node, true));
            pending.extend(
                ast.fixed_children(node)
                    .iter()
                    .chain(ast.multi_children(node))
                    .copied()
                    .map(|child| (child, false)),
            );
        } else {
            match ast.node(node).kind {
                NodeKind::PostDo => normalize_call(ast, node, &mut pipeline_calls),
                NodeKind::Pipeline => normalize_pipeline(ast, node, &mut pipeline_calls),
                _ => {}
            }
        }
    }
    pipeline_calls
}

fn normalize_call(ast: &mut Ast, node: NodeIndex, pipeline_calls: &mut HashSet<NodeIndex>) {
    let children = ast.node(node).children;
    let left = children[0];
    let body = children[1];
    let callback = if ast.node(body).kind == NodeKind::Block {
        ast.builder(NodeKind::Lambda, ast.node(body).span)
            .add_child(body)
            .build()
    } else {
        body
    };
    let (callee, mut arguments) = if ast.node(left).kind == NodeKind::Call {
        if pipeline_calls.remove(&left) {
            pipeline_calls.insert(node);
        }
        (
            ast.fixed_children(left)[0],
            ast.multi_children(left).to_vec(),
        )
    } else {
        (left, Vec::new())
    };
    arguments.push(callback);
    let start = u32::try_from(ast.extra_children.len()).expect("AST child indices fit u32");
    let count = u32::try_from(arguments.len()).expect("AST child counts fit u32");
    ast.extra_children.extend(arguments);
    // Preserve the source expression's identity and span. In particular, exact
    // package/import source-node permissions must never be recreated by name.
    let expression = &mut ast.nodes[node.0 as usize];
    expression.kind = NodeKind::Call;
    expression.children = [callee, NodeIndex::NULL, NodeIndex::NULL, NodeIndex::NULL];
    expression.multi_start = start;
    expression.multi_len = count;
}

fn normalize_pipeline(ast: &mut Ast, node: NodeIndex, pipeline_calls: &mut HashSet<NodeIndex>) {
    let children = ast.node(node).children;
    let lhs = children[0];
    let rhs = children[1];
    let is_rhs_call = !rhs.is_null() && ast.node(rhs).kind == NodeKind::Call;
    let (callee, mut arguments) = if is_rhs_call {
        (
            ast.fixed_children(rhs)[0],
            Vec::with_capacity(1 + ast.multi_children(rhs).len()),
        )
    } else {
        (rhs, Vec::with_capacity(1))
    };
    arguments.push(lhs);
    if is_rhs_call {
        arguments.extend_from_slice(ast.multi_children(rhs));
    }
    let start = u32::try_from(ast.extra_children.len()).expect("AST child indices fit u32");
    let count = u32::try_from(arguments.len()).expect("AST child counts fit u32");
    ast.extra_children.extend(arguments);
    let expression = &mut ast.nodes[node.0 as usize];
    expression.kind = NodeKind::Call;
    expression.children = [callee, NodeIndex::NULL, NodeIndex::NULL, NodeIndex::NULL];
    expression.multi_start = start;
    expression.multi_len = count;
    pipeline_calls.insert(node);
}
