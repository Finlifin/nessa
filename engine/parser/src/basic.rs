use ast::{NodeIndex, NodeKind};
use lexer::token::TokenKind;

use crate::error::{ParseErrorKind, ParseResult};
use crate::parser::{NULL, Parser};
use crate::{expr, pattern, statement};

extern crate str_interner;

// ---------------------------------------------------------------------------
// Rule – describes one alternative in a tryMulti combinator
// ---------------------------------------------------------------------------

pub type ParserFn = fn(&mut Parser) -> ParseResult;

pub struct Rule {
    pub name: &'static str,
    pub parser: ParserFn,
    pub separator: TokenKind,
}

impl Rule {
    #[inline]
    pub fn comma(name: &'static str, parser: ParserFn) -> Self {
        Self {
            name,
            parser,
            separator: TokenKind::Comma,
        }
    }

    #[inline]
    pub fn semicolon(name: &'static str, parser: ParserFn) -> Self {
        Self {
            name,
            parser,
            separator: TokenKind::Semi,
        }
    }
}

// ---------------------------------------------------------------------------
// Combinators
// ---------------------------------------------------------------------------

/// Parse zero or more items separated by the rule's separator.
/// Rules are tried in order; the first successful match is used.
pub fn try_multi(p: &mut Parser, rules: &[Rule]) -> Result<Vec<NodeIndex>, ParseErrorKind> {
    let _g = p.enter();

    let mut nodes = Vec::new();

    'outer: loop {
        for rule in rules {
            let node = (rule.parser)(p)?;
            if node.is_null() {
                continue;
            }

            nodes.push(node);

            if !p.eat_token(rule.separator) {
                break 'outer;
            }
            continue 'outer;
        }

        // No rule matched → end the multi parse
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

    if !p.eat_token(open) {
        return Ok(Vec::new());
    }

    let nodes = try_multi(p, rules)?;

    if !p.eat_token(close) {
        // Build a description of expected rules
        let expected: Vec<&str> = rules.iter().map(|r| r.name).collect();
        let _ = p.err_with_label(
            ParseErrorKind::UnexpectedToken,
            p.next_token_span(),
            format!("Expected {} or closing bracket", expected.join(" or ")),
            p.next_token_span(),
            format!("found `{}`", p.peek_token().kind.lexeme()),
        );
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
        TokenKind::Colon => {
            if p.peek(&[TokenKind::Colon, TokenKind::Indent]) {
                p.next_token(); // consume colon
                (TokenKind::Indent, TokenKind::Outdent)
            } else {
                return Err(ParseErrorKind::InvalidBlockPrefix);
            }
        }
        _ => {
            return Err(ParseErrorKind::InvalidBlockPrefix);
        }
    };

    let result = try_multi_with_bracket(p, rules, open, close);
    result
}

// ---------------------------------------------------------------------------
// Atomic parsers
// ---------------------------------------------------------------------------

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
        TokenKind::Id => Some(NodeKind::Id),
        TokenKind::KwFalse | TokenKind::KwTrue => Some(NodeKind::Bool),
        TokenKind::KwSelfCap => Some(NodeKind::SelfUpper),
        TokenKind::KwSelfLower => Some(NodeKind::SelfLower),
        TokenKind::KwNull => Some(NodeKind::Null),
        TokenKind::Underscore => Some(NodeKind::Underscore),
        _ => None,
    };

    if let Some(kind) = node_kind {
        let text = p.next_token_text();
        let str_id = str_interner::intern(text);
        let span = p.next_token_span();
        p.next_token(); // consume the token
        let idx = p.ast().builder(kind, span).set_str_id(str_id).build();
        return Ok(idx);
    }

    Ok(NULL)
}

/// Parse an identifier.
pub fn try_id(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    if p.peek_token().kind != TokenKind::Id {
        return Ok(NULL);
    }

    let text = p.next_token_text();
    let str_id = str_interner::intern(text);
    let span = p.next_token_span();
    p.next_token();
    let idx = p.ast().builder(NodeKind::Id, span).set_str_id(str_id).build();
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

    if !p.peek(&[TokenKind::Id, TokenKind::Colon]) {
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

    // Look ahead: must be Id followed by `=`.
    if !p.peek(&[TokenKind::Id, TokenKind::Eq]) {
        return Ok(NULL);
    }

    let id = try_id(p)?;
    p.expect_token(TokenKind::Eq)?;
    let e = expr::try_expr(p)?;

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

    if p.peek_token().kind != TokenKind::Dot {
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

    let result = statement::try_definition_or_statement(p);
    result
}

/// Parse a pattern arm: `pattern => block_or_statement`
pub fn try_pattern_arm(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    let pat = pattern::try_pattern(p)?;
    if pat.is_null() {
        return Ok(NULL);
    }

    p.expect_token(TokenKind::FatArrow)?;

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

    p.expect_token(TokenKind::FatArrow)?;

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
            ty = expr::try_expr_without_extended_call(p)?;
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
    if p.peek(&[TokenKind::Dot, TokenKind::Id]) {
        p.eat_tokens(1); // consume dot
        let id = try_id(p)?;
        let mut ty = NULL;
        if p.eat_token(TokenKind::Colon) {
            ty = expr::try_expr_without_extended_call(p)?;
        }
        p.expect_token(TokenKind::Eq)?;
        let init = expr::try_expr_without_extended_call(p)?;
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
            ty = expr::try_expr_without_extended_call(p)?;
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
        ty = expr::try_expr_without_extended_call(p)?;
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

    let items = try_multi(
        p,
        &[Rule::semicolon(
            "definition or statement",
            statement::try_definition_or_statement,
        )],
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
