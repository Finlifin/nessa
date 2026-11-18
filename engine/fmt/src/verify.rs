//! Post-format self-verification: silent re-lex + re-parse, comment retention check,
//! and canonical AST structural equivalence (accounting for `.reorder_imports`).

use ast::{Ast, NodeIndex, NodeKind};
use lexer::TokenKind;

use crate::bridge::{BridgedConstruct, ParsedSource, parse_and_bridge};

/// Re-parse `formatted` silently and verify:
/// 1. Zero lex or parse errors (`DiagnosticContext::new_silent`).
/// 2. Comment count (`--` + `{- -}`) is preserved.
/// 3. Canonical AST structure matches `orig` (accounting for `.reorder_imports` ordering).
pub fn verify_formatted_output(
    orig: &ParsedSource,
    formatted: &str,
    display_name: &str,
) -> Result<(), String> {
    let reparsed = parse_and_bridge(formatted, display_name, false).map_err(|err| {
        format!(
            "formatted output failed to re-parse: {}\n--- formatted ---\n{formatted}",
            err.rendered_diagnostics
        )
    })?;

    if reparsed.comments.len() != orig.comments.len() {
        return Err(format!(
            "comment count changed during formatting (before: {}, after: {})",
            orig.comments.len(),
            reparsed.comments.len()
        ));
    }

    let before_fp = canonical_node_fingerprint(orig, orig.ast.root);
    let after_fp = canonical_node_fingerprint(&reparsed, reparsed.ast.root);
    if before_fp != after_fp {
        let diff_pos = before_fp
            .bytes()
            .zip(after_fp.bytes())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| before_fp.len().min(after_fp.len()));
        let win_lo = diff_pos.saturating_sub(120);
        let b_hi = (diff_pos + 240).min(before_fp.len());
        let a_hi = (diff_pos + 240).min(after_fp.len());
        return Err(format!(
            "AST structure changed at byte {diff_pos}:\n  before: ...{}...\n   after: ...{}...",
            &before_fp[win_lo..b_hi],
            &after_fp[win_lo..a_hi]
        ));
    }

    Ok(())
}

fn canonical_node_fingerprint(parsed: &ParsedSource, node: NodeIndex) -> String {
    if node.is_null() {
        return "0".to_string();
    }

    let (raw_lo, _) = parsed.raw_recorded_range(node);
    if let Some(b) = parsed.bridge.constructs_by_start.get(&raw_lo) {
        return bridged_fingerprint(b);
    }
    let (exact_lo, _) = parsed.exact_byte_range(node, 1);
    if let Some(b) = parsed.bridge.constructs_by_start.get(&exact_lo) {
        return bridged_fingerprint(b);
    }

    let n = parsed.ast.node(node);
    let kind = n.kind;
    let fixed = parsed.ast.fixed_children(node);
    let multi = parsed.ast.multi_children(node);

    match kind {
        NodeKind::Id
        | NodeKind::Int
        | NodeKind::Real
        | NodeKind::Str
        | NodeKind::Char
        | NodeKind::Bool => {
            let text = str_interner::get(n.str_id);
            return format!("({kind:?} {text:?})");
        }
        NodeKind::InfixFnCall => {
            let op_name = str_interner::get(n.str_id);
            let lhs = canonical_node_fingerprint(parsed, fixed[0]);
            let rhs = canonical_node_fingerprint(parsed, fixed[1]);
            return format!("(InfixFnCall {op_name:?} {lhs} {rhs})");
        }
        NodeKind::PathProjectionMulti => {
            let base = canonical_node_fingerprint(parsed, fixed[0]);
            let mut subs: Vec<String> = multi
                .iter()
                .map(|&c| canonical_node_fingerprint(parsed, c))
                .collect();
            subs.sort();
            return format!("(PathProjectionMulti {base} [{}])", subs.join(","));
        }
        NodeKind::ModuleDef => {
            let name = canonical_node_fingerprint(parsed, fixed[0]);
            let (f, l) = parsed.exact_token_bounds(node, 1);
            let (_, nl) = parsed.exact_token_bounds(fixed[0], f);
            let has_inline_body = !multi.is_empty()
                || (nl < l
                    && matches!(
                        parsed.sig_tokens[nl + 1].kind,
                        TokenKind::LBrace | TokenKind::Colon
                    ));
            let items = canonical_container_list(parsed, multi);
            return format!(
                "(ModuleDef inline={has_inline_body} {name} [{}])",
                items.join(",")
            );
        }
        NodeKind::HandlesStatement => {
            let (f, _) = parsed.exact_token_bounds(node, 1);
            let is_async = parsed
                .sig_tokens
                .get(f + 1)
                .is_some_and(|t| t.kind == TokenKind::KwAsync);
            let c0 = canonical_node_fingerprint(parsed, fixed[0]);
            let c1 = canonical_node_fingerprint(parsed, fixed[1]);
            let c2 = canonical_node_fingerprint(parsed, fixed[2]);
            let params: Vec<String> = multi
                .iter()
                .map(|&p| canonical_node_fingerprint(parsed, p))
                .collect();
            return format!(
                "(HandlesStatement async={is_async} {c0} {c1} {c2} [{}])",
                params.join(",")
            );
        }
        _ => {}
    }

    let is_container = matches!(
        kind,
        NodeKind::FileScope
            | NodeKind::Block
            | NodeKind::StructDef
            | NodeKind::EnumDef
            | NodeKind::TraitDef
            | NodeKind::ImplDef
            | NodeKind::ImplTraitDef
            | NodeKind::ExtendDef
            | NodeKind::ExtendTraitDef
    );

    let mut parts = Vec::with_capacity(1 + fixed.len() + 1);
    parts.push(format!("{kind:?}"));
    for &fc in fixed {
        parts.push(canonical_node_fingerprint(parsed, fc));
    }
    if !multi.is_empty() || is_container {
        let multi_fps = if is_container {
            canonical_container_list(parsed, multi)
        } else {
            multi
                .iter()
                .map(|&c| canonical_node_fingerprint(parsed, c))
                .collect()
        };
        parts.push(format!("[{}]", multi_fps.join(",")));
    }

    format!("({})", parts.join(" "))
}

fn canonical_container_list(parsed: &ParsedSource, items: &[NodeIndex]) -> Vec<String> {
    let mut out = Vec::with_capacity(items.len());
    let mut i = 0usize;
    while i < items.len() {
        if let Some(vis) = use_item_vis(&parsed.ast, items[i]) {
            let mut run = Vec::new();
            while i < items.len() && use_item_vis(&parsed.ast, items[i]) == Some(vis) {
                run.push(canonical_node_fingerprint(parsed, items[i]));
                i += 1;
            }
            run.sort();
            out.extend(run);
        } else {
            out.push(canonical_node_fingerprint(parsed, items[i]));
            i += 1;
        }
    }
    out
}

fn use_item_vis(ast: &Ast, mut node: NodeIndex) -> Option<u8> {
    let mut vis = 0u8;
    while !node.is_null() {
        match ast.node(node).kind {
            NodeKind::PubDef => {
                vis = 1;
                node = ast.fixed_children(node)[0];
            }
            NodeKind::PrivateDef => {
                vis = 2;
                node = ast.fixed_children(node)[0];
            }
            NodeKind::UseStatement => return Some(vis),
            _ => return None,
        }
    }
    None
}

fn bridged_fingerprint(b: &BridgedConstruct) -> String {
    match b {
        BridgedConstruct::SkippedUnparsed { .. } => "(BridgedSkipped)".to_string(),
    }
}
