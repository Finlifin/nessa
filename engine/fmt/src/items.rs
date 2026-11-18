//! Top-level and nested item definitions, parameters, `handles` forms, ADTs, traits, and `use` paths.

use ast::{NodeIndex, NodeKind};
use lexer::TokenKind;

use crate::doc::Doc;
use crate::formatter::{ContainerKind, Formatter, ImportGroup, TokenSlot, compare_import_keys};
use crate::patterns::{DelimStyle, find_matching_delim, strip_last_hardline};

impl<'a> Formatter<'a> {
    /// Format a single member inside a container (`FileScope`, `ModuleBody`, `AdtBody`,
    /// `TraitOrImplBody`, or `ValueBlock`), returning `(doc, ends_with_rbrace)`.
    pub fn format_container_member(
        &mut self,
        node: NodeIndex,
        slot: TokenSlot,
        _is_last: bool,
        container_kind: ContainerKind,
    ) -> (Doc, bool) {
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

        let (vis_prefix, inner_node, inner_slot) = self.unwrap_visibility_prefix(node, slot);
        if let Some(bridged) = self.bridged_at(inner_node, inner_slot.first) {
            let ends_brace = self
                .parsed
                .original_src
                .get(bridged.span_bytes().0..bridged.span_bytes().1)
                .is_some_and(|s| s.trim_end().ends_with('}'));
            let b_doc = self.format_bridged(bridged);
            return (Doc::concat([vis_prefix, b_doc]), ends_brace);
        }

        let kind = self.parsed.ast.node(inner_node).kind;
        let (body_doc, ends_with_rbrace) = match kind {
            NodeKind::StructField if container_kind == ContainerKind::AdtBody => {
                (self.format_struct_field(inner_node, inner_slot), false)
            }
            NodeKind::EnumVariant if container_kind == ContainerKind::AdtBody => {
                (self.format_enum_variant(inner_node, inner_slot), false)
            }
            NodeKind::StructField => (self.format_struct_field(inner_node, inner_slot), false),
            NodeKind::EnumVariant => (self.format_enum_variant(inner_node, inner_slot), false),
            NodeKind::UseStatement => {
                let reorder = self.cfg_stack.current().reorder_imports;
                let (u_doc, _, _) = self.format_use_core(inner_node, inner_slot, reorder);
                (u_doc, false)
            }
            NodeKind::ModuleDef => self.format_module_def(inner_node, inner_slot),
            NodeKind::FunctionDef => self.format_fn_or_derive_fn(inner_node, inner_slot, false),
            NodeKind::TraitDeriveFn => self.format_fn_or_derive_fn(inner_node, inner_slot, true),
            NodeKind::TraitDefFn => (self.format_trait_def_fn(inner_node, inner_slot), false),
            NodeKind::EffectDef => (self.format_effect_def(inner_node, inner_slot, false), false),
            NodeKind::AsyncEffectDef => {
                (self.format_effect_def(inner_node, inner_slot, true), false)
            }
            NodeKind::HandlesStatement => self.format_handles_statement(inner_node, inner_slot),
            NodeKind::LetHandler => self.format_let_handler(inner_node, inner_slot),
            NodeKind::GlobalDecl => self.format_global_decl(inner_node, inner_slot),
            NodeKind::AssocDecl => self.format_assoc_decl(inner_node, inner_slot),
            NodeKind::StructDef => (self.format_adt_def(inner_node, inner_slot, "struct"), true),
            NodeKind::EnumDef => (self.format_adt_def(inner_node, inner_slot, "enum"), true),
            NodeKind::TraitDef => (self.format_trait_def(inner_node, inner_slot), true),
            NodeKind::ImplDef => (
                self.format_inherent_impl_or_extend(inner_node, inner_slot, "impl"),
                true,
            ),
            NodeKind::ExtendDef => (
                self.format_inherent_impl_or_extend(inner_node, inner_slot, "extend"),
                true,
            ),
            NodeKind::ImplTraitDef => (
                self.format_trait_impl_or_extend(inner_node, inner_slot, "impl"),
                true,
            ),
            NodeKind::ExtendTraitDef => (
                self.format_trait_impl_or_extend(inner_node, inner_slot, "extend"),
                true,
            ),
            NodeKind::DeriveDef => (self.format_derive_def(inner_node, inner_slot), false),
            NodeKind::Typealias => {
                self.format_typealias_or_newtype(inner_node, inner_slot, "typealias")
            }
            NodeKind::Newtype => {
                self.format_typealias_or_newtype(inner_node, inner_slot, "newtype")
            }
            _ => self.format_statement_or_expr(inner_node, inner_slot),
        };

        (Doc::concat([vis_prefix, body_doc]), ends_with_rbrace)
    }

    fn unwrap_visibility_prefix(
        &mut self,
        mut node: NodeIndex,
        slot: TokenSlot,
    ) -> (Doc, NodeIndex, TokenSlot) {
        let mut vis: Option<&'static str> = None;
        let mut cursor = slot.first;
        let mut inline_comments = Vec::new();

        while !node.is_null() {
            match self.parsed.ast.node(node).kind {
                NodeKind::PubDef => {
                    vis = Some("pub ");
                    let inner = self.parsed.ast.fixed_children(node)[0];
                    let kw_end = self
                        .parsed
                        .sig_tokens
                        .get(cursor)
                        .map_or(0, |t| t.to as usize);
                    let inner_slot = slot.child_slot_from(self.parsed, cursor + 1, inner);
                    let next_lo = self
                        .parsed
                        .sig_tokens
                        .get(inner_slot.first)
                        .map_or(kw_end, |t| t.from as usize);
                    let c_doc = self.comments.take_inline_between(
                        kw_end,
                        next_lo,
                        self.indent_spaces,
                        &self.cfg(),
                    );
                    if !matches!(c_doc, Doc::Nil) {
                        inline_comments.push(c_doc);
                    }
                    node = inner;
                    cursor = inner_slot.first;
                }
                NodeKind::PrivateDef => {
                    vis = Some("private ");
                    let inner = self.parsed.ast.fixed_children(node)[0];
                    let kw_end = self
                        .parsed
                        .sig_tokens
                        .get(cursor)
                        .map_or(0, |t| t.to as usize);
                    let inner_slot = slot.child_slot_from(self.parsed, cursor + 1, inner);
                    let next_lo = self
                        .parsed
                        .sig_tokens
                        .get(inner_slot.first)
                        .map_or(kw_end, |t| t.from as usize);
                    let c_doc = self.comments.take_inline_between(
                        kw_end,
                        next_lo,
                        self.indent_spaces,
                        &self.cfg(),
                    );
                    if !matches!(c_doc, Doc::Nil) {
                        inline_comments.push(c_doc);
                    }
                    node = inner;
                    cursor = inner_slot.first;
                }
                _ => break,
            }
        }

        let inner_slot = TokenSlot::new(cursor, slot.last.max(cursor));
        let prefix = match (vis, inline_comments.is_empty()) {
            (None, true) => Doc::Nil,
            (Some(v), true) => Doc::str(v),
            (None, false) => Doc::concat(inline_comments),
            (Some(v), false) => {
                let mut v_docs = vec![Doc::str(v.trim_end())];
                v_docs.extend(inline_comments);
                Doc::concat(v_docs)
            }
        };
        (prefix, node, inner_slot)
    }

    /// Format a parameter node (`ParamSelf`, `ParamVarargs`, `ParamOptional`, `ParamLambda`, `ParamTyped`, `ParamCatch`).
    pub fn format_parameter(&mut self, param: NodeIndex, slot: TokenSlot) -> Doc {
        if param.is_null() {
            return Doc::Nil;
        }
        let kind = self.parsed.ast.node(param).kind;
        let ch = self.parsed.ast.fixed_children(param);

        match kind {
            NodeKind::ParamSelf => Doc::str("self"),
            NodeKind::ParamVarargs => {
                // `...id (: ty)?`
                let id_str = self.format_ident(ch[0], slot.first + 3);
                if ch[1].is_null() {
                    Doc::concat([Doc::str("..."), Doc::text(id_str)])
                } else {
                    let colon_tok = self
                        .parsed
                        .find_depth_zero_token(
                            slot.first + 4,
                            slot.last,
                            |k| k == TokenKind::Colon,
                            false,
                        )
                        .unwrap_or(slot.first + 4);
                    let ty_slot = slot.child_slot_from(self.parsed, colon_tok + 1, ch[1]);
                    let ty_doc = self.format_expr(ch[1], ty_slot);
                    Doc::concat([Doc::str("..."), Doc::text(id_str), Doc::str(": "), ty_doc])
                }
            }
            NodeKind::ParamOptional => {
                // `.id (: ty)? = init`
                let id_str = self.format_ident(ch[0], slot.first + 1);
                let mut cursor = slot.first + 2;
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
                let init_slot = slot.child_slot_from(self.parsed, eq_tok + 1, ch[2]);
                let init_doc = self.format_expr(ch[2], init_slot);
                Doc::concat([
                    Doc::str("."),
                    Doc::text(id_str),
                    ty_part,
                    Doc::str(" = "),
                    init_doc,
                ])
            }
            NodeKind::ParamLambda => {
                // `lambda pat (: ty)?`
                let pat_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
                let pat_doc = self.format_pattern(ch[0], pat_slot);
                if ch[1].is_null() {
                    Doc::concat([Doc::str("lambda "), pat_doc])
                } else {
                    let colon_tok = self
                        .parsed
                        .find_depth_zero_token(
                            pat_slot.last + 1,
                            slot.last,
                            |k| k == TokenKind::Colon,
                            false,
                        )
                        .unwrap_or(pat_slot.last + 1);
                    let ty_slot = slot.child_slot_from(self.parsed, colon_tok + 1, ch[1]);
                    let ty_doc = self.format_expr(ch[1], ty_slot);
                    Doc::concat([Doc::str("lambda "), pat_doc, Doc::str(": "), ty_doc])
                }
            }
            NodeKind::ParamCatch => {
                // `catch id (: ty)?`
                let id_str = self.format_ident(ch[0], slot.first + 1);
                if ch[1].is_null() {
                    Doc::concat([Doc::str("catch "), Doc::text(id_str)])
                } else {
                    let colon_tok = self
                        .parsed
                        .find_depth_zero_token(
                            slot.first + 2,
                            slot.last,
                            |k| k == TokenKind::Colon,
                            false,
                        )
                        .unwrap_or(slot.first + 2);
                    let ty_slot = slot.child_slot_from(self.parsed, colon_tok + 1, ch[1]);
                    let ty_doc = self.format_expr(ch[1], ty_slot);
                    Doc::concat([
                        Doc::str("catch "),
                        Doc::text(id_str),
                        Doc::str(": "),
                        ty_doc,
                    ])
                }
            }
            NodeKind::ParamTyped => {
                // `pat (: ty)?`
                if ch[1].is_null() {
                    let pat_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                    self.format_pattern(ch[0], pat_slot)
                } else {
                    let pat_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                    let colon_tok = self
                        .parsed
                        .find_depth_zero_token(
                            pat_slot.last + 1,
                            slot.last,
                            |k| k == TokenKind::Colon,
                            false,
                        )
                        .unwrap_or(pat_slot.last + 1);
                    let ty_slot = slot.child_slot_from(self.parsed, colon_tok + 1, ch[1]);
                    let pat_doc = self.format_pattern(ch[0], pat_slot);
                    let ty_doc = self.format_expr(ch[1], ty_slot);
                    Doc::concat([pat_doc, Doc::str(": "), ty_doc])
                }
            }
            _ => self.format_pattern(param, slot),
        }
    }

    fn format_struct_field(&mut self, node: NodeIndex, slot: TokenSlot) -> Doc {
        let ch = self.parsed.ast.fixed_children(node);
        let name_str = self.format_ident(ch[0], slot.first);
        let colon_tok = self
            .parsed
            .find_depth_zero_token(slot.first + 1, slot.last, |k| k == TokenKind::Colon, false)
            .unwrap_or(slot.first + 1);
        let ty_slot = slot.child_slot_from(self.parsed, colon_tok + 1, ch[1]);
        let ty_doc = self.format_expr(ch[1], ty_slot);
        if ch[2].is_null() {
            Doc::concat([Doc::text(name_str), Doc::str(": "), ty_doc])
        } else {
            let eq_tok = self
                .parsed
                .find_depth_zero_token(ty_slot.last + 1, slot.last, |k| k == TokenKind::Eq, false)
                .unwrap_or(ty_slot.last + 1);
            let def_slot = slot.child_slot_from(self.parsed, eq_tok + 1, ch[2]);
            let def_doc = self.format_expr(ch[2], def_slot);
            Doc::concat([
                Doc::text(name_str),
                Doc::str(": "),
                ty_doc,
                Doc::str(" = "),
                def_doc,
            ])
        }
    }

    fn format_enum_variant(&mut self, node: NodeIndex, slot: TokenSlot) -> Doc {
        let ch = self.parsed.ast.fixed_children(node);
        let params = self.parsed.ast.multi_children(node);
        let name_str = self.format_ident(ch[0], slot.first);

        let has_parens = !params.is_empty()
            || self
                .parsed
                .find_depth_zero_token(slot.first + 1, slot.last, |k| k == TokenKind::LParen, false)
                .is_some();

        if !has_parens {
            return Doc::text(name_str);
        }

        let open_paren = self
            .parsed
            .find_depth_zero_token(slot.first + 1, slot.last, |k| k == TokenKind::LParen, false)
            .unwrap_or(slot.first + 1);
        let close_paren = find_matching_delim(
            &self.parsed.sig_tokens,
            open_paren,
            TokenKind::LParen,
            TokenKind::RParen,
        )
        .unwrap_or(slot.last);
        let budget = Some(self.cfg().width_heuristics().fn_call_width);
        let params_doc = self.format_delimited_seq(
            params,
            open_paren,
            close_paren,
            DelimStyle::Parens,
            budget,
            |fmt, p, ps| fmt.format_parameter(p, ps),
        );
        Doc::concat([Doc::text(name_str), params_doc])
    }

    fn format_fn_or_derive_fn(
        &mut self,
        node: NodeIndex,
        slot: TokenSlot,
        is_derive_fn: bool,
    ) -> (Doc, bool) {
        // `FunctionDef` / `TraitDeriveFn`: [0] name, [1] ret_type, [2] body, [3] capability, multi = params
        let ch = self.parsed.ast.fixed_children(node);
        let params = self.parsed.ast.multi_children(node);
        let kw_str = if is_derive_fn { "derive fn " } else { "fn " };
        let kw_toks = if is_derive_fn { 2 } else { 1 };
        let name_tok_floor = slot.first + kw_toks;
        let (name_f, name_l) = self.parsed.exact_token_bounds(ch[0], name_tok_floor);
        let name_str = self.format_ident(ch[0], name_f);

        let mut cursor = name_l + 1;
        let params_doc = if let Some(open_p) =
            self.parsed
                .find_depth_zero_token(cursor, slot.last, |k| k == TokenKind::LParen, false)
            && open_p == cursor
        {
            let close_p = find_matching_delim(
                &self.parsed.sig_tokens,
                open_p,
                TokenKind::LParen,
                TokenKind::RParen,
            )
            .unwrap_or(open_p + 1);
            cursor = close_p + 1;
            self.format_delimited_seq(
                params,
                open_p,
                close_p,
                DelimStyle::Parens,
                None,
                |fmt, p, ps| fmt.format_parameter(p, ps),
            )
        } else {
            Doc::str("()")
        };

        let (sig_tail, next_cursor) = self.format_ret_and_capability(ch[1], ch[3], cursor, slot);
        cursor = next_cursor;

        let body = ch[2];
        let (body_doc, ends_rbrace) = self.format_callable_body(body, cursor, slot);

        (
            Doc::concat([
                Doc::str(kw_str),
                Doc::text(name_str),
                params_doc,
                sig_tail,
                body_doc,
            ]),
            ends_rbrace,
        )
    }

    fn format_trait_def_fn(&mut self, node: NodeIndex, slot: TokenSlot) -> Doc {
        // `TraitDefFn`: [0] name, [1] ret_type, [2] capability, multi = params
        let ch = self.parsed.ast.fixed_children(node);
        let params = self.parsed.ast.multi_children(node);
        let has_def_kw = self
            .parsed
            .sig_tokens
            .get(slot.first)
            .is_some_and(|t| t.kind == TokenKind::KwDef);
        let kw_toks = if has_def_kw { 2 } else { 1 };
        let kw_str = if has_def_kw { "def fn " } else { "fn " };

        let (name_f, name_l) = self.parsed.exact_token_bounds(ch[0], slot.first + kw_toks);
        let name_str = self.format_ident(ch[0], name_f);

        let mut cursor = name_l + 1;
        let params_doc = if cursor <= slot.last
            && self
                .parsed
                .sig_tokens
                .get(cursor)
                .is_some_and(|t| t.kind == TokenKind::LParen)
        {
            let close_p = find_matching_delim(
                &self.parsed.sig_tokens,
                cursor,
                TokenKind::LParen,
                TokenKind::RParen,
            )
            .unwrap_or(cursor + 1);
            let open_p = cursor;
            cursor = close_p + 1;
            self.format_delimited_seq(
                params,
                open_p,
                close_p,
                DelimStyle::Parens,
                None,
                |fmt, p, ps| fmt.format_parameter(p, ps),
            )
        } else {
            Doc::str("()")
        };

        let (sig_tail, _) = self.format_ret_and_capability(ch[1], ch[2], cursor, slot);
        Doc::concat([Doc::str(kw_str), Doc::text(name_str), params_doc, sig_tail])
    }

    fn format_effect_def(&mut self, node: NodeIndex, slot: TokenSlot, is_async: bool) -> Doc {
        // `EffectDef` / `AsyncEffectDef`: [0] name, [1] ret_type, multi = params
        let ch = self.parsed.ast.fixed_children(node);
        let params = self.parsed.ast.multi_children(node);
        let kw_str = if is_async { "async effect " } else { "effect " };
        let kw_toks = if is_async { 2 } else { 1 };
        let (name_f, name_l) = self.parsed.exact_token_bounds(ch[0], slot.first + kw_toks);
        let name_str = self.format_ident(ch[0], name_f);

        let mut cursor = name_l + 1;
        let params_doc = if cursor <= slot.last
            && self
                .parsed
                .sig_tokens
                .get(cursor)
                .is_some_and(|t| t.kind == TokenKind::LParen)
        {
            let open_p = cursor;
            let close_p = find_matching_delim(
                &self.parsed.sig_tokens,
                open_p,
                TokenKind::LParen,
                TokenKind::RParen,
            )
            .unwrap_or(open_p + 1);
            cursor = close_p + 1;
            self.format_delimited_seq(
                params,
                open_p,
                close_p,
                DelimStyle::Parens,
                None,
                |fmt, p, ps| fmt.format_parameter(p, ps),
            )
        } else {
            Doc::str("()")
        };

        let (sig_tail, _) = self.format_ret_and_capability(ch[1], NodeIndex::NULL, cursor, slot);
        Doc::concat([Doc::str(kw_str), Doc::text(name_str), params_doc, sig_tail])
    }

    pub(crate) fn format_ret_and_capability(
        &mut self,
        ret_type: NodeIndex,
        capability: NodeIndex,
        mut cursor: usize,
        outer_slot: TokenSlot,
    ) -> (Doc, usize) {
        let mut parts = Vec::new();
        if !ret_type.is_null() {
            let arrow_tok = self
                .parsed
                .find_depth_zero_token(
                    cursor,
                    outer_slot.last,
                    |k| matches!(k, TokenKind::Arrow | TokenKind::Colon),
                    false,
                )
                .unwrap_or(cursor);
            let ret_slot = outer_slot.child_slot_from(self.parsed, arrow_tok + 1, ret_type);
            let ret_doc = self.format_expr(ret_type, ret_slot);
            parts.push(Doc::str(" -> "));
            parts.push(ret_doc);
            cursor = ret_slot.last + 1;
        }
        if !capability.is_null() {
            let handles_tok = self
                .parsed
                .find_depth_zero_token(
                    cursor,
                    outer_slot.last,
                    |k| k == TokenKind::KwHandles,
                    false,
                )
                .unwrap_or(cursor);
            let cap_slot = outer_slot.child_slot_from(self.parsed, handles_tok + 1, capability);
            let cap_doc = self.format_expr(capability, cap_slot);
            parts.push(Doc::str(" handles "));
            parts.push(cap_doc);
            cursor = cap_slot.last + 1;
        }
        (Doc::concat(parts), cursor)
    }

    /// Format `= expr` or `{ ... }` or braceless statement body of `fn` / `derive fn` / `handles(...)()`.
    fn format_callable_body(
        &mut self,
        body: NodeIndex,
        cursor: usize,
        outer_slot: TokenSlot,
    ) -> (Doc, bool) {
        if body.is_null() {
            return (Doc::str(" {}"), true);
        }
        if self.parsed.ast.node(body).kind == NodeKind::Block {
            // Check whether `= { ... }` was somehow written (in Nessa `{ ... }` after `=` is `Object`,
            // so `NodeKind::Block` never has `=`).
            let prev_end = self
                .parsed
                .sig_tokens
                .get(cursor.saturating_sub(1))
                .map_or(0, |t| t.to as usize);
            let block_slot = TokenSlot::new(cursor, outer_slot.last.max(cursor));
            let block_start = self
                .parsed
                .sig_tokens
                .get(block_slot.first)
                .map_or(prev_end, |t| t.from as usize);
            let mid_comments = self.comments.take_inline_between(
                prev_end,
                block_start,
                self.indent_spaces,
                &self.cfg(),
            );
            let b_doc = self.format_block(body, block_slot, None, ContainerKind::ValueBlock);
            if matches!(mid_comments, Doc::Nil) {
                return (Doc::concat([Doc::space(), b_doc]), true);
            }
            return (Doc::concat([mid_comments, b_doc]), true);
        }

        // Check if `=` was used (`fn f(x: i32) -> i32 = x * 2`).
        let has_eq = self
            .parsed
            .sig_tokens
            .get(cursor)
            .is_some_and(|t| t.kind == TokenKind::Eq);

        if has_eq {
            let eq_tok = cursor;
            let eq_end = self.parsed.sig_tokens[eq_tok].to as usize;
            let body_slot = outer_slot.child_slot_from(self.parsed, eq_tok + 1, body);
            let body_lo = self
                .parsed
                .sig_tokens
                .get(body_slot.first)
                .map_or(eq_end, |t| t.from as usize);
            let tab = self.tab_spaces();

            // Check if comments appear between `=` and `body`.
            if self.has_comments_in_range(eq_end, body_lo) {
                self.indent_spaces += tab;
                let (lead, _, _) = self.comments.take_leading_before(
                    eq_end,
                    body_lo,
                    self.indent_spaces,
                    self.brace_depth,
                    false,
                    &mut self.cfg_stack,
                );
                let (expr_doc, ends_brace) = self.format_expr_with_rbrace(body, body_slot);
                self.indent_spaces -= tab;
                let inner = Doc::concat([Doc::HardLine, lead, expr_doc]);
                return (
                    Doc::concat([Doc::str(" ="), Doc::indent(tab, inner)]),
                    ends_brace,
                );
            }

            let (expr_doc, ends_brace) = self.format_expr_with_rbrace(body, body_slot);
            // Keep `{`/`when`/`if`/call head on the same line after `=` without double-indenting
            // its body; add `tab` indent only if the line breaks right after `=`.
            let doc = Doc::group(Doc::concat([
                Doc::str(" ="),
                Doc::if_break(
                    Doc::indent(tab, Doc::concat([Doc::HardLine, expr_doc.clone()])),
                    Doc::concat([Doc::space(), expr_doc]),
                ),
            ]));
            (doc, ends_brace)
        } else {
            // Braceless statement body (`fn f() return 1`): keep without `=` so AST matches!
            let body_slot = outer_slot.child_slot_from(self.parsed, cursor, body);
            let tab = self.tab_spaces();
            self.indent_spaces += tab;
            let (stmt_doc, ends_brace) = self.format_statement_or_expr(body, body_slot);
            self.indent_spaces -= tab;
            let doc = Doc::group(Doc::concat([Doc::indent(
                tab,
                Doc::concat([Doc::Line, stmt_doc]),
            )]));
            (doc, ends_brace)
        }
    }

    fn format_handles_statement(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        // `HandlesStatement`: [0] effect, [1] ret_type, [2] body, multi = params
        // Forms:
        //   1. `handles(eff)(params) (-> ret)? (= expr | block)`
        //   2. `handles [async] eff_callee(params) (-> ret)? => (block | stmt)`
        let ch = self.parsed.ast.fixed_children(node);
        let params = self.parsed.ast.multi_children(node);
        let second_tok_kind = self
            .parsed
            .sig_tokens
            .get(slot.first + 1)
            .map(|t| t.kind)
            .unwrap_or(TokenKind::Eof);

        if second_tok_kind == TokenKind::LParen {
            // Form 1: `handles(eff)(params) ...`
            let eff_open_p = slot.first + 1;
            let eff_close_p = find_matching_delim(
                &self.parsed.sig_tokens,
                eff_open_p,
                TokenKind::LParen,
                TokenKind::RParen,
            )
            .unwrap_or(eff_open_p + 1);
            let eff_slot = TokenSlot::new(
                eff_open_p + 1,
                eff_close_p.saturating_sub(1).max(eff_open_p + 1),
            )
            .child_slot_from(self.parsed, eff_open_p + 1, ch[0]);
            let eff_doc = self.format_expr(ch[0], eff_slot);

            let param_open_p = eff_close_p + 1;
            let param_close_p = find_matching_delim(
                &self.parsed.sig_tokens,
                param_open_p,
                TokenKind::LParen,
                TokenKind::RParen,
            )
            .unwrap_or(param_open_p + 1);
            let params_doc = self.format_delimited_seq(
                params,
                param_open_p,
                param_close_p,
                DelimStyle::Parens,
                None,
                |fmt, p, ps| fmt.format_parameter(p, ps),
            );

            let (sig_tail, cursor) =
                self.format_ret_and_capability(ch[1], NodeIndex::NULL, param_close_p + 1, slot);
            let (body_doc, ends_brace) = self.format_callable_body(ch[2], cursor, slot);
            (
                Doc::concat([
                    Doc::str("handles("),
                    eff_doc,
                    Doc::str(")"),
                    params_doc,
                    sig_tail,
                    body_doc,
                ]),
                ends_brace,
            )
        } else {
            // Form 2: `handles [async] eff_callee(params) (-> ret)? => body`
            let is_async = second_tok_kind == TokenKind::KwAsync;
            let callee_start = if is_async {
                slot.first + 2
            } else {
                slot.first + 1
            };
            let callee_slot = slot.child_slot_from(self.parsed, callee_start, ch[0]);
            let callee_doc = self.format_expr(ch[0], callee_slot);

            let param_open_p = self
                .parsed
                .find_depth_zero_token(
                    callee_slot.last + 1,
                    slot.last,
                    |k| k == TokenKind::LParen,
                    false,
                )
                .unwrap_or(callee_slot.last + 1);
            let param_close_p = find_matching_delim(
                &self.parsed.sig_tokens,
                param_open_p,
                TokenKind::LParen,
                TokenKind::RParen,
            )
            .unwrap_or(param_open_p + 1);
            let params_doc = self.format_delimited_seq(
                params,
                param_open_p,
                param_close_p,
                DelimStyle::Parens,
                None,
                |fmt, p, ps| fmt.format_parameter(p, ps),
            );

            let (sig_tail, cursor) =
                self.format_ret_and_capability(ch[1], NodeIndex::NULL, param_close_p + 1, slot);
            let arrow_tok = self
                .parsed
                .find_depth_zero_token(cursor, slot.last, |k| k == TokenKind::FatArrow, false)
                .unwrap_or(cursor);
            let (rhs_doc, ends_brace) = self.format_arm_rhs(ch[2], arrow_tok + 1, slot);

            let kw = if is_async {
                "handles async "
            } else {
                "handles "
            };
            (
                Doc::concat([
                    Doc::str(kw),
                    callee_doc,
                    params_doc,
                    sig_tail,
                    Doc::str(" =>"),
                    rhs_doc,
                ]),
                ends_brace,
            )
        }
    }

    fn format_let_handler(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        // `LetHandler`: [0] effect_node (single expr or synthetic `ListOf`), [1] pat, [2] ty, [3] val
        let ch = self.parsed.ast.fixed_children(node);
        let eff_node = ch[0];
        let mut cursor = slot.first;
        let mut handles_parts = Vec::new();

        if self.parsed.ast.node(eff_node).kind == NodeKind::ListOf
            && self.parsed.is_synthetic_list_of(eff_node, cursor)
        {
            let effs = self.parsed.ast.multi_children(eff_node);
            for (idx, &eff) in effs.iter().enumerate() {
                let open_p = self
                    .parsed
                    .find_depth_zero_token(cursor, slot.last, |k| k == TokenKind::LParen, false)
                    .unwrap_or(cursor + 1);
                let close_p = find_matching_delim(
                    &self.parsed.sig_tokens,
                    open_p,
                    TokenKind::LParen,
                    TokenKind::RParen,
                )
                .unwrap_or(open_p + 1);
                let eff_slot =
                    TokenSlot::new(open_p + 1, close_p.saturating_sub(1).max(open_p + 1))
                        .child_slot_from(self.parsed, open_p + 1, eff);
                let eff_doc = self.format_expr(eff, eff_slot);
                if idx > 0 {
                    handles_parts.push(Doc::Line);
                }
                handles_parts.push(Doc::concat([Doc::str("handles("), eff_doc, Doc::str(")")]));
                cursor = close_p + 1;
            }
        } else {
            let open_p = self
                .parsed
                .find_depth_zero_token(cursor, slot.last, |k| k == TokenKind::LParen, false)
                .unwrap_or(cursor + 1);
            let close_p = find_matching_delim(
                &self.parsed.sig_tokens,
                open_p,
                TokenKind::LParen,
                TokenKind::RParen,
            )
            .unwrap_or(open_p + 1);
            let eff_slot = TokenSlot::new(open_p + 1, close_p.saturating_sub(1).max(open_p + 1))
                .child_slot_from(self.parsed, open_p + 1, eff_node);
            let eff_doc = self.format_expr(eff_node, eff_slot);
            handles_parts.push(Doc::concat([Doc::str("handles("), eff_doc, Doc::str(")")]));
            cursor = close_p + 1;
        }

        let let_tok = self
            .parsed
            .find_depth_zero_token(cursor, slot.last, |k| k == TokenKind::KwLet, false)
            .unwrap_or(cursor);
        let pat_slot = slot.child_slot_from(self.parsed, let_tok + 1, ch[1]);
        let pat_doc = self.format_pattern(ch[1], pat_slot);
        cursor = pat_slot.last + 1;

        let ty_part = if ch[2].is_null() {
            Doc::Nil
        } else {
            let colon_tok = self
                .parsed
                .find_depth_zero_token(cursor, slot.last, |k| k == TokenKind::Colon, false)
                .unwrap_or(cursor);
            let ty_slot = slot.child_slot_from(self.parsed, colon_tok + 1, ch[2]);
            cursor = ty_slot.last + 1;
            let ty_doc = self.format_expr(ch[2], ty_slot);
            Doc::concat([Doc::str(": "), ty_doc])
        };

        let eq_tok = self
            .parsed
            .find_depth_zero_token(cursor, slot.last, |k| k == TokenKind::Eq, false)
            .unwrap_or(cursor);
        let val_slot = slot.child_slot_from(self.parsed, eq_tok + 1, ch[3]);
        let (val_doc, ends_brace) = self.format_expr_with_rbrace(ch[3], val_slot);

        let doc = Doc::group(Doc::concat([
            Doc::concat(handles_parts),
            Doc::Line,
            Doc::str("let "),
            pat_doc,
            ty_part,
            Doc::str(" = "),
            val_doc,
        ]));
        (doc, ends_brace)
    }

    fn format_global_decl(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        // `GlobalDecl`: [0] inner = `VarDecl(name, ty, value, NULL)`
        let inner = self.parsed.ast.fixed_children(node)[0];
        let ch = self.parsed.ast.fixed_children(inner);
        let name_str = self.format_ident(ch[0], slot.first + 1);
        let colon_tok = self
            .parsed
            .find_depth_zero_token(slot.first + 2, slot.last, |k| k == TokenKind::Colon, false)
            .unwrap_or(slot.first + 2);
        let ty_slot = slot.child_slot_from(self.parsed, colon_tok + 1, ch[1]);
        let ty_doc = self.format_expr(ch[1], ty_slot);
        let eq_tok = self
            .parsed
            .find_depth_zero_token(ty_slot.last + 1, slot.last, |k| k == TokenKind::Eq, false)
            .unwrap_or(ty_slot.last + 1);
        let val_slot = slot.child_slot_from(self.parsed, eq_tok + 1, ch[2]);
        let (val_doc, ends_brace) = self.format_expr_with_rbrace(ch[2], val_slot);
        let tab = self.tab_spaces();
        let doc = Doc::group(Doc::concat([
            Doc::str("global "),
            Doc::text(name_str),
            Doc::str(": "),
            ty_doc,
            Doc::str(" ="),
            Doc::indent(tab, Doc::concat([Doc::Line, val_doc])),
        ]));
        (doc, ends_brace)
    }

    fn format_assoc_decl(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        let inner = self.parsed.ast.fixed_children(node)[0];
        if inner.is_null() {
            return (Doc::str("assoc"), false);
        }
        if self.parsed.ast.node(inner).kind == NodeKind::AssocBinding {
            let ch = self.parsed.ast.fixed_children(inner);
            let name_str = self.format_ident(ch[0], slot.first + 1);
            let colon_tok = self
                .parsed
                .find_depth_zero_token(slot.first + 2, slot.last, |k| k == TokenKind::Colon, false)
                .unwrap_or(slot.first + 2);
            let ty_slot = slot.child_slot_from(self.parsed, colon_tok + 1, ch[1]);
            let ty_doc = self.format_expr(ch[1], ty_slot);
            let eq_tok = self
                .parsed
                .find_depth_zero_token(ty_slot.last + 1, slot.last, |k| k == TokenKind::Eq, false)
                .unwrap_or(ty_slot.last + 1);
            let val_slot = slot.child_slot_from(self.parsed, eq_tok + 1, ch[2]);
            let (val_doc, ends_brace) = self.format_expr_with_rbrace(ch[2], val_slot);
            let tab = self.tab_spaces();
            let doc = Doc::group(Doc::concat([
                Doc::str("assoc "),
                Doc::text(name_str),
                Doc::str(": "),
                ty_doc,
                Doc::str(" ="),
                Doc::indent(tab, Doc::concat([Doc::Line, val_doc])),
            ]));
            (doc, ends_brace)
        } else {
            let inner_slot = slot.child_slot_from(self.parsed, slot.first + 1, inner);
            let (inner_doc, ends_brace) = self.format_container_member(
                inner,
                inner_slot,
                false,
                ContainerKind::TraitOrImplBody,
            );
            (Doc::concat([Doc::str("assoc "), inner_doc]), ends_brace)
        }
    }

    fn format_adt_def(&mut self, node: NodeIndex, slot: TokenSlot, kw: &'static str) -> Doc {
        let ch = self.parsed.ast.fixed_children(node);
        let members = self.parsed.ast.multi_children(node);
        let (name_f, name_l) = self.parsed.exact_token_bounds(ch[0], slot.first + 1);
        let name_str = self.format_ident(ch[0], name_f);
        let body_doc =
            self.format_braced_member_body(members, name_l + 1, slot.last, ContainerKind::AdtBody);
        Doc::concat([
            Doc::str(kw),
            Doc::space(),
            Doc::text(name_str),
            Doc::space(),
            body_doc,
        ])
    }

    fn format_trait_def(&mut self, node: NodeIndex, slot: TokenSlot) -> Doc {
        // `TraitDef`: [0] name, [1] parents (`NULL` or synthetic `ListOf`), multi = members
        let ch = self.parsed.ast.fixed_children(node);
        let members = self.parsed.ast.multi_children(node);
        let (name_f, name_l) = self.parsed.exact_token_bounds(ch[0], slot.first + 1);
        let name_str = self.format_ident(ch[0], name_f);
        let mut cursor = name_l + 1;

        let parents_doc = if !ch[1].is_null() {
            let parent_exprs = self.parsed.ast.multi_children(ch[1]);
            let open_p = self
                .parsed
                .find_depth_zero_token(cursor, slot.last, |k| k == TokenKind::LParen, false)
                .unwrap_or(cursor);
            let close_p = find_matching_delim(
                &self.parsed.sig_tokens,
                open_p,
                TokenKind::LParen,
                TokenKind::RParen,
            )
            .unwrap_or(open_p + 1);
            cursor = close_p + 1;
            self.format_delimited_seq(
                parent_exprs,
                open_p,
                close_p,
                DelimStyle::Parens,
                None,
                |fmt, pe, ps| fmt.format_expr(pe, ps),
            )
        } else {
            Doc::Nil
        };

        let body_doc = self.format_braced_member_body(
            members,
            cursor,
            slot.last,
            ContainerKind::TraitOrImplBody,
        );
        Doc::concat([
            Doc::str("trait "),
            Doc::text(name_str),
            parents_doc,
            Doc::space(),
            body_doc,
        ])
    }

    fn format_inherent_impl_or_extend(
        &mut self,
        node: NodeIndex,
        slot: TokenSlot,
        kw: &'static str,
    ) -> Doc {
        let ch = self.parsed.ast.fixed_children(node);
        let members = self.parsed.ast.multi_children(node);
        let ty_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
        let ty_doc = self.format_expr(ch[0], ty_slot);
        let body_doc = self.format_braced_member_body(
            members,
            ty_slot.last + 1,
            slot.last,
            ContainerKind::TraitOrImplBody,
        );
        Doc::concat([Doc::str(kw), Doc::space(), ty_doc, Doc::space(), body_doc])
    }

    fn format_trait_impl_or_extend(
        &mut self,
        node: NodeIndex,
        slot: TokenSlot,
        kw: &'static str,
    ) -> Doc {
        let ch = self.parsed.ast.fixed_children(node);
        let members = self.parsed.ast.multi_children(node);
        let tr_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
        let tr_doc = self.format_expr(ch[0], tr_slot);
        let for_tok = self
            .parsed
            .find_depth_zero_token(
                tr_slot.last + 1,
                slot.last,
                |k| k == TokenKind::KwFor,
                false,
            )
            .unwrap_or(tr_slot.last + 1);
        let ty_slot = slot.child_slot_from(self.parsed, for_tok + 1, ch[1]);
        let ty_doc = self.format_expr(ch[1], ty_slot);
        let body_doc = self.format_braced_member_body(
            members,
            ty_slot.last + 1,
            slot.last,
            ContainerKind::TraitOrImplBody,
        );
        Doc::concat([
            Doc::str(kw),
            Doc::space(),
            tr_doc,
            Doc::str(" for "),
            ty_doc,
            Doc::space(),
            body_doc,
        ])
    }

    fn format_derive_def(&mut self, node: NodeIndex, slot: TokenSlot) -> Doc {
        // `DeriveDef`: [0] type_expr, multi = trait_exprs (`derive T1, T2 for Type`)
        let ch = self.parsed.ast.fixed_children(node);
        let traits = self.parsed.ast.multi_children(node);
        let mut cursor = slot.first + 1;
        let mut trait_docs = Vec::with_capacity(traits.len());

        for &tr in traits {
            let tr_slot = slot.child_slot_from(self.parsed, cursor, tr);
            trait_docs.push(self.format_expr(tr, tr_slot));
            cursor = tr_slot.last + 1;
            if cursor <= slot.last && self.parsed.sig_tokens[cursor].kind == TokenKind::Comma {
                cursor += 1;
            }
        }

        let for_tok = self
            .parsed
            .find_depth_zero_token(cursor, slot.last, |k| k == TokenKind::KwFor, false)
            .unwrap_or(cursor);
        let ty_slot = slot.child_slot_from(self.parsed, for_tok + 1, ch[0]);
        let ty_doc = self.format_expr(ch[0], ty_slot);

        let traits_joined = Doc::join(trait_docs, Doc::str(", "));
        Doc::concat([
            Doc::str("derive "),
            traits_joined,
            Doc::str(" for "),
            ty_doc,
        ])
    }

    fn format_typealias_or_newtype(
        &mut self,
        node: NodeIndex,
        slot: TokenSlot,
        kw: &'static str,
    ) -> (Doc, bool) {
        let ch = self.parsed.ast.fixed_children(node);
        let (name_f, name_l) = self.parsed.exact_token_bounds(ch[0], slot.first + 1);
        let name_str = self.format_ident(ch[0], name_f);
        let eq_tok = self
            .parsed
            .find_depth_zero_token(name_l + 1, slot.last, |k| k == TokenKind::Eq, false)
            .unwrap_or(name_l + 1);
        let ty_slot = slot.child_slot_from(self.parsed, eq_tok + 1, ch[1]);
        let (ty_doc, ends_brace) = self.format_expr_with_rbrace(ch[1], ty_slot);
        let tab = self.tab_spaces();
        let doc = Doc::group(Doc::concat([
            Doc::str(kw),
            Doc::space(),
            Doc::text(name_str),
            Doc::str(" ="),
            Doc::indent(tab, Doc::concat([Doc::Line, ty_doc])),
        ]));
        (doc, ends_brace)
    }

    fn format_module_def(&mut self, node: NodeIndex, slot: TokenSlot) -> (Doc, bool) {
        let ch = self.parsed.ast.fixed_children(node);
        let members = self.parsed.ast.multi_children(node);
        let (name_f, name_l) = self.parsed.exact_token_bounds(ch[0], slot.first + 1);
        let name_str = self.format_ident(ch[0], name_f);

        // Distinguish file-backed `mod foo` from inline `mod foo { ... }` / `mod foo:\n ...`!
        let has_inline_body = !members.is_empty()
            || (name_l < slot.last
                && matches!(
                    self.parsed.sig_tokens[name_l + 1].kind,
                    TokenKind::LBrace | TokenKind::Colon
                ));

        if !has_inline_body {
            return (Doc::concat([Doc::str("mod "), Doc::text(name_str)]), false);
        }

        let body_doc = self.format_braced_member_body(
            members,
            name_l + 1,
            slot.last,
            ContainerKind::ModuleBody,
        );
        (
            Doc::concat([
                Doc::str("mod "),
                Doc::text(name_str),
                Doc::space(),
                body_doc,
            ]),
            true,
        )
    }

    /// Format a `{ ... }` (or converted indentation `: ...`) member body for `struct`, `enum`,
    /// `trait`, `impl`, `extend`, `mod`, `when`, `match`, `# { ... }`, `! { ... }`.
    pub fn format_braced_member_body(
        &mut self,
        members: &[NodeIndex],
        body_start_tok: usize,
        body_last_tok: usize,
        container_kind: ContainerKind,
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

        if members.is_empty() && !has_comments {
            return Doc::str("{}");
        }

        let tab = self.tab_spaces();
        let saved_indent = self.indent_spaces;
        self.indent_spaces += tab;
        self.brace_depth += 1;

        let inner_doc = self.format_container_elements(
            members,
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
    // `use` statements and 3-group import sorting
    // -----------------------------------------------------------------------

    pub fn format_use_item_with_group_key(
        &mut self,
        node: NodeIndex,
        slot: TokenSlot,
    ) -> (Doc, ImportGroup, String) {
        let (vis_prefix, inner_node, inner_slot) = self.unwrap_visibility_prefix(node, slot);
        let reorder = self.cfg_stack.current().reorder_imports;
        let (u_doc, group, key) = self.format_use_core(inner_node, inner_slot, reorder);
        (Doc::concat([vis_prefix, u_doc]), group, key)
    }

    fn format_use_core(
        &mut self,
        use_node: NodeIndex,
        slot: TokenSlot,
        sort_selections: bool,
    ) -> (Doc, ImportGroup, String) {
        let path_node = self.parsed.ast.fixed_children(use_node)[0];
        let group = self.classify_use_path_group(path_node);
        let path_slot = slot.child_slot_from(self.parsed, slot.first + 1, path_node);
        let (path_doc, sort_key) = self.format_use_path(path_node, path_slot, sort_selections);
        (Doc::concat([Doc::str("use "), path_doc]), group, sort_key)
    }

    pub fn classify_use_path_group(&self, mut path_node: NodeIndex) -> ImportGroup {
        while !path_node.is_null() {
            match self.parsed.ast.node(path_node).kind {
                NodeKind::PathAsBind => {
                    path_node = self.parsed.ast.fixed_children(path_node)[0];
                }
                NodeKind::SuperPath | NodeKind::PackagePath => {
                    return ImportGroup::IntraPackage;
                }
                NodeKind::PathProjection
                | NodeKind::PathProjectionAll
                | NodeKind::PathProjectionMulti => {
                    path_node = self.parsed.ast.fixed_children(path_node)[0];
                }
                NodeKind::Id => {
                    if self.raw_ident_name(path_node) == "std" {
                        return ImportGroup::Std;
                    }
                    return ImportGroup::External;
                }
                _ => return ImportGroup::External,
            }
        }
        ImportGroup::External
    }

    fn format_use_path(
        &mut self,
        path_node: NodeIndex,
        slot: TokenSlot,
        sort_selections: bool,
    ) -> (Doc, String) {
        if path_node.is_null() {
            return (Doc::Nil, String::new());
        }
        let kind = self.parsed.ast.node(path_node).kind;
        let ch = self.parsed.ast.fixed_children(path_node);

        match kind {
            NodeKind::Id => {
                let s = self.format_ident(path_node, slot.first);
                (Doc::text(s.clone()), s)
            }
            NodeKind::SuperPath => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
                let (inner_doc, inner_key) =
                    self.format_use_path(ch[0], inner_slot, sort_selections);
                (
                    Doc::concat([Doc::str("."), inner_doc]),
                    format!(".{inner_key}"),
                )
            }
            NodeKind::PackagePath => {
                let inner_slot = slot.child_slot_from(self.parsed, slot.first + 1, ch[0]);
                let (inner_doc, inner_key) =
                    self.format_use_path(ch[0], inner_slot, sort_selections);
                (
                    Doc::concat([Doc::str("@"), inner_doc]),
                    format!("@{inner_key}"),
                )
            }
            NodeKind::PathProjection => {
                let lhs_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let dot_tok = self
                    .parsed
                    .find_depth_zero_token(
                        lhs_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::Dot,
                        false,
                    )
                    .unwrap_or(lhs_slot.last + 1);
                let (lhs_doc, lhs_key) = self.format_use_path(ch[0], lhs_slot, sort_selections);
                let rhs_str = self.format_ident(ch[1], dot_tok + 1);
                (
                    Doc::concat([lhs_doc, Doc::str("."), Doc::text(rhs_str.clone())]),
                    format!("{lhs_key}.{rhs_str}"),
                )
            }
            NodeKind::PathProjectionAll => {
                let lhs_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let (lhs_doc, lhs_key) = self.format_use_path(ch[0], lhs_slot, sort_selections);
                (
                    Doc::concat([lhs_doc, Doc::str(".*")]),
                    format!("{lhs_key}.*"),
                )
            }
            NodeKind::PathAsBind => {
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
                let (lhs_doc, lhs_key) = self.format_use_path(ch[0], lhs_slot, sort_selections);
                let alias_str = self.format_ident(ch[1], as_tok + 1);
                (
                    Doc::concat([lhs_doc, Doc::str(" as "), Doc::text(alias_str.clone())]),
                    format!("{lhs_key} as {alias_str}"),
                )
            }
            NodeKind::PathProjectionMulti => {
                let lhs_slot = slot.child_slot_from(self.parsed, slot.first, ch[0]);
                let (lhs_doc, lhs_key) = self.format_use_path(ch[0], lhs_slot, sort_selections);
                let open_brace = self
                    .parsed
                    .find_depth_zero_token(
                        lhs_slot.last + 1,
                        slot.last,
                        |k| k == TokenKind::LBrace,
                        false,
                    )
                    .unwrap_or(lhs_slot.last + 1);
                let close_brace = find_matching_delim(
                    &self.parsed.sig_tokens,
                    open_brace,
                    TokenKind::LBrace,
                    TokenKind::RBrace,
                )
                .unwrap_or(slot.last);
                let open_end = self
                    .parsed
                    .sig_tokens
                    .get(open_brace)
                    .map_or(0, |t| t.to as usize);
                let close_start = self
                    .parsed
                    .sig_tokens
                    .get(close_brace)
                    .map_or(self.parsed.original_src.len(), |t| t.from as usize);

                let items = self.parsed.ast.multi_children(path_node);
                let has_inner_comments = self.has_comments_in_range(open_end, close_start);

                if sort_selections && !has_inner_comments && !items.is_empty() {
                    let mut rendered_subs = Vec::with_capacity(items.len());
                    let mut cur_tok = open_brace + 1;
                    let inner_bound =
                        TokenSlot::new(cur_tok, close_brace.saturating_sub(1).max(cur_tok));
                    for &sub in items {
                        let sub_slot = inner_bound.child_slot_from(self.parsed, cur_tok, sub);
                        let (sub_doc, sub_key) = self.format_use_path(sub, sub_slot, true);
                        rendered_subs.push((sub_key, sub_doc));
                        cur_tok = sub_slot.last + 1;
                        if cur_tok < close_brace
                            && self.parsed.sig_tokens[cur_tok].kind == TokenKind::Comma
                        {
                            cur_tok += 1;
                        }
                    }
                    rendered_subs.sort_by(|a, b| compare_import_keys(&a.0, &b.0));
                    let keys_joined = rendered_subs
                        .iter()
                        .map(|(k, _)| k.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");

                    let tab = self.tab_spaces();
                    let mut seq_parts = Vec::with_capacity(rendered_subs.len() * 2);
                    let sub_len = rendered_subs.len();
                    for (idx, (_, d)) in rendered_subs.into_iter().enumerate() {
                        if idx > 0 {
                            seq_parts.push(Doc::Line);
                        }
                        seq_parts.push(d);
                        if idx + 1 < sub_len {
                            seq_parts.push(Doc::str(","));
                        } else {
                            seq_parts.push(Doc::trailing_comma());
                        }
                    }
                    let multi_doc = Doc::group(Doc::concat([
                        Doc::str("{"),
                        Doc::indent(tab, Doc::concat([Doc::SoftLine, Doc::concat(seq_parts)])),
                        Doc::SoftLine,
                        Doc::str("}"),
                    ]));
                    (
                        Doc::concat([lhs_doc, Doc::str("."), multi_doc]),
                        format!("{lhs_key}.{{{keys_joined}}}"),
                    )
                } else {
                    let mut sub_keys = Vec::with_capacity(items.len());
                    let multi_doc = self.format_delimited_seq(
                        items,
                        open_brace,
                        close_brace,
                        DelimStyle::CompactBraces,
                        None,
                        |fmt, sub, sub_slot| {
                            let (d, k) = fmt.format_use_path(sub, sub_slot, sort_selections);
                            sub_keys.push(k);
                            d
                        },
                    );
                    let _ = strip_last_hardline(Doc::Nil);
                    (
                        Doc::concat([lhs_doc, Doc::str("."), multi_doc]),
                        format!("{lhs_key}.{{{}}}", sub_keys.join(", ")),
                    )
                }
            }
            _ => {
                let s = self.literal_slice(path_node, slot.first).to_string();
                (Doc::text(s.clone()), s)
            }
        }
    }
}
