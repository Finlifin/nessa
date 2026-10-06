use ast::{NodeIndex, NodeKind};
use lexer::token::TokenKind;

use crate::basic::{
    self, Rule, try_block_or_statement, try_catch_arm, try_multi_in_block, try_multi_with_bracket,
    try_pattern_arm, try_property, try_property_assign,
};
use crate::definition;
use crate::error::{ParseErrorKind, ParseResult};
use crate::parser::{NULL, Parser};
use crate::{pattern, statement};

// ---------------------------------------------------------------------------
// Expression parsing options
// ---------------------------------------------------------------------------

/// Options that thread through expression parsing to resolve ambiguities
/// (e.g. extended-call `{}` vs block).
#[derive(Debug, Clone, Copy)]
pub struct ExprOption {
    pub no_extended_call: bool,
}

impl ExprOption {
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

impl Default for ExprOption {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Operator info table (function-based lookup for Pratt parsing)
// ---------------------------------------------------------------------------

/// Operator binding info for the Pratt parser.
pub struct OpInfo {
    pub node_kind: NodeKind,
    pub prec: i32,
}

/// Look up operator info for a token kind.
/// Returns `Invalid` kind with prec -1 for non-operators.
#[inline]
pub fn get_expr_op_info(kind: TokenKind) -> OpInfo {
    match kind {
        // Boolean logic
        TokenKind::KwOr => OpInfo {
            node_kind: NodeKind::BoolOr,
            prec: 20,
        },
        TokenKind::KwAnd => OpInfo {
            node_kind: NodeKind::BoolAnd,
            prec: 30,
        },

        // Comparison
        TokenKind::BangEq => OpInfo {
            node_kind: NodeKind::BoolNotEq,
            prec: 40,
        },
        TokenKind::EqEq => OpInfo {
            node_kind: NodeKind::BoolEq,
            prec: 40,
        },
        TokenKind::GtEq => OpInfo {
            node_kind: NodeKind::BoolGtEq,
            prec: 40,
        },
        TokenKind::Gt => OpInfo {
            node_kind: NodeKind::BoolGt,
            prec: 40,
        },
        TokenKind::LtEq => OpInfo {
            node_kind: NodeKind::BoolLtEq,
            prec: 40,
        },
        TokenKind::Lt => OpInfo {
            node_kind: NodeKind::BoolLt,
            prec: 40,
        },
        TokenKind::KwMatches => OpInfo {
            node_kind: NodeKind::BoolMatches,
            prec: 40,
        },

        // Arrow (function type)
        TokenKind::Arrow => OpInfo {
            node_kind: NodeKind::Arrow,
            prec: 50,
        },

        // Concat
        TokenKind::PlusPlus => OpInfo {
            node_kind: NodeKind::Concat,
            prec: 60,
        },

        // Arithmetic
        TokenKind::Plus => OpInfo {
            node_kind: NodeKind::Add,
            prec: 60,
        },
        TokenKind::Minus => OpInfo {
            node_kind: NodeKind::Sub,
            prec: 60,
        },
        TokenKind::Slash => OpInfo {
            node_kind: NodeKind::Div,
            prec: 70,
        },
        TokenKind::Star => OpInfo {
            node_kind: NodeKind::Mul,
            prec: 70,
        },
        TokenKind::Percent => OpInfo {
            node_kind: NodeKind::Mod,
            prec: 70,
        },

        // Infix function call
        TokenKind::Id => OpInfo {
            node_kind: NodeKind::InfixFnCall,
            prec: 70,
        },

        // Pipeline
        TokenKind::PipeGt => OpInfo {
            node_kind: NodeKind::Pipeline,
            prec: 80,
        },

        // 90 is reserved for prefix expression precedence

        // Call / postfix operators (precedence 100)
        TokenKind::LParen => OpInfo {
            node_kind: NodeKind::Call,
            prec: 100,
        },
        TokenKind::LBrace => OpInfo {
            node_kind: NodeKind::ExtendedCall,
            prec: 100,
        },
        TokenKind::Hash => OpInfo {
            node_kind: NodeKind::EffectPropagation,
            prec: 100,
        },
        TokenKind::Bang => OpInfo {
            node_kind: NodeKind::ErrorPropagation,
            prec: 100,
        },
        TokenKind::Question => OpInfo {
            node_kind: NodeKind::OptionPropagation,
            prec: 100,
        },
        TokenKind::KwMatch => OpInfo {
            node_kind: NodeKind::PostMatch,
            prec: 100,
        },
        TokenKind::KwDo => OpInfo {
            node_kind: NodeKind::PostDo,
            prec: 100,
        },

        // Projection / view (precedence 110)
        TokenKind::Dot => OpInfo {
            node_kind: NodeKind::Projection,
            prec: 110,
        },
        TokenKind::Quote => OpInfo {
            node_kind: NodeKind::View,
            prec: 110,
        },

        _ => OpInfo {
            node_kind: NodeKind::Invalid,
            prec: -1,
        },
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

pub fn try_expr(p: &mut Parser) -> ParseResult {
    try_expr_with_option(p, ExprOption::new())
}

pub fn try_expr_without_extended_call(p: &mut Parser) -> ParseResult {
    try_expr_with_option(p, ExprOption::without_extended_call())
}

pub fn try_expr_with_option(p: &mut Parser, option: ExprOption) -> ParseResult {
    let _g = p.enter();
    let result = try_expr_pratt(p, 0, option);
    result
}

// ---------------------------------------------------------------------------
// Pratt parser – unified infix / postfix expression parsing
// ---------------------------------------------------------------------------

/// Core Pratt parsing loop.
///
/// The key design insight: infix and postfix operators are handled uniformly.
/// For each operator token, we first try `try_post_expr` (which handles
/// postfix operations like call, projection, effect/error propagation).
/// If that returns NULL, we treat it as a binary infix operator.
fn try_expr_pratt(p: &mut Parser, min_prec: i32, option: ExprOption) -> ParseResult {
    // Parse prefix expression as the left operand
    let mut left = try_prefix_expr(p, option)?;
    if left.is_null() {
        return Ok(NULL);
    }

    loop {
        let token = p.peek_token();
        let op = get_expr_op_info(token.kind);

        if op.node_kind == NodeKind::Invalid || op.prec < min_prec {
            break;
        }

        // Try postfix expression first (call, projection, effect/error ops, etc.)
        match try_post_expr(p, token.kind, left, option) {
            Ok(node) if !node.is_null() => {
                left = node;
                continue;
            }
            Err(ParseErrorKind::ControlMeetExtendedCall) => {
                break; // Extended call blocked by option
            }
            Err(e) => return Err(e),
            Ok(_) => {} // NULL – fall through to infix
        }

        // Consume operator token and parse right operand (infix)
        p.next_token();
        let right = try_expr_pratt(p, op.prec + 1, option)?;
        if right.is_null() {
            let _ = p.err_with_label(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected a right operand after binary operator",
                p.next_token_span(),
                format!("found `{}`", p.peek_token().kind.lexeme()),
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
// Prefix expressions
// ---------------------------------------------------------------------------

fn try_prefix_expr(p: &mut Parser, option: ExprOption) -> ParseResult {
    let token = p.peek_token();
    match token.kind {
        // Atomic expressions
        TokenKind::Integer
        | TokenKind::IntBin
        | TokenKind::IntOct
        | TokenKind::IntHex
        | TokenKind::Real
        | TokenKind::RealSci
        | TokenKind::String
        | TokenKind::Char
        | TokenKind::Id
        | TokenKind::KwFalse
        | TokenKind::KwTrue
        | TokenKind::KwSelfCap
        | TokenKind::KwSelfLower
        | TokenKind::KwNull
        | TokenKind::Underscore => basic::try_atomic(p),

        TokenKind::FStringStart => try_fstring(p),

        TokenKind::LParen => try_unit_or_paren_or_tuple(p),
        TokenKind::LBracket => try_list(p),
        TokenKind::LBrace => try_object(p),
        TokenKind::Dot => try_prefix_range_or_symbol(p, option),
        TokenKind::Pipe => try_lambda(p, option),
        TokenKind::KwCase => try_case_map(p, option),
        TokenKind::Hash => try_effect_qualified_type(p),
        TokenKind::Bang => try_error_qualified_type(p),
        TokenKind::KwNot => try_prefix_unary(p, TokenKind::KwNot, NodeKind::BoolNot),
        TokenKind::KwError => try_prefix_unary(p, TokenKind::KwError, NodeKind::ErrorConstruction),
        TokenKind::Question => try_prefix_unary(p, TokenKind::Question, NodeKind::OptionalType),
        TokenKind::Minus => try_prefix_unary(p, TokenKind::Minus, NodeKind::Negative),

        TokenKind::KwFn => try_fn_type_expr(p),
        TokenKind::KwEffect | TokenKind::KwAsync => try_effect_type_expr(p),

        TokenKind::KwIf => statement::try_if_statement(p),
        TokenKind::KwWhen => statement::try_when_statement(p),
        TokenKind::KwReset => try_reset_expr(p),
        TokenKind::KwShift => try_shift_expr(p),

        _ => Ok(NULL),
    }
}

// ---------------------------------------------------------------------------
// Postfix expressions (unified infix/postfix dispatch)
// ---------------------------------------------------------------------------

fn try_post_expr(
    p: &mut Parser,
    kind: TokenKind,
    left: NodeIndex,
    option: ExprOption,
) -> ParseResult {
    match kind {
        TokenKind::LParen => parse_call_expr(p, left),
        TokenKind::LBrace => parse_extended_call_expr(p, left, option),
        TokenKind::Dot => parse_dot_expr(p, left),
        TokenKind::Quote => parse_quote_expr(p, left),
        TokenKind::Hash => parse_effect_handling_expr(p, left),
        TokenKind::Bang => parse_error_handling_expr(p, left),
        TokenKind::Question => parse_option_propagation_expr(p, left),
        TokenKind::KwMatch => parse_post_match_expr(p, left),
        TokenKind::KwMatches => parse_matches_expr(p, left, option),
        TokenKind::KwDo => parse_post_do_expr(p, left, option),
        _ => Ok(NULL),
    }
}

// ---------------------------------------------------------------------------
// Individual expression parsers
// ---------------------------------------------------------------------------

fn parse_call_expr(p: &mut Parser, left: NodeIndex) -> ParseResult {
    let _g = p.enter();
    let mut args = try_multi_with_bracket(
        p,
        &[
            Rule::comma("named argument", basic::try_named_arg),
            Rule::comma("optional argument", try_property_assign),
            Rule::comma("function argument", try_expr),
        ],
        TokenKind::LParen,
        TokenKind::RParen,
    )?;
    // Desugar tacit lambda placeholders (_0, _1, ...) in arguments.
    for arg in args.iter_mut() {
        *arg = maybe_wrap_tacit_lambda(p, *arg);
    }
    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::Call, span)
        .add_child(left)
        .add_multi_children(&args)
        .build();
    Ok(idx)
}

fn parse_extended_call_expr(p: &mut Parser, left: NodeIndex, option: ExprOption) -> ParseResult {
    if option.no_extended_call {
        return Err(ParseErrorKind::ControlMeetExtendedCall);
    }
    let _g = p.enter();
    let args = try_multi_with_bracket(
        p,
        &[
            Rule::comma("property argument", try_property),
            Rule::comma("expression argument", try_expr),
        ],
        TokenKind::LBrace,
        TokenKind::RBrace,
    )?;
    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::ExtendedCall, span)
        .add_child(left)
        .add_multi_children(&args)
        .build();
    Ok(idx)
}

fn parse_dot_expr(p: &mut Parser, left: NodeIndex) -> ParseResult {
    let _g = p.enter();
    p.expect_token(TokenKind::Dot)?;

    let next = p.peek_token();
    let result = match next.kind {
        TokenKind::KwUse => parse_handler_apply_expr(p, left),
        TokenKind::KwAs => parse_type_cast_expr(p, left),
        TokenKind::Dot => parse_range_expr(p, left),
        _ => parse_projection_expr(p, left),
    };
    result
}

/// `expr ' id` — view, same shape as projection (`expr . id`).
fn parse_quote_expr(p: &mut Parser, left: NodeIndex) -> ParseResult {
    let _g = p.enter();
    p.expect_token(TokenKind::Quote)?;
    parse_view_expr(p, left)
}

fn parse_handler_apply_expr(p: &mut Parser, left: NodeIndex) -> ParseResult {
    p.expect_token(TokenKind::KwUse)?;
    p.expect_token(TokenKind::LParen)?;

    let handler = try_expr(p)?;
    if handler.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected expression after `(` in handler apply",
        );
    }

    p.expect_token(TokenKind::RParen)?;

    let span = p.current_span();
    Ok(p.ast()
        .builder(NodeKind::HandlerApplication, span)
        .add_child(left)
        .add_child(handler)
        .build())
}

fn parse_projection_expr(p: &mut Parser, left: NodeIndex) -> ParseResult {
    let id = basic::try_id(p)?;
    if id.is_null() {
        let _ = p.err_with_label(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected identifier after `.` in projection expression",
            p.next_token_span(),
            format!("found `{}`", p.peek_token().kind.lexeme()),
        );
    }
    let span = p.current_span();
    Ok(p.ast()
        .builder(NodeKind::Projection, span)
        .add_child(left)
        .add_child(id)
        .build())
}

/// After `'`, parse the view name id (operator already consumed by [`parse_quote_expr`]).
fn parse_view_expr(p: &mut Parser, left: NodeIndex) -> ParseResult {
    let id = basic::try_id(p)?;
    if id.is_null() {
        let _ = p.err_with_label(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected identifier after `'` in view expression",
            p.next_token_span(),
            format!("found `{}`", p.peek_token().kind.lexeme()),
        );
    }
    let span = p.current_span();
    Ok(p.ast()
        .builder(NodeKind::View, span)
        .add_child(left)
        .add_child(id)
        .build())
}

fn parse_type_cast_expr(p: &mut Parser, left: NodeIndex) -> ParseResult {
    p.expect_token(TokenKind::KwAs)?;
    p.expect_token(TokenKind::LParen)?;
    let ty = try_expr(p)?;
    if ty.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected type expression after `(` in type cast",
        );
    }
    p.expect_token(TokenKind::RParen)?;
    let span = p.current_span();
    Ok(p.ast()
        .builder(NodeKind::TypeCast, span)
        .add_child(left)
        .add_child(ty)
        .build())
}

fn parse_range_expr(p: &mut Parser, left: NodeIndex) -> ParseResult {
    // We're positioned after the first dot (consumed by parse_dot_expr).
    // The second dot is the next token.
    if p.peek(&[TokenKind::Eq]) {
        // ..=expr  (but first dot already consumed, so this is .=)
        p.next_token(); // consume =
        let end = try_expr_without_extended_call(p)?;
        if end.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected expression after `..=`",
            );
        }
        let span = p.current_span();
        Ok(p.ast()
            .builder(NodeKind::RangeFromToInclusive, span)
            .add_child(left)
            .add_child(end)
            .build())
    } else {
        p.next_token(); // consume second dot
        let end = try_expr_without_extended_call(p)?;
        if !end.is_null() {
            let span = p.current_span();
            Ok(p.ast()
                .builder(NodeKind::RangeFromTo, span)
                .add_child(left)
                .add_child(end)
                .build())
        } else {
            let span = p.current_span();
            Ok(p.ast()
                .builder(NodeKind::RangeFrom, span)
                .add_child(left)
                .build())
        }
    }
}

fn parse_effect_handling_expr(p: &mut Parser, left: NodeIndex) -> ParseResult {
    let _g = p.enter();
    p.expect_token(TokenKind::Hash)?;

    if p.peek_token().kind != TokenKind::LBrace {
        let span = p.current_span();
        let idx = p
            .ast()
            .builder(NodeKind::EffectPropagation, span)
            .add_child(left)
            .build();
        return Ok(idx);
    }

    let arms = try_multi_with_bracket(
        p,
        &[Rule::comma("effect handling arm", try_pattern_arm)],
        TokenKind::LBrace,
        TokenKind::RBrace,
    )?;
    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::EffectElimination, span)
        .add_child(left)
        .add_multi_children(&arms)
        .build();
    Ok(idx)
}

fn parse_error_handling_expr(p: &mut Parser, left: NodeIndex) -> ParseResult {
    let _g = p.enter();
    p.expect_token(TokenKind::Bang)?;

    if p.peek_token().kind != TokenKind::LBrace {
        let span = p.current_span();
        let idx = p
            .ast()
            .builder(NodeKind::ErrorPropagation, span)
            .add_child(left)
            .build();
        return Ok(idx);
    }

    let arms = try_multi_with_bracket(
        p,
        &[
            Rule::comma("catching arm", try_catch_arm),
            Rule::comma("error handling arm", try_pattern_arm),
        ],
        TokenKind::LBrace,
        TokenKind::RBrace,
    )?;
    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::ErrorElimination, span)
        .add_child(left)
        .add_multi_children(&arms)
        .build();
    Ok(idx)
}

fn parse_option_propagation_expr(p: &mut Parser, left: NodeIndex) -> ParseResult {
    let _g = p.enter();
    p.expect_token(TokenKind::Question)?;
    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::OptionPropagation, span)
        .add_child(left)
        .build();
    Ok(idx)
}

fn parse_post_match_expr(p: &mut Parser, left: NodeIndex) -> ParseResult {
    let _g = p.enter();
    p.expect_token(TokenKind::KwMatch)?;
    let arms = try_multi_in_block(p, &[Rule::semicolon("match arm", try_pattern_arm)])?;
    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::PostMatch, span)
        .add_child(left)
        .add_multi_children(&arms)
        .build();
    Ok(idx)
}

fn parse_matches_expr(p: &mut Parser, left: NodeIndex, option: ExprOption) -> ParseResult {
    let _g = p.enter();
    p.expect_token(TokenKind::KwMatches)?;

    let pat = if option.no_extended_call {
        pattern::try_pattern_without_extended_call(p)?
    } else {
        pattern::try_pattern(p)?
    };

    if pat.is_null() {
        let _ = p.err_with_label(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected pattern after `matches`",
            p.next_token_span(),
            format!("found `{}`", p.peek_token().kind.lexeme()),
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::BoolMatches, span)
        .add_child(left)
        .add_child(pat)
        .build();
    Ok(idx)
}

fn parse_post_do_expr(p: &mut Parser, left: NodeIndex, option: ExprOption) -> ParseResult {
    let _g = p.enter();
    p.expect_token(TokenKind::KwDo)?;

    if p.peek_token().kind == TokenKind::Pipe {
        let lambda = try_lambda(p, option)?;
        let span = p.current_span();
        let idx = p
            .ast()
            .builder(NodeKind::PostDo, span)
            .add_child(left)
            .add_child(lambda)
            .build();
        return Ok(idx);
    }

    let block = statement::try_block(p)?;
    if block.is_null() {
        let _ = p.err_with_label(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected lambda or block after `do`",
            p.next_token_span(),
            format!("found `{}`", p.peek_token().kind.lexeme()),
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::PostDo, span)
        .add_child(left)
        .add_child(block)
        .build();
    Ok(idx)
}

// ---------------------------------------------------------------------------
// Compound prefix parsers
// ---------------------------------------------------------------------------

/// `()` | `(expr)` | `(expr, expr, ...)`
fn try_unit_or_paren_or_tuple(p: &mut Parser) -> ParseResult {
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

    let first = try_expr(p)?;
    if first.is_null() {
        let _ = p.err_with_label(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected expression in parenthesis or tuple",
            p.next_token_span(),
            format!("found `{}`", p.peek_token().kind.lexeme()),
        );
    }

    if p.eat_token(TokenKind::Comma) {
        let mut elems = vec![first];
        let rest = basic::try_multi(p, &[Rule::comma("tuple element", try_expr)])?;
        elems.extend(rest);
        p.expect_token(TokenKind::RParen)?;
        let span = p.current_span();
        let idx = p
            .ast()
            .builder(NodeKind::Tuple, span)
            .add_multi_children(&elems)
            .build();
        return Ok(idx);
    }

    // Parenthesized expression
    p.expect_token(TokenKind::RParen)?;
    Ok(first)
}

/// `[ expr, ... ]`
fn try_list(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    if !p.peek(&[TokenKind::LBracket]) {
        return Ok(NULL);
    }

    let elems = try_multi_with_bracket(
        p,
        &[Rule::comma("list element", try_expr)],
        TokenKind::LBracket,
        TokenKind::RBracket,
    )?;

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::ListOf, span)
        .add_multi_children(&elems)
        .build();
    Ok(idx)
}

/// `{ prop: val, ... }`
fn try_object(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    if !p.peek(&[TokenKind::LBrace]) {
        return Ok(NULL);
    }

    let elems = try_multi_with_bracket(
        p,
        &[
            Rule::comma("object property", try_property),
            Rule::comma("object element", try_expr),
        ],
        TokenKind::LBrace,
        TokenKind::RBrace,
    )?;

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::Object, span)
        .add_multi_children(&elems)
        .build();
    Ok(idx)
}

/// `..` / `..expr` / `..=expr` / `.id` (symbol)
fn try_prefix_range_or_symbol(p: &mut Parser, option: ExprOption) -> ParseResult {
    let _g = p.enter();

    if p.peek(&[TokenKind::Dot, TokenKind::Dot]) {
        p.eat_tokens(1); // consume first dot (one is consumed here since peek matched)
        // Actually we need to eat both dots
        p.eat_tokens(1); // consume second dot

        if p.eat_token(TokenKind::Eq) {
            // ..=expr
            let to = try_expr_with_option(p, option)?;
            if to.is_null() {
                let _ = p.err(
                    ParseErrorKind::InvalidSyntax,
                    p.next_token_span(),
                    "Expected expression after `..=`",
                );
            }
            let span = p.current_span();
            let idx = p
                .ast()
                .builder(NodeKind::RangeToInclusive, span)
                .add_child(to)
                .build();
            return Ok(idx);
        }

        let to = try_expr_with_option(p, option)?;
        if to.is_null() {
            // ..  (full range)
            let span = p.current_span();
            let idx = p.ast().builder(NodeKind::RangeFull, span).build();
            return Ok(idx);
        }

        // ..expr (range to)
        let span = p.current_span();
        let idx = p
            .ast()
            .builder(NodeKind::RangeTo, span)
            .add_child(to)
            .build();
        return Ok(idx);
    }

    // Otherwise try symbol: .id
    let result = basic::try_symbol(p);
    result
}

fn try_prefix_unary(p: &mut Parser, token_kind: TokenKind, node_kind: NodeKind) -> ParseResult {
    let _g = p.enter();

    if !p.eat_token(token_kind) {
        return Ok(NULL);
    }

    let operand = try_expr_without_extended_call(p)?;
    if operand.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected expression after unary operator",
        );
    }

    let span = p.current_span();
    let idx = p.ast().builder(node_kind, span).add_child(operand).build();
    Ok(idx)
}

fn try_effect_qualified_type(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    if !p.eat_token(TokenKind::Hash) {
        return Ok(NULL);
    }

    let effs = try_expr_pratt(p, 90, ExprOption::new())?;
    if effs.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected effect expression after `#`",
        );
    }

    let inner = try_expr_without_extended_call(p)?;
    if inner.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected inner type after effect expression",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::EffectQualifiedType, span)
        .add_child(effs)
        .add_child(inner)
        .build();
    Ok(idx)
}

fn try_error_qualified_type(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    if !p.eat_token(TokenKind::Bang) {
        return Ok(NULL);
    }

    let errs = try_expr_pratt(p, 90, ExprOption::new())?;
    if errs.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected error type expression after `!`",
        );
    }

    let inner = try_expr_without_extended_call(p)?;
    if inner.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected inner type after error type expression",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::ErrorQualifiedType, span)
        .add_child(errs)
        .add_child(inner)
        .build();
    Ok(idx)
}

/// `fn (param_type*) (-> return_type)?`  — function type expression
fn try_fn_type_expr(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    if !p.eat_token(TokenKind::KwFn) {
        return Ok(NULL);
    }

    // Disambiguate: fn followed by ( is a type expr; fn followed by id is a definition
    if p.peek_token().kind != TokenKind::LParen {
        return Ok(NULL);
    }

    let params = try_multi_with_bracket(
        p,
        &[Rule::comma("parameter type", try_expr)],
        TokenKind::LParen,
        TokenKind::RParen,
    )?;

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::FnType, span)
        .add_multi_children(&params)
        .build();
    Ok(idx)
}

/// `async? effect (param_type*) (-> return_type)?`  — effect type expression
fn try_effect_type_expr(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    // Allow optional `async` prefix
    let _is_async = p.eat_token(TokenKind::KwAsync);

    if !p.eat_token(TokenKind::KwEffect) {
        return Ok(NULL);
    }

    // Disambiguate: effect followed by ( is a type expr; effect followed by id is a definition
    if p.peek_token().kind != TokenKind::LParen {
        return Ok(NULL);
    }

    let params = try_multi_with_bracket(
        p,
        &[Rule::comma("parameter type", try_expr)],
        TokenKind::LParen,
        TokenKind::RParen,
    )?;

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::EffectType, span)
        .add_multi_children(&params)
        .build();
    Ok(idx)
}

fn try_reset_expr(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    if !p.eat_token(TokenKind::KwReset) {
        return Ok(NULL);
    }

    let prompt = try_expr_with_option(p, ExprOption::without_extended_call())?;
    if prompt.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected prompt tag expression after `reset`",
        );
    }

    let body = try_block_or_statement(p)?;
    if body.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected block or statement after `reset`",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::ResetExpr, span)
        .add_child(prompt)
        .add_child(body)
        .build();
    Ok(idx)
}

fn try_shift_expr(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    if !p.eat_token(TokenKind::KwShift) {
        return Ok(NULL);
    }

    let prompt = try_expr_with_option(p, ExprOption::without_extended_call())?;
    if prompt.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected prompt tag expression after `shift`",
        );
    }

    p.expect_token(TokenKind::Comma)?;

    let k_name = basic::try_id(p)?;
    if k_name.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected continuation identifier after `,`",
        );
    }

    let body = try_block_or_statement(p)?;
    if body.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected block or statement after shift continuation name",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::ShiftExpr, span)
        .add_child(prompt)
        .add_child(k_name)
        .add_child(body)
        .build();
    Ok(idx)
}

/// `|params| (-> return_type)? block_or_statement`
fn try_lambda(p: &mut Parser, _option: ExprOption) -> ParseResult {
    let _g = p.enter();

    if !p.peek(&[TokenKind::Pipe]) {
        return Ok(NULL);
    }

    let params = try_multi_with_bracket(
        p,
        &[Rule::comma("lambda parameter", basic::try_parameter)],
        TokenKind::Pipe,
        TokenKind::Pipe,
    )?;

    let return_type = definition::try_return_type(p)?;

    let body = try_block_or_statement(p)?;
    if body.is_null() {
        let _ = p.err_with_label(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected block or statement after lambda parameters",
            p.next_token_span(),
            format!("found `{}`", p.peek_token().kind.lexeme()),
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::Lambda, span)
        .add_child(body)
        .add_child(return_type)
        .add_multi_children(&params)
        .build();
    Ok(idx)
}

/// `case pattern => body` (possibly chained with `|`)
fn try_case_map(p: &mut Parser, option: ExprOption) -> ParseResult {
    let _g = p.enter();

    if !p.peek(&[TokenKind::KwCase]) {
        return Ok(NULL);
    }

    let mut left = try_single_case_map(p, option)?;
    if left.is_null() {
        return Ok(NULL);
    }

    while p.eat_token(TokenKind::Pipe) {
        let right = try_single_case_map(p, option)?;
        if right.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected another case map arm after `|`",
            );
        }
        let span = p.current_span();
        left = p
            .ast()
            .builder(NodeKind::CaseAlternative, span)
            .add_child(left)
            .add_child(right)
            .build();
    }

    Ok(left)
}

fn try_single_case_map(p: &mut Parser, _option: ExprOption) -> ParseResult {
    let _g = p.enter();

    if !p.eat_token(TokenKind::KwCase) {
        return Ok(NULL);
    }

    let pat = pattern::try_pattern(p)?;
    if pat.is_null() {
        let _ = p.err_with_label(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected pattern after `case`",
            p.next_token_span(),
            format!("found `{}`", p.peek_token().kind.lexeme()),
        );
    }

    p.expect_token(TokenKind::FatArrow)?;

    let body = try_block_or_statement(p)?;
    if body.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected block or statement after `=>`",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::CaseMap, span)
        .add_child(pat)
        .add_child(body)
        .build();
    Ok(idx)
}

// ---------------------------------------------------------------------------
// Tacit lambda desugaring — `_0 < _1` → `|_0, _1| _0 < _1`
// ---------------------------------------------------------------------------

/// Check if a node text matches the tacit lambda placeholder pattern `_N`.
/// Returns `Some(N)` if it matches, `None` otherwise.
fn tacit_placeholder_index(name: &str) -> Option<usize> {
    let bytes = name.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'_' && bytes[1..].iter().all(|b| b.is_ascii_digit()) {
        name[1..].parse::<usize>().ok()
    } else {
        None
    }
}

/// Scan an AST subtree for tacit lambda placeholders (`_0`, `_1`, ...).
/// Returns the maximum placeholder index found + 1 (i.e., the arity), or 0 if
/// no placeholders were found.
fn scan_tacit_placeholders(ast: &ast::Ast, node_idx: NodeIndex) -> usize {
    if node_idx.is_null() {
        return 0;
    }
    let node = ast.node(node_idx);
    let mut max_plus_one = 0usize;

    if node.kind == NodeKind::Id {
        let name = str_interner::get(node.str_id);
        if let Some(idx) = tacit_placeholder_index(&name) {
            max_plus_one = max_plus_one.max(idx + 1);
        }
    }

    for &child in ast.fixed_children(node_idx) {
        max_plus_one = max_plus_one.max(scan_tacit_placeholders(ast, child));
    }
    for &child in ast.multi_children(node_idx) {
        max_plus_one = max_plus_one.max(scan_tacit_placeholders(ast, child));
    }
    max_plus_one
}

/// Wrap an argument expression in a Lambda if it contains tacit placeholders.
///
/// `_0 + _1` becomes `|_0, _1| _0 + _1` — the placeholders become regular
/// `ParamTyped` parameters whose names are `_0`, `_1`, etc.
fn maybe_wrap_tacit_lambda(p: &mut Parser, arg: NodeIndex) -> NodeIndex {
    if arg.is_null() {
        return arg;
    }
    let arity = scan_tacit_placeholders(p.ast(), arg);
    if arity == 0 {
        return arg;
    }

    let span = p.ast().node(arg).span;

    // Build ParamTyped nodes for _0, _1, ..., _(arity-1).
    let mut params = Vec::with_capacity(arity);
    for i in 0..arity {
        let name = format!("_{i}");
        let str_id = str_interner::intern(&name);
        // Pattern node (Id).
        let pat = {
            let idx = p.ast().builder(NodeKind::Id, span).build();
            // Set the str_id on the built node.
            p.ast().nodes[idx.0 as usize].str_id = str_id;
            idx
        };
        // No type annotation.
        let param_idx = p
            .ast()
            .builder(NodeKind::ParamTyped, span)
            .add_child(pat)
            .add_child(NULL)
            .build();
        params.push(param_idx);
    }

    // Build the Lambda node: fixed = [body, return_type], multi = params.
    p.ast()
        .builder(NodeKind::Lambda, span)
        .add_child(arg) // body
        .add_child(NULL) // return type
        .add_multi_children(&params)
        .build()
}

// ---------------------------------------------------------------------------
// F-string (interpolated string) parser
// ---------------------------------------------------------------------------
//
// Token stream from lexer:
//   FStringStart  FStringLiteral?  (FStringExprStart  <expr tokens>  FStringExprEnd  FStringLiteral?)*  FStringEnd
//
// We build a `FStringConcat` node whose multi-children are alternating
// `Str` nodes (literal text segments) and arbitrary expression nodes.

fn try_fstring(p: &mut Parser) -> ParseResult {
    let span = p.current_span();
    // Consume FStringStart
    p.expect_token(TokenKind::FStringStart)?;

    let mut parts: Vec<NodeIndex> = Vec::new();

    loop {
        let tok = p.peek_token();
        match tok.kind {
            TokenKind::FStringEnd => {
                p.next_token();
                break;
            }
            TokenKind::FStringLiteral => {
                // Intern the raw text of this segment (no surrounding quotes).
                let seg_span = p.current_span();
                let raw = p.next_token_text().to_owned();
                p.next_token(); // consume FStringLiteral
                let str_id = str_interner::intern(&raw);
                let seg = p.ast().builder(NodeKind::Str, seg_span).set_str_id(str_id).build();
                parts.push(seg);
            }
            TokenKind::FStringExprStart => {
                p.next_token(); // consume `{`
                // Parse the inner expression.
                let expr = try_expr(p)?;
                parts.push(expr);
                p.expect_token(TokenKind::FStringExprEnd)?;
            }
            TokenKind::Eof => {
                let span = p.next_token_span();
                p.err(ParseErrorKind::UnexpectedToken, span, "unterminated f-string")?;
                unreachable!()
            }
            _ => {
                let span = p.next_token_span();
                p.err(ParseErrorKind::UnexpectedToken, span, "unexpected token in f-string")?;
                unreachable!()
            }
        }
    }

    // Build FStringConcat with all parts as multi-children.
    Ok(p.ast()
        .builder(NodeKind::FStringConcat, span)
        .add_multi_children(&parts)
        .build())
}
