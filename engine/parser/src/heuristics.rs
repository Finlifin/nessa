use ast::{NodeIndex, NodeKind};
use lexer::token::TokenKind;

use crate::basic::{self, Rule, try_multi_with_bracket, try_pattern_arm};
use crate::definition;
use crate::error::{ParseError, ParseErrorKind, ParseResult};
use crate::expr;
use crate::parser::{NULL, Parser};
use crate::statement;

extern crate str_interner;

/// Detect and skip C/Rust/JS-style `//` line comments and `/* ... */` block comments.
/// Returns `true` if one or more foreign comments were diagnosed and skipped.
pub(crate) fn try_skip_foreign_comments(p: &mut Parser) -> bool {
    let mut skipped_any = false;

    loop {
        let idx = p.next_non_newline_index();
        let tokens = p.raw_tokens();
        if idx + 1 >= tokens.len() {
            break;
        }
        let t0 = tokens[idx];
        let t1 = tokens[idx + 1];
        if t0.kind != TokenKind::Slash || t0.to != t1.from {
            break;
        }

        let prev_ended_stmt = idx > 0 && tokens[idx - 1].kind.can_end_statement();

        if t1.kind == TokenKind::Slash {
            let mut end_idx = idx + 2;
            let mut hi = t1.to;
            while end_idx < tokens.len()
                && !matches!(tokens[end_idx].kind, TokenKind::Newline | TokenKind::Eof)
            {
                hi = tokens[end_idx].to;
                end_idx += 1;
            }
            let span = p.span_from_to(t0.from, hi);
            let marker_span = p.span_from_to(t0.from, t1.to);
            p.push_error(
                ParseError::with_label(
                    ParseErrorKind::InvalidSyntax,
                    span,
                    "C-style `//` line comments are not supported in Nessa; use `--` instead",
                    marker_span,
                    "foreign `//` comment",
                )
                .add_help("replace `//` with `--` for a line comment"),
            );
            p.set_raw_cursor(end_idx);
            if prev_ended_stmt {
                p.mark_statement_end_here();
            }
            skipped_any = true;
            continue;
        }

        if t1.kind == TokenKind::Star {
            let mut scan = idx + 2;
            let mut closed_at = None;
            while scan + 1 < tokens.len() {
                let a = tokens[scan];
                let b = tokens[scan + 1];
                if a.kind == TokenKind::Star && b.kind == TokenKind::Slash && a.to == b.from {
                    closed_at = Some((scan + 2, b.to));
                    break;
                }
                if a.kind == TokenKind::Eof {
                    break;
                }
                scan += 1;
            }
            let marker_span = p.span_from_to(t0.from, t1.to);
            if let Some((after_close, hi)) = closed_at {
                let span = p.span_from_to(t0.from, hi);
                p.push_error(
                    ParseError::with_label(
                        ParseErrorKind::InvalidSyntax,
                        span,
                        "C-style `/* ... */` block comments are not supported in Nessa; use `{- ... -}` instead",
                        marker_span,
                        "foreign `/*` block comment",
                    )
                    .add_help("replace `/* ... */` with `{- ... -}`"),
                );
                p.set_raw_cursor(after_close);
                if prev_ended_stmt {
                    p.mark_statement_end_here();
                }
            } else {
                p.push_error(
                    ParseError::new(
                        ParseErrorKind::InvalidSyntax,
                        marker_span,
                        "unterminated C-style `/*` block comment; Nessa uses `{- ... -}` for block comments",
                    )
                    .add_help("use `{- ... -}` for block comments in Nessa"),
                );
                let eof_idx = tokens
                    .iter()
                    .position(|t| t.kind == TokenKind::Eof)
                    .unwrap_or(tokens.len());
                p.set_raw_cursor(eof_idx);
            }
            skipped_any = true;
            continue;
        }

        break;
    }

    skipped_any
}

/// Recover from single-quoted multi-character strings `'hello'` or empty `''`
/// at prefix expression or pattern position.
pub(crate) fn try_single_quoted_string_recovery(p: &mut Parser) -> ParseResult {
    let idx = p.next_non_newline_index();
    let tokens = p.raw_tokens();
    if idx >= tokens.len() || tokens[idx].kind != TokenKind::Quote {
        return Ok(NULL);
    }

    let open = tokens[idx];
    let mut scan = idx + 1;
    let mut close_idx = None;
    while scan < tokens.len() {
        match tokens[scan].kind {
            TokenKind::Quote => {
                close_idx = Some(scan);
                break;
            }
            TokenKind::Newline | TokenKind::Eof => break,
            _ => scan += 1,
        }
    }

    if let Some(c_idx) = close_idx {
        let close = tokens[c_idx];
        let full_span = p.span_from_to(open.from, close.to);
        let inner = &p.source_str()[open.to as usize..close.from as usize];
        if inner.is_empty() {
            p.push_error(
                ParseError::new(
                    ParseErrorKind::InvalidSyntax,
                    full_span,
                    "empty character literal `''`; use `\"\"` for an empty string",
                )
                .add_help("write `\"\"` for an empty string literal"),
            );
        } else {
            p.push_error(
                ParseError::new(
                    ParseErrorKind::InvalidSyntax,
                    full_span,
                    "single quotes `'...'` are only for single-character `char` literals; use double quotes `\"...\"` for strings",
                )
                .add_help(format!("write `\"{inner}\"` instead")),
            );
        }
        p.set_raw_cursor(c_idx + 1);
        let str_id = str_interner::intern(&format!("\"{inner}\""));
        return Ok(p
            .ast()
            .builder(NodeKind::Str, full_span)
            .set_str_id(str_id)
            .build());
    }

    let span = p.token_span(open);
    p.push_error(
        ParseError::new(
            ParseErrorKind::InvalidSyntax,
            span,
            "unexpected single quote `'`; character literals hold one character (`'a'`), and strings use double quotes `\"...\"`",
        )
        .add_help("use `\"...\"` for string literals"),
    );
    p.set_raw_cursor(idx + 1);
    let str_id = str_interner::intern("\"\"");
    Ok(p.ast()
        .builder(NodeKind::Str, span)
        .set_str_id(str_id)
        .build())
}

/// Detect foreign infix operators (`&&`, `||`, `===`, `!==`) in Pratt expression parsing.
/// When detected and `prec >= min_prec`, emits a targeted diagnostic, consumes the operator
/// tokens, and returns `Some((recovery_node_kind, prec))`.
pub(crate) fn try_infix_operator_heuristic(
    p: &mut Parser,
    min_prec: i32,
) -> Option<(NodeKind, i32)> {
    let idx = p.next_non_newline_index();
    let tokens = p.raw_tokens();
    if idx + 1 >= tokens.len() {
        return None;
    }
    let t0 = tokens[idx];
    let t1 = tokens[idx + 1];
    if t0.to != t1.from {
        return None;
    }

    match (t0.kind, t1.kind) {
        (TokenKind::Ampersand, TokenKind::Ampersand) => {
            let prec = 30;
            if prec < min_prec {
                return None;
            }
            let span = p.span_from_to(t0.from, t1.to);
            p.push_error(
                ParseError::new(
                    ParseErrorKind::InvalidSyntax,
                    span,
                    "logical AND in Nessa is written `and`, not `&&`",
                )
                .add_help("replace `&&` with `and`"),
            );
            p.set_raw_cursor(idx + 2);
            Some((NodeKind::BoolAnd, prec))
        }
        (TokenKind::Pipe, TokenKind::Pipe) => {
            let prec = 20;
            if prec < min_prec {
                return None;
            }
            let span = p.span_from_to(t0.from, t1.to);
            p.push_error(
                ParseError::new(
                    ParseErrorKind::InvalidSyntax,
                    span,
                    "logical OR in Nessa is written `or`, not `||`",
                )
                .add_help("replace `||` with `or`"),
            );
            p.set_raw_cursor(idx + 2);
            Some((NodeKind::BoolOr, prec))
        }
        (TokenKind::EqEq, TokenKind::Eq) => {
            let prec = 40;
            if prec < min_prec {
                return None;
            }
            let span = p.span_from_to(t0.from, t1.to);
            p.push_error(
                ParseError::new(
                    ParseErrorKind::InvalidSyntax,
                    span,
                    "Nessa uses `==` for equality comparison, not `===`",
                )
                .add_help("replace `===` with `==`"),
            );
            p.set_raw_cursor(idx + 2);
            Some((NodeKind::BoolEq, prec))
        }
        (TokenKind::BangEq, TokenKind::Eq) => {
            let prec = 40;
            if prec < min_prec {
                return None;
            }
            let span = p.span_from_to(t0.from, t1.to);
            p.push_error(
                ParseError::new(
                    ParseErrorKind::InvalidSyntax,
                    span,
                    "Nessa uses `!=` for inequality comparison, not `!==`",
                )
                .add_help("replace `!==` with `!=`"),
            );
            p.set_raw_cursor(idx + 2);
            Some((NodeKind::BoolNotEq, prec))
        }
        _ => None,
    }
}

/// Diagnose and skip `<T, U>` angle-bracket generic parameters on definitions.
pub(crate) fn try_angle_bracket_generics_recovery(p: &mut Parser, construct: &str) {
    if p.peek_token().kind != TokenKind::Lt {
        return;
    }
    let open = p.next_token();
    let mut depth: u32 = 1;
    let mut last_to = open.to;
    while depth > 0 {
        let tok = p.peek_token();
        match tok.kind {
            TokenKind::Lt => {
                last_to = p.next_token().to;
                depth += 1;
            }
            TokenKind::Gt => {
                last_to = p.next_token().to;
                depth -= 1;
            }
            TokenKind::LBrace
            | TokenKind::Indent
            | TokenKind::Semi
            | TokenKind::Eof
            | TokenKind::Outdent => break,
            _ => {
                last_to = p.next_token().to;
            }
        }
    }
    let span = p.span_from_to(open.from, last_to);
    p.push_error(
        ParseError::new(
            ParseErrorKind::InvalidSyntax,
            span,
            format!(
                "angle-bracket generic parameters `<...>` are not supported on {construct} in Nessa"
            ),
        )
        .add_help(
            "use `trait` parameter bounds (e.g. `fn f(x: MyTrait)`), type constructors, or `Any` instead of `<T>`",
        ),
    );
}

/// Expect `=>`, or diagnose `->` with an actionable help message and continue cleanly.
pub(crate) fn expect_fat_arrow_in_arm(
    p: &mut Parser,
    arm_kind: &str,
) -> Result<(), ParseErrorKind> {
    if p.eat_token(TokenKind::FatArrow) {
        return Ok(());
    }
    if p.peek_token().kind == TokenKind::Arrow {
        let arrow = p.next_token();
        let span = p.token_span(arrow);
        p.push_error(
            ParseError::new(
                ParseErrorKind::InvalidSyntax,
                span,
                format!("{arm_kind} uses `=>`, not `->`"),
            )
            .add_help("replace `->` with `=>`"),
        );
        return Ok(());
    }
    p.expect_token(TokenKind::FatArrow)
}

/// Diagnose `let mut x` / `var mut x` / `const mut x` and consume `mut` for recovery.
pub(crate) fn try_eat_binding_mut(p: &mut Parser, decl_kind: NodeKind) {
    if p.peek_nth(0).kind == TokenKind::Id
        && p.next_token_text() == "mut"
        && matches!(
            p.peek_nth(1).kind,
            TokenKind::Id
                | TokenKind::ArbitraryId
                | TokenKind::Underscore
                | TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::LBrace
        )
    {
        let mut_tok = p.next_token();
        let span = p.token_span(mut_tok);
        let (msg, help) = if decl_kind == NodeKind::ConstDecl {
            (
                "`const` bindings are immutable in Nessa and cannot be marked `mut`",
                "use `let` or `var` for a mutable binding, or remove `mut`",
            )
        } else {
            (
                "Nessa does not use `mut`; `let` and `var` bindings are already mutable (`const` is immutable)",
                "remove `mut` (write `let x = ...` or `var x = ...`)",
            )
        };
        p.push_error(ParseError::new(ParseErrorKind::InvalidSyntax, span, msg).add_help(help));
    }
}

/// Diagnose `if x = 1` / `while x = 1` assignment inside a condition.
pub(crate) fn try_condition_assign_recovery(
    p: &mut Parser,
    cond: NodeIndex,
    kw: &str,
) -> Result<NodeIndex, ParseErrorKind> {
    if cond.is_null() || p.peek_token().kind != TokenKind::Eq {
        return Ok(cond);
    }
    let eq_tok = p.next_token();
    let eq_span = p.token_span(eq_tok);
    p.push_error(
        ParseError::new(
            ParseErrorKind::InvalidSyntax,
            eq_span,
            format!("assignment `=` is not allowed in `{kw}` condition"),
        )
        .add_help(
            "use `==` to compare values, or `let pat = expr else { ... }` for conditional binding",
        ),
    );
    let rhs = expr::try_expr_without_extended_call(p)?;
    let full_span = p.ast().node(cond).span.to(p.current_span());
    Ok(p.ast()
        .builder(NodeKind::BoolEq, full_span)
        .add_child(cond)
        .add_child(rhs)
        .build())
}

/// Diagnose prefix `match expr { ... }` misuse and recover as `PostMatch`.
pub(crate) fn try_prefix_match_recovery(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if p.peek_token().kind != TokenKind::KwMatch {
        return Ok(NULL);
    }
    let match_tok = p.next_token();
    let kw_span = p.token_span(match_tok);
    let scrutinee = expr::try_expr_without_extended_call(p)?;
    let scrutinee_text = if !scrutinee.is_null() {
        let sp = p.ast().node(scrutinee).span;
        let lo = sp
            .lo()
            .0
            .saturating_sub(p.raw_tokens().first().map_or(0, |_| 0));
        let _ = lo;
        None::<String>
    } else {
        None
    };
    let _ = scrutinee_text;
    p.push_error(
        ParseError::new(
            ParseErrorKind::InvalidSyntax,
            kw_span,
            "`match` in Nessa is a postfix operator, not a prefix keyword",
        )
        .add_help("write `<expr> match { ... }` instead of `match <expr> { ... }`"),
    );

    if matches!(
        p.peek_token().kind,
        TokenKind::LBrace | TokenKind::Indent | TokenKind::Colon
    ) {
        let arms = basic::try_required_block(
            p,
            "`match`",
            &[Rule::comma_or_semicolon("match arm", try_pattern_arm)],
        )?;
        let span = p.current_span();
        return Ok(p
            .ast()
            .builder(NodeKind::PostMatch, span)
            .add_child(scrutinee)
            .add_multi_children(&arms)
            .build());
    }
    Ok(scrutinee)
}

/// Diagnose foreign statement/definition constructs at statement start (`async fn`,
/// `def`, `function`, `elif`/`elsif`, `switch`, `try`, `throw`/`raise`).
pub(crate) fn try_foreign_statement_or_def_heuristic(p: &mut Parser) -> ParseResult {
    let tok = p.peek_token();
    match tok.kind {
        TokenKind::KwAsync if p.peek(&[TokenKind::KwAsync, TokenKind::KwFn]) => {
            let async_tok = p.next_token();
            let span = p.token_span(async_tok);
            p.push_error(
                ParseError::new(
                    ParseErrorKind::InvalidSyntax,
                    span,
                    "functions in Nessa are not marked `async`; concurrency uses colorless `fn` with `async effect` declarations",
                )
                .add_help("remove `async` and write `fn ...` (use `async effect name(...)` to declare an async effect)"),
            );
            definition::try_fn_def(p)
        }
        TokenKind::KwDef => {
            let def_tok = p.next_token();
            let span = p.token_span(def_tok);
            if p.peek_token().kind == TokenKind::KwFn {
                p.push_error(
                    ParseError::new(
                        ParseErrorKind::InvalidSyntax,
                        span,
                        "`def fn` is only used for required method signatures inside a `trait` body",
                    )
                    .add_help("remove `def` and write `fn ...`"),
                );
                return definition::try_fn_def(p);
            }
            p.push_error(
                ParseError::new(
                    ParseErrorKind::InvalidSyntax,
                    span,
                    "functions in Nessa are declared with `fn`, not `def`",
                )
                .add_help("replace `def` with `fn`"),
            );
            parse_recovered_fn_after_keyword(p)
        }
        TokenKind::Id => {
            let text = p.next_token_text();
            match text {
                "function"
                    if basic::is_ident_token(p.peek_nth(1).kind)
                        && matches!(p.peek_nth(2).kind, TokenKind::LParen | TokenKind::Lt) =>
                {
                    let kw = p.next_token();
                    let span = p.token_span(kw);
                    p.push_error(
                        ParseError::new(
                            ParseErrorKind::InvalidSyntax,
                            span,
                            "functions in Nessa are declared with `fn`, not `function`",
                        )
                        .add_help("replace `function` with `fn`"),
                    );
                    parse_recovered_fn_after_keyword(p)
                }
                "elif" | "elsif" if looks_like_elif_branch(p) => {
                    let kw_text = text.to_owned();
                    let kw = p.next_token();
                    let span = p.token_span(kw);
                    p.push_error(
                        ParseError::new(
                            ParseErrorKind::InvalidSyntax,
                            span,
                            format!("Nessa uses `else if`, not `{kw_text}`"),
                        )
                        .add_help(format!("replace `{kw_text}` with `else if`")),
                    );
                    statement::parse_if_after_keyword(p)
                }
                "switch" if looks_like_switch_stmt(p) => {
                    let kw = p.next_token();
                    let span = p.token_span(kw);
                    p.push_error(
                        ParseError::new(
                            ParseErrorKind::InvalidSyntax,
                            span,
                            "Nessa uses postfix `match`, not `switch`",
                        )
                        .add_help("write `<expr> match { pat => body, ... }`"),
                    );
                    let scrutinee = expr::try_expr_without_extended_call(p)?;
                    if matches!(
                        p.peek_token().kind,
                        TokenKind::LBrace | TokenKind::Indent | TokenKind::Colon
                    ) {
                        let _ = statement::try_block(p);
                    }
                    Ok(scrutinee)
                }
                "try"
                    if matches!(
                        p.peek_nth(1).kind,
                        TokenKind::LBrace | TokenKind::Indent | TokenKind::Colon
                    ) =>
                {
                    let _g = p.enter();
                    let kw = p.next_token();
                    let span = p.token_span(kw);
                    p.push_error(
                        ParseError::new(
                            ParseErrorKind::InvalidSyntax,
                            span,
                            "Nessa does not use `try { ... } catch`; errors are typed values (`!Err T`)",
                        )
                        .add_help(
                            "propagate errors with `call()!` or handle them with `call() ! { error e => ..., v! => ... }`",
                        ),
                    );
                    let try_block = statement::try_block(p)?;
                    if p.eat_token(TokenKind::KwCatch) {
                        if p.peek_token().kind == TokenKind::LParen {
                            let _ = try_multi_with_bracket(
                                p,
                                &[Rule::comma("catch parameter", basic::try_parameter)],
                                TokenKind::LParen,
                                TokenKind::RParen,
                            );
                        } else {
                            let _ = basic::try_id(p);
                        }
                        if matches!(
                            p.peek_token().kind,
                            TokenKind::LBrace | TokenKind::Indent | TokenKind::Colon
                        ) {
                            let _ = statement::try_block(p);
                        }
                    }
                    Ok(try_block)
                }
                "throw" | "raise" if looks_like_throw_stmt(p) => {
                    let kw_text = text.to_owned();
                    let _g = p.enter();
                    let kw = p.next_token();
                    let span = p.token_span(kw);
                    p.push_error(
                        ParseError::new(
                            ParseErrorKind::InvalidSyntax,
                            span,
                            format!(
                                "Nessa does not have `{kw_text}`; return typed errors with `return error <Err>`"
                            ),
                        )
                        .add_help("declare `-> !MyError RetType` and write `return error MyError(...)`"),
                    );
                    let payload = expr::try_expr(p)?;
                    let full_span = p.current_span();
                    let err_node = p
                        .ast()
                        .builder(NodeKind::ErrorConstruction, full_span)
                        .add_child(payload)
                        .build();
                    Ok(p.ast()
                        .builder(NodeKind::ReturnStatement, full_span)
                        .add_child(err_node)
                        .add_child(NULL)
                        .build())
                }
                _ => Ok(NULL),
            }
        }
        _ => Ok(NULL),
    }
}

fn parse_recovered_fn_after_keyword(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    let name = basic::try_id(p)?;
    try_angle_bracket_generics_recovery(p, "function");
    let params = try_multi_with_bracket(
        p,
        &[Rule::comma("function parameter", basic::try_parameter)],
        TokenKind::LParen,
        TokenKind::RParen,
    )?;
    let ret_type = definition::try_return_type(p)?;
    let body = if p.eat_token(TokenKind::Eq) {
        expr::try_expr(p)?
    } else {
        basic::try_block_or_statement(p)?
    };
    let span = p.current_span();
    Ok(p.ast()
        .builder(NodeKind::FunctionDef, span)
        .add_child(name)
        .add_child(ret_type)
        .add_child(body)
        .add_child(NULL)
        .add_multi_children(&params)
        .build())
}

pub(crate) fn looks_like_elif_branch(p: &Parser) -> bool {
    let next = p.peek_nth(1).kind;
    if matches!(
        next,
        TokenKind::Eq
            | TokenKind::PlusEq
            | TokenKind::MinusEq
            | TokenKind::StarEq
            | TokenKind::SlashEq
            | TokenKind::PercentEq
            | TokenKind::Colon
            | TokenKind::Dot
            | TokenKind::Semi
            | TokenKind::Comma
            | TokenKind::Eof
            | TokenKind::RBrace
            | TokenKind::Outdent
    ) {
        return false;
    }
    has_block_opener_on_header(p)
}

fn looks_like_switch_stmt(p: &Parser) -> bool {
    let next = p.peek_nth(1).kind;
    if matches!(
        next,
        TokenKind::Eq
            | TokenKind::Colon
            | TokenKind::Dot
            | TokenKind::Semi
            | TokenKind::Eof
            | TokenKind::LBrace
    ) {
        return false;
    }
    has_block_opener_on_header(p)
}

fn looks_like_throw_stmt(p: &Parser) -> bool {
    let idx = p.next_non_newline_index();
    let tokens = p.raw_tokens();
    if idx + 1 >= tokens.len() {
        return false;
    }
    // Must be on the same line (not `throw\n...`), and not `throw = ...`, `throw(...)`, `throw.foo`.
    let next_raw = tokens[idx + 1];
    !matches!(
        next_raw.kind,
        TokenKind::Newline
            | TokenKind::Semi
            | TokenKind::Eof
            | TokenKind::Eq
            | TokenKind::PlusEq
            | TokenKind::MinusEq
            | TokenKind::StarEq
            | TokenKind::SlashEq
            | TokenKind::PercentEq
            | TokenKind::LParen
            | TokenKind::LBrace
            | TokenKind::Dot
            | TokenKind::Colon
            | TokenKind::Comma
            | TokenKind::RBrace
            | TokenKind::RParen
            | TokenKind::RBracket
            | TokenKind::Outdent
    )
}

fn has_block_opener_on_header(p: &Parser) -> bool {
    let mut idx = p.next_non_newline_index() + 1;
    let tokens = p.raw_tokens();
    let mut paren_depth: u32 = 0;
    while idx < tokens.len() {
        match tokens[idx].kind {
            TokenKind::LParen => paren_depth += 1,
            TokenKind::RParen => paren_depth = paren_depth.saturating_sub(1),
            TokenKind::LBrace if paren_depth == 0 => return true,
            TokenKind::Colon
                if paren_depth == 0
                    && idx + 2 < tokens.len()
                    && tokens[idx + 1].kind == TokenKind::Newline
                    && tokens[idx + 2].kind == TokenKind::Indent =>
            {
                return true;
            }
            TokenKind::Newline if paren_depth == 0 => {
                return idx + 1 < tokens.len() && tokens[idx + 1].kind == TokenKind::Indent;
            }
            TokenKind::Semi | TokenKind::Eof | TokenKind::Outdent => return false,
            _ => {}
        }
        idx += 1;
    }
    false
}
