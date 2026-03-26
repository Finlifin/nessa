use ast::NodeKind;
use lexer::token::TokenKind;

use crate::basic::{self, Rule, try_multi_in_block, try_multi_with_bracket};
use crate::error::{ParseErrorKind, ParseResult};
use crate::expr;
use crate::parser::{NULL, Parser};
use crate::statement;

extern crate str_interner;

// ---------------------------------------------------------------------------
// Visibility / scope modifiers (pub, global, assoc)
// ---------------------------------------------------------------------------

/// `pub <def>`
pub fn try_pub_def(p: &mut Parser) -> ParseResult {
    try_modifier_def(p, TokenKind::KwPub, NodeKind::PubDef)
}

/// `global <def>`
pub fn try_global_def(p: &mut Parser) -> ParseResult {
    try_modifier_def(p, TokenKind::KwGlobal, NodeKind::GlobalDecl)
}

/// `assoc <def>`
pub fn try_assoc_def(p: &mut Parser) -> ParseResult {
    try_modifier_def(p, TokenKind::KwAssoc, NodeKind::AssocDecl)
}

fn try_modifier_def(p: &mut Parser, keyword: TokenKind, node_kind: NodeKind) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(keyword) {
        return Ok(NULL);
    }

    let inner = statement::try_definition_or_statement(p)?;
    if inner.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected definition after modifier keyword",
        );
    }

    let span = p.current_span();
    let idx = p.ast().builder(node_kind, span).add_child(inner).build();
    Ok(idx)
}

// ---------------------------------------------------------------------------
// Function definition
// ---------------------------------------------------------------------------

/// `fn name(params) (-> return_type)? (handles expr)? ((= expr) | block)`
pub fn try_fn_def(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwFn) {
        return Ok(NULL);
    }

    let name = basic::try_id(p)?;
    if name.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected function name after `fn`",
        );
    }

    let params = try_multi_with_bracket(
        p,
        &[Rule::comma("function parameter", basic::try_parameter)],
        TokenKind::LParen,
        TokenKind::RParen,
    )?;

    let ret_type = try_return_type(p)?;
    let capability = try_capability(p)?;

    let body = basic::try_block_or_statement(p)?;
    if body.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected function body",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::FunctionDef, span)
        .add_child(name)
        .add_child(ret_type)
        .add_child(body)
        .add_child(capability)
        .add_multi_children(&params)
        .build();
    Ok(idx)
}

/// `-> type_expr`
pub fn try_return_type(p: &mut Parser) -> ParseResult {
    if !p.eat_token(TokenKind::Arrow) {
        return Ok(NULL);
    }

    let ty = expr::try_expr_without_extended_call(p)?;
    if ty.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected return type expression after `->`",
        );
    }

    Ok(ty)
}

/// `handles expr` — capability annotation for functions.
fn try_capability(p: &mut Parser) -> ParseResult {
    if !p.eat_token(TokenKind::KwHandles) {
        return Ok(NULL);
    }

    let cap = expr::try_expr_without_extended_call(p)?;
    if cap.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected effect expression after `handles`",
        );
    }

    Ok(cap)
}

// ---------------------------------------------------------------------------
// Effect definition
// ---------------------------------------------------------------------------

/// `async? effect name(params) (-> return_type)?`
pub fn try_effect_def(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    // Allow optional `async` prefix
    let is_async = p.eat_token(TokenKind::KwAsync);

    if !p.eat_token(TokenKind::KwEffect) {
        return Ok(NULL);
    }

    let name = basic::try_id(p)?;
    if name.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected effect name after `effect`",
        );
    }

    let params = try_multi_with_bracket(
        p,
        &[Rule::comma("effect parameter", basic::try_parameter)],
        TokenKind::LParen,
        TokenKind::RParen,
    )?;

    let ret_type = try_return_type(p)?;

    let kind = if is_async {
        NodeKind::AsyncEffectDef
    } else {
        NodeKind::EffectDef
    };

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(kind, span)
        .add_child(name)
        .add_child(ret_type)
        .add_multi_children(&params)
        .build();
    Ok(idx)
}

// ---------------------------------------------------------------------------
// Handles statement
// ---------------------------------------------------------------------------

/// `handles(expr)(param*) (-> expr)? ((= expr) | block)`
pub fn try_handles_def(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwHandles) {
        return Ok(NULL);
    }

    // Parse effect expression in parens: handles(effect_expr)
    p.expect_token(TokenKind::LParen)?;
    let effect = expr::try_expr(p)?;
    if effect.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected effect expression after `handles(`",
        );
    }
    p.expect_token(TokenKind::RParen)?;

    // Parse handler parameters: (param*)
    let params = try_multi_with_bracket(
        p,
        &[Rule::comma("handler parameter", basic::try_parameter)],
        TokenKind::LParen,
        TokenKind::RParen,
    )?;

    let ret_type = try_return_type(p)?;

    let body = basic::try_block_or_statement(p)?;
    if body.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected handler body",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::HandlesStatement, span)
        .add_child(effect)
        .add_child(ret_type)
        .add_child(body)
        .add_multi_children(&params)
        .build();
    Ok(idx)
}

// ---------------------------------------------------------------------------
// Struct definition
// ---------------------------------------------------------------------------

/// `struct name? block`
pub fn try_struct_def(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwStruct) {
        return Ok(NULL);
    }

    let name = basic::try_id(p)?;
    if name.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected struct name after `struct`",
        );
    }

    let defs = try_multi_in_block(
        p,
        &[
            Rule::comma("struct field", try_struct_field),
            Rule::semicolon(
                "definition or statement",
                statement::try_definition_or_statement,
            ),
        ],
    )?;

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::StructDef, span)
        .add_child(name)
        .add_multi_children(&defs)
        .build();
    Ok(idx)
}

/// `name: type (= default)?`
fn try_struct_field(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    let name = basic::try_id(p)?;
    if name.is_null() {
        return Ok(NULL);
    }

    p.expect_token(TokenKind::Colon)?;

    let ty = expr::try_expr_without_extended_call(p)?;
    if ty.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected type expression in struct field",
        );
    }

    let default = if p.eat_token(TokenKind::Eq) {
        let d = expr::try_expr(p)?;
        if d.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected default value expression after `=`",
            );
        }
        d
    } else {
        NULL
    };

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::StructField, span)
        .add_child(name)
        .add_child(ty)
        .add_child(default)
        .build();
    Ok(idx)
}

// ---------------------------------------------------------------------------
// Enum definition
// ---------------------------------------------------------------------------

/// `enum name { (enum_variant | statement)* }`
pub fn try_enum_def(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwEnum) {
        return Ok(NULL);
    }

    let name = basic::try_id(p)?;
    if name.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected enum name after `enum`",
        );
    }

    let variants = try_multi_in_block(
        p,
        &[
            Rule::comma("enum variant", try_enum_variant),
            Rule::semicolon(
                "definition or statement",
                statement::try_definition_or_statement,
            ),
        ],
    )?;

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::EnumDef, span)
        .add_child(name)
        .add_multi_children(&variants)
        .build();
    Ok(idx)
}

/// `variant_name(param*)` or `variant_name`
fn try_enum_variant(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    let name = basic::try_id(p)?;
    if name.is_null() {
        return Ok(NULL);
    }

    let params = if p.peek(&[TokenKind::LParen]) {
        try_multi_with_bracket(
            p,
            &[Rule::comma("variant parameter", basic::try_parameter)],
            TokenKind::LParen,
            TokenKind::RParen,
        )?
    } else {
        vec![]
    };

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::EnumVariant, span)
        .add_child(name)
        .add_multi_children(&params)
        .build();
    Ok(idx)
}

// ---------------------------------------------------------------------------
// Trait definition
// ---------------------------------------------------------------------------

/// `trait name ((expr+))? { (trait_def_fn | trait_derive_fn | statement)* }`
pub fn try_trait_def(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwTrait) {
        return Ok(NULL);
    }

    let name = basic::try_id(p)?;
    if name.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected trait name after `trait`",
        );
    }

    // Parse optional parent traits: (Parent1, Parent2, ...)
    let parents = {
        let parent_exprs = try_multi_with_bracket(
            p,
            &[Rule::comma("parent trait", expr::try_expr)],
            TokenKind::LParen,
            TokenKind::RParen,
        )?;
        if parent_exprs.is_empty() {
            NULL
        } else {
            let span = p.current_span();
            p.ast()
                .builder(NodeKind::ListOf, span)
                .add_multi_children(&parent_exprs)
                .build()
        }
    };

    let members = try_multi_in_block(p, &[Rule::semicolon("trait member", try_trait_member)])?;

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::TraitDef, span)
        .add_child(name)
        .add_child(parents)
        .add_multi_children(&members)
        .build();
    Ok(idx)
}

fn try_trait_member(p: &mut Parser) -> ParseResult {
    // `derive fn ...` — default implementation
    if p.peek(&[TokenKind::KwDerive]) {
        return try_trait_derive_fn(p);
    }
    // `assoc ...` — associated type / constant declaration
    if p.peek(&[TokenKind::KwAssoc]) {
        return try_assoc_def(p);
    }
    // `def fn ...` — required method (eat optional `def` keyword)
    if p.peek(&[TokenKind::KwDef]) {
        p.eat_token(TokenKind::KwDef);
    }
    try_fn_signature(p)
}

/// `fn name(params) (-> return_type)? (handles expr)?` (no body – for traits)
fn try_fn_signature(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwFn) {
        return Ok(NULL);
    }

    let name = basic::try_id(p)?;
    if name.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected function name in signature",
        );
    }

    let params = try_multi_with_bracket(
        p,
        &[Rule::comma("parameter", basic::try_parameter)],
        TokenKind::LParen,
        TokenKind::RParen,
    )?;

    let ret_type = try_return_type(p)?;
    let capability = try_capability(p)?;

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::TraitDefFn, span)
        .add_child(name)
        .add_child(ret_type)
        .add_child(capability)
        .add_multi_children(&params)
        .build();
    Ok(idx)
}

/// `derive fn name(params) (-> return_type)? (handles expr)? ((= expr) | block)`
fn try_trait_derive_fn(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwDerive) {
        return Ok(NULL);
    }
    if !p.eat_token(TokenKind::KwFn) {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected `fn` after `derive` in trait",
        );
    }

    let name = basic::try_id(p)?;
    if name.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected function name after `derive fn`",
        );
    }

    let params = try_multi_with_bracket(
        p,
        &[Rule::comma("parameter", basic::try_parameter)],
        TokenKind::LParen,
        TokenKind::RParen,
    )?;

    let ret_type = try_return_type(p)?;
    let capability = try_capability(p)?;

    let body = basic::try_block_or_statement(p)?;
    if body.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected body for `derive fn`",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::TraitDeriveFn, span)
        .add_child(name)
        .add_child(ret_type)
        .add_child(body)
        .add_child(capability)
        .add_multi_children(&params)
        .build();
    Ok(idx)
}

// ---------------------------------------------------------------------------
// Impl definition
// ---------------------------------------------------------------------------

/// `impl trait_expr for type_expr block` or `impl type_expr block`
pub fn try_impl_def(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwImpl) {
        return Ok(NULL);
    }

    let first_expr = expr::try_expr_without_extended_call(p)?;
    if first_expr.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected expression after `impl`",
        );
    }

    let (node_kind, trait_expr, type_expr) = if p.eat_token(TokenKind::KwFor) {
        let second_expr = expr::try_expr_without_extended_call(p)?;
        if second_expr.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected type expression after `for` in impl",
            );
        }
        // impl Trait for Type: first_expr = trait, second_expr = type
        (NodeKind::ImplTraitDef, first_expr, second_expr)
    } else {
        // impl Type: first_expr = type
        (NodeKind::ImplDef, NULL, first_expr)
    };

    let members = try_multi_in_block(
        p,
        &[Rule::semicolon(
            "impl member",
            statement::try_definition_or_statement,
        )],
    )?;

    let span = p.current_span();

    let idx = if node_kind == NodeKind::ImplTraitDef {
        // ImplTraitDef: [0] trait  [1] type  multi = members
        p.ast()
            .builder(NodeKind::ImplTraitDef, span)
            .add_child(trait_expr)
            .add_child(type_expr)
            .add_multi_children(&members)
            .build()
    } else {
        // ImplDef: [0] type  multi = members
        p.ast()
            .builder(NodeKind::ImplDef, span)
            .add_child(type_expr)
            .add_multi_children(&members)
            .build()
    };

    Ok(idx)
}

// ---------------------------------------------------------------------------
// Extend definition
// ---------------------------------------------------------------------------

/// `extend trait_expr for type_expr block` or `extend type_expr block`
pub fn try_extend_def(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwExtend) {
        return Ok(NULL);
    }

    let first_expr = expr::try_expr_without_extended_call(p)?;
    if first_expr.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected expression after `extend`",
        );
    }

    let (node_kind, trait_expr, type_expr) = if p.eat_token(TokenKind::KwFor) {
        let second_expr = expr::try_expr_without_extended_call(p)?;
        if second_expr.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected type expression after `for` in extend",
            );
        }
        // extend Trait for Type
        (NodeKind::ExtendTraitDef, first_expr, second_expr)
    } else {
        (NodeKind::ExtendDef, NULL, first_expr)
    };

    let members = try_multi_in_block(
        p,
        &[Rule::semicolon(
            "extend member",
            statement::try_definition_or_statement,
        )],
    )?;

    let span = p.current_span();

    let idx = if node_kind == NodeKind::ExtendTraitDef {
        // ExtendTraitDef: [0] trait  [1] type  multi = members
        p.ast()
            .builder(NodeKind::ExtendTraitDef, span)
            .add_child(trait_expr)
            .add_child(type_expr)
            .add_multi_children(&members)
            .build()
    } else {
        // ExtendDef: [0] type  multi = members
        p.ast()
            .builder(NodeKind::ExtendDef, span)
            .add_child(type_expr)
            .add_multi_children(&members)
            .build()
    };

    Ok(idx)
}

// ---------------------------------------------------------------------------
// Derive definition
// ---------------------------------------------------------------------------

/// `derive expr+ for expr`
pub fn try_derive_def(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwDerive) {
        return Ok(NULL);
    }

    // Parse one or more trait expressions before `for`
    let mut trait_exprs = Vec::new();
    loop {
        let tr = expr::try_expr_without_extended_call(p)?;
        if tr.is_null() {
            break;
        }
        trait_exprs.push(tr);
        if !p.eat_token(TokenKind::Comma) {
            break;
        }
    }

    if trait_exprs.is_empty() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected trait expression after `derive`",
        );
    }

    p.expect_token(TokenKind::KwFor)?;

    let type_expr = expr::try_expr_without_extended_call(p)?;
    if type_expr.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected type expression after `for` in derive",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::DeriveDef, span)
        .add_child(type_expr)
        .add_multi_children(&trait_exprs)
        .build();
    Ok(idx)
}

// ---------------------------------------------------------------------------
// Type alias / Newtype
// ---------------------------------------------------------------------------

/// `typealias name = type_expr`
pub fn try_type_alias_def(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwTypealias) {
        return Ok(NULL);
    }

    let name = basic::try_id(p)?;
    if name.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected name after `typealias`",
        );
    }

    p.expect_token(TokenKind::Eq)?;

    let ty = expr::try_expr(p)?;
    if ty.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected type expression after `=` in typealias",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::Typealias, span)
        .add_child(name)
        .add_child(ty)
        .build();
    Ok(idx)
}

/// `newtype name = type_expr`
pub fn try_newtype_def(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwNewtype) {
        return Ok(NULL);
    }

    let name = basic::try_id(p)?;
    if name.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected name after `newtype`",
        );
    }

    p.expect_token(TokenKind::Eq)?;

    let ty = expr::try_expr(p)?;
    if ty.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected type expression after `=` in newtype",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::Newtype, span)
        .add_child(name)
        .add_child(ty)
        .build();
    Ok(idx)
}

// ---------------------------------------------------------------------------
// Module definition
// ---------------------------------------------------------------------------

/// `mod name block?`
pub fn try_mod_def(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwMod) {
        return Ok(NULL);
    }

    let name = basic::try_id(p)?;
    if name.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected module name after `mod`",
        );
    }

    // Optional body - for inline modules
    let body = try_multi_in_block(
        p,
        &[Rule::semicolon(
            "module member",
            statement::try_definition_or_statement,
        )],
    );

    let members = body.unwrap_or_default();

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::ModuleDef, span)
        .add_child(name)
        .add_multi_children(&members)
        .build();
    Ok(idx)
}

// ---------------------------------------------------------------------------
// Use statement
// ---------------------------------------------------------------------------

/// `use path`
pub fn try_use_statement(p: &mut Parser) -> ParseResult {
    let _g = p.enter();
    if !p.eat_token(TokenKind::KwUse) {
        return Ok(NULL);
    }

    let path = try_use_path(p)?;
    if path.is_null() {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Expected path after `use`",
        );
    }

    let span = p.current_span();
    let idx = p
        .ast()
        .builder(NodeKind::UseStatement, span)
        .add_child(path)
        .add_multi_children(&[])
        .build();
    Ok(idx)
}

/// Parse use path: `id(.id)*(.*|.{id, ...})?` or `super.path` or `package.path`
fn try_use_path(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    // Handle super/package prefix
    let mut path = if p.eat_token(TokenKind::KwSelfLower) {
        // self prefix
        let str_id = str_interner::intern("self");
        let span = p.current_span();
        p.ast()
            .builder(NodeKind::Id, span)
            .set_str_id(str_id)
            .build()
    } else {
        let id = basic::try_id(p)?;
        if id.is_null() {
            return Ok(NULL);
        }
        id
    };

    // Parse .id chains and terminal .* or .{...}
    while p.peek(&[TokenKind::Dot]) {
        p.next_token(); // consume dot

        let next = p.peek_token();
        match next.kind {
            TokenKind::Star => {
                // .*
                p.next_token();
                let span = p.current_span();
                path = p
                    .ast()
                    .builder(NodeKind::PathProjectionAll, span)
                    .add_child(path)
                    .add_multi_children(&[])
                    .build();
                break;
            }
            TokenKind::LBrace => {
                // .{ id, id, ... }
                let items = try_multi_with_bracket(
                    p,
                    &[Rule::comma("use item", try_use_item)],
                    TokenKind::LBrace,
                    TokenKind::RBrace,
                )?;
                let span = p.current_span();
                path = p
                    .ast()
                    .builder(NodeKind::PathProjectionMulti, span)
                    .add_child(path)
                    .add_child(NULL)
                    .add_multi_children(&items)
                    .build();
                break;
            }
            _ => {
                // .id
                let id = basic::try_id(p)?;
                if id.is_null() {
                    let _ = p.err(
                        ParseErrorKind::InvalidSyntax,
                        p.next_token_span(),
                        "Expected identifier after `.` in use path",
                    );
                    break;
                }
                let span = p.current_span();
                path = p
                    .ast()
                    .builder(NodeKind::PathProjection, span)
                    .add_child(path)
                    .add_child(id)
                    .build();
            }
        }
    }

    Ok(path)
}

/// `id` or `id as alias`
fn try_use_item(p: &mut Parser) -> ParseResult {
    let _g = p.enter();

    let id = basic::try_id(p)?;
    if id.is_null() {
        return Ok(NULL);
    }

    if p.eat_token(TokenKind::KwAs) {
        let alias = basic::try_id(p)?;
        if alias.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected alias name after `as`",
            );
        }
        let span = p.current_span();
        let idx = p
            .ast()
            .builder(NodeKind::PathAsBind, span)
            .add_child(id)
            .add_child(alias)
            .build();
        return Ok(idx);
    }

    Ok(id)
}
