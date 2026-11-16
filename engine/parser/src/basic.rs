use ast::{NodeIndex, NodeKind};
use lexer::token::TokenKind;

use crate::error::{ParseError, ParseErrorKind, ParseResult};
use crate::parser::{NULL, Parser};
use crate::{expr, heuristics, pattern, statement};

extern crate str_interner;

// ---------------------------------------------------------------------------
// Rule – describes one alternative in a tryMulti combinator
// ---------------------------------------------------------------------------

pub type ParserFn = fn(&mut Parser) -> ParseResult;

pub struct Rule {
    pub name: &'static str,
    pub parser: ParserFn,
    pub separator: TokenKind,
    pub additional_separator: Option<TokenKind>,
}

impl Rule {
    #[inline]
    pub fn comma(name: &'static str, parser: ParserFn) -> Self {
        Self {
            name,
            parser,
            separator: TokenKind::Comma,
            additional_separator: None,
        }
    }

    #[inline]
    pub fn semicolon(name: &'static str, parser: ParserFn) -> Self {
        Self {
            name,
            parser,
            separator: TokenKind::Semi,
            additional_separator: None,
        }
    }

    pub fn comma_or_semicolon(name: &'static str, parser: ParserFn) -> Self {
        Self {
            name,
            parser,
            separator: TokenKind::Semi,
            additional_separator: Some(TokenKind::Comma),
        }
    }
}

// ---------------------------------------------------------------------------
// Combinators
// ---------------------------------------------------------------------------

/// Parse zero or more items separated by the rule's separator.
/// Rules are tried in order; the first successful match is used.
pub fn try_multi(p: &mut Parser, rules: &[Rule]) -> Result<Vec<NodeIndex>, ParseErrorKind> {
    try_multi_until(p, rules, None)
}

fn try_multi_until(
    p: &mut Parser,
    rules: &[Rule],
    close: Option<TokenKind>,
) -> Result<Vec<NodeIndex>, ParseErrorKind> {
    let _g = p.enter();

    let mut nodes = Vec::new();
    let is_block_or_file = matches!(
        close,
        Some(TokenKind::Eof | TokenKind::RBrace | TokenKind::Outdent)
    );
    let allow_comma = rules.iter().any(|r| {
        r.separator == TokenKind::Comma || r.additional_separator == Some(TokenKind::Comma)
    });

    'outer: loop {
        heuristics::try_skip_foreign_comments(p);

        let peek_kind = p.peek_token().kind;
        if close == Some(peek_kind) || peek_kind == TokenKind::Eof {
            break;
        }

        let start_cursor = p.raw_cursor();
        let errors_before = p.error_count();

        for rule in rules {
            match (rule.parser)(p) {
                Ok(node) if node.is_null() => {
                    if is_block_or_file && p.error_count() > errors_before {
                        p.synchronize_statement(start_cursor, close, allow_comma);
                        if p.raw_cursor() <= start_cursor {
                            let next_kind = p.peek_token().kind;
                            if close == Some(next_kind)
                                || next_kind == TokenKind::Eof
                                || (close != Some(TokenKind::Eof)
                                    && matches!(next_kind, TokenKind::RBrace | TokenKind::Outdent))
                            {
                                break 'outer;
                            }
                            p.next_token();
                        }
                        continue 'outer;
                    }
                    continue;
                }
                Ok(node) => {
                    nodes.push(node);
                    heuristics::try_skip_foreign_comments(p);

                    if p.eat_token(rule.separator)
                        || rule
                            .additional_separator
                            .is_some_and(|separator| p.eat_token(separator))
                    {
                        if p.raw_cursor() <= start_cursor {
                            break 'outer;
                        }
                        continue 'outer;
                    }

                    if is_block_or_file {
                        let next_kind = p.peek_token().kind;
                        if close == Some(next_kind)
                            || next_kind == TokenKind::Eof
                            || (close != Some(TokenKind::Eof)
                                && matches!(next_kind, TokenKind::RBrace | TokenKind::Outdent))
                        {
                            break 'outer;
                        }
                        if p.error_count() > errors_before {
                            if !p.had_newline_since(start_cursor) {
                                let sync_from = p.raw_cursor();
                                p.synchronize_statement(sync_from, close, allow_comma);
                            }
                            if p.raw_cursor() <= start_cursor {
                                p.next_token();
                            }
                            continue 'outer;
                        }
                        let span = p.next_token_span();
                        p.push_error(ParseError::with_label(
                            ParseErrorKind::UnexpectedToken,
                            span,
                            format!("Unexpected `{}` after {}", next_kind.lexeme(), rule.name),
                            span,
                            format!("found `{}`", next_kind.lexeme()),
                        ));
                        let sync_from = p.raw_cursor();
                        p.synchronize_statement(sync_from, close, allow_comma);
                        if p.raw_cursor() <= sync_from {
                            let after_kind = p.peek_token().kind;
                            if close == Some(after_kind)
                                || after_kind == TokenKind::Eof
                                || (close != Some(TokenKind::Eof)
                                    && matches!(after_kind, TokenKind::RBrace | TokenKind::Outdent))
                            {
                                break 'outer;
                            }
                            p.next_token();
                        }
                        continue 'outer;
                    }

                    break 'outer;
                }
                Err(kind) => {
                    if kind == ParseErrorKind::ControlMeetExtendedCall
                        || (kind == ParseErrorKind::InvalidBlockPrefix
                            && p.error_count() == errors_before)
                    {
                        return Err(kind);
                    }
                    if is_block_or_file && p.error_count() > errors_before {
                        p.synchronize_statement(start_cursor, close, allow_comma);
                        if p.raw_cursor() <= start_cursor {
                            let next_kind = p.peek_token().kind;
                            if close == Some(next_kind)
                                || next_kind == TokenKind::Eof
                                || (close != Some(TokenKind::Eof)
                                    && matches!(next_kind, TokenKind::RBrace | TokenKind::Outdent))
                            {
                                break 'outer;
                            }
                            p.next_token();
                        }
                        continue 'outer;
                    }
                    return Err(kind);
                }
            }
        }

        // No rule matched.
        if is_block_or_file {
            let next_kind = p.peek_token().kind;
            if close == Some(next_kind) || next_kind == TokenKind::Eof {
                break;
            }
            if close == Some(TokenKind::Eof)
                || !matches!(next_kind, TokenKind::RBrace | TokenKind::Outdent)
            {
                let expected: Vec<&str> = rules.iter().map(|r| r.name).collect();
                let msg = if close == Some(TokenKind::Eof) {
                    "Expected definition or statement, or end of file".to_string()
                } else {
                    format!("Expected {} or closing bracket", expected.join(" or "))
                };
                let span = p.next_token_span();
                p.push_error(ParseError::with_label(
                    ParseErrorKind::UnexpectedToken,
                    span,
                    msg,
                    span,
                    format!("found `{}`", next_kind.lexeme()),
                ));
                p.synchronize_statement(start_cursor, close, allow_comma);
                if p.raw_cursor() <= start_cursor {
                    p.next_token();
                }
                continue 'outer;
            }
        }

        break;
    }

    Ok(nodes)
}

/// Parse items inside brackets: `open (rules)* close`.
pub fn try_multi_with_bracket(
    p: &mut Parser,
    rules: &[Rule],
    open: TokenKind,
    close: TokenKind,
) -> Result<Vec<NodeIndex>, ParseErrorKind> {
    let _g = p.enter();

    let open_tok = p.peek_token();
    if !p.eat_token(open) {
        return Ok(Vec::new());
    }

    let nodes = try_multi_until(p, rules, Some(close))?;

    if !p.eat_token(close) {
        let expected: Vec<&str> = rules.iter().map(|r| r.name).collect();
        let span = p.next_token_span();
        let mut err = ParseError::with_label(
            ParseErrorKind::UnexpectedToken,
            span,
            format!("Expected {} or closing bracket", expected.join(" or ")),
            span,
            format!("found `{}`", p.peek_token().kind.lexeme()),
        );
        if open != TokenKind::Indent {
            err = err.add_note_label(
                p.token_span(open_tok),
                format!("unclosed `{}` opened here", open.lexeme()),
            );
        }
        p.push_error(err);
    }

    Ok(nodes)
}

/// Parse items inside a block (braces or indentation).
pub fn try_multi_in_block(
    p: &mut Parser,
    rules: &[Rule],
) -> Result<Vec<NodeIndex>, ParseErrorKind> {
    let _g = p.enter();

    let (open, close) = match p.peek_token().kind {
        TokenKind::LBrace => (TokenKind::LBrace, TokenKind::RBrace),
        TokenKind::Indent => (TokenKind::Indent, TokenKind::Outdent),
        TokenKind::Colon if p.peek(&[TokenKind::Colon, TokenKind::Indent]) => {
            p.next_token(); // consume colon
            (TokenKind::Indent, TokenKind::Outdent)
        }
        _ => {
            return Err(ParseErrorKind::InvalidBlockPrefix);
        }
    };

    try_multi_with_bracket(p, rules, open, close)
}

/// Parse the mandatory block of `construct`, reporting a missing block.
pub fn try_required_block(
    p: &mut Parser,
    construct: &str,
    rules: &[Rule],
) -> Result<Vec<NodeIndex>, ParseErrorKind> {
    match try_multi_in_block(p, rules) {
        Err(ParseErrorKind::InvalidBlockPrefix) => {
            let span = p.next_token_span();
            let found = format!("found `{}`", p.peek_token().kind.lexeme());
            p.err_with_label(
                ParseErrorKind::InvalidBlockPrefix,
                span,
                format!("Expected `{{` or an indented block after {construct}"),
                span,
                found,
            )
            .map(|()| Vec::new())
        }
        result => result,
    }
}

// ---------------------------------------------------------------------------
// Atomic parsers
// ---------------------------------------------------------------------------

/// Whether `kind` is an ordinary or backtick-quoted identifier token.
#[inline]
pub fn is_ident_token(kind: TokenKind) -> bool {
    matches!(kind, TokenKind::Id | TokenKind::ArbitraryId)
}

/// Strip surrounding backticks from a `TokenKind::ArbitraryId` slice.
#[inline]
pub fn unquote_ident_text(kind: TokenKind, raw: &str) -> &str {
    if kind == TokenKind::ArbitraryId
        && raw.len() >= 2
        && raw.starts_with('`')
        && raw.ends_with('`')
    {
        &raw[1..raw.len() - 1]
    } else {
        raw
    }
}

/// Parse atomic expressions (literals, identifiers, keywords).
pub fn try_atomic(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    let token = p.peek_token();
    let node_kind: Option<NodeKind> = match token.kind {
        TokenKind::Integer | TokenKind::IntBin | TokenKind::IntOct | TokenKind::IntHex => {
            Some(NodeKind::Int)
        }
        TokenKind::Real | TokenKind::RealSci => Some(NodeKind::Real),
        TokenKind::String => Some(NodeKind::Str),
        TokenKind::Char => Some(NodeKind::Char),
        TokenKind::Id | TokenKind::ArbitraryId => Some(NodeKind::Id),
        TokenKind::KwFalse | TokenKind::KwTrue => Some(NodeKind::Bool),
        TokenKind::KwSelfCap => Some(NodeKind::SelfUpper),
        TokenKind::KwSelfLower => Some(NodeKind::SelfLower),
        TokenKind::KwNull => Some(NodeKind::Null),
        TokenKind::Underscore => Some(NodeKind::Underscore),
        _ => None,
    };

    if let Some(kind) = node_kind {
        let text = unquote_ident_text(token.kind, p.next_token_text());
        let str_id = str_interner::intern(text);
        let span = p.next_token_span();
        p.next_token(); // consume the token
        let idx = p.ast().builder(kind, span).set_str_id(str_id).build();
        return Ok(idx);
    }

    Ok(NULL)
}

/// Parse an identifier (ordinary or backtick-quoted arbitrary identifier).
pub fn try_id(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    let kind = p.peek_token().kind;
    if !is_ident_token(kind) {
        return Ok(NULL);
    }

    let text = unquote_ident_text(kind, p.next_token_text());
    let str_id = str_interner::intern(text);
    let span = p.next_token_span();
    p.next_token();
    let idx = p
        .ast()
        .builder(NodeKind::Id, span)
        .set_str_id(str_id)
        .build();
    Ok(idx)
}

/// Parse a symbol: `.` id
pub fn try_symbol(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    if !p.peek(&[TokenKind::Dot]) {
        return Ok(NULL);
    }

    p.next_token(); // consume dot

    let id = try_id(p)?;
    if id.is_null() {
        let _ = p.err_with_label(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected identifier after `.`",
            p.next_token_span(),
            format!("found `{}`", p.peek_token().kind.lexeme()),
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::Symbol, span)
        .add_child(id)
        .build();
    Ok(idx)
}

/// Parse a property: `id : expr`
pub fn try_property(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    if !is_ident_token(p.peek_nth(0).kind) || p.peek_nth(1).kind != TokenKind::Colon {
        return Ok(NULL);
    }

    let id = try_id(p)?;
    p.eat_tokens(1); // consume colon

    let e = expr::try_expr(p)?;
    if e.is_null() {
        let _ = p.err_with_label(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected an expression after `:`",
            p.next_token_span(),
            format!("found `{}`", p.peek_token().kind.lexeme()),
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::Property, span)
        .add_child(id)
        .add_child(e)
        .build();
    Ok(idx)
}

/// Parse a named argument: `id = expr` (without leading dot).
/// Used in function call argument lists for optional parameters.
pub fn try_named_arg(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    // Look ahead: must be Id/ArbitraryId followed by `=`.
    if !is_ident_token(p.peek_nth(0).kind) || p.peek_nth(1).kind != TokenKind::Eq {
        return Ok(NULL);
    }

    let id = try_id(p)?;
    p.expect_token(TokenKind::Eq)?;
    let e = expr::try_expr(p)?;
    if e.is_null() {
        p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected a value for named argument",
        )?;
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::NamedArg, span)
        .add_child(id)
        .add_child(e)
        .build();
    Ok(idx)
}

/// Parse a property assignment: `.id = expr`
pub fn try_property_assign(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    if p.peek_nth(0).kind != TokenKind::Dot
        || !is_ident_token(p.peek_nth(1).kind)
        || p.peek_nth(2).kind != TokenKind::Eq
    {
        return Ok(NULL);
    }
    p.next_token(); // consume dot

    let id = try_id(p)?;
    if id.is_null() {
        let _ = p.err_with_label(
            ParseErrorKind::InvalidSyntax,
            p.current_span(),
            "Expected an identifier after `.`",
            p.next_token_span(),
            format!("found `{}`", p.peek_token().kind.lexeme()),
        );
    }

    if !p.eat_token(TokenKind::Eq) {
        let _ = p.err_with_label(
            ParseErrorKind::UnexpectedToken,
            p.next_token_span(),
            "Expected `=` after property identifier",
            p.next_token_span(),
            format!("found `{}`", p.peek_token().kind.lexeme()),
        );
    }

    let e = expr::try_expr(p)?;
    if e.is_null() {
        let _ = p.err_with_label(
            ParseErrorKind::InvalidSyntax,
            p.current_span(),
            "Expected an expression after `=`",
            p.next_token_span(),
            format!("found `{}`", p.peek_token().kind.lexeme()),
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::PropertyAssignment, span)
        .add_child(id)
        .add_child(e)
        .build();
    Ok(idx)
}

/// Try block first, fall back to definition-or-statement.
pub fn try_block_or_statement(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    match statement::try_block(p) {
        Ok(block) if !block.is_null() => {
            return Ok(block);
        }
        Err(ParseErrorKind::InvalidBlockPrefix) => {
            // Not a block, try statement
        }
        Err(e) => {
            return Err(e);
        }
        Ok(_) => {}
    }

    statement::try_definition_or_statement(p)
}

/// Parse a pattern arm: `pattern => block_or_statement`
pub fn try_pattern_arm(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    let pat = pattern::try_refutable_pattern(p)?;
    if pat.is_null() {
        return Ok(NULL);
    }

    heuristics::expect_fat_arrow_in_arm(p, "match/handler arm")?;

    let body = try_block_or_statement(p)?;
    if body.is_null() {
        let _ = p.err_with_label(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected a block or statement after `=>`",
            p.next_token_span(),
            format!("found `{}`", p.peek_token().kind.lexeme()),
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::CaseArm, span)
        .add_child(pat)
        .add_child(body)
        .build();
    Ok(idx)
}

/// Parse a catch arm: `catch id => block_or_statement`
pub fn try_catch_arm(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    if !p.eat_token(TokenKind::KwCatch) {
        return Ok(NULL);
    }

    let id = try_id(p)?;
    if id.is_null() {
        let _ = p.err_with_label(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected an identifier after `catch`",
            p.next_token_span(),
            format!("found `{}`", p.peek_token().kind.lexeme()),
        );
    }

    heuristics::expect_fat_arrow_in_arm(p, "`catch` arm")?;

    let body = try_block_or_statement(p)?;
    if body.is_null() {
        let _ = p.err_with_label(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected a block or statement after `=>`",
            p.next_token_span(),
            format!("found `{}`", p.peek_token().kind.lexeme()),
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::CatchArm, span)
        .add_child(id)
        .add_child(body)
        .build();
    Ok(idx)
}

/// Parse a parameter.
///
/// Cases:
/// - `self`
/// - `...id (: type)?`
/// - `.id (: type)? = expr`    (optional parameter)
/// - `lambda pattern (: type)?`
/// - `pattern (: type)?`
pub fn try_parameter(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    // Case: self
    if p.eat_token(TokenKind::KwSelfLower) {
        let span = p.current_span();
        let idx = p.ast().builder(NodeKind::ParamSelf, span).build();
        return Ok(idx);
    }

    // Case: ...id (: type)?
    if p.peek(&[TokenKind::Dot, TokenKind::Dot, TokenKind::Dot]) {
        p.eat_tokens(3);
        let id = try_id(p)?;
        if id.is_null() {
            let _ = p.err_with_label(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected an identifier after `...`",
                p.next_token_span(),
                format!("found `{}`", p.peek_token().kind.lexeme()),
            );
        }
        let mut ty = NULL;
        if p.eat_token(TokenKind::Colon) {
            ty = p.with_type_context(expr::try_expr_without_extended_call)?;
        }
        let span = p.current_span();
        let idx = p
            .ast()
            .builder(NodeKind::ParamVarargs, span)
            .add_child(id)
            .add_child(ty)
            .build();
        return Ok(idx);
    }

    // Case: .id (: type)? = expr  (optional parameter)
    if p.peek_nth(0).kind == TokenKind::Dot && is_ident_token(p.peek_nth(1).kind) {
        p.eat_tokens(1); // consume dot
        let id = try_id(p)?;
        let mut ty = NULL;
        if p.eat_token(TokenKind::Colon) {
            ty = p.with_type_context(expr::try_expr_without_extended_call)?;
        }
        p.expect_token(TokenKind::Eq)?;
        let init = expr::try_expr(p)?;
        if init.is_null() {
            p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected a default value for optional parameter",
            )?;
        }
        let span = p.current_span();
        let idx = p
            .ast()
            .builder(NodeKind::ParamOptional, span)
            .add_child(id)
            .add_child(ty)
            .add_child(init)
            .build();
        return Ok(idx);
    }

    // Case: lambda pattern (: type)?
    if p.eat_token(TokenKind::KwLambda) {
        let pat = pattern::try_pattern(p)?;
        let mut ty = NULL;
        if p.eat_token(TokenKind::Colon) {
            ty = p.with_type_context(expr::try_expr_without_extended_call)?;
        }
        let span = p.current_span();
        let idx = p
            .ast()
            .builder(NodeKind::ParamLambda, span)
            .add_child(pat)
            .add_child(ty)
            .build();
        return Ok(idx);
    }

    // Default: pattern (: type)?
    let pat = pattern::try_pattern(p)?;
    if pat.is_null() {
        return Ok(NULL);
    }
    let mut ty = NULL;
    if p.eat_token(TokenKind::Colon) {
        ty = p.with_type_context(expr::try_expr_without_extended_call)?;
    }
    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::ParamTyped, span)
        .add_child(pat)
        .add_child(ty)
        .build();
    Ok(idx)
}

/// file_scope -> statements* eof
pub fn try_file_scope(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    let items = try_multi_until(
        p,
        &[Rule::semicolon(
            "definition or statement",
            statement::try_definition_or_statement,
        )],
        Some(TokenKind::Eof),
    )?;

    if !p.eat_token(TokenKind::Eof) {
        let span = p.next_token_span();
        let msg = format!("found `{}`", p.peek_token().kind.lexeme());
        let _ = p.err_with_label(
            ParseErrorKind::UnexpectedToken,
            span,
            "Expected definition or statement, or end of file",
            span,
            msg,
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::FileScope, span)
        .add_multi_children(&items)
        .build();
    Ok(idx)
}
