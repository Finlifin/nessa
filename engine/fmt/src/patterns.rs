//! Delimited sequence helper and pattern formatting for all Nessa pattern constructs.

use ast::{NodeIndex, NodeKind};
use lexer::{Token, TokenKind};

use crate::doc::Doc;
use crate::formatter::{Formatter, TokenSlot};

/// Delimiter style for comma-separated sequences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelimStyle {
    /// `(a, b)` — no inner spaces when flat.
    Parens,
    /// `(a,)` — 1-element tuple requiring trailing comma even when flat.
    SingleTupleParens,
    /// `[a, b]` — no inner spaces when flat.
    Brackets,
    /// `|a, b|` — lambda parameters.
    Pipes,
    /// `{ a, b }` — 1 space inside `{` and `}` when non-empty and flat; `{}` when empty.
    SpacedBraces,
    /// `{a, b}` — no spaces inside `{` and `}` when flat (used by `use a.{b, c}`).
    CompactBraces,
}

impl DelimStyle {
    fn open_str(self) -> &'static str {
        match self {
            Self::Parens | Self::SingleTupleParens => "(",
            Self::Brackets => "[",
            Self::Pipes => "|",
            Self::SpacedBraces | Self::CompactBraces => "{",
        }
    }

    fn close_str(self) -> &'static str {
        match self {
            Self::Parens | Self::SingleTupleParens => ")",
            Self::Brackets => "]",
            Self::Pipes => "|",
            Self::SpacedBraces | Self::CompactBraces => "}",
        }
    }
}

pub fn find_matching_delim(
    tokens: &[Token],
    open_idx: usize,
    open_kind: TokenKind,
    close_kind: TokenKind,
) -> Option<usize> {
    if tokens.get(open_idx)?.kind != open_kind {
        return None;
    }
    if open_kind == close_kind {
        // E.g. `|params|` where `Pipe` is both open and close (nested `(...)`/`[...]`/`{...}` tracked).
        let mut inner_depth = 0usize;
        for (idx, tok) in tokens.iter().enumerate().skip(open_idx + 1) {
            match tok.kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => inner_depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    inner_depth = inner_depth.saturating_sub(1);
                }
                k if k == close_kind && inner_depth == 0 => return Some(idx),
                TokenKind::Eof => break,
                _ => {}
            }
        }
        return None;
    }
    let mut depth = 0usize;
    for (idx, tok) in tokens.iter().enumerate().skip(open_idx) {
        if tok.kind == open_kind {
            depth += 1;
        } else if tok.kind == close_kind {
            depth -= 1;
            if depth == 0 {
                return Some(idx);
            }
        } else if tok.kind == TokenKind::Eof {
            break;
        }
    }
    None
}

pub fn strip_last_hardline(doc: Doc) -> Doc {
    match doc {
        Doc::HardLine => Doc::Nil,
        Doc::Concat(mut items) => {
            if matches!(items.last(), Some(Doc::HardLine)) {
                items.pop();
            }
            Doc::concat(items)
        }
        other => other,
    }
}

impl<'a> Formatter<'a> {
    /// Format a comma-separated delimited list `( ... )`, `[ ... ]`, `| ... |`, or `{ ... }`
    /// with trivia preservation, optional width budget, and vertical trailing-comma rule.
    pub fn format_delimited_seq<F>(
        &mut self,
        elems: &[NodeIndex],
        open_tok_idx: usize,
        close_tok_idx: usize,
        style: DelimStyle,
        flat_budget: Option<usize>,
        mut render_elem: F,
    ) -> Doc
    where
        F: FnMut(&mut Self, NodeIndex, TokenSlot) -> Doc,
    {
        let open_end_byte = self
            .parsed
            .sig_tokens
            .get(open_tok_idx)
            .map_or(0, |t| t.to as usize);
        let close_start_byte = self
            .parsed
            .sig_tokens
            .get(close_tok_idx)
            .map_or(self.parsed.original_src.len(), |t| t.from as usize);

        if elems.is_empty() {
            let (inner_comments, _, _) = self.comments.take_leading_before(
                open_end_byte,
                close_start_byte,
                self.indent_spaces + self.tab_spaces(),
                self.brace_depth,
                false,
                &mut self.cfg_stack,
            );
            if matches!(inner_comments, Doc::Nil) {
                return Doc::concat([Doc::str(style.open_str()), Doc::str(style.close_str())]);
            }
            let tab = self.tab_spaces();
            return Doc::concat([
                Doc::str(style.open_str()),
                Doc::indent(
                    tab,
                    Doc::concat([Doc::HardLine, strip_last_hardline(inner_comments)]),
                ),
                Doc::HardLine,
                Doc::str(style.close_str()),
            ]);
        }

        let mut slots = Vec::with_capacity(elems.len());
        let mut cursor_tok = open_tok_idx + 1;
        let inner_bound = close_tok_idx.saturating_sub(1).max(cursor_tok);
        let outer = TokenSlot::new(cursor_tok, inner_bound);

        for &elem in elems {
            let s = outer.child_slot_from(self.parsed, cursor_tok, elem);
            let lo = self.parsed.sig_tokens[s.first].from as usize;
            let mut hi = self.parsed.sig_tokens[s.last].to as usize;
            if let Some(b) = self.parsed.bridge.constructs_by_start.get(&lo) {
                hi = hi.max(b.span_bytes().1);
            }
            let mut sep_end_byte = hi;
            let mut next_tok = s.last + 1;
            if next_tok < close_tok_idx
                && matches!(
                    self.parsed.sig_tokens[next_tok].kind,
                    TokenKind::Comma | TokenKind::Semi
                )
            {
                sep_end_byte = self.parsed.sig_tokens[next_tok].to as usize;
                next_tok += 1;
            }
            let end_line = self.parsed.line_of_byte(sep_end_byte.saturating_sub(1));
            slots.push((elem, s, lo, hi, sep_end_byte, end_line));
            cursor_tok = next_tok;
        }

        let tab = self.tab_spaces();
        let saved_indent = self.indent_spaces;
        self.indent_spaces += tab;
        if matches!(style, DelimStyle::SpacedBraces | DelimStyle::CompactBraces) {
            self.brace_depth += 1;
        }

        let mut item_docs: Vec<Doc> = Vec::with_capacity(elems.len() * 2);
        let mut prev_end = open_end_byte;
        let mut forced_multiline = false;

        for (i, &(elem, elem_slot, lo, hi, sep_end, end_line)) in slots.iter().enumerate() {
            let (lead, _, dir_flags) = self.comments.take_leading_before(
                prev_end,
                lo,
                self.indent_spaces,
                self.brace_depth,
                i > 0,
                &mut self.cfg_stack,
            );
            if !matches!(lead, Doc::Nil) {
                forced_multiline = true;
            }
            if i > 0 {
                let user_blank = self.parsed.blank_lines_in_gap(prev_end, lo) > 0;
                if user_blank {
                    forced_multiline = true;
                    item_docs.push(Doc::BlankLine);
                } else {
                    item_docs.push(Doc::Line);
                }
            }
            if !matches!(lead, Doc::Nil) {
                item_docs.push(lead);
            }

            let rendered = if dir_flags.skip {
                forced_multiline = true;
                self.format_skipped_bytes(lo, hi)
            } else if dir_flags.preserve {
                forced_multiline = true;
                self.format_preserved_bytes(lo, hi)
            } else {
                render_elem(self, elem, elem_slot)
            };

            let is_last = i + 1 == slots.len();
            let comma_doc = if !is_last || style == DelimStyle::SingleTupleParens {
                Doc::str(",")
            } else if style == DelimStyle::Pipes {
                Doc::Nil
            } else {
                Doc::trailing_comma()
            };

            let next_limit = slots.get(i + 1).map_or(close_start_byte, |n| n.2);
            let cfg_now = self.cfg();
            let trailing = self.comments.take_same_line_trailing(
                end_line,
                next_limit,
                self.indent_spaces,
                &cfg_now,
            );
            if !matches!(trailing, Doc::Nil) {
                forced_multiline = true;
            }

            item_docs.push(rendered);
            item_docs.push(comma_doc);
            if !matches!(trailing, Doc::Nil) {
                item_docs.push(trailing);
            }
            prev_end = sep_end;
        }

        let (tail_comments, _, _) = self.comments.take_leading_before(
            prev_end,
            close_start_byte,
            self.indent_spaces,
            self.brace_depth,
            true,
            &mut self.cfg_stack,
        );
        if !matches!(tail_comments, Doc::Nil) {
            forced_multiline = true;
            item_docs.push(Doc::HardLine);
            item_docs.push(strip_last_hardline(tail_comments));
        }

        if matches!(style, DelimStyle::SpacedBraces | DelimStyle::CompactBraces) {
            self.brace_depth -= 1;
            self.cfg_stack.pop_above(self.brace_depth);
        }
        self.indent_spaces = saved_indent;

        let open_break = if style == DelimStyle::SpacedBraces {
            Doc::Line
        } else {
            Doc::SoftLine
        };
        let close_break = if style == DelimStyle::SpacedBraces {
            Doc::Line
        } else {
            Doc::SoftLine
        };

        if forced_multiline {
            Doc::concat([
                Doc::str(style.open_str()),
                Doc::indent(tab, Doc::concat([Doc::HardLine, Doc::concat(item_docs)])),
                Doc::HardLine,
                Doc::str(style.close_str()),
            ])
        } else {
            let inner = Doc::concat([
                Doc::str(style.open_str()),
                Doc::indent(tab, Doc::concat([open_break, Doc::concat(item_docs)])),
                close_break,
                Doc::str(style.close_str()),
            ]);
            if let Some(budget) = flat_budget {
                Doc::group_with_budget(budget, inner)
            } else {
                Doc::group(inner)
            }
        }
    }

    /// Check whether the first token of `slot` starts with `-` (requiring a space after `..` / `..=` / `-`).
    pub fn slot_starts_with_minus(&self, slot: TokenSlot) -> bool {
        self.parsed
            .sig_tokens
            .get(slot.first)
            .is_some_and(|t| t.kind == TokenKind::Minus)
    }

    /// Format any Nessa pattern node, preserving explicit outer parentheses `(pat)`.
    pub fn format_pattern(&mut self, node: NodeIndex, slot: TokenSlot) -> Doc {
        if node.is_null() {
            return Doc::Nil;
        }
        let (paren_count, inner_slot) = slot.unwrap_parens(self.parsed, node);
        if paren_count > 0 {
            let inner_doc = self.format_pattern_unwrapped(node, inner_slot);
            let open = "(".repeat(paren_count);
            let close = ")".repeat(paren_count);
            return Doc::concat([Doc::text(open), inner_doc, Doc::text(close)]);
        }
        self.format_pattern_unwrapped(node, inner_slot)
    }

    fn format_pattern_unwrapped(&mut self, node: NodeIndex, slot: TokenSlot) -> Doc {
        let kind = self.parsed.ast.node(node).kind;
        let ch = self.parsed.ast.fixed_children(node);

        match kind {
            NodeKind::Id => Doc::text(self.format_ident(node, slot.first)),
            NodeKind::Underscore => Doc::str("_"),
            NodeKind::Null => Doc::str("null"),
            NodeKind::Unit => Doc::str("()"),
            NodeKind::SelfLower => Doc::str("self"),
            NodeKind::SelfUpper => Doc::str("Self"),
            NodeKind::Int | NodeKind::Real | NodeKind::Str | NodeKind::Char | NodeKind::Bool => {
                Doc::text(self.literal_slice(node, slot.first))
            }
            NodeKind::Symbol => {
                let id_str = self.format_ident(ch[0], slot.first + 1);
                Doc::concat([Doc::str("."), Doc::text(id_str)])
            }
            NodeKind::Negative => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
                let inner_doc = self.format_pattern(ch[0], inner_slot);
                Doc::concat([Doc::str("-"), inner_doc])
            }
            NodeKind::PatternNot => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
                let inner_doc = self.format_pattern(ch[0], inner_slot);
                Doc::concat([Doc::str("not "), inner_doc])
            }
            NodeKind::PatternError => {
                // `error pat` or `!pat`: check first token in `slot`.
                let is_bang = self
                    .parsed
                    .sig_tokens
                    .get(slot.first)
                    .is_some_and(|t| t.kind == TokenKind::Bang);
                let inner_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
                let inner_doc = self.format_pattern(ch[0], inner_slot);
                if is_bang {
                    Doc::concat([Doc::str("!"), inner_doc])
                } else {
                    Doc::concat([Doc::str("error "), inner_doc])
                }
            }
            NodeKind::PatternEffect => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
                let inner_doc = self.format_pattern(ch[0], inner_slot);
                Doc::concat([Doc::str("#"), inner_doc])
            }
            NodeKind::PatternAsync => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
                let inner_doc = self.format_pattern(ch[0], inner_slot);
                Doc::concat([Doc::str("async "), inner_doc])
            }
            NodeKind::PatternTypeFamily => {
                // `E.*`
                let left_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let left_doc = self.format_pattern(ch[0], left_slot);
                Doc::concat([left_doc, Doc::str(".*")])
            }
            NodeKind::PropertyPattern => {
                // Postfix projection in pattern: `pat.id` (e.g. `Option.some`, `IpAddr.v4`).
                let left_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let dot_tok = self
                    .parsed
                    .find_depth_zero_token(
                        left_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::Dot,
                        false,
                    )
                    .unwrap_or(left_slot.last + 1);
                let left_doc = self.format_pattern(ch[0], left_slot);
                let id_str = self.format_ident(ch[1], dot_tok + 1);
                Doc::concat([left_doc, Doc::str("."), Doc::text(id_str)])
            }
            NodeKind::Property => {
                // Named field inside record/brace destructuring pattern: `id: pat`
                let id_str = self.format_ident(ch[0], slot.first);
                let colon_tok = self
                    .parsed
                    .find_depth_zero_token(
                        slot.first + 1,
                        slot.last,
                        |k| k == TokenKind::Colon,
                        false,
                    )
                    .unwrap_or(slot.first + 1);
                let pat_slot = slot.child_slot_from(self.parsed, colon_tok + 1, ch[1]);
                let pat_doc = self.format_pattern(ch[1], pat_slot);
                Doc::concat([Doc::text(id_str), Doc::str(": "), pat_doc])
            }
            NodeKind::PatternOptionSome => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let inner_doc = self.format_pattern(ch[0], inner_slot);
                Doc::concat([inner_doc, Doc::str("?")])
            }
            NodeKind::PatternErrorOk => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let inner_doc = self.format_pattern(ch[0], inner_slot);
                Doc::concat([inner_doc, Doc::str("!")])
            }
            NodeKind::PatternRestBind => {
                if ch[0].is_null() {
                    Doc::str("...")
                } else {
                    let inner_slot = slot.child_slot_from(self.parsed, slot.first + 3, ch[0]);
                    let inner_doc = self.format_pattern(ch[0], inner_slot);
                    Doc::concat([Doc::str("..."), inner_doc])
                }
            }
            NodeKind::PatternAsBind => {
                let lhs_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let as_tok = self
                    .parsed
                    .find_depth_zero_token(
                        lhs_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::KwAs,
                        false,
                    )
                    .unwrap_or(lhs_slot.last + 1);
                let lhs_doc = self.format_pattern(ch[0], lhs_slot);
                let id_str = self.format_ident(ch[1], as_tok + 1);
                Doc::concat([lhs_doc, Doc::str(" as "), Doc::text(id_str)])
            }
            NodeKind::PatternTyped => {
                // `(id | _): Type`
                let binder_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let colon_tok = self
                    .parsed
                    .find_depth_zero_token(
                        binder_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::Colon,
                        false,
                    )
                    .unwrap_or(binder_slot.last + 1);
                let ty_slot = slot.child_slot_from(self.parsed, colon_tok + 1, ch[1]);
                let binder_doc = self.format_pattern(ch[0], binder_slot);
                let ty_doc = self.format_expr(ch[1], ty_slot);
                Doc::concat([binder_doc, Doc::str(": "), ty_doc])
            }
            NodeKind::PatternOr => {
                let lhs_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let or_tok = self
                    .parsed
                    .find_depth_zero_token(
                        lhs_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::KwOr,
                        false,
                    )
                    .unwrap_or(lhs_slot.last + 1);
                let rhs_slot = slot.child_slot_from(self.parsed, or_tok + 1, ch[1]);
                let lhs_doc = self.format_pattern(ch[0], lhs_slot);
                let rhs_doc = self.format_pattern(ch[1], rhs_slot);
                let tab = self.tab_spaces();
                Doc::group(Doc::concat([
                    lhs_doc,
                    Doc::indent(tab, Doc::concat([Doc::Line, Doc::str("or "), rhs_doc])),
                ]))
            }
            NodeKind::PatternIfGuard => {
                let pat_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let if_tok = self
                    .parsed
                    .find_depth_zero_token(
                        pat_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::KwIf,
                        false,
                    )
                    .unwrap_or(pat_slot.last + 1);
                let guard_slot = slot.child_slot_from(self.parsed, if_tok + 1, ch[1]);
                let pat_doc = self.format_pattern(ch[0], pat_slot);
                let guard_doc = self.format_expr(ch[1], guard_slot);
                let tab = self.tab_spaces();
                Doc::group(Doc::concat([
                    pat_doc,
                    Doc::indent(tab, Doc::concat([Doc::Line, Doc::str("if "), guard_doc])),
                ]))
            }
            NodeKind::PatternAndIs => {
                // `lhs_pat and expr is rhs_pat`
                let lhs_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let and_tok = self
                    .parsed
                    .find_depth_zero_token(
                        lhs_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::KwAnd,
                        false,
                    )
                    .unwrap_or(lhs_slot.last + 1);
                let expr_slot = slot.child_slot_from(self.parsed, and_tok + 1, ch[1]);
                let is_tok = self
                    .parsed
                    .find_depth_zero_token(
                        expr_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::KwIs,
                        false,
                    )
                    .unwrap_or(expr_slot.last + 1);
                let rhs_slot = slot.child_slot_from(self.parsed, is_tok + 1, ch[2]);
                let lhs_doc = self.format_pattern(ch[0], lhs_slot);
                let expr_doc = self.format_expr(ch[1], expr_slot);
                let rhs_doc = self.format_pattern(ch[2], rhs_slot);
                let tab = self.tab_spaces();
                Doc::group(Doc::concat([
                    lhs_doc,
                    Doc::indent(
                        tab,
                        Doc::concat([
                            Doc::Line,
                            Doc::str("and "),
                            expr_doc,
                            Doc::str(" is "),
                            rhs_doc,
                        ]),
                    ),
                ]))
            }
            NodeKind::PatternCall => {
                let ctor_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let args = self.parsed.ast.multi_children(node);
                let open_paren = self
                    .parsed
                    .find_depth_zero_token(
                        ctor_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::LParen,
                        false,
                    )
                    .unwrap_or(ctor_slot.last + 1);
                let ctor_doc = self.format_pattern(ch[0], ctor_slot);
                let budget = Some(self.cfg().width_heuristics().fn_call_width);
                let args_doc = self.format_delimited_seq(
                    args,
                    open_paren,
                    slot.last,
                    DelimStyle::Parens,
                    budget,
                    |fmt, arg, arg_slot| fmt.format_pattern(arg, arg_slot),
                );
                Doc::concat([ctor_doc, args_doc])
            }
            NodeKind::PatternExtendedCall => {
                let ctor_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let args = self.parsed.ast.multi_children(node);
                let open_brace = self
                    .parsed
                    .find_depth_zero_token(
                        ctor_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::LBrace,
                        false,
                    )
                    .unwrap_or(ctor_slot.last + 1);
                let ctor_doc = self.format_pattern(ch[0], ctor_slot);
                let budget = Some(self.cfg().width_heuristics().struct_variant_width);
                let args_doc = self.format_delimited_seq(
                    args,
                    open_brace,
                    slot.last,
                    DelimStyle::SpacedBraces,
                    budget,
                    |fmt, arg, arg_slot| fmt.format_pattern(arg, arg_slot),
                );
                Doc::concat([ctor_doc, Doc::space(), args_doc])
            }
            NodeKind::PatternTuple => {
                let elems = self.parsed.ast.multi_children(node);
                let style = if elems.len() == 1 {
                    DelimStyle::SingleTupleParens
                } else {
                    DelimStyle::Parens
                };
                let budget = Some(self.cfg().width_heuristics().fn_call_width);
                self.format_delimited_seq(
                    elems,
                    slot.first,
                    slot.last,
                    style,
                    budget,
                    |fmt, el, el_slot| fmt.format_pattern(el, el_slot),
                )
            }
            NodeKind::PatternList => {
                let elems = self.parsed.ast.multi_children(node);
                let budget = Some(self.cfg().width_heuristics().array_width);
                self.format_delimited_seq(
                    elems,
                    slot.first,
                    slot.last,
                    DelimStyle::Brackets,
                    budget,
                    |fmt, el, el_slot| fmt.format_pattern(el, el_slot),
                )
            }
            NodeKind::PatternRecord => {
                let props = self.parsed.ast.multi_children(node);
                let budget = Some(self.cfg().width_heuristics().struct_lit_width);
                self.format_delimited_seq(
                    props,
                    slot.first,
                    slot.last,
                    DelimStyle::SpacedBraces,
                    budget,
                    |fmt, prop, prop_slot| fmt.format_pattern(prop, prop_slot),
                )
            }
            NodeKind::PatternFromExpr => {
                let expr_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                self.format_expr(ch[0], expr_slot)
            }
            _ => self.format_expr(node, slot),
        }
    }
}
