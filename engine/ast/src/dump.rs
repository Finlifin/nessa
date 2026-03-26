use std::fmt::Write;

use crate::{Ast, NodeIndex, NodeType};

/// Dump the AST rooted at `root` as an S-expression string (compact, single line).
pub fn dump_ast_to_string(ast: &Ast, root: NodeIndex) -> String {
    let mut buf = String::new();
    dump_node(ast, root, &mut buf);
    buf
}

/// Dump the AST rooted at `root` as a pretty-printed, indented S-expression.
pub fn dump_ast_to_string_pretty(ast: &Ast, root: NodeIndex) -> String {
    let mut buf = String::new();
    dump_node_pretty(ast, root, &mut buf, 0);
    buf.push('\n');
    buf
}

/// Dump the AST rooted at `root` as an S-expression to stdout.
pub fn dump_ast(ast: &Ast, root: NodeIndex) {
    println!("{}", dump_ast_to_string(ast, root));
}

fn dump_node(ast: &Ast, index: NodeIndex, buf: &mut String) {
    if index.is_null() {
        buf.push_str("nil");
        return;
    }

    let node = ast.node(index);
    let kind_name = format!("{:?}", node.kind);
    let nt = node.kind.node_type();

    match nt {
        NodeType::NoChild => {
            // Leaf node – print source text if available, otherwise just the kind.
            let text = span_text(ast, node.span);
            if let Some(t) = text {
                let _ = write!(buf, "({kind_name} \"{t}\")");
            } else {
                let _ = write!(buf, "({kind_name})");
            }
        }

        NodeType::SingleChild => {
            let _ = write!(buf, "({kind_name} ");
            dump_node(ast, node.children[0], buf);
            buf.push(')');
        }

        NodeType::DoubleChildren => {
            let _ = write!(buf, "({kind_name}");
            for &child in &node.children[..2] {
                buf.push(' ');
                dump_node(ast, child, buf);
            }
            buf.push(')');
        }

        NodeType::TripleChildren => {
            let _ = write!(buf, "({kind_name}");
            for &child in &node.children[..3] {
                buf.push(' ');
                dump_node(ast, child, buf);
            }
            buf.push(')');
        }

        NodeType::QuadrupleChildren => {
            let _ = write!(buf, "({kind_name}");
            for &child in &node.children[..4] {
                buf.push(' ');
                dump_node(ast, child, buf);
            }
            buf.push(')');
        }

        NodeType::MultiChildren => {
            let multi = ast.multi_children(index);
            let _ = write!(buf, "({kind_name}");
            for &child in multi {
                buf.push(' ');
                dump_node(ast, child, buf);
            }
            buf.push(')');
        }

        NodeType::SingleWithMulti => {
            let multi = ast.multi_children(index);
            let _ = write!(buf, "({kind_name} ");
            dump_node(ast, node.children[0], buf);
            for &child in multi {
                buf.push(' ');
                dump_node(ast, child, buf);
            }
            buf.push(')');
        }

        NodeType::DoubleWithMulti => {
            let multi = ast.multi_children(index);
            let _ = write!(buf, "({kind_name}");
            for &child in &node.children[..2] {
                buf.push(' ');
                dump_node(ast, child, buf);
            }
            for &child in multi {
                buf.push(' ');
                dump_node(ast, child, buf);
            }
            buf.push(')');
        }

        NodeType::TripleWithMulti => {
            let multi = ast.multi_children(index);
            let _ = write!(buf, "({kind_name}");
            for &child in &node.children[..3] {
                buf.push(' ');
                dump_node(ast, child, buf);
            }
            for &child in multi {
                buf.push(' ');
                dump_node(ast, child, buf);
            }
            buf.push(')');
        }

        NodeType::QuadrupleWithMulti => {
            let multi = ast.multi_children(index);
            let _ = write!(buf, "({kind_name}");
            for &child in &node.children[..4] {
                buf.push(' ');
                dump_node(ast, child, buf);
            }
            for &child in multi {
                buf.push(' ');
                dump_node(ast, child, buf);
            }
            buf.push(')');
        }
    }
}

// ---------------------------------------------------------------------------
// Pretty-print (indented) S-expression dump
// ---------------------------------------------------------------------------

fn dump_node_pretty(ast: &Ast, index: NodeIndex, buf: &mut String, depth: usize) {
    if index.is_null() {
        buf.push_str("nil");
        return;
    }

    let node = ast.node(index);
    let kind_name = format!("{:?}", node.kind);
    let nt = node.kind.node_type();

    // Collect all children into a flat vec for uniform handling.
    let mut children: Vec<NodeIndex> = Vec::new();
    match nt {
        NodeType::NoChild => {}
        NodeType::SingleChild => children.extend_from_slice(&node.children[..1]),
        NodeType::DoubleChildren => children.extend_from_slice(&node.children[..2]),
        NodeType::TripleChildren => children.extend_from_slice(&node.children[..3]),
        NodeType::QuadrupleChildren => children.extend_from_slice(&node.children[..4]),
        NodeType::MultiChildren => children.extend_from_slice(ast.multi_children(index)),
        NodeType::SingleWithMulti => {
            children.extend_from_slice(&node.children[..1]);
            children.extend_from_slice(ast.multi_children(index));
        }
        NodeType::DoubleWithMulti => {
            children.extend_from_slice(&node.children[..2]);
            children.extend_from_slice(ast.multi_children(index));
        }
        NodeType::TripleWithMulti => {
            children.extend_from_slice(&node.children[..3]);
            children.extend_from_slice(ast.multi_children(index));
        }
        NodeType::QuadrupleWithMulti => {
            children.extend_from_slice(&node.children[..4]);
            children.extend_from_slice(ast.multi_children(index));
        }
    }

    if children.is_empty() {
        // Leaf node.
        let text = span_text(ast, node.span);
        if let Some(t) = text {
            let _ = write!(buf, "({kind_name} \"{t}\")");
        } else {
            let _ = write!(buf, "({kind_name})");
        }
        return;
    }

    let _ = write!(buf, "({kind_name}");
    let indent = depth + 1;
    for &child in &children {
        buf.push('\n');
        for _ in 0..indent {
            buf.push_str("  ");
        }
        dump_node_pretty(ast, child, buf, indent);
    }
    buf.push(')');
}

/// Try to extract the source text for a span from the AST's stored source.
fn span_text(ast: &Ast, span: rustc_span::Span) -> Option<&str> {
    if ast.source.is_empty() || span.is_dummy() {
        return None;
    }
    let lo = span.lo().0 as usize;
    let hi = span.hi().0 as usize;
    ast.source.get(lo..hi)
}
