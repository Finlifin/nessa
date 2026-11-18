//! Lexical pre-scanner, `--%` `.off`/`.skip` error masking, and exact AST token-slot resolver.

use std::collections::HashMap;

use ast::{Ast, NodeIndex, NodeKind};
use diagnostic::{DiagnosticContext, NessaError};
use lexer::{Token, TokenKind};
use parser::Parser;
use rustc_span::{BytePos, FileName, SourceMap, source_map::FilePathMapping};

use crate::directives::{DirectiveLine, DirectiveWarning, parse_directive_comment};

/// Kind of comment encountered in source order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentKind {
    /// `-- ...` single-line comment.
    Line,
    /// `{- ... -}` nestable block comment.
    Block,
}

/// A comment token (either `--` line comment or `{- ... -}` block comment) in source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentItem {
    pub kind: CommentKind,
    pub byte_start: usize,
    pub byte_end: usize,
    pub line_start: usize,
    pub line_no: usize,
    pub col_no: usize,
    /// True if non-whitespace code precedes this comment on the same line.
    pub has_code_before_on_line: bool,
    /// True if non-whitespace code follows this comment on the same line (block comments).
    pub has_code_after_on_line: bool,
    /// If this comment is a standalone `--%` directive line, indexes into `directives`.
    pub directive_index: Option<usize>,
}

/// A formatting-disabled region started by `--% .off`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OffRegion {
    pub off_directive_idx: usize,
    pub on_directive_idx: Option<usize>,
    /// Start byte of the disabled body (immediately after the `--% .off` line).
    pub body_start: usize,
    /// End byte of the disabled body (start of `--% .on` line, closing `}`, or EOF).
    pub body_end: usize,
}

/// Bridged construct for unparseable code explicitly protected by `--% .skip`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BridgedConstruct {
    /// Code skipped via `--% .skip` that could not be parsed by `parser`.
    SkippedUnparsed { start: usize, end: usize },
}

impl BridgedConstruct {
    pub fn span_bytes(&self) -> (usize, usize) {
        match *self {
            Self::SkippedUnparsed { start, end } => (start, end),
        }
    }
}

/// Metadata linking shadow-buffer placeholder AST nodes back to original source ranges.
#[derive(Debug, Default, Clone)]
pub struct BridgeMap {
    /// Keyed by start byte offset of the placeholder node in source.
    pub constructs_by_start: HashMap<usize, BridgedConstruct>,
}

/// Complete pre-scan + parse result for a single Nessa source buffer.
pub struct ParsedSource {
    pub original_src: String,
    pub shadow_src: String,
    pub line_starts: Vec<usize>,
    pub comments: Vec<CommentItem>,
    pub directives: Vec<DirectiveLine>,
    pub warnings: Vec<DirectiveWarning>,
    pub off_regions: Vec<OffRegion>,
    pub bridge: BridgeMap,
    pub raw_tokens: Vec<Token>,
    /// Significant (non-layout, non-comment) tokens wrapped with `Sof` at `[0]` and `Eof` at `[len - 1]`.
    pub sig_tokens: Vec<Token>,
    pub ast: Ast,
    pub base_pos: BytePos,
}

/// Failure when lexing or parsing input source.
#[derive(Debug, Clone)]
pub struct ParseFailure {
    pub rendered_diagnostics: String,
}

impl ParsedSource {
    /// Return raw `(lo, hi)` byte range recorded on `node` by `Parser` (0-based into `original_src`).
    /// Note that `Parser::current_span()` sets `hi` to the `.to` of the lookahead token at `p.cursor`,
    /// so callers needing exact node bounds should use `exact_token_bounds` / `exact_byte_range`.
    pub fn raw_recorded_range(&self, node: NodeIndex) -> (usize, usize) {
        if node.is_null() {
            return (0, 0);
        }
        let Some(range) = self.ast.original_node_range(node) else {
            let sp = self.ast.node(node).span;
            if sp.is_dummy() {
                return (0, 0);
            }
            let lo = sp.lo().0.saturating_sub(self.base_pos.0) as usize;
            let hi = sp.hi().0.saturating_sub(self.base_pos.0) as usize;
            return (lo, hi);
        };
        let lo = range.start as usize;
        let mut hi = range.end as usize;
        if let Some(bridged) = self.bridge.constructs_by_start.get(&lo) {
            hi = hi.max(bridged.span_bytes().1);
        }
        (lo, hi)
    }

    /// Resolve the exact `(first_tok_idx, last_tok_idx)` in `self.sig_tokens` for `node`,
    /// given that `node` cannot start before `floor_tok_idx`.
    pub fn exact_token_bounds(&self, node: NodeIndex, floor_tok_idx: usize) -> (usize, usize) {
        let max_real = self.sig_tokens.len().saturating_sub(2);
        if max_real == 0 || node.is_null() {
            return (0, 0);
        }
        let floor = floor_tok_idx.clamp(1, max_real);
        self.resolve_node_bounds(node, floor, max_real, false)
    }

    fn resolve_node_bounds(
        &self,
        node: NodeIndex,
        floor: usize,
        max_real: usize,
        in_fstring: bool,
    ) -> (usize, usize) {
        if node.is_null() {
            return (0, 0);
        }
        let kind = self.ast.node(node).kind;
        let (raw_lo, raw_hi) = self.raw_recorded_range(node);

        // Check bridged placeholder first (`_f` or `fn f(){}`).
        if let Some(bridged) = self.bridge.constructs_by_start.get(&raw_lo) {
            let (bs, be) = bridged.span_bytes();
            let first = self
                .sig_tokens
                .partition_point(|t| {
                    t.kind == TokenKind::Sof || ((t.to as usize) <= bs && t.kind != TokenKind::Eof)
                })
                .clamp(floor, max_real);
            let idx_hi = self.sig_tokens.partition_point(|t| {
                t.kind == TokenKind::Sof || ((t.from as usize) < be && t.kind != TokenKind::Eof)
            });
            let last = idx_hi.saturating_sub(1).clamp(first, max_real);
            return (first, last);
        }

        // Synthetic tacit lambda (`_0 + 1` wrapped in `Lambda` where params share `body.span`):
        if kind == NodeKind::Lambda && self.is_tacit_lambda(node) {
            let body = self.ast.fixed_children(node)[0];
            return self.resolve_node_bounds(body, floor, max_real, in_fstring);
        }

        // Synthetic `ListOf` created by `LetHandler` (multiple `handles(e)`) or `TraitDef` parents:
        // its elements are real, and there are no `[` / `]` brackets.
        if kind == NodeKind::ListOf && self.is_synthetic_list_of(node, floor) {
            let elems = self.ast.multi_children(node);
            if let (Some(&first_el), Some(&last_el)) = (elems.first(), elems.last()) {
                let (f, _) = self.resolve_node_bounds(first_el, floor, max_real, in_fstring);
                let (_, l) = self.resolve_node_bounds(last_el, f, max_real, in_fstring);
                return (f, l);
            }
        }

        // 1. Atomic single-token leaves constructed with `next_token_span()`:
        if is_atomic_leaf_kind(kind)
            && !(in_fstring && kind == NodeKind::Str)
            && let Some(tok_idx) = self.find_exact_single_token(raw_lo, raw_hi, floor, max_real)
        {
            return (tok_idx, tok_idx);
        }

        // `FStringLiteral` inside `FStringConcat` was built with `current_span()` before `next_token()`:
        if in_fstring && kind == NodeKind::Str {
            let idx = self
                .sig_tokens
                .partition_point(|t| {
                    t.kind == TokenKind::Sof
                        || ((t.from as usize) < raw_hi && t.kind != TokenKind::Eof)
                })
                .clamp(floor, max_real);
            // Walk back or forward to nearest `FStringLiteral` at `>= floor`.
            if self.sig_tokens[idx].kind == TokenKind::FStringLiteral {
                return (idx, idx);
            }
            for cand in floor..=max_real {
                if self.sig_tokens[cand].kind == TokenKind::FStringLiteral
                    && (self.sig_tokens[cand].to as usize) >= raw_hi
                {
                    return (cand, cand);
                }
            }
        }

        // 2. Compute child bounds union in source order!
        let child_fstring = in_fstring || kind == NodeKind::FStringConcat;
        let ordered_children = self.ordered_children_of(node);
        let mut min_child_first: Option<usize> = None;
        let mut max_child_last: Option<usize> = None;
        let mut child_floor = floor;

        for child in ordered_children {
            if child.is_null() {
                continue;
            }
            let (cf, cl) = self.resolve_node_bounds(child, child_floor, max_real, child_fstring);
            if cf > 0 && cl > 0 {
                min_child_first = Some(min_child_first.map_or(cf, |cur| cur.min(cf)));
                // Advance `child_floor` monotonically if children are in source order; note that
                // if outer parens wrapped `child`, `cl` is still `<=` any subsequent sibling.
                max_child_last = Some(max_child_last.map_or(cl, |cur| cur.max(cl)));
                child_floor = cl.min(max_real);
            }
        }

        // Determine `first_tok`:
        // Own `raw_lo` gives the first non-layout token at or after `raw_lo` (subject to `floor`).
        let mut own_first = self
            .sig_tokens
            .partition_point(|t| {
                t.kind == TokenKind::Sof || ((t.to as usize) <= raw_lo && t.kind != TokenKind::Eof)
            })
            .clamp(floor, max_real);

        if kind == NodeKind::FStringConcat {
            // `try_fstring` recorded `current_span()` before `FStringStart`, so advance to `FStringStart`.
            while own_first < max_real && self.sig_tokens[own_first].kind != TokenKind::FStringStart
            {
                own_first += 1;
            }
        } else if kind == NodeKind::IfStatement {
            // `parse_if_after_keyword` enters AFTER `KwIf` (or `elif` recovery `Id`) was consumed,
            // so `raw_lo` points to the condition's first token rather than `if` itself.
            if own_first > floor
                && matches!(
                    self.sig_tokens[own_first - 1].kind,
                    TokenKind::KwIf | TokenKind::Id
                )
                && self.sig_tokens[own_first].kind != TokenKind::KwIf
            {
                own_first -= 1;
            }
        } else if !token_can_start_own_rule(kind, self.sig_tokens[own_first].kind)
            && own_first < max_real
        {
            own_first += 1;
        }

        let mut first_tok = if let Some(cf) = min_child_first {
            // Postfix nodes (`Call`, `Projection`, `View`, `PostMatch`, `ErrorElimination`, etc.)
            // enter AFTER `left` (`children[0]`), so `cf < own_first`!
            // Also if `left` was parenthesized `(a + b).c`, include outer `(` around `left` if within `floor`?
            // Wait: `count_outer_parens` handles outer parens around `node` itself; for `(a + b).c`,
            // the `(` before `a + b` starts `left`'s slot, which we check below!
            if is_postfix_with_left_child(kind) {
                let left = self.ast.fixed_children(node)[0];
                let left_start = self.left_operand_first_tok_with_parens(left, floor, cf);
                left_start.min(own_first)
            } else {
                cf.min(own_first)
            }
        } else {
            own_first
        };
        first_tok = first_tok.clamp(floor, max_real);

        // Determine `last_tok`:
        // In `Parser::current_span()`, `raw_hi == raw_tokens[end].to` where `end == p.cursor` (the
        // unconsumed token right after `node`), OR at EOF/Outdent where zero-width tokens share `raw_hi`.
        let search_from = max_child_last.unwrap_or(first_tok).max(first_tok);
        let last_tok = self.resolve_last_tok(
            node,
            kind,
            first_tok,
            search_from,
            max_child_last,
            raw_hi,
            max_real,
        );

        (first_tok, last_tok.max(first_tok))
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_last_tok(
        &self,
        node: NodeIndex,
        kind: NodeKind,
        first_tok: usize,
        search_from: usize,
        max_child_last: Option<usize>,
        raw_hi: usize,
        max_real: usize,
    ) -> usize {
        // Upper bound in `sig_tokens`: no consumed token can have `to > raw_hi`.
        // Moreover, if the token whose `.to == raw_hi` is a real lookahead token (`from < to == raw_hi`)
        // AND there is NO zero-width token (`Outdent`/`Newline`/`Eof`) at `raw_hi..raw_hi` in `raw_tokens`,
        // then `p.cursor` MUST have been that real lookahead token, so `node` ended strictly BEFORE it!
        let has_zero_width_at_hi = self
            .raw_tokens
            .iter()
            .any(|rt| rt.from == rt.to && (rt.to as usize) == raw_hi);

        let upper_sig = if has_zero_width_at_hi {
            let idx = self.sig_tokens.partition_point(|t| {
                t.kind == TokenKind::Sof || ((t.to as usize) <= raw_hi && t.kind != TokenKind::Eof)
            });
            idx.saturating_sub(1).clamp(first_tok, max_real)
        } else {
            // Strictly before the unique real token whose `.to == raw_hi`!
            let idx = self.sig_tokens.partition_point(|t| {
                t.kind == TokenKind::Sof || ((t.to as usize) < raw_hi && t.kind != TokenKind::Eof)
            });
            idx.saturating_sub(1).clamp(first_tok, max_real)
        };

        if upper_sig <= search_from {
            // Even if a child was parenthesized `f((x))`, advance across closing `)` up to `upper_sig`.
            return search_from;
        }

        // When `has_zero_width_at_hi` is true (e.g. before `Outdent` or `Eof`), `upper_sig` might either
        // belong to `node` (e.g. closing delimiter or wrapped `)` around rightmost child) OR be the next
        // token if `raw_hi` happened to match a zero-width token before `upper_sig` (note: if `upper_sig`
        // starts at `>= raw_hi`, `t.to <= raw_hi` already excluded it because real tokens have `from < to`!).
        // Wait: since every real token in `sig_tokens` has `from < to`, ANY token in `sig_tokens` with
        // `t.to <= raw_hi` MUST have `t.from < raw_hi`! And zero-width tokens at `raw_hi..raw_hi` only
        // appear AFTER all tokens with `t.to <= raw_hi`!
        // Let's double check: can a zero-width token at `P..P` appear BEFORE a token with `to == P`?
        // NO! Because token positions are monotonic (`from <= to`, non-decreasing), any token after `P..P`
        // has `from >= P`, and if it is a real token (`from < to`), it has `to > P`!!
        // Therefore `upper_sig` IS ALWAYS the exact last `sig_token` consumed before `p.cursor`!!
        let _ = (node, kind, max_child_last);
        upper_sig
    }

    fn left_operand_first_tok_with_parens(
        &self,
        left: NodeIndex,
        floor: usize,
        left_first: usize,
    ) -> usize {
        // Check if `(` tokens immediately preceding `left_first` wrap `left` and close before the postfix op!
        let max_real = self.sig_tokens.len().saturating_sub(2);
        let (_, left_last) = self.resolve_node_bounds(left, floor, max_real, false);
        let mut f = left_first;
        let mut l = left_last;
        while f > floor
            && l < max_real
            && self.sig_tokens[f - 1].kind == TokenKind::LParen
            && self.sig_tokens[l + 1].kind == TokenKind::RParen
            && matching_paren_at(&self.sig_tokens, f - 1) == Some(l + 1)
        {
            f -= 1;
            l += 1;
        }
        f
    }

    fn find_exact_single_token(
        &self,
        raw_lo: usize,
        raw_hi: usize,
        floor: usize,
        max_real: usize,
    ) -> Option<usize> {
        if raw_lo >= raw_hi {
            return None;
        }
        let idx = self.sig_tokens.partition_point(|t| {
            t.kind == TokenKind::Sof || ((t.from as usize) < raw_lo && t.kind != TokenKind::Eof)
        });
        if idx >= floor && idx <= max_real {
            let tok = self.sig_tokens[idx];
            if tok.from as usize == raw_lo && tok.to as usize == raw_hi {
                return Some(idx);
            }
        }
        None
    }

    /// Return children of `node` in source order (compensating for `Lambda`, `DeriveDef`, etc.
    /// whose fixed vs multi children are stored out of source order).
    fn ordered_children_of(&self, node: NodeIndex) -> Vec<NodeIndex> {
        let kind = self.ast.node(node).kind;
        let fixed = self.ast.fixed_children(node);
        let multi = self.ast.multi_children(node);
        let mut out = Vec::with_capacity(fixed.len() + multi.len());

        match kind {
            NodeKind::Lambda => {
                // Source order: `|multi = params| (-> fixed[1] = ret)? (fixed[0] = body)`
                out.extend_from_slice(multi);
                if !fixed[1].is_null() {
                    out.push(fixed[1]);
                }
                if !fixed[0].is_null() {
                    out.push(fixed[0]);
                }
            }
            NodeKind::FunctionDef | NodeKind::TraitDeriveFn => {
                // Source order: `fn fixed[0](multi) (-> fixed[1])? (handles fixed[3])? fixed[2]`
                if !fixed[0].is_null() {
                    out.push(fixed[0]);
                }
                out.extend_from_slice(multi);
                if !fixed[1].is_null() {
                    out.push(fixed[1]);
                }
                if !fixed[3].is_null() {
                    out.push(fixed[3]);
                }
                if !fixed[2].is_null() {
                    out.push(fixed[2]);
                }
            }
            NodeKind::TraitDefFn | NodeKind::EffectDef | NodeKind::AsyncEffectDef => {
                // Source order: `fixed[0](multi) (-> fixed[1])? (handles fixed[2])?`
                if !fixed[0].is_null() {
                    out.push(fixed[0]);
                }
                out.extend_from_slice(multi);
                for &c in &fixed[1..] {
                    if !c.is_null() {
                        out.push(c);
                    }
                }
            }
            NodeKind::HandlesStatement => {
                // Source order: `handles fixed[0](multi) (-> fixed[1])? fixed[2]`
                if !fixed[0].is_null() {
                    out.push(fixed[0]);
                }
                out.extend_from_slice(multi);
                if !fixed[1].is_null() {
                    out.push(fixed[1]);
                }
                if !fixed[2].is_null() {
                    out.push(fixed[2]);
                }
            }
            NodeKind::DeriveDef => {
                // Source order: `derive multi+ for fixed[0]`
                out.extend_from_slice(multi);
                if !fixed[0].is_null() {
                    out.push(fixed[0]);
                }
            }
            _ => {
                for &c in fixed {
                    if !c.is_null() {
                        out.push(c);
                    }
                }
                out.extend_from_slice(multi);
            }
        }
        out
    }

    /// True if `node` (`NodeKind::Lambda`) was synthesized by `maybe_wrap_tacit_lambda` (`_0 + 1`).
    pub fn is_tacit_lambda(&self, node: NodeIndex) -> bool {
        if self.ast.node(node).kind != NodeKind::Lambda {
            return false;
        }
        let fixed = self.ast.fixed_children(node);
        let params = self.ast.multi_children(node);
        if params.is_empty() || !fixed[1].is_null() {
            return false;
        }
        let body_span = self.ast.node(fixed[0]).span;
        let first_param_span = self.ast.node(params[0]).span;
        !body_span.is_dummy() && body_span == first_param_span
    }

    /// True if `node` (`NodeKind::ListOf`) is a synthetic bracketless list (in `LetHandler` or `TraitDef`).
    pub fn is_synthetic_list_of(&self, node: NodeIndex, floor: usize) -> bool {
        if self.ast.node(node).kind != NodeKind::ListOf {
            return false;
        }
        let (raw_lo, _) = self.raw_recorded_range(node);
        let max_real = self.sig_tokens.len().saturating_sub(2);
        if max_real == 0 {
            return false;
        }
        let tok_idx = self
            .sig_tokens
            .partition_point(|t| {
                t.kind == TokenKind::Sof || ((t.to as usize) <= raw_lo && t.kind != TokenKind::Eof)
            })
            .clamp(floor.min(max_real), max_real);
        self.sig_tokens[tok_idx].kind != TokenKind::LBracket
    }

    /// Exact `(lo, hi)` byte range in `original_src` for `node` given `floor_tok_idx`.
    pub fn exact_byte_range(&self, node: NodeIndex, floor_tok_idx: usize) -> (usize, usize) {
        let (first_tok, last_tok) = self.exact_token_bounds(node, floor_tok_idx);
        if first_tok == 0 || last_tok == 0 || first_tok >= self.sig_tokens.len() {
            return self.raw_recorded_range(node);
        }
        let lo = self.sig_tokens[first_tok].from as usize;
        let mut hi = self.sig_tokens[last_tok].to as usize;
        if let Some(bridged) = self.bridge.constructs_by_start.get(&lo) {
            hi = hi.max(bridged.span_bytes().1);
        }
        (lo, hi)
    }

    /// Count explicit user-written `( ... )` parenthesis pairs wrapping `node` inside
    /// the enclosing token slot `[slot_first_tok ..= slot_last_tok]`.
    pub fn count_outer_parens(
        &self,
        node: NodeIndex,
        slot_first_tok: usize,
        slot_last_tok: usize,
    ) -> (usize, usize, usize) {
        if slot_first_tok == 0
            || slot_last_tok == 0
            || slot_first_tok > slot_last_tok
            || slot_last_tok >= self.sig_tokens.len()
        {
            let (f, l) = self.exact_token_bounds(node, slot_first_tok.max(1));
            return (0, f, l);
        }
        let mut left = slot_first_tok;
        let mut right = slot_last_tok;
        let mut count = 0usize;

        while left < right
            && self.sig_tokens[left].kind == TokenKind::LParen
            && self.sig_tokens[right].kind == TokenKind::RParen
            && matching_paren_at(&self.sig_tokens, left) == Some(right)
        {
            let (inner_f, inner_l) = self.exact_token_bounds(node, left);
            if inner_f <= left || inner_l >= right {
                break;
            }
            count += 1;
            left += 1;
            right -= 1;
        }
        (count, left, right)
    }

    /// Find a token matching `pred` at delimiter depth 0 within `[slot_first ..= slot_last]`.
    pub fn find_depth_zero_token(
        &self,
        slot_first: usize,
        slot_last: usize,
        pred: impl Fn(TokenKind) -> bool,
        reverse: bool,
    ) -> Option<usize> {
        if slot_first == 0 || slot_first > slot_last || slot_last >= self.sig_tokens.len() {
            return None;
        }
        let mut depth = 0isize;
        let mut matches_at_zero = Vec::new();
        for idx in slot_first..=slot_last {
            let kind = self.sig_tokens[idx].kind;
            match kind {
                TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::LBrace
                | TokenKind::FStringExprStart => {
                    if depth == 0 && pred(kind) {
                        matches_at_zero.push(idx);
                    }
                    depth += 1;
                }
                TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::RBrace
                | TokenKind::FStringExprEnd => {
                    depth = (depth - 1).max(0);
                    if depth == 0 && pred(kind) {
                        matches_at_zero.push(idx);
                    }
                }
                _ => {
                    if depth == 0 && pred(kind) {
                        matches_at_zero.push(idx);
                    }
                }
            }
        }
        if reverse {
            matches_at_zero.pop()
        } else {
            matches_at_zero.first().copied()
        }
    }

    /// 1-based line number for a byte offset.
    pub fn line_of_byte(&self, byte_pos: usize) -> usize {
        self.line_starts.partition_point(|&start| start <= byte_pos)
    }

    /// 0-based column in spaces (expanding `\t` by `tab_spaces`) at `byte_pos` on its line.
    pub fn visual_column(&self, byte_pos: usize, tab_spaces: usize) -> usize {
        let line_idx = self.line_of_byte(byte_pos).saturating_sub(1);
        let line_start = self.line_starts.get(line_idx).copied().unwrap_or(0);
        let slice = self
            .original_src
            .get(line_start..byte_pos.min(self.original_src.len()))
            .unwrap_or("");
        let mut col = 0usize;
        for ch in slice.chars() {
            if ch == '\t' {
                col += tab_spaces - (col % tab_spaces);
            } else {
                col += 1;
            }
        }
        col
    }

    /// Count blank lines strictly between `end_a` and `start_b`.
    pub fn blank_lines_in_gap(&self, end_a: usize, start_b: usize) -> usize {
        if start_b <= end_a || end_a > self.original_src.len() {
            return 0;
        }
        let slice = &self.original_src[end_a..start_b.min(self.original_src.len())];
        let newline_count = slice.bytes().filter(|&b| b == b'\n').count();
        newline_count.saturating_sub(1)
    }

    /// Check if `[lo..hi]` lies inside any `--% .off` disabled region.
    pub fn enclosing_off_region(&self, lo: usize, hi: usize) -> Option<&OffRegion> {
        self.off_regions
            .iter()
            .find(|r| lo >= r.body_start && hi <= r.body_end && r.body_start < r.body_end)
    }
}

pub fn matching_paren_at(tokens: &[Token], open_idx: usize) -> Option<usize> {
    if tokens.get(open_idx)?.kind != TokenKind::LParen {
        return None;
    }
    let mut depth = 0usize;
    for (idx, tok) in tokens.iter().enumerate().skip(open_idx) {
        match tok.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx);
                }
            }
            TokenKind::Eof => break,
            _ => {}
        }
    }
    None
}

fn is_atomic_leaf_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Int
            | NodeKind::Real
            | NodeKind::Str
            | NodeKind::Char
            | NodeKind::Bool
            | NodeKind::Id
            | NodeKind::SelfUpper
            | NodeKind::SelfLower
            | NodeKind::Null
            | NodeKind::Underscore
    )
}

fn is_postfix_with_left_child(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Call
            | NodeKind::ExtendedCall
            | NodeKind::Projection
            | NodeKind::View
            | NodeKind::Await
            | NodeKind::TypeCast
            | NodeKind::HandlerApplication
            | NodeKind::EffectPropagation
            | NodeKind::EffectElimination
            | NodeKind::ErrorPropagation
            | NodeKind::ErrorElimination
            | NodeKind::OptionPropagation
            | NodeKind::PostMatch
            | NodeKind::BoolMatches
            | NodeKind::PostDo
            | NodeKind::RangeFrom
            | NodeKind::RangeFromTo
            | NodeKind::RangeFromToInclusive
            | NodeKind::PatternCall
            | NodeKind::PatternExtendedCall
            | NodeKind::PropertyPattern
            | NodeKind::PatternTypeFamily
            | NodeKind::PatternOptionSome
            | NodeKind::PatternErrorOk
    )
}

fn token_can_start_own_rule(kind: NodeKind, tok: TokenKind) -> bool {
    if matches!(
        tok,
        TokenKind::Semi
            | TokenKind::Comma
            | TokenKind::RParen
            | TokenKind::RBracket
            | TokenKind::RBrace
            | TokenKind::FatArrow
            | TokenKind::Arrow
            | TokenKind::Eq
            | TokenKind::PlusEq
            | TokenKind::MinusEq
            | TokenKind::StarEq
            | TokenKind::SlashEq
            | TokenKind::PercentEq
    ) {
        return false;
    }
    if tok == TokenKind::Colon {
        // `:` can start an indentation-based `Block` (`if cond: \n    ...`)!
        return kind == NodeKind::Block;
    }
    match kind {
        NodeKind::Object | NodeKind::PatternRecord => tok == TokenKind::LBrace,
        NodeKind::ListOf | NodeKind::PatternList => tok == TokenKind::LBracket,
        NodeKind::Tuple | NodeKind::PatternTuple | NodeKind::Unit => tok == TokenKind::LParen,
        NodeKind::StructDef => tok == TokenKind::KwStruct,
        NodeKind::EnumDef => tok == TokenKind::KwEnum,
        NodeKind::TraitDef => tok == TokenKind::KwTrait,
        NodeKind::ImplDef | NodeKind::ImplTraitDef => tok == TokenKind::KwImpl,
        NodeKind::ExtendDef | NodeKind::ExtendTraitDef => tok == TokenKind::KwExtend,
        NodeKind::DeriveDef | NodeKind::TraitDeriveFn => tok == TokenKind::KwDerive,
        NodeKind::FunctionDef => tok == TokenKind::KwFn,
        NodeKind::TraitDefFn => matches!(tok, TokenKind::KwFn | TokenKind::KwDef),
        NodeKind::EffectDef => tok == TokenKind::KwEffect,
        NodeKind::AsyncEffectDef => tok == TokenKind::KwAsync,
        NodeKind::HandlesStatement | NodeKind::LetHandler => tok == TokenKind::KwHandles,
        NodeKind::LetDecl => tok == TokenKind::KwLet,
        NodeKind::VarDecl => matches!(tok, TokenKind::KwVar | TokenKind::KwGlobal),
        NodeKind::ConstDecl => tok == TokenKind::KwConst,
        NodeKind::GlobalDecl => tok == TokenKind::KwGlobal,
        NodeKind::AssocDecl => tok == TokenKind::KwAssoc,
        NodeKind::UseStatement => tok == TokenKind::KwUse,
        NodeKind::ModuleDef => tok == TokenKind::KwMod,
        NodeKind::Typealias => tok == TokenKind::KwTypealias,
        NodeKind::Newtype => tok == TokenKind::KwNewtype,
        NodeKind::IfStatement => matches!(tok, TokenKind::KwIf | TokenKind::Id),
        NodeKind::WhenStatement => tok == TokenKind::KwWhen,
        NodeKind::WhileLoop => tok == TokenKind::KwWhile,
        NodeKind::ForLoop => tok == TokenKind::KwFor,
        NodeKind::DeferStatement => tok == TokenKind::KwDefer,
        NodeKind::ReturnStatement => tok == TokenKind::KwReturn,
        NodeKind::ResumeStatement => tok == TokenKind::KwResume,
        NodeKind::BreakStatement => tok == TokenKind::KwBreak,
        NodeKind::ContinueStatement => tok == TokenKind::KwContinue,
        NodeKind::ResetExpr => tok == TokenKind::KwReset,
        NodeKind::ShiftExpr => tok == TokenKind::KwShift,
        NodeKind::PubDef => tok == TokenKind::KwPub,
        NodeKind::PrivateDef => tok == TokenKind::KwPrivate,
        NodeKind::Symbol | NodeKind::ParamOptional | NodeKind::SuperPath => tok == TokenKind::Dot,
        NodeKind::PackagePath => tok == TokenKind::At,
        _ => true,
    }
}

/// Parse and index `src` (named `display_name` for diagnostics).
/// When `emit_diagnostics` is true, lex/parse errors are rendered through `DiagnosticContext` to stderr.
pub fn parse_and_bridge(
    src: &str,
    display_name: &str,
    emit_diagnostics: bool,
) -> Result<ParsedSource, ParseFailure> {
    let scan = pre_scan_source(src);
    let mut shadow_bytes = scan.shadow_bytes;
    let mut bridge = scan.bridge;

    // Attempt 1: parse directly with `.off` / `.skip` unmasked.
    match try_lex_and_parse(&shadow_bytes, display_name) {
        Ok((shadow_src, raw_tokens, sig_tokens, ast, base_pos)) => Ok(ParsedSource {
            original_src: src.to_string(),
            shadow_src,
            line_starts: scan.line_starts,
            comments: scan.comments,
            directives: scan.directives,
            warnings: scan.warnings,
            off_regions: scan.off_regions,
            bridge,
            raw_tokens,
            sig_tokens,
            ast,
            base_pos,
        }),
        Err(_first_err_offset) => {
            // Mask all `--% .off` regions (preserving newlines and byte offsets) and retry!
            let mut masked_off = false;
            for region in &scan.off_regions {
                if region.body_start < region.body_end {
                    blank_preserving_newlines(
                        &mut shadow_bytes,
                        region.body_start,
                        region.body_end,
                    );
                    masked_off = true;
                }
            }
            if masked_off
                && let Ok((shadow_src, raw_tokens, sig_tokens, ast, base_pos)) =
                    try_lex_and_parse(&shadow_bytes, display_name)
            {
                return Ok(ParsedSource {
                    original_src: src.to_string(),
                    shadow_src,
                    line_starts: scan.line_starts,
                    comments: scan.comments,
                    directives: scan.directives,
                    warnings: scan.warnings,
                    off_regions: scan.off_regions,
                    bridge,
                    raw_tokens,
                    sig_tokens,
                    ast,
                    base_pos,
                });
            }

            // Mask `--% .skip` targets (trying `_f` expression placeholder first, then `fn f()` for traits).
            let skip_targets = collect_skip_targets(src, &scan.directives, &scan.off_regions);
            if !skip_targets.is_empty() {
                for use_trait_fn in [false, true] {
                    let mut candidate_bytes = shadow_bytes.clone();
                    let mut candidate_bridge = bridge.clone();
                    let mut applied = false;
                    for &(start, end) in &skip_targets {
                        if write_skip_placeholder(&mut candidate_bytes, start, end, use_trait_fn) {
                            candidate_bridge
                                .constructs_by_start
                                .insert(start, BridgedConstruct::SkippedUnparsed { start, end });
                            applied = true;
                        }
                    }
                    if applied
                        && let Ok((shadow_src, raw_tokens, sig_tokens, ast, base_pos)) =
                            try_lex_and_parse(&candidate_bytes, display_name)
                    {
                        bridge = candidate_bridge;
                        return Ok(ParsedSource {
                            original_src: src.to_string(),
                            shadow_src,
                            line_starts: scan.line_starts,
                            comments: scan.comments,
                            directives: scan.directives,
                            warnings: scan.warnings,
                            off_regions: scan.off_regions,
                            bridge,
                            raw_tokens,
                            sig_tokens,
                            ast,
                            base_pos,
                        });
                    }
                }
            }

            let rendered =
                emit_lex_or_parse_failure(&shadow_bytes, src, display_name, emit_diagnostics);
            Err(ParseFailure {
                rendered_diagnostics: rendered,
            })
        }
    }
}

type LexParseOutput = (String, Vec<Token>, Vec<Token>, Ast, BytePos);

fn try_lex_and_parse(shadow_bytes: &[u8], display_name: &str) -> Result<LexParseOutput, usize> {
    let shadow_src = std::str::from_utf8(shadow_bytes)
        .map_err(|e| e.valid_up_to())?
        .to_string();
    let map = SourceMap::new(FilePathMapping::empty());
    let file = map.new_source_file(
        FileName::Custom(display_name.to_string()),
        shadow_src.clone(),
    );
    let diag_ctx = DiagnosticContext::new_silent(&map);
    let (raw_tokens, lex_errors) = lexer::tokenize(&shadow_src);
    if let Some(first_err) = lex_errors.first() {
        return Err(first_err.from as usize);
    }

    let ast = Parser::new(&raw_tokens, &shadow_src, &diag_ctx, file.start_pos).parse();
    if diag_ctx.has_errors() || ast.root.is_null() {
        let first_off = diag_ctx
            .diagnostics()
            .iter()
            .find_map(|d| d.primary_span)
            .map(|sp| sp.lo().0.saturating_sub(file.start_pos.0) as usize)
            .unwrap_or(0);
        return Err(first_off);
    }

    let sig_tokens = build_sig_tokens(&raw_tokens, shadow_src.len() as u32);
    Ok((shadow_src, raw_tokens, sig_tokens, ast, file.start_pos))
}

fn build_sig_tokens(raw_tokens: &[Token], src_len: u32) -> Vec<Token> {
    let mut sig = Vec::with_capacity(raw_tokens.len() + 2);
    sig.push(Token::new(TokenKind::Sof, 0, 0));
    for &tok in raw_tokens {
        if matches!(
            tok.kind,
            TokenKind::Newline
                | TokenKind::Indent
                | TokenKind::Outdent
                | TokenKind::Comment
                | TokenKind::Sof
                | TokenKind::Eof
        ) {
            continue;
        }
        sig.push(tok);
    }
    sig.push(Token::new(TokenKind::Eof, src_len, src_len));
    sig
}

fn emit_lex_or_parse_failure(
    shadow_bytes: &[u8],
    original_src: &str,
    display_name: &str,
    emit_to_stderr: bool,
) -> String {
    let shadow_str = std::str::from_utf8(shadow_bytes).unwrap_or(original_src);
    let map = SourceMap::new(FilePathMapping::empty());
    let orig_file = map.new_source_file(
        FileName::Custom(display_name.to_string()),
        original_src.to_string(),
    );
    let diag_ctx = if emit_to_stderr {
        DiagnosticContext::new(&map)
    } else {
        DiagnosticContext::new_silent(&map)
    };

    let (raw_tokens, lex_errors) = lexer::tokenize(shadow_str);
    if !lex_errors.is_empty() {
        for err in &lex_errors {
            err.emit(&diag_ctx, orig_file.start_pos);
        }
        let first_msg = diag_ctx
            .diagnostics()
            .first()
            .map(|d| format_diagnostic_plain(&map, d))
            .unwrap_or_else(|| format!("{:?}", lex_errors[0]));
        return format!("lexical error in `{display_name}`: {first_msg}");
    }

    let _ast = Parser::new(&raw_tokens, shadow_str, &diag_ctx, orig_file.start_pos).parse();
    let first_msg = diag_ctx
        .diagnostics()
        .first()
        .map(|d| format_diagnostic_plain(&map, d))
        .unwrap_or_else(|| "invalid syntax".to_string());
    format!("syntax error in `{display_name}`: {first_msg}")
}

fn format_diagnostic_plain(map: &SourceMap, diag: &diagnostic::Diagnostic) -> String {
    if let Some(sp) = diag.primary_span
        && !sp.is_dummy()
    {
        let loc = map.lookup_char_pos(sp.lo());
        return format!("{}:{}: {}", loc.line, loc.col.0 + 1, diag.message);
    }
    diag.message.clone()
}

struct PreScanResult {
    shadow_bytes: Vec<u8>,
    line_starts: Vec<usize>,
    comments: Vec<CommentItem>,
    directives: Vec<DirectiveLine>,
    warnings: Vec<DirectiveWarning>,
    off_regions: Vec<OffRegion>,
    bridge: BridgeMap,
}

fn compute_line_starts(src: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (idx, b) in src.bytes().enumerate() {
        if b == b'\n' {
            starts.push(idx + 1);
        }
    }
    starts
}

fn pre_scan_source(src: &str) -> PreScanResult {
    let line_starts = compute_line_starts(src);
    let shadow = src.as_bytes().to_vec();
    let mut comments = Vec::new();
    let mut directives: Vec<DirectiveLine> = Vec::new();
    let mut warnings = Vec::new();
    let mut off_regions: Vec<OffRegion> = Vec::new();
    let bridge = BridgeMap::default();

    // Active `--% .off` state: `(directive_idx, brace_depth_at_off, body_start_byte)`.
    let mut active_off: Option<(usize, usize, usize)> = None;

    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut brace_depth = 0usize;

    while i < len {
        let b = bytes[i];

        if b.is_ascii_whitespace() {
            i += 1;
            continue;
        }

        // Line comment: `-- ...`
        if b == b'-' && i + 1 < len && bytes[i + 1] == b'-' {
            let comment_start = i;
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            let mut comment_end = i;
            if comment_end > comment_start && bytes[comment_end - 1] == b'\r' {
                comment_end -= 1;
            }
            let line_end_with_newline = if i < len && bytes[i] == b'\n' {
                i + 1
            } else {
                i
            };
            let line_idx = line_starts
                .partition_point(|&s| s <= comment_start)
                .saturating_sub(1);
            let line_start = line_starts[line_idx];
            let prefix_on_line = &src[line_start..comment_start];
            let has_code_before = prefix_on_line.chars().any(|c| !matches!(c, ' ' | '\t'));
            let col_no = prefix_on_line.chars().count() + 1;
            let raw_comment = &src[comment_start..comment_end];

            let mut directive_index = None;
            if !has_code_before && raw_comment.starts_with("--%") {
                let (actions, normalized, warn) =
                    parse_directive_comment(raw_comment, line_idx + 1, col_no);
                let is_valid = warn.is_none();
                if let Some(w) = warn {
                    warnings.push(w);
                }
                let d_idx = directives.len();
                let dir = DirectiveLine {
                    line_start,
                    comment_start,
                    comment_end,
                    line_end_with_newline,
                    line_no: line_idx + 1,
                    col_no,
                    brace_depth,
                    raw_text: raw_comment.to_string(),
                    normalized_text: normalized,
                    actions,
                    is_valid,
                };
                if dir.has_off() && active_off.is_none() {
                    active_off = Some((d_idx, brace_depth, line_end_with_newline));
                } else if dir.has_on()
                    && let Some((off_idx, _, body_start)) = active_off.take()
                {
                    off_regions.push(OffRegion {
                        off_directive_idx: off_idx,
                        on_directive_idx: Some(d_idx),
                        body_start,
                        body_end: line_start.max(body_start),
                    });
                }
                directives.push(dir);
                directive_index = Some(d_idx);
            }

            comments.push(CommentItem {
                kind: CommentKind::Line,
                byte_start: comment_start,
                byte_end: comment_end,
                line_start,
                line_no: line_idx + 1,
                col_no,
                has_code_before_on_line: has_code_before,
                has_code_after_on_line: false,
                directive_index,
            });
            continue;
        }

        // Block comment: `{- ... -}` (nestable).
        if b == b'{' && i + 1 < len && bytes[i + 1] == b'-' {
            let comment_start = i;
            let line_idx = line_starts
                .partition_point(|&s| s <= comment_start)
                .saturating_sub(1);
            let line_start = line_starts[line_idx];
            let prefix_on_line = &src[line_start..comment_start];
            let has_code_before = prefix_on_line.chars().any(|c| !matches!(c, ' ' | '\t'));
            let col_no = prefix_on_line.chars().count() + 1;

            i += 2;
            let mut depth = 1usize;
            while i < len && depth > 0 {
                if bytes[i] == b'{' && i + 1 < len && bytes[i + 1] == b'-' {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'-' && i + 1 < len && bytes[i + 1] == b'}' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            let comment_end = i;
            let mut rest_idx = comment_end;
            let mut has_code_after = false;
            while rest_idx < len && bytes[rest_idx] != b'\n' && bytes[rest_idx] != b'\r' {
                if bytes[rest_idx] == b'-' && rest_idx + 1 < len && bytes[rest_idx + 1] == b'-' {
                    break;
                }
                if !matches!(bytes[rest_idx], b' ' | b'\t') {
                    has_code_after = true;
                    break;
                }
                rest_idx += 1;
            }
            comments.push(CommentItem {
                kind: CommentKind::Block,
                byte_start: comment_start,
                byte_end: comment_end,
                line_start,
                line_no: line_idx + 1,
                col_no,
                has_code_before_on_line: has_code_before,
                has_code_after_on_line: has_code_after,
                directive_index: None,
            });
            continue;
        }

        // String literal `"..."` (including `{{` / `}}` and `{expr}` f-string interpolation,
        // while still collecting any comments inside `{expr}`? Note: Nessa f-strings forbid `\n`
        // inside string literals, and `skip_nessa_string` skips to closing `"` cleanly).
        if b == b'"' {
            i = skip_nessa_string(src, i);
            continue;
        }

        // Backtick arbitrary identifier `` `...` ``.
        if b == b'`' {
            i = skip_backtick_ident(bytes, i);
            continue;
        }

        // Single quote `'`: char literal (`'a'`, `'\n'`, `'\x41'`, `'\u0041'`), `'{...}` macro,
        // or view operator (`expr'view`, `.__foo'builtin`, `` x'`view` ``).
        if b == b'\'' {
            i = skip_quote_or_char_or_macro(src, i);
            continue;
        }

        // Check if an unclosed `--% .off` region is closed by exiting its enclosing `{}` block!
        if b == b'}' {
            if let Some((off_idx, off_depth, body_start)) = active_off
                && brace_depth == off_depth
                && off_depth > 0
            {
                active_off = None;
                let line_idx = line_starts.partition_point(|&s| s <= i).saturating_sub(1);
                let close_line_start = line_starts[line_idx];
                let prefix = &src[close_line_start..i];
                let body_end = if prefix.chars().all(|c| matches!(c, ' ' | '\t')) {
                    close_line_start
                } else {
                    i
                };
                off_regions.push(OffRegion {
                    off_directive_idx: off_idx,
                    on_directive_idx: None,
                    body_start,
                    body_end: body_end.max(body_start),
                });
            }
            brace_depth = brace_depth.saturating_sub(1);
            i += 1;
            continue;
        }

        if b == b'{' {
            brace_depth += 1;
            i += 1;
            continue;
        }

        if let Some(ch) = src[i..].chars().next() {
            i += ch.len_utf8();
        } else {
            break;
        }
    }

    // Close any unclosed `--% .off` at EOF.
    if let Some((off_idx, _, body_start)) = active_off.take() {
        off_regions.push(OffRegion {
            off_directive_idx: off_idx,
            on_directive_idx: None,
            body_start,
            body_end: len.max(body_start),
        });
    }

    PreScanResult {
        shadow_bytes: shadow,
        line_starts,
        comments,
        directives,
        warnings,
        off_regions,
        bridge,
    }
}

fn blank_preserving_newlines(buf: &mut [u8], start: usize, end: usize) {
    let end = end.min(buf.len());
    for b in &mut buf[start.min(end)..end] {
        if *b != b'\n' && *b != b'\r' {
            *b = b' ';
        }
    }
}

/// Skip a Nessa `"..."` string literal starting at `start` (`bytes[start] == b'"'`),
/// handling `\` escapes, `{{` / `}}` literal braces, and `{expr}` interpolations.
pub fn skip_nessa_string(src: &str, start: usize) -> usize {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let mut i = start + 1;
    while i < len {
        match bytes[i] {
            b'"' => return i + 1,
            b'\n' => return i,
            b'\\' => {
                i += 1;
                if let Some(ch) = src.get(i..).and_then(|s| s.chars().next()) {
                    i += ch.len_utf8();
                }
            }
            b'{' if bytes.get(i + 1) == Some(&b'{') => {
                i += 2;
            }
            b'}' if bytes.get(i + 1) == Some(&b'}') => {
                i += 2;
            }
            b'{' => {
                // Interpolated `{ expr }` inside f-string!
                i += 1;
                let mut depth = 1usize;
                while i < len && depth > 0 {
                    match bytes[i] {
                        b'\n' => return i,
                        b'"' => {
                            i = skip_nessa_string(src, i);
                            continue;
                        }
                        b'`' => {
                            i = skip_backtick_ident(bytes, i);
                            continue;
                        }
                        b'\'' => {
                            i = skip_quote_or_char_or_macro(src, i);
                            continue;
                        }
                        b'{' if bytes.get(i + 1) == Some(&b'-') => {
                            i += 2;
                            let mut cd = 1usize;
                            while i < len && cd > 0 {
                                if bytes[i] == b'{' && i + 1 < len && bytes[i + 1] == b'-' {
                                    cd += 1;
                                    i += 2;
                                } else if bytes[i] == b'-' && i + 1 < len && bytes[i + 1] == b'}' {
                                    cd -= 1;
                                    i += 2;
                                } else {
                                    i += 1;
                                }
                            }
                            continue;
                        }
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
            }
            _ => {
                if let Some(ch) = src[i..].chars().next() {
                    i += ch.len_utf8();
                } else {
                    i += 1;
                }
            }
        }
    }
    len
}

fn skip_backtick_ident(bytes: &[u8], start: usize) -> usize {
    let len = bytes.len();
    let mut i = start + 1;
    while i < len {
        match bytes[i] {
            b'`' => return i + 1,
            b'\n' => return i,
            _ => i += 1,
        }
    }
    len
}

/// Mirror `Lexer::lex_quote_or_char_or_macro` (`engine/lexer/src/lexer.rs`) so `'view` / `'builtin`
/// ticks without a closing `'` are NEVER misread as an unclosed character literal!
pub fn skip_quote_or_char_or_macro(src: &str, start: usize) -> usize {
    let bytes = src.as_bytes();
    let len = bytes.len();
    let after_quote = start + 1;
    if after_quote >= len {
        return len;
    }

    match bytes[after_quote] {
        b'{' => skip_macro_content(bytes, after_quote),
        b' ' | b'\t' => {
            if let Some(end) = try_consume_closed_char_literal(src, after_quote) {
                return end;
            }
            let mut j = after_quote;
            while j < len && matches!(bytes[j], b' ' | b'\t') {
                j += 1;
            }
            if j < len && bytes[j] == b'{' {
                return skip_macro_content(bytes, j);
            }
            after_quote
        }
        _ => {
            if let Some(end) = try_consume_closed_char_literal(src, after_quote) {
                end
            } else {
                // View operator `'` — leave following identifier / backtick untouched!
                after_quote
            }
        }
    }
}

fn skip_macro_content(bytes: &[u8], open_brace: usize) -> usize {
    let len = bytes.len();
    let mut i = open_brace + 1;
    let mut depth = 1usize;
    while i < len && depth > 0 {
        if bytes[i] == b'{' {
            depth += 1;
        } else if bytes[i] == b'}' {
            depth -= 1;
        }
        i += 1;
    }
    i
}

/// Return `Some(end_after_closing_quote)` iff `src[after_quote..]` is a closed Nessa char literal.
fn try_consume_closed_char_literal(src: &str, after_quote: usize) -> Option<usize> {
    let s = src.get(after_quote..)?;
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    if bytes[0] == b'\\' {
        let rest = s.get(1..)?;
        let esc = *rest.as_bytes().first()?;
        let payload_bytes = match esc {
            b'n' | b't' | b'r' | b'\\' | b'"' | b'\'' | b'0' => 2usize,
            b'x' if rest.len() >= 3
                && rest.as_bytes()[1].is_ascii_hexdigit()
                && rest.as_bytes()[2].is_ascii_hexdigit() =>
            {
                4
            }
            b'u' if rest.len() >= 5
                && rest.as_bytes()[1..5].iter().all(|b| b.is_ascii_hexdigit()) =>
            {
                6
            }
            _ => return None,
        };
        if s.as_bytes().get(payload_bytes) == Some(&b'\'') {
            return Some(after_quote + payload_bytes + 1);
        }
        return None;
    }

    let mut chars = s.chars();
    let first = chars.next()?;
    if first == '\'' || first == '\n' || first == '\r' {
        return None;
    }
    if chars.next() == Some('\'') {
        Some(after_quote + first.len_utf8() + 1)
    } else {
        None
    }
}

fn collect_skip_targets(
    src: &str,
    directives: &[DirectiveLine],
    off_regions: &[OffRegion],
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for dir in directives {
        if dir.skip_flag() != Some(true) {
            continue;
        }
        if off_regions
            .iter()
            .any(|r| dir.comment_start >= r.body_start && dir.comment_end <= r.body_end)
        {
            continue;
        }
        if let Some(range) = scan_skip_target_range(src, dir.line_end_with_newline) {
            out.push(range);
        }
    }
    out
}

fn scan_skip_target_range(src: &str, from_byte: usize) -> Option<(usize, usize)> {
    let target_start = skip_ws_and_comments(src, from_byte);
    let bytes = src.as_bytes();
    let len = bytes.len();
    if target_start >= len {
        return None;
    }

    let mut i = target_start;
    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut saw_non_ws = false;

    while i < len {
        let b = bytes[i];
        if b == b'"' {
            saw_non_ws = true;
            i = skip_nessa_string(src, i);
            continue;
        }
        if b == b'`' {
            saw_non_ws = true;
            i = skip_backtick_ident(bytes, i);
            continue;
        }
        if b == b'\'' {
            saw_non_ws = true;
            i = skip_quote_or_char_or_macro(src, i);
            continue;
        }
        if b == b'-' && i + 1 < len && bytes[i + 1] == b'-' {
            // Stop before a trailing comment at top-level newline!
            if brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 && saw_non_ws {
                let end = trim_end_horizontal_ws(src, target_start, i);
                return (end > target_start).then_some((target_start, end));
            }
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b == b'{' && i + 1 < len && bytes[i + 1] == b'-' {
            i += 2;
            let mut cd = 1usize;
            while i < len && cd > 0 {
                if bytes[i] == b'{' && i + 1 < len && bytes[i + 1] == b'-' {
                    cd += 1;
                    i += 2;
                } else if bytes[i] == b'-' && i + 1 < len && bytes[i + 1] == b'}' {
                    cd -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }

        match b {
            b'(' => {
                saw_non_ws = true;
                paren_depth += 1;
            }
            b')' => {
                paren_depth = paren_depth.saturating_sub(1);
            }
            b'[' => {
                saw_non_ws = true;
                bracket_depth += 1;
            }
            b']' => {
                bracket_depth = bracket_depth.saturating_sub(1);
            }
            b'{' => {
                saw_non_ws = true;
                brace_depth += 1;
            }
            b'}' => {
                if brace_depth == 0 {
                    let end = trim_end_horizontal_ws(src, target_start, i);
                    return (end > target_start).then_some((target_start, end));
                }
                brace_depth -= 1;
                if brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 {
                    // Check if `else` immediately follows `}`!
                    let after = skip_ws_and_comments(src, i + 1);
                    if src.get(after..).is_some_and(|rest| {
                        rest.starts_with("else")
                            && !rest[4..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
                    }) {
                        i += 1;
                        continue;
                    }
                    return Some((target_start, i + 1));
                }
            }
            b';' if brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 => {
                let end = trim_end_horizontal_ws(src, target_start, i);
                return (end > target_start).then_some((target_start, end));
            }
            b'\n' if brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 && saw_non_ws => {
                // Check if next non-comment token opens `{`:
                let after_nl = skip_ws_and_comments(src, i + 1);
                if bytes.get(after_nl) == Some(&b'{') {
                    i += 1;
                    continue;
                }
                let mut line_end = i;
                if line_end > target_start && bytes[line_end - 1] == b'\r' {
                    line_end -= 1;
                }
                let end = trim_end_horizontal_ws(src, target_start, line_end);
                return (end > target_start).then_some((target_start, end));
            }
            c if !c.is_ascii_whitespace() => {
                saw_non_ws = true;
            }
            _ => {}
        }

        if let Some(ch) = src[i..].chars().next() {
            i += ch.len_utf8();
        } else {
            break;
        }
    }

    if saw_non_ws && brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 {
        let end = trim_end_horizontal_ws(src, target_start, len);
        return (end > target_start).then_some((target_start, end));
    }
    None
}

fn trim_end_horizontal_ws(src: &str, start: usize, mut end: usize) -> usize {
    let bytes = src.as_bytes();
    while end > start && matches!(bytes[end - 1], b' ' | b'\t' | b'\r') {
        end -= 1;
    }
    end
}

fn write_skip_placeholder(shadow: &mut [u8], start: usize, end: usize, use_trait_fn: bool) -> bool {
    let avail_first_line = shadow[start..end]
        .iter()
        .position(|&b| b == b'\n' || b == b'\r')
        .unwrap_or(end - start);
    if use_trait_fn {
        if avail_first_line < 6 {
            return false;
        }
        blank_preserving_newlines(shadow, start, end);
        shadow[start..start + 6].copy_from_slice(b"fn f()");
        true
    } else {
        if avail_first_line < 2 {
            return false;
        }
        blank_preserving_newlines(shadow, start, end);
        shadow[start..start + 2].copy_from_slice(b"_f");
        true
    }
}

fn skip_ws_and_comments(src: &str, mut i: usize) -> usize {
    let bytes = src.as_bytes();
    let len = bytes.len();
    while i < len {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if bytes[i] == b'-' && i + 1 < len && bytes[i + 1] == b'-' {
            while i < len && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'{' && i + 1 < len && bytes[i + 1] == b'-' {
            i += 2;
            let mut d = 1usize;
            while i < len && d > 0 {
                if bytes[i] == b'{' && i + 1 < len && bytes[i + 1] == b'-' {
                    d += 1;
                    i += 2;
                } else if bytes[i] == b'-' && i + 1 < len && bytes[i + 1] == b'}' {
                    d -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        break;
    }
    i
}
