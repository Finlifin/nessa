//! Statement, control-flow, block, closure, f-string, type-expression, and operator formatting.

use ast::{NodeIndex, NodeKind};
use lexer::TokenKind;

use crate::config::FormatConfig;
use crate::doc::{Doc, print_doc};
use crate::formatter::{ContainerKind, Formatter, TokenSlot, unwrap_modifiers_kind};
use crate::patterns::{DelimStyle, find_matching_delim};

impl<'a> Formatter<'a> {
    /// Format a statement or expression node, returning `(doc, ends_with_rbrace)`.
    pub fn format_statement_or_expr(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        if node.is_null() {
            return (Doc::Nil, false);
        }
        if let Some(bridged) = self.bridged_at(node, slot.first) {
            let ends_brace = self
                .parsed
                .original_src
                .get(bridged.span_bytes().0..bridged.span_bytes().1)
                .is_some_and(|s| s.trim_end().ends_with('}'));
            return (self.format_bridged(bridged), ends_brace);
        }

        let kind = self.parsed.ast.node(node).kind;
        match kind {
            NodeKind::ExprStatement => {
                let inner = self.parsed.ast.fixed_children(node)[0];
                let inner_slot = slot.child_slot_from(self.parsed, slot.first, inner);
                self.format_statement_or_expr(inner, inner_slot)
            }
            NodeKind::LetDecl => self.format_local_binding(node, slot, "let "),
            NodeKind::ConstDecl => self.format_local_binding(node, slot, "const "),
            NodeKind::VarDecl => self.format_local_binding(node, slot, "var "),
            NodeKind::Assign => self.format_assign_stmt(node, slot, "="),
            NodeKind::AddAssign => self.format_assign_stmt(node, slot, "+="),
            NodeKind::SubAssign => self.format_assign_stmt(node, slot, "-="),
            NodeKind::MulAssign => self.format_assign_stmt(node, slot, "*="),
            NodeKind::DivAssign => self.format_assign_stmt(node, slot, "/="),
            NodeKind::ModAssign => self.format_assign_stmt(node, slot, "%="),
            NodeKind::DeferStatement => self.format_defer_stmt(node, slot),
            NodeKind::ReturnStatement => self.format_jump_with_value(node, slot, "return"),
            NodeKind::ResumeStatement => self.format_jump_with_value(node, slot, "resume"),
            NodeKind::BreakStatement => self.format_loop_jump(node, slot, "break"),
            NodeKind::ContinueStatement => self.format_loop_jump(node, slot, "continue"),
            NodeKind::ForLoop => (self.format_for_loop(node, slot), true),
            NodeKind::WhileLoop => (self.format_while_loop(node, slot), true),
            NodeKind::Block => (
                self.format_block(node, slot, None, ContainerKind::ValueBlock),
                true,
            ),
            _ => self.format_expr_with_rbrace(node, slot),
        }
    }

    /// Format an expression node, returning `(doc, ends_with_rbrace)`.
    pub fn format_expr_with_rbrace(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        if node.is_null() {
            return (Doc::Nil, false);
        }
        if self.parsed.ast.node(node).kind == NodeKind::ExprStatement {
            let inner = self.parsed.ast.fixed_children(node)[0];
            let inner_slot = slot.child_slot_from(self.parsed, slot.first, inner);
            return self.format_expr_with_rbrace(inner, inner_slot);
        }
        let (paren_count, inner_slot) = slot.unwrap_parens(self.parsed, node);
        if paren_count > 0 {
            let (inner_doc, _) = self.format_expr_unwrapped(node, inner_slot);
            let open = "(".repeat(paren_count);
            let close = ")".repeat(paren_count);
            return (
                Doc::concat([Doc::text(open), inner_doc, Doc::text(close)]),
                false,
            );
        }
        self.format_expr_unwrapped(node, inner_slot)
    }

    /// Format an expression node.
    pub fn format_expr(&mut self, node: NodeIndex, slot: TokenSlot) -> Doc {
        self.format_expr_with_rbrace(node, slot).0
    }

    pub fn has_comments_in_range(&self, start_byte: usize, end_byte: usize) -> bool {
        if end_byte <= start_byte {
            return false;
        }
        let idx = self
            .parsed
            .comments
            .partition_point(|c| c.byte_start < start_byte);
        self.parsed
            .comments
            .get(idx)
            .is_some_and(|c| c.byte_start < end_byte)
    }

    /// Format a `{ ... }` (or converted `: ...`) `Block` node.
    pub fn format_block(
        &mut self,
        block_node: NodeIndex,
        block_slot: TokenSlot,
        single_line_budget: Option<usize>,
        container_kind: ContainerKind,
    ) -> Doc {
        if block_node.is_null() {
            return Doc::str("{}");
        }
        let elems = self.parsed.ast.multi_children(block_node);

        let has_lbrace = self
            .parsed
            .sig_tokens
            .get(block_slot.first)
            .is_some_and(|t| t.kind == TokenKind::LBrace);
        let has_colon = !has_lbrace
            && self
                .parsed
                .sig_tokens
                .get(block_slot.first)
                .is_some_and(|t| t.kind == TokenKind::Colon);

        let (inner_first_tok, open_end_byte) = if has_lbrace || has_colon {
            (
                block_slot.first + 1,
                self.parsed.sig_tokens[block_slot.first].to as usize,
            )
        } else {
            let prev_end = self
                .parsed
                .sig_tokens
                .get(block_slot.first.saturating_sub(1))
                .map_or(0, |t| t.to as usize);
            (block_slot.first, prev_end)
        };

        let has_rbrace = has_lbrace
            && self
                .parsed
                .sig_tokens
                .get(block_slot.last)
                .is_some_and(|t| t.kind == TokenKind::RBrace);

        let (inner_last_tok, close_start_byte) = if has_rbrace {
            (
                block_slot.last.saturating_sub(1).max(inner_first_tok),
                self.parsed.sig_tokens[block_slot.last].from as usize,
            )
        } else {
            (
                block_slot.last.max(inner_first_tok),
                self.parsed
                    .sig_tokens
                    .get(block_slot.last)
                    .map_or(self.parsed.original_src.len(), |t| t.to as usize),
            )
        };

        let has_comments_in_block = self
            .comments
            .peek()
            .is_some_and(|(_, c)| c.byte_start < close_start_byte);

        if elems.is_empty() && !has_comments_in_block {
            return Doc::str("{}");
        }

        let tab = self.tab_spaces();

        // Single-line block check (`if c { a } else { b }`, `let x? = opt else { return 0 }`):
        if let Some(budget) = single_line_budget
            && budget > 0
            && elems.len() == 1
            && !has_comments_in_block
            && can_single_line_block_elem(&self.parsed.ast, elems[0])
        {
            let saved_indent = self.indent_spaces;
            self.indent_spaces += tab;
            self.brace_depth += 1;

            let inner_bound = TokenSlot::new(inner_first_tok, inner_last_tok);
            let elem_slot = inner_bound.child_slot_from(self.parsed, inner_first_tok, elems[0]);
            let (elem_doc, _) =
                self.format_container_member(elems[0], elem_slot, true, container_kind);

            self.brace_depth -= 1;
            self.cfg_stack.pop_above(self.brace_depth);
            self.indent_spaces = saved_indent;

            let block_doc = Doc::concat([
                Doc::str("{"),
                Doc::indent(tab, Doc::concat([Doc::Line, elem_doc])),
                Doc::Line,
                Doc::str("}"),
            ]);
            return Doc::group_with_budget(budget, block_doc);
        }

        let saved_indent = self.indent_spaces;
        self.indent_spaces += tab;
        self.brace_depth += 1;

        let inner_doc = self.format_container_elements(
            elems,
            inner_first_tok,
            inner_last_tok,
            open_end_byte,
            close_start_byte,
            container_kind,
        );

        self.brace_depth -= 1;
        self.cfg_stack.pop_above(self.brace_depth);
        self.indent_spaces = saved_indent;

        if matches!(inner_doc, Doc::Nil) {
            Doc::str("{}")
        } else {
            Doc::concat([
                Doc::str("{"),
                Doc::indent(tab, Doc::concat([Doc::HardLine, inner_doc])),
                Doc::HardLine,
                Doc::str("}"),
            ])
        }
    }

    // -----------------------------------------------------------------------
    // Statements
    // -----------------------------------------------------------------------

    fn format_local_binding(
        &mut self,
        node: NodeIndex,
        slot: TokenSlot,
        kw: &'static str,
    ) -> (Doc, bool) {
        // `LetDecl` / `ConstDecl` / `VarDecl`: [0] pat, [1] ty, [2] val, [3] else_branch
        let ch = self.parsed.ast.fixed_children(node);
        let pat_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
        let pat_doc = self.format_pattern(ch[0], pat_slot);
        let mut cursor = pat_slot.last + 1;

        let ty_part = if ch[1].is_null() {
            Doc::Nil
        } else {
            let colon_tok = self
                .parsed
                .find_depth_zero_token(cursor, slot.last, |k| k == TokenKind::Colon, false)
                .unwrap_or(cursor);
            let ty_slot = slot.child_slot_from(self.parsed, colon_tok + 1, ch[1]);
            cursor = ty_slot.last + 1;
            let ty_doc = self.format_expr(ch[1], ty_slot);
            Doc::concat([Doc::str(": "), ty_doc])
        };

        let eq_tok = self
            .parsed
            .find_depth_zero_token(cursor, slot.last, |k| k == TokenKind::Eq, false)
            .unwrap_or(cursor);

        if ch[3].is_null() {
            let val_slot = slot.child_slot_from(self.parsed, eq_tok + 1, ch[2]);
            let (val_doc, ends_brace) = self.format_expr_with_rbrace(ch[2], val_slot);
            let tab = self.tab_spaces();
            // If `val_doc` starts with `{` / `[` / `(` or fits on the line, keep ` = <val>`;
            // otherwise break after `=`.
            let doc = Doc::group(Doc::concat([
                Doc::str(kw),
                pat_doc,
                ty_part,
                Doc::str(" ="),
                Doc::if_break(
                    Doc::indent(tab, Doc::concat([Doc::HardLine, val_doc.clone()])),
                    Doc::concat([Doc::space(), val_doc]),
                ),
            ]));
            (doc, ends_brace)
        } else {
            // `let pat = val else { ... }`
            let else_tok = self
                .parsed
                .find_depth_zero_token(eq_tok + 1, slot.last, |k| k == TokenKind::KwElse, false)
                .unwrap_or(slot.last);
            let val_bound = TokenSlot::new(eq_tok + 1, else_tok.saturating_sub(1).max(eq_tok + 1));
            let val_slot = val_bound.child_slot_from(self.parsed, eq_tok + 1, ch[2]);
            let val_doc = self.format_expr(ch[2], val_slot);

            let else_node = ch[3];
            let else_slot = TokenSlot::new(else_tok + 1, slot.last.max(else_tok + 1));
            let let_else_budget = self.cfg().width_heuristics().single_line_let_else_max_width;
            let (else_doc, ends_brace) = if self.parsed.ast.node(else_node).kind == NodeKind::Block
            {
                (
                    self.format_block(
                        else_node,
                        else_slot,
                        Some(let_else_budget),
                        ContainerKind::ValueBlock,
                    ),
                    true,
                )
            } else {
                let s_slot = else_slot.child_slot_from(self.parsed, else_tok + 1, else_node);
                self.format_statement_or_expr(else_node, s_slot)
            };

            let doc = Doc::group(Doc::concat([
                Doc::str(kw),
                pat_doc,
                ty_part,
                Doc::str(" = "),
                val_doc,
                Doc::str(" else "),
                else_doc,
            ]));
            (doc, ends_brace)
        }
    }

    fn format_assign_stmt(
        &mut self,
        node: NodeIndex,
        slot: TokenSlot,
        op_str: &'static str,
    ) -> (Doc, bool) {
        let ch = self.parsed.ast.fixed_children(node);
        let lhs_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
        let op_tok = lhs_slot.last + 1;
        let rhs_slot = slot.child_slot_from(self.parsed, op_tok + 1, ch[1]);
        let lhs_doc = self.format_expr(ch[0], lhs_slot);
        let (rhs_doc, ends_brace) = self.format_expr_with_rbrace(ch[1], rhs_slot);
        let tab = self.tab_spaces();
        let doc = Doc::group(Doc::concat([
            lhs_doc,
            Doc::space(),
            Doc::str(op_str),
            Doc::if_break(
                Doc::indent(tab, Doc::concat([Doc::HardLine, rhs_doc.clone()])),
                Doc::concat([Doc::space(), rhs_doc]),
            ),
        ]));
        (doc, ends_brace)
    }

    fn format_defer_stmt(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        let body = self.parsed.ast.fixed_children(node)[0];
        if body.is_null() {
            return (Doc::str("defer"), false);
        }
        if self.parsed.ast.node(body).kind == NodeKind::Block {
            let block_slot = TokenSlot::new(slot.first + 1, slot.last.max(slot.first + 1));
            let b_doc = self.format_block(body, block_slot, None, ContainerKind::ValueBlock);
            (Doc::concat([Doc::str("defer "), b_doc]), true)
        } else {
            let body_slot = slot.child_slot_from(self.parsed, slot.first + 1, body);
            let (b_doc, ends_brace) = self.format_statement_or_expr(body, body_slot);
            (Doc::concat([Doc::str("defer "), b_doc]), ends_brace)
        }
    }

    fn format_jump_with_value(
        &mut self,
        node: NodeIndex,
        slot: TokenSlot,
        kw: &'static str,
    ) -> (Doc, bool) {
        // `ReturnStatement` / `ResumeStatement`: [0] val, [1] guard
        let ch = self.parsed.ast.fixed_children(node);
        let mut parts = vec![Doc::str(kw)];
        let mut cursor = slot.first + 1;
        let mut ends_brace = false;

        if !ch[0].is_null() {
            let val_end_bound = if ch[1].is_null() {
                slot.last
            } else {
                self.parsed
                    .find_depth_zero_token(cursor, slot.last, |k| k == TokenKind::KwIf, true)
                    .map_or(slot.last, |t| t.saturating_sub(1).max(cursor))
            };
            let val_slot =
                TokenSlot::new(cursor, val_end_bound).child_slot_from(self.parsed, cursor, ch[0]);
            let (val_doc, eb) = self.format_expr_with_rbrace(ch[0], val_slot);
            parts.push(Doc::space());
            parts.push(val_doc);
            cursor = val_slot.last + 1;
            ends_brace = eb && ch[1].is_null();
        }

        if !ch[1].is_null() {
            let if_tok = self
                .parsed
                .find_depth_zero_token(cursor, slot.last, |k| k == TokenKind::KwIf, false)
                .unwrap_or(cursor);
            let guard_slot = slot.child_slot_from(self.parsed, if_tok + 1, ch[1]);
            let (guard_doc, eb) = self.format_expr_with_rbrace(ch[1], guard_slot);
            parts.push(Doc::str(" if "));
            parts.push(guard_doc);
            ends_brace = eb;
        }

        (Doc::concat(parts), ends_brace)
    }

    fn format_loop_jump(
        &mut self,
        node: NodeIndex,
        slot: TokenSlot,
        kw: &'static str,
    ) -> (Doc, bool) {
        // `BreakStatement` / `ContinueStatement`: [0] label, [1] guard
        let ch = self.parsed.ast.fixed_children(node);
        let mut parts = vec![Doc::str(kw)];
        let mut cursor = slot.first + 1;
        let mut ends_brace = false;

        if !ch[0].is_null() {
            let (lf, ll) = self.parsed.exact_token_bounds(ch[0], cursor);
            let lbl_str = self.format_ident(ch[0], lf);
            parts.push(Doc::space());
            parts.push(Doc::text(lbl_str));
            cursor = ll + 1;
        }

        if !ch[1].is_null() {
            let if_tok = self
                .parsed
                .find_depth_zero_token(cursor, slot.last, |k| k == TokenKind::KwIf, false)
                .unwrap_or(cursor);
            let guard_slot = slot.child_slot_from(self.parsed, if_tok + 1, ch[1]);
            let (guard_doc, eb) = self.format_expr_with_rbrace(ch[1], guard_slot);
            parts.push(Doc::str(" if "));
            parts.push(guard_doc);
            ends_brace = eb;
        }

        (Doc::concat(parts), ends_brace)
    }

    fn format_for_loop(&mut self, node: NodeIndex, slot: TokenSlot) -> Doc {
        // `ForLoop`: [0] label, [1] pattern, [2] iter_expr, [3] body
        let ch = self.parsed.ast.fixed_children(node);
        let mut cursor = slot.first + 1;
        let label_part = if !ch[0].is_null() {
            // `:label ` (`cursor` is `TokenKind::Colon`)
            let colon_tok = self
                .parsed
                .find_depth_zero_token(cursor, slot.last, |k| k == TokenKind::Colon, false)
                .unwrap_or(cursor);
            let (lf, ll) = self.parsed.exact_token_bounds(ch[0], colon_tok + 1);
            let lbl_str = self.format_ident(ch[0], lf);
            cursor = ll + 1;
            Doc::concat([Doc::str(":"), Doc::text(lbl_str), Doc::space()])
        } else {
            Doc::Nil
        };

        let pat_slot = slot.child_slot_from(self.parsed, cursor, ch[1]);
        let pat_doc = self.format_pattern(ch[1], pat_slot);
        let in_tok = self
            .parsed
            .find_depth_zero_token(
                pat_slot.last + 1,
                slot.last,
                |k| k == TokenKind::KwIn,
                false,
            )
            .unwrap_or(pat_slot.last + 1);
        let iter_slot = slot.child_slot_from(self.parsed, in_tok + 1, ch[2]);
        let iter_doc = self.format_expr(ch[2], iter_slot);

        let body = ch[3];
        let body_slot = TokenSlot::new(iter_slot.last + 1, slot.last.max(iter_slot.last + 1));
        let body_doc = self.format_control_body_as_block_or_stmt(body, body_slot);

        Doc::concat([
            Doc::str("for "),
            label_part,
            pat_doc,
            Doc::str(" in "),
            iter_doc,
            Doc::space(),
            body_doc,
        ])
    }

    fn format_while_loop(&mut self, node: NodeIndex, slot: TokenSlot) -> Doc {
        // `WhileLoop`: [0] label, [1] condition, [2] body
        let ch = self.parsed.ast.fixed_children(node);
        let mut cursor = slot.first + 1;
        let label_part = if !ch[0].is_null() {
            let colon_tok = self
                .parsed
                .find_depth_zero_token(cursor, slot.last, |k| k == TokenKind::Colon, false)
                .unwrap_or(cursor);
            let (lf, ll) = self.parsed.exact_token_bounds(ch[0], colon_tok + 1);
            let lbl_str = self.format_ident(ch[0], lf);
            cursor = ll + 1;
            Doc::concat([Doc::str(":"), Doc::text(lbl_str), Doc::space()])
        } else {
            Doc::Nil
        };

        let cond_slot = slot.child_slot_from(self.parsed, cursor, ch[1]);
        let cond_doc = self.format_expr(ch[1], cond_slot);
        let body_slot = TokenSlot::new(cond_slot.last + 1, slot.last.max(cond_slot.last + 1));
        let body_doc = self.format_control_body_as_block_or_stmt(ch[2], body_slot);

        Doc::concat([
            Doc::str("while "),
            label_part,
            cond_doc,
            Doc::space(),
            body_doc,
        ])
    }

    fn format_control_body_as_block_or_stmt(&mut self, body: NodeIndex, slot: TokenSlot) -> Doc {
        if body.is_null() {
            return Doc::str("{}");
        }
        if self.parsed.ast.node(body).kind == NodeKind::Block {
            self.format_block(body, slot, None, ContainerKind::ValueBlock)
        } else {
            let s_slot = slot.child_slot_from(self.parsed, slot.first, body);
            self.format_statement_or_expr(body, s_slot).0
        }
    }

    // -----------------------------------------------------------------------
    // Expressions
    // -----------------------------------------------------------------------

    fn format_expr_unwrapped(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        if let Some(bridged) = self.bridged_at(node, slot.first) {
            let ends_brace = self
                .parsed
                .original_src
                .get(bridged.span_bytes().0..bridged.span_bytes().1)
                .is_some_and(|s| s.trim_end().ends_with('}'));
            return (self.format_bridged(bridged), ends_brace);
        }

        let kind = self.parsed.ast.node(node).kind;
        let ch = self.parsed.ast.fixed_children(node);

        match kind {
            // Atomic leaves
            NodeKind::Id => (Doc::text(self.format_ident(node, slot.first)), false),
            NodeKind::Null => (Doc::str("null"), false),
            NodeKind::SelfLower => (Doc::str("self"), false),
            NodeKind::SelfUpper => (Doc::str("Self"), false),
            NodeKind::Underscore => (Doc::str("_"), false),
            NodeKind::Unit => (Doc::str("()"), false),
            NodeKind::RangeFull => (Doc::str(".."), false),
            NodeKind::Int | NodeKind::Real | NodeKind::Str | NodeKind::Char | NodeKind::Bool => {
                (Doc::text(self.literal_slice(node, slot.first)), false)
            }
            NodeKind::FStringConcat => (self.format_fstring(node, slot), false),
            NodeKind::Symbol => {
                let id_str = self.format_ident(ch[0], slot.first + 1);
                (Doc::concat([Doc::str("."), Doc::text(id_str)]), false)
            }
            NodeKind::SpreadArg => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first + 3, ch[0]);
                let (inner_doc, eb) = self.format_expr_with_rbrace(ch[0], inner_slot);
                (Doc::concat([Doc::str("..."), inner_doc]), eb)
            }
            NodeKind::NamedArg => {
                let starts_dot = self
                    .parsed
                    .sig_tokens
                    .get(slot.first)
                    .is_some_and(|t| t.kind == TokenKind::Dot);
                let id_floor = if starts_dot {
                    slot.first + 1
                } else {
                    slot.first
                };
                let id_str = self.format_ident(ch[0], id_floor);
                let eq_tok = self
                    .parsed
                    .find_depth_zero_token(id_floor + 1, slot.last, |k| k == TokenKind::Eq, false)
                    .unwrap_or(id_floor + 1);
                let val_slot = slot.child_slot_from(self.parsed, eq_tok + 1, ch[1]);
                let (val_doc, eb) = self.format_expr_with_rbrace(ch[1], val_slot);
                (
                    Doc::concat([Doc::text(id_str), Doc::str(" = "), val_doc]),
                    eb,
                )
            }
            NodeKind::PropertyAssignment => {
                let id_str = self.format_ident(ch[0], slot.first + 1);
                let eq_tok = self
                    .parsed
                    .find_depth_zero_token(slot.first + 2, slot.last, |k| k == TokenKind::Eq, false)
                    .unwrap_or(slot.first + 2);
                let val_slot = slot.child_slot_from(self.parsed, eq_tok + 1, ch[1]);
                let (val_doc, eb) = self.format_expr_with_rbrace(ch[1], val_slot);
                (
                    Doc::concat([Doc::str("."), Doc::text(id_str), Doc::str(" = "), val_doc]),
                    eb,
                )
            }
            NodeKind::Property => (self.format_property_expr(node, slot), false),

            // Collections
            NodeKind::Tuple => {
                let elems = self.parsed.ast.multi_children(node);
                let style = if elems.len() == 1 {
                    DelimStyle::SingleTupleParens
                } else {
                    DelimStyle::Parens
                };
                let budget = Some(self.cfg().width_heuristics().fn_call_width);
                let doc = self.format_delimited_seq(
                    elems,
                    slot.first,
                    slot.last,
                    style,
                    budget,
                    |fmt, el, el_slot| fmt.format_expr(el, el_slot),
                );
                (doc, false)
            }
            NodeKind::ListOf => {
                let elems = self.parsed.ast.multi_children(node);
                let budget = Some(self.cfg().width_heuristics().array_width);
                let doc = self.format_delimited_seq(
                    elems,
                    slot.first,
                    slot.last,
                    DelimStyle::Brackets,
                    budget,
                    |fmt, el, el_slot| fmt.format_expr(el, el_slot),
                );
                (doc, false)
            }
            NodeKind::Object => {
                let props = self.parsed.ast.multi_children(node);
                let budget = Some(self.cfg().width_heuristics().struct_lit_width);
                let doc = self.format_delimited_seq(
                    props,
                    slot.first,
                    slot.last,
                    DelimStyle::SpacedBraces,
                    budget,
                    |fmt, prop, prop_slot| fmt.format_expr(prop, prop_slot),
                );
                (doc, true)
            }

            // Lambda & CaseMap
            NodeKind::Lambda => self.format_lambda(node, slot),
            NodeKind::CaseMap => self.format_case_map(node, slot),
            NodeKind::CaseAlternative => {
                let lhs_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let pipe_tok = self
                    .parsed
                    .find_depth_zero_token(
                        lhs_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::Pipe,
                        false,
                    )
                    .unwrap_or(lhs_slot.last + 1);
                let rhs_slot = slot.child_slot_from(self.parsed, pipe_tok + 1, ch[1]);
                let lhs_doc = self.format_expr(ch[0], lhs_slot);
                let (rhs_doc, eb) = self.format_expr_with_rbrace(ch[1], rhs_slot);
                let tab = self.tab_spaces();
                // Note: `|` has `can_start_statement() == false`, so breaking before `|` is safe!
                let doc = Doc::group(Doc::concat([
                    lhs_doc,
                    Doc::indent(tab, Doc::concat([Doc::Line, Doc::str("| "), rhs_doc])),
                ]));
                (doc, eb)
            }

            // Prefix unary & qualified types
            NodeKind::Negative => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
                // Prevent `- -x` from turning into `--x` line comment!
                let sep = if self.slot_starts_with_minus(inner_slot) {
                    Doc::space()
                } else {
                    Doc::Nil
                };
                let (inner_doc, eb) = self.format_expr_with_rbrace(ch[0], inner_slot);
                (Doc::concat([Doc::str("-"), sep, inner_doc]), eb)
            }
            NodeKind::BoolNot => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
                let (inner_doc, eb) = self.format_expr_with_rbrace(ch[0], inner_slot);
                (Doc::concat([Doc::str("not "), inner_doc]), eb)
            }
            NodeKind::ErrorConstruction => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
                let (inner_doc, eb) = self.format_expr_with_rbrace(ch[0], inner_slot);
                (Doc::concat([Doc::str("error "), inner_doc]), eb)
            }
            NodeKind::OptionalType => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
                let (inner_doc, eb) = self.format_expr_with_rbrace(ch[0], inner_slot);
                (Doc::concat([Doc::str("?"), inner_doc]), eb)
            }
            NodeKind::EffectQualifiedType => {
                let eff_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
                let ty_slot = slot.child_slot_from(self.parsed, eff_slot.last + 1, ch[1]);
                let eff_doc = self.format_expr(ch[0], eff_slot);
                let (ty_doc, eb) = self.format_expr_with_rbrace(ch[1], ty_slot);
                (
                    Doc::concat([Doc::str("#"), eff_doc, Doc::space(), ty_doc]),
                    eb,
                )
            }
            NodeKind::ErrorQualifiedType => {
                let err_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
                let ty_slot = slot.child_slot_from(self.parsed, err_slot.last + 1, ch[1]);
                let err_doc = self.format_expr(ch[0], err_slot);
                let (ty_doc, eb) = self.format_expr_with_rbrace(ch[1], ty_slot);
                (
                    Doc::concat([Doc::str("!"), err_doc, Doc::space(), ty_doc]),
                    eb,
                )
            }
            NodeKind::FnType => {
                let params = self.parsed.ast.multi_children(node);
                let open_p = self
                    .parsed
                    .find_depth_zero_token(
                        slot.first + 1,
                        slot.last,
                        |k| k == TokenKind::LParen,
                        false,
                    )
                    .unwrap_or(slot.first + 1);
                let p_doc = self.format_delimited_seq(
                    params,
                    open_p,
                    slot.last,
                    DelimStyle::Parens,
                    None,
                    |fmt, p, ps| fmt.format_expr(p, ps),
                );
                (Doc::concat([Doc::str("fn"), p_doc]), false)
            }
            NodeKind::EffectType | NodeKind::AsyncEffectType => {
                let is_async = kind == NodeKind::AsyncEffectType;
                let kw = if is_async { "async effect" } else { "effect" };
                let kw_toks = if is_async { 2 } else { 1 };
                let params = self.parsed.ast.multi_children(node);
                let open_p = self
                    .parsed
                    .find_depth_zero_token(
                        slot.first + kw_toks,
                        slot.last,
                        |k| k == TokenKind::LParen,
                        false,
                    )
                    .unwrap_or(slot.first + kw_toks);
                let p_doc = self.format_delimited_seq(
                    params,
                    open_p,
                    slot.last,
                    DelimStyle::Parens,
                    None,
                    |fmt, p, ps| fmt.format_expr(p, ps),
                );
                (Doc::concat([Doc::str(kw), p_doc]), false)
            }
            NodeKind::ResetExpr => {
                let prompt_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
                let prompt_doc = self.format_expr(ch[0], prompt_slot);
                let body_slot =
                    TokenSlot::new(prompt_slot.last + 1, slot.last.max(prompt_slot.last + 1));
                let is_block =
                    !ch[1].is_null() && self.parsed.ast.node(ch[1]).kind == NodeKind::Block;
                let body_doc = self.format_control_body_as_block_or_stmt(ch[1], body_slot);
                (
                    Doc::concat([Doc::str("reset "), prompt_doc, Doc::space(), body_doc]),
                    is_block,
                )
            }
            NodeKind::ShiftExpr => {
                let prompt_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
                let prompt_doc = self.format_expr(ch[0], prompt_slot);
                let comma_tok = self
                    .parsed
                    .find_depth_zero_token(
                        prompt_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::Comma,
                        false,
                    )
                    .unwrap_or(prompt_slot.last + 1);
                let (kf, kl) = self.parsed.exact_token_bounds(ch[1], comma_tok + 1);
                let k_str = self.format_ident(ch[1], kf);
                let body_slot = TokenSlot::new(kl + 1, slot.last.max(kl + 1));
                let is_block =
                    !ch[2].is_null() && self.parsed.ast.node(ch[2]).kind == NodeKind::Block;
                let body_doc = self.format_control_body_as_block_or_stmt(ch[2], body_slot);
                (
                    Doc::concat([
                        Doc::str("shift "),
                        prompt_doc,
                        Doc::str(", "),
                        Doc::text(k_str),
                        Doc::space(),
                        body_doc,
                    ]),
                    is_block,
                )
            }

            // Control-flow expressions (`if`, `when`, `Block`)
            NodeKind::IfStatement => self.format_if_expr(node, slot),
            NodeKind::WhenStatement => (self.format_when_expr(node, slot), true),
            NodeKind::Block => (
                self.format_block(node, slot, None, ContainerKind::ValueBlock),
                true,
            ),

            // Ranges
            NodeKind::RangeFrom => {
                let lo_slot =
                    TokenSlot::new(slot.first, slot.last.saturating_sub(2).max(slot.first))
                        .child_slot_from(self.parsed, slot.first, ch[0]);
                let lo_doc = self.format_expr(ch[0], lo_slot);
                (Doc::concat([lo_doc, Doc::str("..")]), false)
            }
            NodeKind::RangeTo => {
                let hi_slot = slot.child_slot_from(self.parsed, slot.first + 2, ch[0]);
                let hi_doc = self.format_expr(ch[0], hi_slot);
                (Doc::concat([Doc::str(".."), hi_doc]), false)
            }
            NodeKind::RangeToInclusive => {
                let hi_slot = slot.child_slot_from(self.parsed, slot.first + 3, ch[0]);
                let hi_doc = self.format_expr(ch[0], hi_slot);
                (Doc::concat([Doc::str("..="), hi_doc]), false)
            }
            NodeKind::RangeFromTo | NodeKind::RangeFromToInclusive => {
                let (op_str, op_toks) = if kind == NodeKind::RangeFromToInclusive {
                    ("..=", 3usize)
                } else {
                    ("..", 2usize)
                };
                let lo_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let hi_slot = slot.child_slot_from(self.parsed, lo_slot.last + 1 + op_toks, ch[1]);
                let lo_doc = self.format_expr(ch[0], lo_slot);
                let hi_doc = self.format_expr(ch[1], hi_slot);
                (Doc::concat([lo_doc, Doc::str(op_str), hi_doc]), false)
            }

            // Postfix expressions & chains
            NodeKind::Projection
            | NodeKind::View
            | NodeKind::Await
            | NodeKind::TypeCast
            | NodeKind::HandlerApplication
            | NodeKind::Call
            | NodeKind::ExtendedCall
            | NodeKind::OptionPropagation
            | NodeKind::ErrorPropagation
            | NodeKind::EffectPropagation
            | NodeKind::ErrorElimination
            | NodeKind::EffectElimination
            | NodeKind::PostMatch
            | NodeKind::PostDo => self.format_postfix(node, slot),

            // Binary expressions
            NodeKind::Add => self.format_binary_op(node, slot, "+", TokenKind::Plus),
            NodeKind::Sub => self.format_binary_op(node, slot, "-", TokenKind::Minus),
            NodeKind::Mul => self.format_binary_op(node, slot, "*", TokenKind::Star),
            NodeKind::Div => self.format_binary_op(node, slot, "/", TokenKind::Slash),
            NodeKind::Mod => self.format_binary_op(node, slot, "%", TokenKind::Percent),
            NodeKind::Concat => self.format_binary_op(node, slot, "++", TokenKind::PlusPlus),
            NodeKind::BoolEq => self.format_binary_op(node, slot, "==", TokenKind::EqEq),
            NodeKind::BoolNotEq => self.format_binary_op(node, slot, "!=", TokenKind::BangEq),
            NodeKind::BoolGt => self.format_binary_op(node, slot, ">", TokenKind::Gt),
            NodeKind::BoolGtEq => self.format_binary_op(node, slot, ">=", TokenKind::GtEq),
            NodeKind::BoolLt => self.format_binary_op(node, slot, "<", TokenKind::Lt),
            NodeKind::BoolLtEq => self.format_binary_op(node, slot, "<=", TokenKind::LtEq),
            NodeKind::BoolAnd => self.format_binary_op(node, slot, "and", TokenKind::KwAnd),
            NodeKind::BoolOr => self.format_binary_op(node, slot, "or", TokenKind::KwOr),
            NodeKind::Arrow => self.format_binary_op(node, slot, "->", TokenKind::Arrow),
            NodeKind::Pipeline => self.format_pipeline(node, slot),
            NodeKind::InfixFnCall => self.format_infix_fn_call(node, slot),
            NodeKind::BoolMatches => self.format_bool_matches(node, slot),

            // Arms when visited directly
            NodeKind::CaseArm => self.format_case_arm(node, slot),
            NodeKind::CatchArm => self.format_catch_arm(node, slot),
            NodeKind::ConditionArm => self.format_condition_arm(node, slot),
            NodeKind::ElseConditionArm => self.format_else_condition_arm(node, slot),

            _ => (Doc::text(self.literal_slice(node, slot.first)), false),
        }
    }

    pub fn format_property_expr(&mut self, prop: NodeIndex, slot: TokenSlot) -> Doc {
        let ch = self.parsed.ast.fixed_children(prop);
        let id_str = self.format_ident(ch[0], slot.first);
        let colon_tok = self
            .parsed
            .find_depth_zero_token(slot.first + 1, slot.last, |k| k == TokenKind::Colon, false)
            .unwrap_or(slot.first + 1);
        let val_slot = slot.child_slot_from(self.parsed, colon_tok + 1, ch[1]);
        let val_doc = self.format_expr(ch[1], val_slot);
        Doc::concat([Doc::text(id_str), Doc::str(": "), val_doc])
    }

    fn format_fstring(&mut self, node: NodeIndex, slot: TokenSlot) -> Doc {
        // Walk `sig_tokens` from `FStringStart` (`slot.first`) to `FStringEnd` (`slot.last`),
        // preserving `FStringLiteral` slices (including `{{` / `}}` and escapes) verbatim and
        // formatting interpolated `{expr}` single-line!
        let parts = self.parsed.ast.multi_children(node);
        let mut part_idx = 0usize;
        let mut tok_idx = slot.first;
        let mut out = String::from("\"");

        if self
            .parsed
            .sig_tokens
            .get(tok_idx)
            .is_some_and(|t| t.kind == TokenKind::FStringStart)
        {
            tok_idx += 1;
        }

        while tok_idx <= slot.last && tok_idx < self.parsed.sig_tokens.len() {
            let tok = self.parsed.sig_tokens[tok_idx];
            match tok.kind {
                TokenKind::FStringEnd => {
                    out.push('"');
                    break;
                }
                TokenKind::FStringLiteral => {
                    let seg = &self.parsed.original_src[tok.from as usize..tok.to as usize];
                    out.push_str(seg);
                    if parts.get(part_idx).is_some_and(|&p| {
                        !p.is_null() && self.parsed.ast.node(p).kind == NodeKind::Str
                    }) {
                        part_idx += 1;
                    }
                    tok_idx += 1;
                }
                TokenKind::FStringExprStart => {
                    let close_tok = find_matching_delim(
                        &self.parsed.sig_tokens,
                        tok_idx,
                        TokenKind::FStringExprStart,
                        TokenKind::FStringExprEnd,
                    )
                    .unwrap_or(slot.last);
                    let raw_inner = &self.parsed.original_src
                        [tok.to as usize..self.parsed.sig_tokens[close_tok].from as usize];
                    let expr_node = parts.get(part_idx).copied().unwrap_or(NodeIndex::NULL);
                    part_idx += 1;

                    let has_inner_comments = self.has_comments_in_range(
                        tok.to as usize,
                        self.parsed.sig_tokens[close_tok].from as usize,
                    );
                    if !expr_node.is_null() && !has_inner_comments {
                        let expr_bound = TokenSlot::new(
                            tok_idx + 1,
                            close_tok.saturating_sub(1).max(tok_idx + 1),
                        );
                        let expr_slot =
                            expr_bound.child_slot_from(self.parsed, tok_idx + 1, expr_node);
                        let sub_cfg = FormatConfig {
                            max_width: 10_000,
                            ..Default::default()
                        };
                        let mut sub_fmt = Formatter::new(self.parsed, sub_cfg);
                        let expr_doc = sub_fmt.format_expr(expr_node, expr_slot);
                        let rendered = print_doc(&expr_doc, 10_000, "\n");
                        let trimmed = rendered.trim_end_matches('\n');
                        if !trimmed.is_empty() && !trimmed.contains('\n') {
                            out.push('{');
                            out.push_str(trimmed);
                            out.push('}');
                        } else {
                            out.push('{');
                            out.push_str(raw_inner.trim());
                            out.push('}');
                        }
                    } else {
                        self.comments.advance_past_verbatim(
                            self.parsed.sig_tokens[close_tok].to as usize,
                            self.brace_depth,
                            &mut self.cfg_stack,
                        );
                        out.push('{');
                        out.push_str(raw_inner.trim());
                        out.push('}');
                    }
                    tok_idx = close_tok + 1;
                }
                _ => {
                    tok_idx += 1;
                }
            }
        }

        if !out.ends_with('"') {
            out.push('"');
        }
        Doc::text(out)
    }

    fn format_lambda(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        // Check tacit lambda (`_0 + 1` inside `Call` or `SpreadArg`)!
        if self.parsed.is_tacit_lambda(node) {
            let body = self.parsed.ast.fixed_children(node)[0];
            let body_slot = slot.child_slot_from(self.parsed, slot.first, body);
            return self.format_expr_with_rbrace(body, body_slot);
        }

        // `Lambda`: [0] body, [1] return_type, multi = params
        let ch = self.parsed.ast.fixed_children(node);
        let params = self.parsed.ast.multi_children(node);
        let open_pipe = slot.first;
        let close_pipe = find_matching_delim(
            &self.parsed.sig_tokens,
            open_pipe,
            TokenKind::Pipe,
            TokenKind::Pipe,
        )
        .unwrap_or(open_pipe + 1);

        let params_doc = self.format_delimited_seq(
            params,
            open_pipe,
            close_pipe,
            DelimStyle::Pipes,
            None,
            |fmt, p, ps| fmt.format_parameter(p, ps),
        );

        let (ret_doc, cursor) =
            self.format_ret_and_capability(ch[1], NodeIndex::NULL, close_pipe + 1, slot);

        let body = ch[0];
        if !body.is_null() && self.parsed.ast.node(body).kind == NodeKind::Block {
            let block_slot = TokenSlot::new(cursor, slot.last.max(cursor));
            let budget = if ch[1].is_null() {
                Some(self.cfg().width_heuristics().fn_call_width)
            } else {
                None
            };
            let b_doc = self.format_block(body, block_slot, budget, ContainerKind::ValueBlock);
            (
                Doc::concat([params_doc, ret_doc, Doc::space(), b_doc]),
                true,
            )
        } else {
            let body_slot = slot.child_slot_from(self.parsed, cursor, body);
            let (b_doc, eb) = self.format_statement_or_expr(body, body_slot);
            let tab = self.tab_spaces();
            // Do NOT break right after `|params|` if `b_doc` starts with `+`/`-`, wait: `|` has
            // `can_end_statement() == false` (`Pipe`), so breaking after `|params|` IS 100% safe!
            let doc = if ch[1].is_null() {
                Doc::group(Doc::concat([
                    params_doc,
                    Doc::indent(tab, Doc::concat([Doc::Line, b_doc])),
                ]))
            } else {
                Doc::concat([params_doc, ret_doc, Doc::space(), b_doc])
            };
            (doc, eb)
        }
    }

    fn format_case_map(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        let ch = self.parsed.ast.fixed_children(node);
        let pat_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
        let pat_doc = self.format_pattern(ch[0], pat_slot);
        let arrow_tok = self
            .parsed
            .find_depth_zero_token(
                pat_slot.last + 1,
                slot.last,
                |k| k == TokenKind::FatArrow,
                false,
            )
            .unwrap_or(pat_slot.last + 1);
        let (rhs_doc, eb) = self.format_arm_rhs(ch[1], arrow_tok + 1, slot);
        (
            Doc::concat([Doc::str("case "), pat_doc, Doc::str(" =>"), rhs_doc]),
            eb,
        )
    }

    fn format_if_expr(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        // `IfStatement`: [0] cond, [1] then_block, [2] else_branch
        let ch = self.parsed.ast.fixed_children(node);
        let cond_start =
            if self.parsed.sig_tokens.get(slot.first).map(|t| t.kind) == Some(TokenKind::KwIf) {
                slot.first + 1
            } else {
                slot.first
            };
        let cond_slot = slot.child_slot_from(self.parsed, cond_start, ch[0]);
        let cond_doc = self.format_expr(ch[0], cond_slot);

        if ch[2].is_null() {
            let then_slot = TokenSlot::new(cond_slot.last + 1, slot.last.max(cond_slot.last + 1));
            let is_block = !ch[1].is_null() && self.parsed.ast.node(ch[1]).kind == NodeKind::Block;
            let then_doc = self.format_control_body_as_block_or_stmt(ch[1], then_slot);
            return (
                Doc::concat([Doc::str("if "), cond_doc, Doc::space(), then_doc]),
                is_block,
            );
        }

        // Locate `else` token separating `then_block` and `else_branch`:
        // Note that `then_block` starts at `cond_slot.last + 1`. Find `then_block`'s own slot first!
        let (_, then_raw_last) = self.parsed.exact_token_bounds(ch[1], cond_slot.last + 1);
        let else_tok = self
            .parsed
            .find_depth_zero_token(
                then_raw_last + 1,
                slot.last,
                |k| k == TokenKind::KwElse,
                false,
            )
            .unwrap_or(then_raw_last + 1);

        let then_slot = TokenSlot::new(
            cond_slot.last + 1,
            else_tok.saturating_sub(1).max(cond_slot.last + 1),
        );
        let else_slot = TokenSlot::new(else_tok + 1, slot.last.max(else_tok + 1));

        let then_end_byte = self
            .parsed
            .sig_tokens
            .get(then_slot.last)
            .map_or(0, |t| t.to as usize);
        let else_start_byte = self
            .parsed
            .sig_tokens
            .get(else_tok)
            .map_or(then_end_byte, |t| t.from as usize);
        let has_mid_comments = self.has_comments_in_range(then_end_byte, else_start_byte);

        // Single-line `if cond { a } else { b }` eligibility:
        let if_else_budget = self.cfg().width_heuristics().single_line_if_else_max_width;
        let both_simple_blocks = if_else_budget > 0
            && !has_mid_comments
            && !ch[1].is_null()
            && self.parsed.ast.node(ch[1]).kind == NodeKind::Block
            && self.parsed.ast.node(ch[2]).kind == NodeKind::Block
            && self.is_single_expr_block_without_comments(ch[1], then_slot)
            && self.is_single_expr_block_without_comments(ch[2], else_slot);

        if both_simple_blocks {
            let then_doc = self.format_Single_or_multi_if_branch(ch[1], then_slot);
            let else_doc = self.format_Single_or_multi_if_branch(ch[2], else_slot);
            let doc = Doc::group_with_budget(
                if_else_budget,
                Doc::concat([
                    Doc::str("if "),
                    cond_doc,
                    Doc::space(),
                    then_doc,
                    Doc::str(" else "),
                    else_doc,
                ]),
            );
            return (doc, true);
        }

        let then_doc = self.format_control_body_as_block_or_stmt(ch[1], then_slot);
        let mid_comments = if has_mid_comments {
            self.comments.take_inline_between(
                then_end_byte,
                else_start_byte,
                self.indent_spaces,
                &self.cfg(),
            )
        } else {
            Doc::Nil
        };

        let else_node = ch[2];
        let (else_doc, ends_brace) =
            if self.parsed.ast.node(else_node).kind == NodeKind::IfStatement {
                self.format_if_expr(else_node, else_slot)
            } else if self.parsed.ast.node(else_node).kind == NodeKind::Block {
                (
                    self.format_block(else_node, else_slot, None, ContainerKind::ValueBlock),
                    true,
                )
            } else {
                let s_slot = else_slot.child_slot_from(self.parsed, else_slot.first, else_node);
                self.format_statement_or_expr(else_node, s_slot)
            };

        let between = if matches!(mid_comments, Doc::Nil) {
            Doc::str(" else ")
        } else {
            Doc::concat([mid_comments, Doc::str("else ")])
        };

        (
            Doc::concat([
                Doc::str("if "),
                cond_doc,
                Doc::space(),
                then_doc,
                between,
                else_doc,
            ]),
            ends_brace,
        )
    }

    #[allow(non_snake_case)]
    fn format_Single_or_multi_if_branch(&mut self, block_node: NodeIndex, slot: TokenSlot) -> Doc {
        let elems = self.parsed.ast.multi_children(block_node);
        if elems.is_empty() {
            return Doc::str("{}");
        }
        let tab = self.tab_spaces();
        let saved_indent = self.indent_spaces;
        self.indent_spaces += tab;
        self.brace_depth += 1;
        let inner_first = slot.first + 1;
        let inner_last = slot.last.saturating_sub(1).max(inner_first);
        let elem_slot = TokenSlot::new(inner_first, inner_last).child_slot_from(
            self.parsed,
            inner_first,
            elems[0],
        );
        let (elem_doc, _) =
            self.format_container_member(elems[0], elem_slot, true, ContainerKind::ValueBlock);
        self.brace_depth -= 1;
        self.cfg_stack.pop_above(self.brace_depth);
        self.indent_spaces = saved_indent;
        Doc::concat([
            Doc::str("{"),
            Doc::indent(tab, Doc::concat([Doc::Line, elem_doc])),
            Doc::Line,
            Doc::str("}"),
        ])
    }

    fn is_single_expr_block_without_comments(
        &self,
        block_node: NodeIndex,
        slot: TokenSlot,
    ) -> bool {
        let elems = self.parsed.ast.multi_children(block_node);
        if elems.len() != 1 {
            return false;
        }
        let has_lbrace = self
            .parsed
            .sig_tokens
            .get(slot.first)
            .is_some_and(|t| t.kind == TokenKind::LBrace);
        let has_rbrace = self
            .parsed
            .sig_tokens
            .get(slot.last)
            .is_some_and(|t| t.kind == TokenKind::RBrace);
        if !has_lbrace || !has_rbrace {
            return false;
        }
        let open_end = self.parsed.sig_tokens[slot.first].to as usize;
        let close_start = self.parsed.sig_tokens[slot.last].from as usize;
        if self.has_comments_in_range(open_end, close_start) {
            return false;
        }
        can_single_line_block_elem(&self.parsed.ast, elems[0])
    }

    fn format_when_expr(&mut self, node: NodeIndex, slot: TokenSlot) -> Doc {
        let arms = self.parsed.ast.multi_children(node);
        let arms_doc = self.format_arm_block(arms, slot.first + 1, slot.last);
        Doc::concat([Doc::str("when "), arms_doc])
    }

    /// Format `{ arm1, arm2, ... }` for `when`, `match`, `# { ... }`, and `! { ... }`.
    pub fn format_arm_block(
        &mut self,
        arms: &[NodeIndex],
        body_start_tok: usize,
        body_last_tok: usize,
    ) -> Doc {
        let has_lbrace = self
            .parsed
            .sig_tokens
            .get(body_start_tok)
            .is_some_and(|t| t.kind == TokenKind::LBrace);
        let has_colon = !has_lbrace
            && self
                .parsed
                .sig_tokens
                .get(body_start_tok)
                .is_some_and(|t| t.kind == TokenKind::Colon);

        let (inner_first_tok, open_end_byte) = if has_lbrace || has_colon {
            (
                body_start_tok + 1,
                self.parsed.sig_tokens[body_start_tok].to as usize,
            )
        } else {
            let prev_end = self
                .parsed
                .sig_tokens
                .get(body_start_tok.saturating_sub(1))
                .map_or(0, |t| t.to as usize);
            (body_start_tok, prev_end)
        };

        let has_rbrace = has_lbrace
            && self
                .parsed
                .sig_tokens
                .get(body_last_tok)
                .is_some_and(|t| t.kind == TokenKind::RBrace);

        let (inner_last_tok, close_start_byte) = if has_rbrace {
            (
                body_last_tok.saturating_sub(1).max(inner_first_tok),
                self.parsed.sig_tokens[body_last_tok].from as usize,
            )
        } else {
            (
                body_last_tok.max(inner_first_tok),
                self.parsed
                    .sig_tokens
                    .get(body_last_tok)
                    .map_or(self.parsed.original_src.len(), |t| t.to as usize),
            )
        };

        let has_comments = self
            .comments
            .peek()
            .is_some_and(|(_, c)| c.byte_start < close_start_byte);

        if arms.is_empty() && !has_comments {
            return Doc::str("{}");
        }

        let tab = self.tab_spaces();
        let saved_indent = self.indent_spaces;
        self.indent_spaces += tab;
        self.brace_depth += 1;

        let mut arm_infos = Vec::with_capacity(arms.len());
        let mut cursor_tok = inner_first_tok;
        for &arm in arms {
            let bound = TokenSlot::new(cursor_tok, inner_last_tok.max(cursor_tok));
            let s = bound.child_slot_from(self.parsed, cursor_tok, arm);
            let lo = self.parsed.sig_tokens[s.first].from as usize;
            let hi = self.parsed.sig_tokens[s.last].to as usize;
            let mut sep_end = hi;
            let mut next_tok = s.last + 1;
            while next_tok <= inner_last_tok
                && next_tok < self.parsed.sig_tokens.len()
                && matches!(
                    self.parsed.sig_tokens[next_tok].kind,
                    TokenKind::Comma | TokenKind::Semi
                )
            {
                sep_end = self.parsed.sig_tokens[next_tok].to as usize;
                next_tok += 1;
            }
            let end_line = self.parsed.line_of_byte(sep_end.saturating_sub(1));
            arm_infos.push((arm, s, lo, hi, sep_end, end_line));
            cursor_tok = next_tok;
        }

        let mut parts: Vec<Doc> = Vec::new();
        let mut prev_end = open_end_byte;

        for (i, &(arm, arm_slot, lo, hi, sep_end, end_line)) in arm_infos.iter().enumerate() {
            let (lead, gap_cur, dir_flags) = self.comments.take_leading_before(
                prev_end,
                lo,
                self.indent_spaces,
                self.brace_depth,
                i > 0,
                &mut self.cfg_stack,
            );
            if i > 0 {
                let user_blank = self.parsed.blank_lines_in_gap(prev_end, lo) > 0
                    || self.parsed.blank_lines_in_gap(gap_cur, lo) > 0;
                if user_blank && !crate::formatter::doc_starts_with_line_break(&lead) {
                    parts.push(Doc::BlankLine);
                } else {
                    parts.push(Doc::HardLine);
                }
            }
            if !matches!(lead, Doc::Nil) {
                parts.push(lead);
            }

            let is_last = i + 1 == arm_infos.len();
            let next_arm_first_tok = arm_infos.get(i + 1).map(|a| a.1.first);

            let (arm_doc, ends_brace) = if dir_flags.skip {
                (self.format_skipped_bytes(lo, hi), false)
            } else if dir_flags.preserve {
                (self.format_preserved_bytes(lo, hi), false)
            } else {
                self.format_expr_unwrapped(arm, arm_slot)
            };

            // Emit `,` after non-block arms (and after block arms if the next arm starts with a
            // token like `.symbol` or `not pat` that doesn't trigger virtual `;` across `\n`!).
            let need_comma = if !ends_brace {
                true
            } else if !is_last
                && let Some(nt) = next_arm_first_tok
                && let Some(tok) = self.parsed.sig_tokens.get(nt)
            {
                !tok.kind.can_start_statement()
                    && !(tok.kind == TokenKind::KwElse
                        && self
                            .parsed
                            .sig_tokens
                            .get(nt + 1)
                            .is_some_and(|t2| t2.kind == TokenKind::FatArrow))
            } else {
                false
            };

            parts.push(arm_doc);
            if need_comma {
                parts.push(Doc::str(","));
            }

            let next_limit = arm_infos.get(i + 1).map_or(close_start_byte, |n| n.2);
            let cfg_now = self.cfg();
            let trailing = self.comments.take_same_line_trailing(
                end_line,
                next_limit,
                self.indent_spaces,
                &cfg_now,
            );
            if !matches!(trailing, Doc::Nil) {
                parts.push(trailing);
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
            if !parts.is_empty() {
                parts.push(Doc::HardLine);
            }
            parts.push(crate::formatter::trim_trailing_hardline(tail_comments));
        }

        self.brace_depth -= 1;
        self.cfg_stack.pop_above(self.brace_depth);
        self.indent_spaces = saved_indent;

        if parts.is_empty() {
            Doc::str("{}")
        } else {
            Doc::concat([
                Doc::str("{"),
                Doc::indent(tab, Doc::concat([Doc::HardLine, Doc::concat(parts)])),
                Doc::HardLine,
                Doc::str("}"),
            ])
        }
    }

    fn format_case_arm(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        let ch = self.parsed.ast.fixed_children(node);
        let pat_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
        let pat_doc = self.format_pattern(ch[0], pat_slot);
        let arrow_tok = self
            .parsed
            .find_depth_zero_token(
                pat_slot.last + 1,
                slot.last,
                |k| k == TokenKind::FatArrow,
                false,
            )
            .unwrap_or(pat_slot.last + 1);
        let (rhs_doc, eb) = self.format_arm_rhs(ch[1], arrow_tok + 1, slot);
        (Doc::concat([pat_doc, Doc::str(" =>"), rhs_doc]), eb)
    }

    fn format_catch_arm(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        let ch = self.parsed.ast.fixed_children(node);
        let (id_f, id_l) = self.parsed.exact_token_bounds(ch[0], slot.first + 1);
        let id_str = self.format_ident(ch[0], id_f);
        let arrow_tok = self
            .parsed
            .find_depth_zero_token(id_l + 1, slot.last, |k| k == TokenKind::FatArrow, false)
            .unwrap_or(id_l + 1);
        let (rhs_doc, eb) = self.format_arm_rhs(ch[1], arrow_tok + 1, slot);
        (
            Doc::concat([
                Doc::str("catch "),
                Doc::text(id_str),
                Doc::str(" =>"),
                rhs_doc,
            ]),
            eb,
        )
    }

    fn format_condition_arm(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        let ch = self.parsed.ast.fixed_children(node);
        let cond_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
        let cond_doc = self.format_expr(ch[0], cond_slot);
        let arrow_tok = self
            .parsed
            .find_depth_zero_token(
                cond_slot.last + 1,
                slot.last,
                |k| k == TokenKind::FatArrow,
                false,
            )
            .unwrap_or(cond_slot.last + 1);
        let (rhs_doc, eb) = self.format_arm_rhs(ch[1], arrow_tok + 1, slot);
        (Doc::concat([cond_doc, Doc::str(" =>"), rhs_doc]), eb)
    }

    fn format_else_condition_arm(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        let ch = self.parsed.ast.fixed_children(node);
        let arrow_tok = self
            .parsed
            .find_depth_zero_token(
                slot.first + 1,
                slot.last,
                |k| k == TokenKind::FatArrow,
                false,
            )
            .unwrap_or(slot.first + 1);
        let (rhs_doc, eb) = self.format_arm_rhs(ch[0], arrow_tok + 1, slot);
        (Doc::concat([Doc::str("else =>"), rhs_doc]), eb)
    }

    /// Format the RHS of `=>` in `CaseArm`, `CatchArm`, `ConditionArm`, `ElseConditionArm`, `CaseMap`,
    /// or `handles ... =>`.
    /// IMPORTANT: Never emit a line break immediately after `=>` unless `body` is a `Block` (which
    /// starts with `{` on the same line), because `=>\n` triggers `TokenKind::Indent` in Nessa's lexer!
    pub fn format_arm_rhs(
        &mut self,
        body: NodeIndex,
        rhs_start_tok: usize,
        outer_slot: TokenSlot,
    ) -> (Doc, bool) {
        if body.is_null() {
            return (Doc::str(" {}"), true);
        }
        if self.parsed.ast.node(body).kind == NodeKind::Block {
            let block_slot = TokenSlot::new(rhs_start_tok, outer_slot.last.max(rhs_start_tok));
            let b_doc = self.format_block(body, block_slot, None, ContainerKind::ValueBlock);
            (Doc::concat([Doc::space(), b_doc]), true)
        } else {
            let body_slot = outer_slot.child_slot_from(self.parsed, rhs_start_tok, body);
            let arrow_end = self
                .parsed
                .sig_tokens
                .get(rhs_start_tok.saturating_sub(1))
                .map_or(0, |t| t.to as usize);
            let body_lo = self
                .parsed
                .sig_tokens
                .get(body_slot.first)
                .map_or(arrow_end, |t| t.from as usize);
            let inline_c = self.comments.take_inline_between(
                arrow_end,
                body_lo,
                self.indent_spaces,
                &self.cfg(),
            );
            let (b_doc, eb) = self.format_statement_or_expr(body, body_slot);
            if matches!(inline_c, Doc::Nil) {
                (Doc::concat([Doc::space(), b_doc]), eb)
            } else {
                // If a `--` line comment appeared right after `=>` on a non-block arm, wrap in `{ ... }`?
                // Note: if `--` appeared on the `=>` line, Nessa's lexer ALREADY saw `=>\n` and emitted
                // `Indent`, so `Parser` ALREADY built `NodeKind::Block` (handled in the branch above)!
                // Thus `inline_c` here can only be an inline `{- ... -}` block comment on the same line!
                (Doc::concat([inline_c, b_doc]), eb)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Postfix & Binary Operators
    // -----------------------------------------------------------------------

    fn format_postfix(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        let kind = self.parsed.ast.node(node).kind;
        let ch = self.parsed.ast.fixed_children(node);

        match kind {
            NodeKind::Projection | NodeKind::View => {
                let obj_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let op_kind = if kind == NodeKind::Projection {
                    TokenKind::Dot
                } else {
                    TokenKind::Quote
                };
                let op_str = if kind == NodeKind::Projection {
                    "."
                } else {
                    "'"
                };
                let op_tok = self
                    .parsed
                    .find_depth_zero_token(obj_slot.last + 1, slot.last, |k| k == op_kind, false)
                    .unwrap_or(obj_slot.last + 1);
                let obj_doc = self.format_expr(ch[0], obj_slot);
                let field_doc = if self.parsed.ast.node(ch[1]).kind == NodeKind::Int {
                    Doc::text(self.literal_slice(ch[1], op_tok + 1))
                } else {
                    Doc::text(self.format_ident(ch[1], op_tok + 1))
                };
                (Doc::concat([obj_doc, Doc::str(op_str), field_doc]), false)
            }
            NodeKind::Await => {
                let obj_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let obj_doc = self.format_expr(ch[0], obj_slot);
                (Doc::concat([obj_doc, Doc::str(".await")]), false)
            }
            NodeKind::TypeCast => {
                let obj_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let open_p = self
                    .parsed
                    .find_depth_zero_token(
                        obj_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::LParen,
                        false,
                    )
                    .unwrap_or(obj_slot.last + 3);
                let ty_bound =
                    TokenSlot::new(open_p + 1, slot.last.saturating_sub(1).max(open_p + 1));
                let ty_slot = ty_bound.child_slot_from(self.parsed, open_p + 1, ch[1]);
                let obj_doc = self.format_expr(ch[0], obj_slot);
                let ty_doc = self.format_expr(ch[1], ty_slot);
                (
                    Doc::concat([obj_doc, Doc::str(".as("), ty_doc, Doc::str(")")]),
                    false,
                )
            }
            NodeKind::HandlerApplication => {
                let obj_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let open_p = self
                    .parsed
                    .find_depth_zero_token(
                        obj_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::LParen,
                        false,
                    )
                    .unwrap_or(obj_slot.last + 3);
                let h_bound =
                    TokenSlot::new(open_p + 1, slot.last.saturating_sub(1).max(open_p + 1));
                let h_slot = h_bound.child_slot_from(self.parsed, open_p + 1, ch[1]);
                let obj_doc = self.format_expr(ch[0], obj_slot);
                let h_doc = self.format_expr(ch[1], h_slot);
                (
                    Doc::concat([obj_doc, Doc::str(".use("), h_doc, Doc::str(")")]),
                    false,
                )
            }
            NodeKind::Call => {
                let callee_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let args = self.parsed.ast.multi_children(node);
                let open_p = self
                    .parsed
                    .find_depth_zero_token(
                        callee_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::LParen,
                        false,
                    )
                    .unwrap_or(callee_slot.last + 1);
                let callee_doc = self.format_expr(ch[0], callee_slot);
                let budget = Some(self.cfg().width_heuristics().fn_call_width);
                let args_doc = self.format_delimited_seq(
                    args,
                    open_p,
                    slot.last,
                    DelimStyle::Parens,
                    budget,
                    |fmt, arg, arg_slot| fmt.format_expr(arg, arg_slot),
                );
                (Doc::concat([callee_doc, args_doc]), false)
            }
            NodeKind::ExtendedCall => {
                let callee_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let args = self.parsed.ast.multi_children(node);
                let open_b = self
                    .parsed
                    .find_depth_zero_token(
                        callee_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::LBrace,
                        false,
                    )
                    .unwrap_or(callee_slot.last + 1);
                let callee_doc = self.format_expr(ch[0], callee_slot);
                let budget = Some(self.cfg().width_heuristics().struct_lit_width);
                let args_doc = self.format_delimited_seq(
                    args,
                    open_b,
                    slot.last,
                    DelimStyle::SpacedBraces,
                    budget,
                    |fmt, arg, arg_slot| fmt.format_expr(arg, arg_slot),
                );
                (Doc::concat([callee_doc, Doc::space(), args_doc]), true)
            }
            NodeKind::OptionPropagation => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let inner_doc = self.format_expr(ch[0], inner_slot);
                (Doc::concat([inner_doc, Doc::str("?")]), false)
            }
            NodeKind::ErrorPropagation => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let inner_doc = self.format_expr(ch[0], inner_slot);
                (Doc::concat([inner_doc, Doc::str("!")]), false)
            }
            NodeKind::EffectPropagation => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let inner_doc = self.format_expr(ch[0], inner_slot);
                (Doc::concat([inner_doc, Doc::str("#")]), false)
            }
            NodeKind::EffectElimination => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let hash_tok = self
                    .parsed
                    .find_depth_zero_token(
                        inner_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::Hash,
                        false,
                    )
                    .unwrap_or(inner_slot.last + 1);
                let inner_doc = self.format_expr(ch[0], inner_slot);
                let arms = self.parsed.ast.multi_children(node);
                let arms_doc = self.format_arm_block(arms, hash_tok + 1, slot.last);
                (Doc::concat([inner_doc, Doc::str(" # "), arms_doc]), true)
            }
            NodeKind::ErrorElimination => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let bang_tok = self
                    .parsed
                    .find_depth_zero_token(
                        inner_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::Bang,
                        false,
                    )
                    .unwrap_or(inner_slot.last + 1);
                let inner_doc = self.format_expr(ch[0], inner_slot);
                let arms = self.parsed.ast.multi_children(node);
                let arms_doc = self.format_arm_block(arms, bang_tok + 1, slot.last);
                (Doc::concat([inner_doc, Doc::str(" ! "), arms_doc]), true)
            }
            NodeKind::PostMatch => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let match_tok = self
                    .parsed
                    .find_depth_zero_token(
                        inner_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::KwMatch,
                        false,
                    )
                    .unwrap_or(inner_slot.last + 1);
                let inner_doc = self.format_expr(ch[0], inner_slot);
                let arms = self.parsed.ast.multi_children(node);
                let arms_doc = self.format_arm_block(arms, match_tok + 1, slot.last);
                (
                    Doc::concat([inner_doc, Doc::str(" match "), arms_doc]),
                    true,
                )
            }
            NodeKind::PostDo => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let do_tok = self
                    .parsed
                    .find_depth_zero_token(
                        inner_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::KwDo,
                        false,
                    )
                    .unwrap_or(inner_slot.last + 1);
                let inner_doc = self.format_expr(ch[0], inner_slot);
                let rhs = ch[1];
                if !rhs.is_null() && self.parsed.ast.node(rhs).kind == NodeKind::Block {
                    let b_slot = TokenSlot::new(do_tok + 1, slot.last.max(do_tok + 1));
                    let b_doc = self.format_block(rhs, b_slot, None, ContainerKind::ValueBlock);
                    (Doc::concat([inner_doc, Doc::str(" do "), b_doc]), true)
                } else {
                    let rhs_slot = slot.child_slot_from(self.parsed, do_tok + 1, rhs);
                    let (rhs_doc, eb) = self.format_expr_with_rbrace(rhs, rhs_slot);
                    (Doc::concat([inner_doc, Doc::str(" do "), rhs_doc]), eb)
                }
            }
            _ => unreachable!(),
        }
    }

    /// Format standard binary operator with trailing operator before `Doc::Line` (`lhs op\n    rhs`)
    /// so `Parser::next_token_inner` NEVER inserts a virtual `;` before `+` / `-`!
    fn format_binary_op(
        &mut self,
        node: NodeIndex,
        slot: TokenSlot,
        op_str: &'static str,
        op_kind: TokenKind,
    ) -> (Doc, bool) {
        let ch = self.parsed.ast.fixed_children(node);
        let lhs_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
        let op_tok = self
            .parsed
            .find_depth_zero_token(lhs_slot.last + 1, slot.last, |k| k == op_kind, false)
            .unwrap_or(lhs_slot.last + 1);
        let rhs_slot = slot.child_slot_from(self.parsed, op_tok + 1, ch[1]);

        let lhs_doc = self.format_expr(ch[0], lhs_slot);

        let op_end = self
            .parsed
            .sig_tokens
            .get(op_tok)
            .map_or(0, |t| t.to as usize);
        let rhs_lo = self
            .parsed
            .sig_tokens
            .get(rhs_slot.first)
            .map_or(op_end, |t| t.from as usize);
        let lhs_end = self
            .parsed
            .sig_tokens
            .get(lhs_slot.last)
            .map_or(0, |t| t.to as usize);
        let op_lo = self
            .parsed
            .sig_tokens
            .get(op_tok)
            .map_or(lhs_end, |t| t.from as usize);

        let pre_op_c =
            self.comments
                .take_inline_between(lhs_end, op_lo, self.indent_spaces, &self.cfg());
        let post_op_c =
            self.comments
                .take_inline_between(op_end, rhs_lo, self.indent_spaces, &self.cfg());

        let (rhs_doc, eb) = self.format_expr_with_rbrace(ch[1], rhs_slot);
        let tab = self.tab_spaces();

        // Guard `- -x`: if `op_kind == Minus` and `rhs` starts with `-`, flat mode already has a space ` - -x`.
        if !matches!(pre_op_c, Doc::Nil) || !matches!(post_op_c, Doc::Nil) {
            return (
                Doc::concat([
                    lhs_doc,
                    pre_op_c,
                    Doc::space(),
                    Doc::str(op_str),
                    post_op_c,
                    Doc::space(),
                    rhs_doc,
                ]),
                eb,
            );
        }

        let doc = Doc::group(Doc::concat([
            lhs_doc,
            Doc::space(),
            Doc::str(op_str),
            Doc::if_break(
                Doc::indent(tab, Doc::concat([Doc::HardLine, rhs_doc.clone()])),
                Doc::concat([Doc::space(), rhs_doc]),
            ),
        ]));
        (doc, eb)
    }

    fn format_pipeline(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        // Flatten left-associative `|>` chain (`a |> b |> c`) unless an inner `Pipeline` was
        // explicitly wrapped in parentheses `(a |> b) |> c`.
        let mut stages: Vec<(NodeIndex, TokenSlot, usize)> = Vec::new();
        let mut cur = node;
        let mut cur_slot = slot;

        while !cur.is_null() && self.parsed.ast.node(cur).kind == NodeKind::Pipeline {
            let (parens, unwrapped_slot) = cur_slot.unwrap_parens(self.parsed, cur);
            if parens > 0 && cur != node {
                break;
            }
            let ch = self.parsed.ast.fixed_children(cur);
            let lhs_slot = unwrapped_slot.child_slot_from(self.parsed, unwrapped_slot.first, ch[0]);
            let pipe_tok = self
                .parsed
                .find_depth_zero_token(
                    lhs_slot.last + 1,
                    unwrapped_slot.last,
                    |k| k == TokenKind::PipeGt,
                    false,
                )
                .unwrap_or(lhs_slot.last + 1);
            let rhs_slot = unwrapped_slot.child_slot_from(self.parsed, pipe_tok + 1, ch[1]);
            stages.push((ch[1], rhs_slot, pipe_tok));
            cur = ch[0];
            cur_slot = lhs_slot;
        }
        stages.reverse();

        let head_doc = self.format_expr(cur, cur_slot);
        let mut prev_last_tok = cur_slot.last;
        let tab = self.tab_spaces();
        let mut chain_parts = Vec::with_capacity(stages.len() * 3);
        let mut last_eb = false;

        for (rhs_node, rhs_slot, _pipe_tok) in stages {
            let prev_end = self
                .parsed
                .sig_tokens
                .get(prev_last_tok)
                .map_or(0, |t| t.to as usize);
            let rhs_lo = self
                .parsed
                .sig_tokens
                .get(rhs_slot.first)
                .map_or(prev_end, |t| t.from as usize);
            let inline_c = self.comments.take_inline_between(
                prev_end,
                rhs_lo,
                self.indent_spaces + tab,
                &self.cfg(),
            );
            let (rhs_doc, eb) = self.format_expr_with_rbrace(rhs_node, rhs_slot);
            last_eb = eb;
            if matches!(inline_c, Doc::Nil) {
                chain_parts.push(Doc::Line);
                chain_parts.push(Doc::str("|> "));
                chain_parts.push(rhs_doc);
            } else {
                chain_parts.push(inline_c);
                chain_parts.push(Doc::str("|> "));
                chain_parts.push(rhs_doc);
            }
            prev_last_tok = rhs_slot.last;
        }

        let budget = self.cfg().width_heuristics().chain_width;
        let doc = Doc::group_with_budget(
            budget,
            Doc::concat([head_doc, Doc::indent(tab, Doc::concat(chain_parts))]),
        );
        (doc, last_eb)
    }

    fn format_infix_fn_call(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        let ch = self.parsed.ast.fixed_children(node);
        let lhs_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
        let op_tok_idx = self
            .parsed
            .find_depth_zero_token(
                lhs_slot.last + 1,
                slot.last,
                |k| matches!(k, TokenKind::Id | TokenKind::ArbitraryId),
                false,
            )
            .unwrap_or(lhs_slot.last + 1);
        let op_spelling = if let Some(tok) = self.parsed.sig_tokens.get(op_tok_idx)
            && tok.kind == TokenKind::ArbitraryId
        {
            self.parsed.original_src[tok.from as usize..tok.to as usize].to_string()
        } else {
            str_interner::get(self.parsed.ast.node(node).str_id).to_string()
        };
        let rhs_slot = slot.child_slot_from(self.parsed, op_tok_idx + 1, ch[1]);
        let lhs_doc = self.format_expr(ch[0], lhs_slot);
        let (rhs_doc, eb) = self.format_expr_with_rbrace(ch[1], rhs_slot);
        // Keep infix identifier on the same line as `lhs` and `rhs` start (`Id` canboth end and start
        // a statement across `\n` in `Parser::next_token_inner`!).
        (
            Doc::concat([
                lhs_doc,
                Doc::space(),
                Doc::text(op_spelling),
                Doc::space(),
                rhs_doc,
            ]),
            eb,
        )
    }

    fn format_bool_matches(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        let ch = self.parsed.ast.fixed_children(node);
        let expr_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
        let matches_tok = self
            .parsed
            .find_depth_zero_token(
                expr_slot.last + 1,
                slot.last,
                |k| k == TokenKind::KwMatches,
                false,
            )
            .unwrap_or(expr_slot.last + 1);
        let pat_slot = slot.child_slot_from(self.parsed, matches_tok + 1, ch[1]);
        let expr_doc = self.format_expr(ch[0], expr_slot);
        let pat_doc = self.format_pattern(ch[1], pat_slot);
        let tab = self.tab_spaces();
        let doc = Doc::group(Doc::concat([
            expr_doc,
            Doc::str(" matches"),
            Doc::indent(tab, Doc::concat([Doc::Line, pat_doc])),
        ]));
        (doc, false)
    }
}

fn can_single_line_block_elem(ast: &ast::Ast, node: NodeIndex) -> bool {
    let Some(kind) = unwrap_modifiers_kind(ast, node) else {
        return false;
    };
    !matches!(
        kind,
        NodeKind::Block
            | NodeKind::IfStatement
            | NodeKind::WhenStatement
            | NodeKind::ForLoop
            | NodeKind::WhileLoop
            | NodeKind::PostMatch
            | NodeKind::EffectElimination
            | NodeKind::ErrorElimination
            | NodeKind::FunctionDef
            | NodeKind::StructDef
            | NodeKind::EnumDef
            | NodeKind::TraitDef
            | NodeKind::ImplDef
            | NodeKind::ImplTraitDef
            | NodeKind::ExtendDef
            | NodeKind::ExtendTraitDef
            | NodeKind::ModuleDef
    )
}
