use ast::{NodeIndex, NodeKind};
use lexer::token::TokenKind;

use crate::basic::{self, try_multi_with_bracket, Rule};
use crate::error::{ParseErrorKind, ParseResult};
use crate::parser::{Parser, NULL};

// ---------------------------------------------------------------------------
// Pattern parsing options
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct PatternOption {
    pub no_extended_call: bool,
}

impl PatternOption {
    pub fn new() -> Self {
        Self {
            no_extended_call: false,
        }
    }

    pub fn without_extended_call() -> Self {
        Self {
            no_extended_call: true,
        }
    }
}

impl Default for PatternOption {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Operator info for pattern Pratt parsing
// ---------------------------------------------------------------------------

pub struct PatternOpInfo {
    pub node_kind: NodeKind,
    pub prec: i32,
}

#[inline]
pub fn get_pattern_op_info(kind: TokenKind) -> PatternOpInfo {
    match kind {
        // Boolean guards
        TokenKind::KwIf => PatternOpInfo { node_kind: NodeKind::PatternIfGuard, prec: 10 },
        TokenKind::KwOr => PatternOpInfo { node_kind: NodeKind::PatternOr, prec: 20 },
        TokenKind::KwAnd => PatternOpInfo { node_kind: NodeKind::PatternAndIs, prec: 30 },

        // Binding/alias
        TokenKind::KwAs => PatternOpInfo { node_kind: NodeKind::PatternAsBind, prec: 90 },

        // Constructor / destruction
        TokenKind::LParen => PatternOpInfo { node_kind: NodeKind::PatternCall, prec: 100 },
        TokenKind::LBrace => PatternOpInfo { node_kind: NodeKind::PatternExtendedCall, prec: 100 },
        TokenKind::Question => PatternOpInfo { node_kind: NodeKind::PatternOptionSome, prec: 100 },
        TokenKind::Bang => PatternOpInfo { node_kind: NodeKind::PatternErrorOk, prec: 100 },

        // Projection
        TokenKind::Dot => PatternOpInfo { node_kind: NodeKind::PropertyPattern, prec: 110 },

        _ => PatternOpInfo { node_kind: NodeKind::Invalid, prec: -1 },
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

pub fn try_pattern(p: &mut Parser) -> ParseResult {
    try_pattern_with_option(p, PatternOption::new())
}

pub fn try_pattern_without_extended_call(p: &mut Parser) -> ParseResult {
    try_pattern_with_option(p, PatternOption::without_extended_call())
}

pub fn try_pattern_with_option(p: &mut Parser, option: PatternOption) -> ParseResult {
    let _g = p.enter();
    let result = try_pattern_pratt(p, 0, option);
    result
}

// ---------------------------------------------------------------------------
// Pratt parser for patterns
// ---------------------------------------------------------------------------

fn try_pattern_pratt(
    p: &mut Parser,
    min_prec: i32,
    option: PatternOption,
) -> ParseResult {
    let mut left = try_prefix_pattern(p, option)?;
    if left.is_null() {
        return Ok(NULL);
    }

    loop {
        let token = p.peek_token();
        let op = get_pattern_op_info(token.kind);

        if op.node_kind == NodeKind::Invalid || op.prec < min_prec {
            break;
        }

        match try_post_pattern(p, token.kind, left, option) {
            Ok(node) if !node.is_null() => {
                left = node;
                continue;
            }
            Err(ParseErrorKind::ControlMeetExtendedCall) => {
                break;
            }
            Err(e) => return Err(e),
            Ok(_) => {} // NULL – fall through to infix
        }

        // Consume operator, parse right side (infix)
        p.next_token();
        let right = try_pattern_pratt(p, op.prec + 1, option)?;
        if right.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected pattern after binary operator",
            );
        }

        let span = p.current_span();
        left = p
            .ast()
            .builder(op.node_kind, span)
            .add_child(left)
            .add_child(right)
            .build();
    }

    Ok(left)
}

// ---------------------------------------------------------------------------
// Prefix patterns
// ---------------------------------------------------------------------------

fn try_prefix_pattern(p: &mut Parser, option: PatternOption) -> ParseResult {
    let token = p.peek_token();
    match token.kind {
        // Literals and identifiers
        TokenKind::Integer
        | TokenKind::IntBin
        | TokenKind::IntOct
        | TokenKind::IntHex
        | TokenKind::Real
        | TokenKind::RealSci
        | TokenKind::String
        | TokenKind::Char
        | TokenKind::KwFalse
        | TokenKind::KwTrue
        | TokenKind::KwNull => basic::try_atomic(p),

        TokenKind::Id => basic::try_id(p),
        TokenKind::Underscore => {
            p.next_token();
            let span = p.current_span();
            Ok(p.ast().builder(NodeKind::Underscore, span).build())
        }

        TokenKind::Dot => {
            // Check for `..` (rest pattern) vs `.id` (symbol)
            if p.peek(&[TokenKind::Dot, TokenKind::Dot]) {
                try_rest_pattern(p, option)
            } else {
                basic::try_symbol(p)
            }
        }
        TokenKind::LParen => try_paren_pattern(p, option),
        TokenKind::LBracket => try_list_pattern(p, option),
        TokenKind::LBrace => try_object_pattern(p, option),
        TokenKind::Minus => try_negative_pattern(p),
        TokenKind::Hash => try_effect_pattern(p, option),
        TokenKind::Bang => try_error_pattern(p, option),
        TokenKind::KwAsync => try_async_pattern(p, option),

        _ => Ok(NULL),
    }
}

// ---------------------------------------------------------------------------
// Postfix patterns
// ---------------------------------------------------------------------------

fn try_post_pattern(
    p: &mut Parser,
    kind: TokenKind,
    left: NodeIndex,
    option: PatternOption,
) -> ParseResult {
    match kind {
        TokenKind::LParen => parse_des_call_paren(p, left, option),
        TokenKind::LBrace => parse_des_call_brace(p, left, option),
        TokenKind::Dot => parse_pattern_property(p, left),
        TokenKind::Question => parse_pattern_optional_unwrap(p, left),
        TokenKind::Bang => parse_pattern_error_unwrap(p, left),
        _ => Ok(NULL),
    }
}

// ---------------------------------------------------------------------------
// Individual pattern parsers
// ---------------------------------------------------------------------------

/// `(pat, pat, ...)` | `()` | `(pat)`
fn try_paren_pattern(p: &mut Parser, option: PatternOption) -> ParseResult {
    let _g = p.enter();

    if !p.peek(&[TokenKind::LParen]) {
        return Ok(NULL);
    }
    p.next_token(); // consume (

    // ()
    if p.eat_token(TokenKind::RParen) {
        let span = p.current_span();
        let idx = p.ast().builder(NodeKind::Unit, span).build();
        return Ok(idx);
    }

    let first = try_pattern_with_option(p, option)?;
    if first.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected pattern in parenthesized expression",
        );
    }

    if p.eat_token(TokenKind::Comma) {
        let mut elems = vec![first];
        let rest = basic::try_multi(
            p,
            &[Rule::comma("tuple pattern element", try_pattern)],
        )?;
        elems.extend(rest);
        p.expect_token(TokenKind::RParen)?;
        let span = p.current_span();
        let idx = p
            .ast()
        .builder(NodeKind::PatternTuple, span)
            .add_multi_children(&elems)
            .build();
        return Ok(idx);
    }

    p.expect_token(TokenKind::RParen)?;
    Ok(first)
}

/// `[pat, pat, ...]`
fn try_list_pattern(p: &mut Parser, _option: PatternOption) -> ParseResult {
    let _g = p.enter();

    if !p.peek(&[TokenKind::LBracket]) {
        return Ok(NULL);
    }

    let elems = try_multi_with_bracket(
        p,
        &[Rule::comma("list pattern element", try_pattern)],
        TokenKind::LBracket,
        TokenKind::RBracket,
    )?;

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::PatternList, span)
        .add_multi_children(&elems)
        .build();
    Ok(idx)
}

/// `{ prop: pat, ... }`
fn try_object_pattern(p: &mut Parser, _option: PatternOption) -> ParseResult {
    let _g = p.enter();

    if !p.peek(&[TokenKind::LBrace]) {
        return Ok(NULL);
    }

    let elems = try_multi_with_bracket(
        p,
        &[Rule::comma("object pattern property", try_pattern_property_item)],
        TokenKind::LBrace,
        TokenKind::RBrace,
    )?;

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::PatternRecord, span)
        .add_multi_children(&elems)
        .build();
    Ok(idx)
}

/// `id: pattern` - named property in object pattern
fn try_pattern_property_item(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    let id = basic::try_id(p)?;
    if id.is_null() {
        return Ok(NULL);
    }

    if !p.eat_token(TokenKind::Colon) {
        // Just a name → shorthand for id: id
        return Ok(id);
    }

    let pat = try_pattern(p)?;
    if pat.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected pattern after `:` in object pattern",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::Property, span)
        .add_child(id)
        .add_child(pat)
        .build();
    Ok(idx)
}

/// `..` or `..pat`
fn try_rest_pattern(p: &mut Parser, option: PatternOption) -> ParseResult {
    let _g = p.enter();

    // Consume two dots
    if !p.peek(&[TokenKind::Dot, TokenKind::Dot]) {
        return Ok(NULL);
    }
    p.next_token(); // first dot
    p.next_token(); // second dot

    let pat = try_pattern_with_option(p, option)?;
    let span = p.current_span();

    let idx = if pat.is_null() {
        p.ast().builder(NodeKind::PatternRestBind, span).build()
    } else {
        p.ast()
            .builder(NodeKind::PatternRestBind, span)
            .add_child(pat)
            .build()
    };

    Ok(idx)
}

/// `-expr` (negative literal pattern)
fn try_negative_pattern(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    if !p.eat_token(TokenKind::Minus) {
        return Ok(NULL);
    }

    let inner = basic::try_atomic(p)?;
    if inner.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected numeric literal after `-` in pattern",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::Negative, span)
        .add_child(inner)
        .build();
    Ok(idx)
}

/// `#pat` - effect pattern
fn try_effect_pattern(p: &mut Parser, option: PatternOption) -> ParseResult {
    let _g = p.enter();

    if !p.eat_token(TokenKind::Hash) {
        return Ok(NULL);
    }

    let inner = try_pattern_pratt(p, 90, option)?;
    if inner.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected pattern after `#` in effect pattern",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::PatternError, span)
        .add_child(inner)
        .build();
    Ok(idx)
}

/// `!pat` - error pattern
fn try_error_pattern(p: &mut Parser, option: PatternOption) -> ParseResult {
    let _g = p.enter();

    if !p.eat_token(TokenKind::Bang) {
        return Ok(NULL);
    }

    let inner = try_pattern_pratt(p, 90, option)?;
    if inner.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected pattern after `!` in error pattern",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::PatternNot, span)
        .add_child(inner)
        .build();
    Ok(idx)
}

/// `async pat` - async pattern (marks effect call for async execution)
fn try_async_pattern(p: &mut Parser, option: PatternOption) -> ParseResult {
    let _g = p.enter();

    if !p.eat_token(TokenKind::KwAsync) {
        return Ok(NULL);
    }

    let inner = try_pattern_pratt(p, 90, option)?;
    if inner.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected pattern after `async`",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::PatternAsync, span)
        .add_child(inner)
        .build();
    Ok(idx)
}

/// Postfix: `name(pat, ...)` - destructuring call
fn parse_des_call_paren(
    p: &mut Parser,
    left: NodeIndex,
    _option: PatternOption,
) -> ParseResult {
    let _g = p.enter();

    let args = try_multi_with_bracket(
        p,
        &[Rule::comma("destructor argument", try_pattern)],
        TokenKind::LParen,
        TokenKind::RParen,
    )?;

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::PatternCall, span)
        .add_child(left)
        .add_multi_children(&args)
        .build();
    Ok(idx)
}

/// Postfix: `name { prop: pat, ... }` - destructuring brace call
fn parse_des_call_brace(
    p: &mut Parser,
    left: NodeIndex,
    option: PatternOption,
) -> ParseResult {
    if option.no_extended_call {
        return Err(ParseErrorKind::ControlMeetExtendedCall);
    }
    let _g = p.enter();

    let args = try_multi_with_bracket(
        p,
        &[Rule::comma("destructor property", try_pattern_property_item)],
        TokenKind::LBrace,
        TokenKind::RBrace,
    )?;

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::PatternExtendedCall, span)
        .add_child(left)
        .add_multi_children(&args)
        .build();
    Ok(idx)
}

/// Postfix: `pat.id` - property access in pattern
fn parse_pattern_property(p: &mut Parser, left: NodeIndex) -> ParseResult {
    let _g = p.enter();
    p.expect_token(TokenKind::Dot)?;

    let id = basic::try_id(p)?;
    if id.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected identifier after `.` in pattern property",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::PropertyPattern, span)
        .add_child(left)
        .add_child(id)
        .build();
    Ok(idx)
}

/// Postfix: `pat?` - optional unwrap
fn parse_pattern_optional_unwrap(p: &mut Parser, left: NodeIndex) -> ParseResult {
    let _g = p.enter();
    p.expect_token(TokenKind::Question)?;

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::PatternOptionSome, span)
        .add_child(left)
        .build();
    Ok(idx)
}

/// Postfix: `pat!` - error unwrap
fn parse_pattern_error_unwrap(p: &mut Parser, left: NodeIndex) -> ParseResult {
    let _g = p.enter();
    p.expect_token(TokenKind::Bang)?;

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::PatternErrorOk, span)
        .add_child(left)
        .build();
    Ok(idx)
}
