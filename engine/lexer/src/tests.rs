use crate::error::LexErrorKind;
use crate::lexer::Lexer;
use crate::token::{TokenKind, lookup_keyword};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Tokenize and return (kinds_without_eof, errors).
fn lex(src: &str) -> (Vec<TokenKind>, Vec<crate::error::LexError>) {
    let mut lexer = Lexer::new(src);
    let tokens = lexer.tokenize();
    let kinds: Vec<_> = tokens
        .iter()
        .filter(|t| t.kind != TokenKind::Eof)
        .map(|t| t.kind)
        .collect();
    (kinds, lexer.errors)
}

/// Tokenize and assert no errors.
fn kinds_ok(src: &str) -> Vec<TokenKind> {
    let (k, e) = lex(src);
    assert!(e.is_empty(), "unexpected errors: {:?}", e);
    k
}

/// Tokenize and return the text slices (excluding EOF).
fn texts(src: &str) -> Vec<String> {
    let mut lexer = Lexer::new(src);
    lexer
        .tokenize()
        .into_iter()
        .filter(|t| t.kind != TokenKind::Eof)
        .map(|t| t.text(src).to_string())
        .collect()
}

// =========================================================================
// Single-character punctuation
// =========================================================================

#[test]
fn test_single_char_punctuation() {
    let src = ". : , ; ( ) [ ] } ? ^ ~ @ $ & #";
    let k = kinds_ok(src);
    assert_eq!(
        k,
        vec![
            TokenKind::Dot,
            TokenKind::Colon,
            TokenKind::Comma,
            TokenKind::Semi,
            TokenKind::LParen,
            TokenKind::RParen,
            TokenKind::LBracket,
            TokenKind::RBracket,
            TokenKind::RBrace,
            TokenKind::Question,
            TokenKind::Caret,
            TokenKind::Tilde,
            TokenKind::At,
            TokenKind::Dollar,
            TokenKind::Ampersand,
            TokenKind::Hash,
        ]
    );
}

#[test]
fn test_backslash() {
    assert_eq!(kinds_ok("\\"), vec![TokenKind::Backslash]);
}

#[test]
fn test_lbrace() {
    assert_eq!(kinds_ok("{"), vec![TokenKind::LBrace]);
}

// =========================================================================
// Multi-character operators
// =========================================================================

#[test]
fn test_two_char_operators() {
    let cases: &[(&str, TokenKind)] = &[
        ("!=", TokenKind::BangEq),
        ("==", TokenKind::EqEq),
        ("=>", TokenKind::FatArrow),
        ("+=", TokenKind::PlusEq),
        ("-=", TokenKind::MinusEq),
        ("->", TokenKind::Arrow),
        ("*=", TokenKind::StarEq),
        ("/=", TokenKind::SlashEq),
        ("%=", TokenKind::PercentEq),
        ("<=", TokenKind::LtEq),
        (">=", TokenKind::GtEq),
        ("|>", TokenKind::PipeGt),
    ];
    for &(src, expected) in cases {
        assert_eq!(kinds_ok(src), vec![expected], "failed for {:?}", src);
    }
}

#[test]
fn test_single_then_different_char() {
    // `!x` should be `!` then `x`
    let k = kinds_ok("!x");
    assert_eq!(k, vec![TokenKind::Bang, TokenKind::Id]);
}

#[test]
fn test_pipe_alone() {
    assert_eq!(kinds_ok("|"), vec![TokenKind::Pipe]);
}

// =========================================================================
// Keywords (including the 3 new nessa2 keywords)
// =========================================================================

#[test]
fn test_all_keywords() {
    let keywords_src = [
        ("_", TokenKind::Underscore),
        ("and", TokenKind::KwAnd),
        ("as", TokenKind::KwAs),
        ("assoc", TokenKind::KwAssoc),
        ("async", TokenKind::KwAsync),
        ("atomic", TokenKind::KwAtomic),
        ("await", TokenKind::KwAwait),
        ("break", TokenKind::KwBreak),
        ("case", TokenKind::KwCase),
        ("catch", TokenKind::KwCatch),
        ("const", TokenKind::KwConst),
        ("continue", TokenKind::KwContinue),
        ("def", TokenKind::KwDef),
        ("defer", TokenKind::KwDefer),
        ("derive", TokenKind::KwDerive),
        ("do", TokenKind::KwDo),
        ("effect", TokenKind::KwEffect),
        ("else", TokenKind::KwElse),
        ("enum", TokenKind::KwEnum),
        ("error", TokenKind::KwError),
        ("extend", TokenKind::KwExtend),
        ("extern", TokenKind::KwExtern),
        ("false", TokenKind::KwFalse),
        ("fn", TokenKind::KwFn),
        ("for", TokenKind::KwFor),
        ("global", TokenKind::KwGlobal),
        ("handles", TokenKind::KwHandles),
        ("if", TokenKind::KwIf),
        ("impl", TokenKind::KwImpl),
        ("in", TokenKind::KwIn),
        ("is", TokenKind::KwIs),
        ("itself", TokenKind::KwItself),
        ("lambda", TokenKind::KwLambda),
        ("let", TokenKind::KwLet),
        ("match", TokenKind::KwMatch),
        ("matches", TokenKind::KwMatches),
        ("mod", TokenKind::KwMod),
        ("newtype", TokenKind::KwNewtype),
        ("not", TokenKind::KwNot),
        ("null", TokenKind::KwNull),
        ("or", TokenKind::KwOr),
        ("private", TokenKind::KwPrivate),
        ("pub", TokenKind::KwPub),
        ("reset", TokenKind::KwReset),
        ("resume", TokenKind::KwResume),
        ("return", TokenKind::KwReturn),
        ("self", TokenKind::KwSelfLower),
        ("Self", TokenKind::KwSelfCap),
        ("shift", TokenKind::KwShift),
        ("static", TokenKind::KwStatic),
        ("struct", TokenKind::KwStruct),
        ("test", TokenKind::KwTest),
        ("trait", TokenKind::KwTrait),
        ("true", TokenKind::KwTrue),
        ("typealias", TokenKind::KwTypealias),
        ("use", TokenKind::KwUse),
        ("when", TokenKind::KwWhen),
        ("while", TokenKind::KwWhile),
        ("where", TokenKind::KwWhere),
    ];

    for (word, expected) in &keywords_src {
        assert_eq!(
            lookup_keyword(word),
            Some(*expected),
            "lookup_keyword failed for {:?}",
            word,
        );
        assert_eq!(kinds_ok(word), vec![*expected], "lex failed for {:?}", word,);
    }
}

#[test]
fn test_new_keywords_assoc_global_pub() {
    let k = kinds_ok("assoc global pub");
    assert_eq!(
        k,
        vec![TokenKind::KwAssoc, TokenKind::KwGlobal, TokenKind::KwPub]
    );
}

#[test]
fn test_errdefer_is_not_keyword() {
    // `errdefer` was removed in nessa2; it should lex as an identifier.
    assert_eq!(lookup_keyword("errdefer"), None);
    assert_eq!(kinds_ok("errdefer"), vec![TokenKind::Id]);
}

#[test]
fn test_keyword_prefix_is_identifier() {
    // `matching` is not a keyword – it should be an identifier.
    assert_eq!(kinds_ok("matching"), vec![TokenKind::Id]);
    assert_eq!(kinds_ok("define"), vec![TokenKind::Id]);
    assert_eq!(kinds_ok("truethy"), vec![TokenKind::Id]);
}

// =========================================================================
// Identifiers
// =========================================================================

#[test]
fn test_simple_identifier() {
    assert_eq!(kinds_ok("foo"), vec![TokenKind::Id]);
    assert_eq!(texts("foo"), vec!["foo"]);
}

#[test]
fn test_identifier_with_underscores_and_digits() {
    assert_eq!(kinds_ok("foo_bar_42"), vec![TokenKind::Id]);
    assert_eq!(texts("foo_bar_42"), vec!["foo_bar_42"]);
}

#[test]
fn test_unicode_identifier() {
    assert_eq!(kinds_ok("日本語"), vec![TokenKind::Id]);
}

#[test]
fn test_arbitrary_id() {
    assert_eq!(kinds_ok("`hello world`"), vec![TokenKind::ArbitraryId]);
    assert_eq!(texts("`hello world`"), vec!["`hello world`"]);
}

#[test]
fn test_arbitrary_id_unterminated() {
    let (k, e) = lex("`hello");
    assert_eq!(k, vec![TokenKind::Invalid]);
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].kind, LexErrorKind::InvalidArbitraryId);
}

#[test]
fn test_arbitrary_id_newline() {
    let (k, e) = lex("`hello\nworld`");
    assert_eq!(k[0], TokenKind::Invalid);
    assert!(e.iter().any(|e| e.kind == LexErrorKind::InvalidArbitraryId));
}

// =========================================================================
// Integer literals
// =========================================================================

#[test]
fn test_decimal_integer() {
    assert_eq!(kinds_ok("42"), vec![TokenKind::Integer]);
    assert_eq!(texts("42"), vec!["42"]);
}

#[test]
fn test_zero() {
    assert_eq!(kinds_ok("0"), vec![TokenKind::Integer]);
}

#[test]
fn test_binary_integer() {
    assert_eq!(kinds_ok("0b1010"), vec![TokenKind::IntBin]);
    assert_eq!(kinds_ok("0B1010"), vec![TokenKind::IntBin]);
}

#[test]
fn test_octal_integer() {
    assert_eq!(kinds_ok("0o777"), vec![TokenKind::IntOct]);
    assert_eq!(kinds_ok("0O777"), vec![TokenKind::IntOct]);
}

#[test]
fn test_hex_integer() {
    assert_eq!(kinds_ok("0xFF"), vec![TokenKind::IntHex]);
    assert_eq!(kinds_ok("0XAB"), vec![TokenKind::IntHex]);
}

#[test]
fn test_binary_no_digits() {
    let (k, e) = lex("0b");
    assert_eq!(k, vec![TokenKind::Invalid]);
    assert_eq!(e[0].kind, LexErrorKind::InvalidNumber);
}

#[test]
fn test_octal_no_digits() {
    let (k, e) = lex("0o");
    assert_eq!(k, vec![TokenKind::Invalid]);
    assert_eq!(e[0].kind, LexErrorKind::InvalidNumber);
}

#[test]
fn test_hex_no_digits() {
    let (k, e) = lex("0x");
    assert_eq!(k, vec![TokenKind::Invalid]);
    assert_eq!(e[0].kind, LexErrorKind::InvalidNumber);
}

// =========================================================================
// Float literals
// =========================================================================

#[test]
fn test_real_literal() {
    assert_eq!(kinds_ok("3.14"), vec![TokenKind::Real]);
    assert_eq!(texts("3.14"), vec!["3.14"]);
}

#[test]
fn test_scientific_notation() {
    assert_eq!(kinds_ok("1.23e10"), vec![TokenKind::RealSci]);
    assert_eq!(kinds_ok("1.23E-4"), vec![TokenKind::RealSci]);
    assert_eq!(kinds_ok("1.23e+4"), vec![TokenKind::RealSci]);
}

#[test]
fn test_scientific_missing_exponent() {
    let (k, e) = lex("1.23e");
    assert_eq!(k, vec![TokenKind::Invalid]);
    assert_eq!(e[0].kind, LexErrorKind::InvalidNumber);
}

#[test]
fn test_integer_dot_non_digit() {
    // `42.method` should yield Integer, Dot, Id – not a float.
    let k = kinds_ok("42.method");
    assert_eq!(k, vec![TokenKind::Integer, TokenKind::Dot, TokenKind::Id]);
}

// =========================================================================
// String literals
// =========================================================================

#[test]
fn test_simple_string() {
    assert_eq!(kinds_ok(r#""hello""#), vec![TokenKind::String]);
}

#[test]
fn test_string_escapes() {
    assert_eq!(kinds_ok(r#""a\nb""#), vec![TokenKind::String]);
    assert_eq!(kinds_ok(r#""a\tb""#), vec![TokenKind::String]);
    assert_eq!(kinds_ok(r#""a\\b""#), vec![TokenKind::String]);
    assert_eq!(kinds_ok(r#""a\"b""#), vec![TokenKind::String]);
    assert_eq!(kinds_ok(r#""a\'b""#), vec![TokenKind::String]);
    assert_eq!(kinds_ok(r#""a\rb""#), vec![TokenKind::String]);
}

#[test]
fn test_string_hex_escape() {
    assert_eq!(kinds_ok(r#""\x41""#), vec![TokenKind::String]);
}

#[test]
fn test_string_unicode_escape() {
    assert_eq!(kinds_ok(r#""\u0041""#), vec![TokenKind::String]);
}

#[test]
fn test_string_invalid_escape() {
    let (k, e) = lex(r#""\q""#);
    assert_eq!(k, vec![TokenKind::Invalid]);
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].kind, LexErrorKind::InvalidStrLiteral);
}

#[test]
fn test_unterminated_string() {
    let (k, e) = lex(r#""hello"#);
    assert_eq!(k, vec![TokenKind::Invalid]);
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].kind, LexErrorKind::InvalidStrLiteral);
}

#[test]
fn test_string_with_newline() {
    let (k, e) = lex("\"hello\nworld\"");
    assert_eq!(k[0], TokenKind::Invalid);
    assert!(e.iter().any(|e| e.kind == LexErrorKind::InvalidStrLiteral));
}

// =========================================================================
// Character literals
// =========================================================================

#[test]
fn test_char_literal() {
    assert_eq!(kinds_ok("'a'"), vec![TokenKind::Char]);
}

#[test]
fn test_char_escape() {
    assert_eq!(kinds_ok("'\\n'"), vec![TokenKind::Char]);
    assert_eq!(kinds_ok("'\\t'"), vec![TokenKind::Char]);
    assert_eq!(kinds_ok("'\\''"), vec![TokenKind::Char]);
}

#[test]
fn test_empty_char() {
    let (k, e) = lex("''");
    assert_eq!(k, vec![TokenKind::Invalid]);
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].kind, LexErrorKind::InvalidCharLiteral);
}

#[test]
fn test_unterminated_char() {
    let (k, e) = lex("'a");
    assert_eq!(k, vec![TokenKind::Invalid]);
    assert_eq!(e[0].kind, LexErrorKind::InvalidCharLiteral);
}

#[test]
fn test_char_too_long() {
    // `'ab'` is not a char literal; `'` is the view operator, then `ab`, then `'`.
    let k = kinds_ok("'ab'");
    assert_eq!(k[0], TokenKind::Quote);
    assert_eq!(k[1], TokenKind::Id);
}

#[test]
fn test_view_quote_without_space() {
    let k = kinds_ok(".print'builtin");
    assert!(k.contains(&TokenKind::Dot));
    assert!(k.contains(&TokenKind::Quote));
    assert_eq!(
        k.iter().filter(|&&t| t == TokenKind::Id).count(),
        2,
        "expected print and builtin ids, got {k:?}"
    );
}

// =========================================================================
// Quote and macro content
// =========================================================================

#[test]
fn test_standalone_quote() {
    let k = kinds_ok("' foo");
    // `'` followed by space → quote token, then identifier
    assert_eq!(k[0], TokenKind::Quote);
}

#[test]
fn test_macro_content() {
    assert_eq!(kinds_ok("'{x + y}"), vec![TokenKind::MacroContent]);
}

#[test]
fn test_macro_content_nested_braces() {
    assert_eq!(kinds_ok("'{a {b} c}"), vec![TokenKind::MacroContent]);
}

#[test]
fn test_macro_with_spaces() {
    // `' {x}` — quote, space(s), then `{` → macro content
    assert_eq!(kinds_ok("' {x}"), vec![TokenKind::MacroContent]);
}

// =========================================================================
// Comments
// =========================================================================

#[test]
fn test_line_comment_skipped() {
    let k = kinds_ok("-- this is a comment");
    assert!(k.is_empty(), "line comment should produce no tokens");
}

#[test]
fn test_line_comment_before_code() {
    let k = kinds_ok("-- comment\nfoo");
    // The comment is consumed; then newline + identifier.
    assert!(k.contains(&TokenKind::Newline));
    assert!(k.contains(&TokenKind::Id));
}

#[test]
fn test_block_comment_skipped() {
    let k = kinds_ok("{- block comment -}");
    assert!(k.is_empty());
}

#[test]
fn test_nested_block_comment() {
    let k = kinds_ok("{- outer {- inner -} still outer -}");
    assert!(k.is_empty());
}

#[test]
fn test_code_around_block_comment() {
    let k = kinds_ok("a {- comment -} b");
    assert_eq!(k, vec![TokenKind::Id, TokenKind::Id]);
}

// =========================================================================
// Layout tokens: Newline / Indent / Outdent
// =========================================================================

#[test]
fn test_newline_emitted() {
    let k = kinds_ok("a\nb");
    assert!(k.contains(&TokenKind::Newline));
}

#[test]
fn test_format_sensitive_colon_trigger() {
    // `:` followed by newline + deeper indent → Indent
    let src = "if cond:\n    body";
    let k = kinds_ok(src);
    assert!(
        k.contains(&TokenKind::Indent),
        "expected Indent after `:` trigger, got {:?}",
        k
    );
}

#[test]
fn test_format_sensitive_fat_arrow_trigger() {
    let src = "x =>\n    body";
    let k = kinds_ok(src);
    assert!(
        k.contains(&TokenKind::Indent),
        "expected Indent after `=>` trigger, got {:?}",
        k
    );
}

#[test]
fn test_format_sensitive_do_trigger() {
    let src = "list.map do\n    body";
    let k = kinds_ok(src);
    assert!(
        k.contains(&TokenKind::Indent),
        "expected Indent after `do` trigger, got {:?}",
        k
    );
}

#[test]
fn test_no_indent_without_trigger() {
    // Without a trigger, deeper indentation should NOT produce Indent.
    let src = "a\n    b";
    let k = kinds_ok(src);
    assert!(
        !k.contains(&TokenKind::Indent),
        "Indent should NOT appear without trigger, got {:?}",
        k
    );
}

#[test]
fn test_outdent_on_dedent() {
    let src = "if cond:\n    body\nafter";
    let k = kinds_ok(src);
    assert!(
        k.contains(&TokenKind::Outdent),
        "expected Outdent when indentation decreases, got {:?}",
        k
    );
}

#[test]
fn test_multiple_outdents() {
    let src = "a:\n    b:\n        c\nd";
    let k = kinds_ok(src);
    let outdent_count = k.iter().filter(|&&t| t == TokenKind::Outdent).count();
    assert_eq!(
        outdent_count, 2,
        "expected 2 Outdents for nested dedent, got {:?}",
        k
    );
}

#[test]
fn test_outdent_at_eof() {
    let src = "a:\n    b";
    let k = kinds_ok(src);
    assert!(
        k.contains(&TokenKind::Outdent),
        "expected Outdent at EOF for open indent, got {:?}",
        k
    );
}

#[test]
fn test_blank_lines_ignored_in_indent() {
    // Blank lines between trigger and indented body should not confuse
    // the indentation tracking.
    let src = "a:\n\n    b";
    let k = kinds_ok(src);
    assert!(
        k.contains(&TokenKind::Indent),
        "expected Indent even with blank lines, got {:?}",
        k
    );
}

#[test]
fn test_non_trigger_eq_does_not_indent() {
    // `=` is NOT a nessa2 format-sensitive trigger (only `:`, `=>`, `do`).
    let src = "x =\n    42";
    let k = kinds_ok(src);
    assert!(
        !k.contains(&TokenKind::Indent),
        "`=` should NOT trigger indentation in nessa2, got {:?}",
        k
    );
}

#[test]
fn test_non_trigger_match_does_not_indent() {
    // `match` is NOT a trigger in nessa2.
    let src = "match\n    x";
    let k = kinds_ok(src);
    assert!(
        !k.contains(&TokenKind::Indent),
        "`match` should NOT trigger indentation in nessa2, got {:?}",
        k
    );
}

#[test]
fn test_virtual_newline_after_outdent() {
    // After each Outdent the lexer injects a virtual Newline.
    let src = "a:\n    b\nc";
    let k = kinds_ok(src);

    let outdent_pos = k.iter().position(|&t| t == TokenKind::Outdent).unwrap();
    assert_eq!(
        k[outdent_pos + 1],
        TokenKind::Newline,
        "expected virtual Newline after Outdent, got {:?}",
        k
    );
}

// =========================================================================
// Full expression / statement tokenization
// =========================================================================

#[test]
fn test_let_binding() {
    let k = kinds_ok("let x = 42");
    assert_eq!(
        k,
        vec![
            TokenKind::KwLet,
            TokenKind::Id,
            TokenKind::Eq,
            TokenKind::Integer,
        ]
    );
}

#[test]
fn test_function_definition() {
    let k = kinds_ok("fn add(a, b):\n    a + b");
    assert!(k.contains(&TokenKind::KwFn));
    assert!(k.contains(&TokenKind::Indent));
    assert!(k.contains(&TokenKind::Plus));
}

#[test]
fn test_pipe_chain() {
    let k = kinds_ok("x |> f |> g");
    assert_eq!(
        k,
        vec![
            TokenKind::Id,
            TokenKind::PipeGt,
            TokenKind::Id,
            TokenKind::PipeGt,
            TokenKind::Id,
        ]
    );
}

#[test]
fn test_pattern_match_arms() {
    let src = "x =>\n    case 1 => 2\n    case 3 => 4";
    let k = kinds_ok(src);
    assert!(k.contains(&TokenKind::KwCase));
    assert!(k.contains(&TokenKind::FatArrow));
}

// =========================================================================
// Token positions
// =========================================================================

#[test]
fn test_token_positions() {
    let src = "ab cd";
    let mut lexer = Lexer::new(src);
    let tokens = lexer.tokenize();

    let ab = &tokens[0];
    assert_eq!(ab.kind, TokenKind::Id);
    assert_eq!(ab.from, 0);
    assert_eq!(ab.to, 2);
    assert_eq!(ab.text(src), "ab");

    let cd = &tokens[1];
    assert_eq!(cd.kind, TokenKind::Id);
    assert_eq!(cd.from, 3);
    assert_eq!(cd.to, 5);
    assert_eq!(cd.text(src), "cd");
}

#[test]
fn test_eof_at_end() {
    let src = "x";
    let mut lexer = Lexer::new(src);
    let tokens = lexer.tokenize();
    assert_eq!(tokens.last().unwrap().kind, TokenKind::Eof);
}

// =========================================================================
// Edge cases
// =========================================================================

#[test]
fn test_empty_source() {
    let k = kinds_ok("");
    assert!(k.is_empty());
}

#[test]
fn test_whitespace_only() {
    let k = kinds_ok("   \t  ");
    assert!(k.is_empty());
}

#[test]
fn test_newlines_only() {
    let k = kinds_ok("\n\n\n");
    // Should just produce newlines (no indent/outdent since column stays 0).
    for kind in &k {
        assert_eq!(*kind, TokenKind::Newline);
    }
}

#[test]
fn test_consecutive_operators() {
    let k = kinds_ok("+-*");
    assert_eq!(k, vec![TokenKind::Plus, TokenKind::Minus, TokenKind::Star]);
}

#[test]
fn test_unexpected_character() {
    let (k, e) = lex("\u{FEFF}"); // BOM
    assert_eq!(k, vec![TokenKind::Invalid]);
    assert_eq!(e[0].kind, LexErrorKind::UnexpectedChar);
}

// =========================================================================
// TokenKind utility methods
// =========================================================================

#[test]
fn test_can_end_statement() {
    assert!(TokenKind::Id.can_end_statement());
    assert!(TokenKind::Integer.can_end_statement());
    assert!(TokenKind::RParen.can_end_statement());
    assert!(TokenKind::KwTrue.can_end_statement());
    assert!(TokenKind::Outdent.can_end_statement());

    assert!(!TokenKind::Plus.can_end_statement());
    assert!(!TokenKind::LParen.can_end_statement());
    assert!(!TokenKind::Comma.can_end_statement());
}

#[test]
fn test_can_start_statement() {
    assert!(TokenKind::KwLet.can_start_statement());
    assert!(TokenKind::KwFn.can_start_statement());
    assert!(TokenKind::KwGlobal.can_start_statement());
    assert!(TokenKind::KwPub.can_start_statement());
    assert!(TokenKind::KwAssoc.can_start_statement());
    assert!(TokenKind::Id.can_start_statement());
    assert!(TokenKind::Integer.can_start_statement());

    assert!(!TokenKind::Eq.can_start_statement());
    assert!(!TokenKind::Comma.can_start_statement());
}

#[test]
fn test_lexeme_round_trip() {
    // For operator/keyword tokens the lexeme should match the source text.
    let ops = ["+", "-", "*", "/", "%", "!", "!=", "==", "=>", "->", "|>"];
    for op in ops {
        let k = kinds_ok(op);
        assert_eq!(k.len(), 1);
        assert_eq!(k[0].lexeme(), op, "lexeme mismatch for {:?}", op);
    }
}

// =========================================================================
// Token::span integration with rustc_span
// =========================================================================

#[test]
fn test_token_span() {
    let src = "hello";
    let mut lexer = Lexer::new(src);
    let tokens = lexer.tokenize();
    let tok = &tokens[0];
    let sp = tok.span();
    assert_eq!(sp.lo().0, 0);
    assert_eq!(sp.hi().0, 5);
}

// =========================================================================
// Complex multi-line programs
// =========================================================================

#[test]
fn test_multiline_function() {
    let src = "\
fn fibonacci(n):
    if n <= 1:
        return n
    return fibonacci(n - 1) + fibonacci(n - 2)
";
    let (k, e) = lex(src);
    assert!(e.is_empty(), "unexpected errors: {:?}", e);
    assert!(k.contains(&TokenKind::KwFn));
    assert!(k.contains(&TokenKind::KwIf));
    assert!(k.contains(&TokenKind::KwReturn));
    // Two indent levels: fn body and if body
    let indent_count = k.iter().filter(|&&t| t == TokenKind::Indent).count();
    assert_eq!(indent_count, 2, "expected 2 Indents, got {:?}", k);
}

#[test]
fn test_global_and_pub_in_context() {
    let src = "pub global MAX_SIZE : i32 = 1024";
    let k = kinds_ok(src);
    assert_eq!(k[0], TokenKind::KwPub);
    assert_eq!(k[1], TokenKind::KwGlobal);
}

#[test]
fn test_assoc_in_trait() {
    let src = "trait Functor:\n    assoc F : Type";
    let k = kinds_ok(src);
    assert!(k.contains(&TokenKind::KwTrait));
    assert!(k.contains(&TokenKind::KwAssoc));
}

#[test]
fn test_do_block() {
    let src = "list.map do\n    |x| x * 2";
    let k = kinds_ok(src);
    assert!(k.contains(&TokenKind::KwDo));
    assert!(k.contains(&TokenKind::Indent));
}

#[test]
fn test_mixed_comments_and_code() {
    let src = "\
-- top-level comment
let x = 42 -- inline comment
{- block
   comment -}
let y = x + 1
";
    let (k, e) = lex(src);
    assert!(e.is_empty(), "unexpected errors: {:?}", e);
    // Should have two `let` bindings
    let let_count = k.iter().filter(|&&t| t == TokenKind::KwLet).count();
    assert_eq!(let_count, 2);
}

#[test]
fn test_string_with_all_escapes() {
    let src = r#""\n\t\r\\\"\'\x41\u0041""#;
    assert_eq!(kinds_ok(src), vec![TokenKind::String]);
}

#[test]
fn test_number_followed_by_dot_method() {
    // Ensure `0.foo` is Integer + Dot + Id, not a float parse error.
    let k = kinds_ok("0.foo");
    assert_eq!(k, vec![TokenKind::Integer, TokenKind::Dot, TokenKind::Id]);
}
