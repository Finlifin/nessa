//! Lexer integration tests — verify tokenisation of various Nessa constructs.

use lexer::{LexErrorKind, Token, TokenKind, tokenize};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Tokenize and return only the "meaningful" tokens (skip Sof, Eof, Newline,
/// Indent, Outdent, Comment).
fn meaningful_tokens(src: &str) -> Vec<Token> {
    let (tokens, errors) = tokenize(src);
    assert!(errors.is_empty(), "Unexpected lex errors: {errors:?}");
    tokens
        .into_iter()
        .filter(|t| {
            !matches!(
                t.kind,
                TokenKind::Sof
                    | TokenKind::Eof
                    | TokenKind::Newline
                    | TokenKind::Indent
                    | TokenKind::Outdent
                    | TokenKind::Comment
            )
        })
        .collect()
}

/// Tokenize and return only the token kinds (skip layout tokens).
fn token_kinds(src: &str) -> Vec<TokenKind> {
    meaningful_tokens(src).iter().map(|t| t.kind).collect()
}

/// Tokenize and return ALL token kinds including layout tokens (but not Sof/Eof).
fn all_token_kinds(src: &str) -> Vec<TokenKind> {
    let (tokens, errors) = tokenize(src);
    assert!(errors.is_empty(), "Unexpected lex errors: {errors:?}");
    tokens
        .into_iter()
        .filter(|t| !matches!(t.kind, TokenKind::Sof | TokenKind::Eof))
        .map(|t| t.kind)
        .collect()
}

/// Tokenize and expect errors; return the error list.
fn lex_errors(src: &str) -> Vec<lexer::LexError> {
    let (_tokens, errors) = tokenize(src);
    errors
}

// ===========================================================================
// Integer literals
// ===========================================================================

#[test]
fn lex_integer_decimal() {
    let kinds = token_kinds("42");
    assert_eq!(kinds, vec![TokenKind::Integer]);
}

#[test]
fn lex_integer_zero() {
    let kinds = token_kinds("0");
    assert_eq!(kinds, vec![TokenKind::Integer]);
}

#[test]
fn lex_integer_binary() {
    let kinds = token_kinds("0b1010");
    assert_eq!(kinds, vec![TokenKind::IntBin]);
}

#[test]
fn lex_integer_octal() {
    let kinds = token_kinds("0o777");
    assert_eq!(kinds, vec![TokenKind::IntOct]);
}

#[test]
fn lex_integer_hex() {
    let kinds = token_kinds("0xFF");
    assert_eq!(kinds, vec![TokenKind::IntHex]);
}

// ===========================================================================
// Real / float literals
// ===========================================================================

#[test]
fn lex_real_simple() {
    let kinds = token_kinds("3.14");
    assert_eq!(kinds, vec![TokenKind::Real]);
}

#[test]
fn lex_real_scientific() {
    let kinds = token_kinds("1.23e-4");
    assert_eq!(kinds, vec![TokenKind::RealSci]);
}

// ===========================================================================
// String / Char literals
// ===========================================================================

#[test]
fn lex_string_literal() {
    let kinds = token_kinds("\"hello world\"");
    assert_eq!(kinds, vec![TokenKind::String]);
}

#[test]
fn lex_string_with_escape() {
    let kinds = token_kinds("\"line\\n\"");
    assert_eq!(kinds, vec![TokenKind::String]);
}

#[test]
fn lex_char_literal() {
    let kinds = token_kinds("'a'");
    assert_eq!(kinds, vec![TokenKind::Char]);
}

#[test]
fn lex_char_escape() {
    let kinds = token_kinds("'\\n'");
    assert_eq!(kinds, vec![TokenKind::Char]);
}

// ===========================================================================
// Keywords
// ===========================================================================

#[test]
fn lex_keywords_misc() {
    assert_eq!(token_kinds("fn"), vec![TokenKind::KwFn]);
    assert_eq!(token_kinds("let"), vec![TokenKind::KwLet]);
    assert_eq!(token_kinds("var"), vec![TokenKind::KwVar]);
    assert_eq!(token_kinds("const"), vec![TokenKind::KwConst]);
    assert_eq!(token_kinds("if"), vec![TokenKind::KwIf]);
    assert_eq!(token_kinds("else"), vec![TokenKind::KwElse]);
    assert_eq!(token_kinds("while"), vec![TokenKind::KwWhile]);
    assert_eq!(token_kinds("for"), vec![TokenKind::KwFor]);
    assert_eq!(token_kinds("in"), vec![TokenKind::KwIn]);
    assert_eq!(token_kinds("return"), vec![TokenKind::KwReturn]);
    assert_eq!(token_kinds("break"), vec![TokenKind::KwBreak]);
    assert_eq!(token_kinds("continue"), vec![TokenKind::KwContinue]);
}

#[test]
fn lex_keywords_definition() {
    assert_eq!(token_kinds("struct"), vec![TokenKind::KwStruct]);
    assert_eq!(token_kinds("enum"), vec![TokenKind::KwEnum]);
    assert_eq!(token_kinds("trait"), vec![TokenKind::KwTrait]);
    assert_eq!(token_kinds("impl"), vec![TokenKind::KwImpl]);
    assert_eq!(token_kinds("extend"), vec![TokenKind::KwExtend]);
    assert_eq!(token_kinds("derive"), vec![TokenKind::KwDerive]);
    assert_eq!(token_kinds("effect"), vec![TokenKind::KwEffect]);
    assert_eq!(token_kinds("handles"), vec![TokenKind::KwHandles]);
    assert_eq!(token_kinds("typealias"), vec![TokenKind::KwTypealias]);
    assert_eq!(token_kinds("newtype"), vec![TokenKind::KwNewtype]);
    assert_eq!(token_kinds("mod"), vec![TokenKind::KwMod]);
    assert_eq!(token_kinds("use"), vec![TokenKind::KwUse]);
}

#[test]
fn lex_keywords_visibility() {
    assert_eq!(token_kinds("pub"), vec![TokenKind::KwPub]);
    assert_eq!(token_kinds("global"), vec![TokenKind::KwGlobal]);
    assert_eq!(token_kinds("assoc"), vec![TokenKind::KwAssoc]);
}

#[test]
fn lex_keywords_boolean() {
    assert_eq!(token_kinds("true"), vec![TokenKind::KwTrue]);
    assert_eq!(token_kinds("false"), vec![TokenKind::KwFalse]);
    assert_eq!(token_kinds("null"), vec![TokenKind::KwNull]);
    assert_eq!(token_kinds("and"), vec![TokenKind::KwAnd]);
    assert_eq!(token_kinds("or"), vec![TokenKind::KwOr]);
    assert_eq!(token_kinds("not"), vec![TokenKind::KwNot]);
}

#[test]
fn lex_keywords_effect_control() {
    assert_eq!(token_kinds("reset"), vec![TokenKind::KwReset]);
    assert_eq!(token_kinds("shift"), vec![TokenKind::KwShift]);
    assert_eq!(token_kinds("resume"), vec![TokenKind::KwResume]);
    assert_eq!(token_kinds("defer"), vec![TokenKind::KwDefer]);
    assert_eq!(token_kinds("async"), vec![TokenKind::KwAsync]);
    assert_eq!(token_kinds("await"), vec![TokenKind::KwAwait]);
}

#[test]
fn lex_keyword_self() {
    assert_eq!(token_kinds("self"), vec![TokenKind::KwSelfLower]);
    assert_eq!(token_kinds("Self"), vec![TokenKind::KwSelfCap]);
}

#[test]
fn lex_underscore() {
    assert_eq!(token_kinds("_"), vec![TokenKind::Underscore]);
}

// ===========================================================================
// Identifiers
// ===========================================================================

#[test]
fn lex_identifier_simple() {
    let kinds = token_kinds("myVar");
    assert_eq!(kinds, vec![TokenKind::Id]);
}

#[test]
fn lex_identifier_with_underscores() {
    let kinds = token_kinds("my_var_2");
    assert_eq!(kinds, vec![TokenKind::Id]);
}

#[test]
fn lex_identifier_text() {
    let toks = meaningful_tokens("hello");
    assert_eq!(toks.len(), 1);
    assert_eq!(toks[0].text("hello"), "hello");
}

// ===========================================================================
// Operators & punctuation
// ===========================================================================

#[test]
fn lex_arithmetic_operators() {
    let kinds = token_kinds("+ - * / %");
    assert_eq!(
        kinds,
        vec![
            TokenKind::Plus,
            TokenKind::Minus,
            TokenKind::Star,
            TokenKind::Slash,
            TokenKind::Percent,
        ]
    );
}

#[test]
fn lex_comparison_operators() {
    let kinds = token_kinds("== != < <= > >=");
    assert_eq!(
        kinds,
        vec![
            TokenKind::EqEq,
            TokenKind::BangEq,
            TokenKind::Lt,
            TokenKind::LtEq,
            TokenKind::Gt,
            TokenKind::GtEq,
        ]
    );
}

#[test]
fn lex_assignment_operators() {
    let kinds = token_kinds("= += -= *= /= %=");
    assert_eq!(
        kinds,
        vec![
            TokenKind::Eq,
            TokenKind::PlusEq,
            TokenKind::MinusEq,
            TokenKind::StarEq,
            TokenKind::SlashEq,
            TokenKind::PercentEq,
        ]
    );
}

#[test]
fn lex_misc_operators() {
    assert_eq!(token_kinds("=>"), vec![TokenKind::FatArrow]);
    assert_eq!(token_kinds("->"), vec![TokenKind::Arrow]);
    assert_eq!(token_kinds("|>"), vec![TokenKind::PipeGt]);
    assert_eq!(token_kinds("++"), vec![TokenKind::PlusPlus]);
    assert_eq!(token_kinds("!"), vec![TokenKind::Bang]);
    assert_eq!(token_kinds("?"), vec![TokenKind::Question]);
    assert_eq!(token_kinds("@"), vec![TokenKind::At]);
}

#[test]
fn lex_delimiters() {
    let kinds = token_kinds("( ) [ ] { }");
    assert_eq!(
        kinds,
        vec![
            TokenKind::LParen,
            TokenKind::RParen,
            TokenKind::LBracket,
            TokenKind::RBracket,
            TokenKind::LBrace,
            TokenKind::RBrace,
        ]
    );
}

#[test]
fn lex_separator_tokens() {
    assert_eq!(token_kinds(","), vec![TokenKind::Comma]);
    assert_eq!(token_kinds(":"), vec![TokenKind::Colon]);
    assert_eq!(token_kinds("."), vec![TokenKind::Dot]);
    assert_eq!(token_kinds("#"), vec![TokenKind::Hash]);
}

// ===========================================================================
// Compound expressions (token sequences)
// ===========================================================================

#[test]
fn lex_let_binding() {
    let kinds = token_kinds("let x = 42");
    assert_eq!(
        kinds,
        vec![
            TokenKind::KwLet,
            TokenKind::Id,
            TokenKind::Eq,
            TokenKind::Integer
        ]
    );
}

#[test]
fn lex_function_header() {
    let kinds = token_kinds("fn add(a: i32, b: i32) -> i32");
    assert_eq!(
        kinds,
        vec![
            TokenKind::KwFn,
            TokenKind::Id, // add
            TokenKind::LParen,
            TokenKind::Id, // a
            TokenKind::Colon,
            TokenKind::Id, // i32
            TokenKind::Comma,
            TokenKind::Id, // b
            TokenKind::Colon,
            TokenKind::Id, // i32
            TokenKind::RParen,
            TokenKind::Arrow,
            TokenKind::Id, // i32
        ]
    );
}

#[test]
fn lex_arithmetic_expression() {
    let kinds = token_kinds("1 + 2 * 3");
    assert_eq!(
        kinds,
        vec![
            TokenKind::Integer,
            TokenKind::Plus,
            TokenKind::Integer,
            TokenKind::Star,
            TokenKind::Integer,
        ]
    );
}

#[test]
fn lex_method_chain() {
    let kinds = token_kinds("foo.bar(x).baz");
    assert_eq!(
        kinds,
        vec![
            TokenKind::Id, // foo
            TokenKind::Dot,
            TokenKind::Id, // bar
            TokenKind::LParen,
            TokenKind::Id, // x
            TokenKind::RParen,
            TokenKind::Dot,
            TokenKind::Id, // baz
        ]
    );
}

#[test]
fn lex_struct_definition() {
    let kinds = token_kinds("struct Point { x: i32, y: i32 }");
    assert!(kinds.contains(&TokenKind::KwStruct));
    assert!(kinds.contains(&TokenKind::LBrace));
    assert!(kinds.contains(&TokenKind::RBrace));
    assert_eq!(kinds.iter().filter(|k| **k == TokenKind::Colon).count(), 2);
}

#[test]
fn lex_pipeline_expression() {
    let kinds = token_kinds("data |> transform |> output");
    let pipe_count = kinds.iter().filter(|k| **k == TokenKind::PipeGt).count();
    assert_eq!(pipe_count, 2);
}

#[test]
fn lex_match_arm() {
    let kinds = token_kinds("Some(x) => x + 1");
    assert!(kinds.contains(&TokenKind::FatArrow));
}

// ===========================================================================
// Layout tokens (Newline, Indent, Outdent)
// ===========================================================================

#[test]
fn lex_newline() {
    let kinds = all_token_kinds("a\nb");
    assert!(kinds.contains(&TokenKind::Newline));
}

// ===========================================================================
// Comments
// ===========================================================================

#[test]
fn lex_line_comment() {
    // Comments are consumed by the lexer — no Comment token is produced.
    let kinds = all_token_kinds("-- this is a comment\nx");
    assert!(
        !kinds.contains(&TokenKind::Comment),
        "Line comments should be consumed, not emitted"
    );
    // The identifier after the comment line should still be lexed.
    assert!(kinds.contains(&TokenKind::Id));
}

// ===========================================================================
// Error cases
// ===========================================================================

#[test]
fn lex_unterminated_string() {
    let errors = lex_errors("\"unterminated");
    assert!(!errors.is_empty(), "Expected error for unterminated string");
    assert!(
        errors
            .iter()
            .any(|e| e.kind == LexErrorKind::InvalidStrLiteral),
        "Expected InvalidStrLiteral error, got: {errors:?}"
    );
}

// ===========================================================================
// Token span (from/to) correctness
// ===========================================================================

#[test]
fn lex_token_spans_single() {
    let toks = meaningful_tokens("hello");
    assert_eq!(toks.len(), 1);
    assert_eq!(toks[0].from, 0);
    assert_eq!(toks[0].to, 5);
}

#[test]
fn lex_token_spans_multi() {
    let src = "let x = 1";
    let toks = meaningful_tokens(src);
    // 'let' spans [0,3)
    assert_eq!(toks[0].text(src), "let");
    // 'x' spans somewhere in the middle
    assert_eq!(toks[1].text(src), "x");
    // '=' spans
    assert_eq!(toks[2].text(src), "=");
    // '1' at end
    assert_eq!(toks[3].text(src), "1");
}

// ===========================================================================
// Edge cases
// ===========================================================================

#[test]
fn lex_empty_source() {
    let (tokens, errors) = tokenize("");
    assert!(errors.is_empty());
    // Empty source produces only an Eof token.
    assert_eq!(
        tokens.len(),
        1,
        "Expected exactly 1 token (Eof), got: {tokens:?}"
    );
    assert_eq!(tokens[0].kind, TokenKind::Eof);
}

#[test]
fn lex_whitespace_only() {
    let (tokens, errors) = tokenize("   \t  ");
    assert!(errors.is_empty());
    // Should produce Sof and Eof with no meaningful tokens.
    let meaningful: Vec<_> = tokens
        .iter()
        .filter(|t| !matches!(t.kind, TokenKind::Sof | TokenKind::Eof | TokenKind::Newline))
        .collect();
    assert!(
        meaningful.is_empty(),
        "Expected no meaningful tokens, got: {meaningful:?}"
    );
}

#[test]
fn lex_symbol_dot_prefix() {
    // `.foo` should give a Dot followed by Id
    let kinds = token_kinds(".foo");
    assert_eq!(kinds[0], TokenKind::Dot);
}

#[test]
fn lex_multiple_ids_on_one_line() {
    let kinds = token_kinds("a b c");
    assert_eq!(kinds, vec![TokenKind::Id, TokenKind::Id, TokenKind::Id]);
}
