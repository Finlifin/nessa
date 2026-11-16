use ast::NodeKind;
use lexer::token::TokenKind;

use crate::basic::{self, Rule, try_multi_in_block};
use crate::definition;
use crate::error::{ParseError, ParseErrorKind, ParseResult};
use crate::expr;
use crate::heuristics;
use crate::parser::{NULL, Parser};
use crate::pattern;

// ---------------------------------------------------------------------------
// Blocks
// ---------------------------------------------------------------------------

/// Parse a block: `{ stmts }` | `: indent stmts outdent` | `indent stmts outdent`
pub fn try_block(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    let stmts = try_multi_in_block(
        p,
        &[Rule::semicolon("statement", try_definition_or_statement)],
    )?;

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::Block, span)
        .add_multi_children(&stmts)
        .build();
    Ok(idx)
}

// ---------------------------------------------------------------------------
// Definition or statement
// ---------------------------------------------------------------------------

/// Dispatches to either a definition or a statement.
pub fn try_definition_or_statement(p: &mut Parser) -> ParseResult {
    heuristics::try_skip_foreign_comments(p);
    let heuristic = heuristics::try_foreign_statement_or_def_heuristic(p)?;
    if !heuristic.is_null() {
        return Ok(heuristic);
    }

    let token = p.peek_token();
    match token.kind {
        // Definitions
        TokenKind::KwFn => definition::try_fn_def(p),
        TokenKind::KwEffect => definition::try_effect_def(p),
        TokenKind::KwHandles => definition::try_handles_def(p),
        TokenKind::KwStruct => definition::try_struct_def(p),
        TokenKind::KwEnum => definition::try_enum_def(p),
        TokenKind::KwTrait => definition::try_trait_def(p),
        TokenKind::KwImpl => definition::try_impl_def(p),
        TokenKind::KwExtend => definition::try_extend_def(p),
        TokenKind::KwAsync => definition::try_effect_def(p),
        TokenKind::KwDerive => definition::try_derive_def(p),
        TokenKind::KwTypealias => definition::try_type_alias_def(p),
        TokenKind::KwNewtype => definition::try_newtype_def(p),
        TokenKind::KwMod => definition::try_mod_def(p),
        TokenKind::KwUse => definition::try_use_statement(p),
        TokenKind::KwPub => definition::try_pub_def(p),
        TokenKind::KwPrivate => definition::try_private_def(p),
        TokenKind::KwGlobal => definition::try_global_def(p),
        TokenKind::KwAssoc => definition::try_assoc_def(p),
        // Statements
        _ => try_statement(p),
    }
}

// ---------------------------------------------------------------------------
// Statements
// ---------------------------------------------------------------------------

fn try_statement(p: &mut Parser) -> ParseResult {
    let token = p.peek_token();
    match token.kind {
        TokenKind::KwHandles => definition::try_handles_def(p),
        TokenKind::KwLet => try_let_statement(p),
        TokenKind::KwConst => try_const_statement(p),
        TokenKind::KwVar => try_var_statement(p),
        TokenKind::KwDefer => try_defer_statement(p),
        TokenKind::KwReturn => try_return_statement(p),
        TokenKind::KwResume => try_resume_statement(p),
        TokenKind::KwBreak => try_break_statement(p),
        TokenKind::KwContinue => try_continue_statement(p),
        TokenKind::KwFor => try_for_statement(p),
        TokenKind::KwWhile => try_while_statement(p),
        TokenKind::KwIf => try_if_statement(p),
        TokenKind::KwWhen => try_when_statement(p),
        _ => try_expr_statement(p),
    }
}

// ---------------------------------------------------------------------------
// Variable declarations
// ---------------------------------------------------------------------------

/// `let pattern (: type)? = expr`
fn try_let_statement(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwLet) {
        return Ok(NULL);
    }
    parse_binding(p, NodeKind::LetDecl)
}

/// `const pattern (: type)? = expr`
fn try_const_statement(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwConst) {
        return Ok(NULL);
    }
    parse_binding(p, NodeKind::ConstDecl)
}

/// `var pattern (: type)? = expr`
fn try_var_statement(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwVar) {
        return Ok(NULL);
    }
    parse_binding(p, NodeKind::VarDecl)
}

fn parse_binding(p: &mut Parser, decl_kind: NodeKind) -> ParseResult {
    heuristics::try_eat_binding_mut(p, decl_kind);

    let pat = pattern::try_pattern(p)?;
    if pat.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected pattern in binding declaration",
        );
    }

    let type_ann = if p.eat_token(TokenKind::Colon) {
        let ty = p.with_type_context(expr::try_expr_without_extended_call)?;
        if ty.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected type expression after `:` in declaration",
            );
        }
        ty
    } else {
        NULL
    };

    p.expect_token(TokenKind::Eq)?;

    let value = expr::try_expr(p)?;
    if value.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected expression after `=` in declaration",
        );
    }

    let else_branch = if p.eat_token(TokenKind::KwElse) {
        let b = basic::try_block_or_statement(p)?;
        if b.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected block or statement after `else` in declaration",
            );
        }
        b
    } else {
        NULL
    };

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(decl_kind, span)
        .add_child(pat)
        .add_child(type_ann)
        .add_child(value)
        .add_child(else_branch)
        .build();
    Ok(idx)
}

// ---------------------------------------------------------------------------
// Defer
// ---------------------------------------------------------------------------

/// `defer block_or_statement`
fn try_defer_statement(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwDefer) {
        return Ok(NULL);
    }

    let body = basic::try_block_or_statement(p)?;
    if body.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected block or statement after `defer`",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::DeferStatement, span)
        .add_child(body)
        .build();
    Ok(idx)
}

// ---------------------------------------------------------------------------
// Return / Resume / Break / Continue
// ---------------------------------------------------------------------------

/// `return expr? (if expr)?`
fn try_return_statement(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwReturn) {
        return Ok(NULL);
    }

    let val = expr::try_expr_without_extended_call(p)?;

    let guard = if p.eat_token(TokenKind::KwIf) {
        let cond = expr::try_expr(p)?;
        if cond.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected expression after `return ... if`",
            );
        }
        cond
    } else {
        NULL
    };

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::ReturnStatement, span)
        .add_child(val)
        .add_child(guard)
        .build();
    Ok(idx)
}

/// `resume expr? (if expr)?`
fn try_resume_statement(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwResume) {
        return Ok(NULL);
    }

    let val = expr::try_expr_without_extended_call(p)?;

    let guard = if p.eat_token(TokenKind::KwIf) {
        let cond = expr::try_expr(p)?;
        if cond.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected expression after `resume ... if`",
            );
        }
        cond
    } else {
        NULL
    };

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::ResumeStatement, span)
        .add_child(val)
        .add_child(guard)
        .build();
    Ok(idx)
}

/// `break id? (if expr)?`
fn try_break_statement(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwBreak) {
        return Ok(NULL);
    }

    let label = basic::try_id(p)?;

    let guard = if p.eat_token(TokenKind::KwIf) {
        let cond = expr::try_expr(p)?;
        if cond.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected expression after `break ... if`",
            );
        }
        cond
    } else {
        NULL
    };

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::BreakStatement, span)
        .add_child(label)
        .add_child(guard)
        .build();
    Ok(idx)
}

/// `continue id? (if expr)?`
fn try_continue_statement(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwContinue) {
        return Ok(NULL);
    }

    let label = basic::try_id(p)?;

    let guard = if p.eat_token(TokenKind::KwIf) {
        let cond = expr::try_expr(p)?;
        if cond.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected expression after `continue ... if`",
            );
        }
        cond
    } else {
        NULL
    };

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::ContinueStatement, span)
        .add_child(label)
        .add_child(guard)
        .build();
    Ok(idx)
}

// ---------------------------------------------------------------------------
// Loops
// ---------------------------------------------------------------------------

/// `for (: id)? pattern in expr block`
fn try_for_statement(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwFor) {
        return Ok(NULL);
    }

    // Optional label: `for : label pattern in expr block`
    let label = if p.eat_token(TokenKind::Colon) {
        let lbl = basic::try_id(p)?;
        if lbl.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected label name after `:` in `for` loop",
            );
        }
        lbl
    } else {
        NULL
    };

    let pat = pattern::try_refutable_pattern_without_extended_call(p)?;
    if pat.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected pattern after `for`",
        );
    }

    p.expect_token(TokenKind::KwIn)?;

    let iter = expr::try_expr_without_extended_call(p)?;
    if iter.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected iterable expression after `in`",
        );
    }

    let body = basic::try_block_or_statement(p)?;
    if body.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected block or statement as `for` body",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::ForLoop, span)
        .add_child(label)
        .add_child(pat)
        .add_child(iter)
        .add_child(body)
        .build();
    Ok(idx)
}

/// `while (: id)? expr block`
fn try_while_statement(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwWhile) {
        return Ok(NULL);
    }

    // Optional label: `while : label expr block`
    let label = if p.eat_token(TokenKind::Colon) {
        let lbl = basic::try_id(p)?;
        if lbl.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected label name after `:` in `while` loop",
            );
        }
        lbl
    } else {
        NULL
    };

    let cond = expr::try_expr_without_extended_call(p)?;
    if cond.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected condition expression after `while`",
        );
    }
    let cond = heuristics::try_condition_assign_recovery(p, cond, "while")?;

    let body = basic::try_block_or_statement(p)?;
    if body.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected block or statement as `while` body",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::WhileLoop, span)
        .add_child(label)
        .add_child(cond)
        .add_child(body)
        .build();
    Ok(idx)
}

// ---------------------------------------------------------------------------
// If / When (expressions/statements)
// ---------------------------------------------------------------------------

/// `if expr block (else if expr block)* (else block)?`
pub fn try_if_statement(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwIf) {
        return Ok(NULL);
    }
    parse_if_after_keyword(p)
}

pub(crate) fn parse_if_after_keyword(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    let cond = expr::try_expr_without_extended_call(p)?;
    if cond.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected condition after `if`",
        );
    }
    let cond = heuristics::try_condition_assign_recovery(p, cond, "if")?;

    let then_block = basic::try_block_or_statement(p)?;
    if then_block.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected block after `if` condition",
        );
    }

    let else_branch = if p.eat_token(TokenKind::KwElse) {
        if p.peek_token().kind == TokenKind::KwIf {
            try_if_statement(p)?
        } else {
            let block = basic::try_block_or_statement(p)?;
            if block.is_null() {
                let _ = p.err(
                    ParseErrorKind::InvalidSyntax,
                    p.next_token_span(),
                    "Expected block after `else`",
                );
            }
            block
        }
    } else if p.peek_token().kind == TokenKind::Id
        && matches!(p.next_token_text(), "elif" | "elsif")
        && heuristics::looks_like_elif_branch(p)
    {
        let kw_text = p.next_token_text().to_owned();
        let kw = p.next_token();
        let kw_span = p.token_span(kw);
        p.push_error(
            ParseError::new(
                ParseErrorKind::InvalidSyntax,
                kw_span,
                format!("Nessa uses `else if`, not `{kw_text}`"),
            )
            .add_help(format!("replace `{kw_text}` with `else if`")),
        );
        parse_if_after_keyword(p)?
    } else {
        NULL
    };

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::IfStatement, span)
        .add_child(cond)
        .add_child(then_block)
        .add_child(else_branch)
        .build();
    Ok(idx)
}

/// `when { (condition_arm | else_condition_arm)* }`
pub fn try_when_statement(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwWhen) {
        return Ok(NULL);
    }

    let arms = basic::try_required_block(
        p,
        "`when`",
        &[
            Rule::comma_or_semicolon("else condition arm", try_else_condition_arm),
            Rule::comma_or_semicolon("condition arm", try_condition_arm),
        ],
    )?;

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::WhenStatement, span)
        .add_multi_children(&arms)
        .build();
    Ok(idx)
}

/// `condition_expr => body`
fn try_condition_arm(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    let cond = expr::try_expr_without_extended_call(p)?;
    if cond.is_null() {
        return Ok(NULL);
    }

    heuristics::expect_fat_arrow_in_arm(p, "`when` condition arm")?;

    let body = basic::try_block_or_statement(p)?;
    if body.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected block or statement after `=>` in condition arm",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::ConditionArm, span)
        .add_child(cond)
        .add_child(body)
        .build();
    Ok(idx)
}

/// `else => body`
fn try_else_condition_arm(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    if !p.eat_token(TokenKind::KwElse) {
        return Ok(NULL);
    }

    heuristics::expect_fat_arrow_in_arm(p, "`else` condition arm")?;

    let body = basic::try_block_or_statement(p)?;
    if body.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected block or statement after `else =>`",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::ElseConditionArm, span)
        .add_child(body)
        .build();
    Ok(idx)
}

// ---------------------------------------------------------------------------
// Expression statement (including assignment)
// ---------------------------------------------------------------------------

fn try_expr_statement(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    let lhs = expr::try_expr(p)?;
    if lhs.is_null() {
        return Ok(NULL);
    }

    let token = p.peek_token();
    let assign_kind = match token.kind {
        TokenKind::Eq => Some(NodeKind::Assign),
        TokenKind::PlusEq => Some(NodeKind::AddAssign),
        TokenKind::MinusEq => Some(NodeKind::SubAssign),
        TokenKind::StarEq => Some(NodeKind::MulAssign),
        TokenKind::SlashEq => Some(NodeKind::DivAssign),
        TokenKind::PercentEq => Some(NodeKind::ModAssign),
        _ => None,
    };

    if let Some(kind) = assign_kind {
        p.next_token(); // consume assignment op
        let rhs = expr::try_expr(p)?;
        if rhs.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected expression after assignment operator",
            );
        }
        let span = p.current_span();
        let idx = p
            .ast()
            .builder(kind, span)
            .add_child(lhs)
            .add_child(rhs)
            .build();
        return Ok(idx);
    }

    Ok(lhs)
}
