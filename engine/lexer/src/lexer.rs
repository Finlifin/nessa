use std::collections::VecDeque;

use crate::error::{LexError, LexErrorKind};
use crate::token::{Index, Token, TokenKind, lookup_keyword};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn is_id_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_id_continue(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

// ---------------------------------------------------------------------------
// Lexer
// ---------------------------------------------------------------------------

/// The nessa lexer.
///
/// It converts source text into a stream of [`Token`]s.  In addition to the
/// ordinary tokens, the lexer emits virtual `Indent` / `Outdent` / `Newline`
/// layout tokens that encode the indentation structure.
///
/// ## Format-sensitive context
///
/// Nessa uses a *format-sensitive* indentation scheme.  When a **trigger
/// token** (`:`, `=>`, or `do`) appears immediately before a newline, the
/// lexer enters a format-sensitive context: the next line's indentation
/// level is recorded, and an `Indent` token is emitted.  All subsequent
/// lines at that level become part of the same block.  When the indentation
/// drops back, one or more `Outdent` tokens are emitted.
///
/// Outside a format-sensitive context the lexer still tracks indentation
/// changes for outdent purposes but will **not** open a new indent level.
pub struct Lexer<'src> {
    src: &'src str,
    bytes: &'src [u8],
    cursor: usize,

    // -- layout handling --
    pending: VecDeque<Token>,
    indent_stack: Vec<u32>,
    line_start: usize,
    last_emitted_was_newline: bool,

    // -- format-sensitive trigger --
    can_start_indent: bool,
    last_token_kind: Option<TokenKind>,

    // -- state flags --
    last_was_outdent: bool,
    skip_indent_handling: bool,

    // -- collected errors --
    pub errors: Vec<LexError>,
}

impl<'src> Lexer<'src> {
    pub fn new(src: &'src str) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            cursor: 0,
            pending: VecDeque::new(),
            indent_stack: vec![0],
            line_start: 0,
            last_emitted_was_newline: false,
            can_start_indent: false,
            last_token_kind: None,
            last_was_outdent: false,
            skip_indent_handling: false,
            errors: Vec::new(),
        }
    }

    // -- low-level helpers ------------------------------------------------

    #[inline]
    fn at_end(&self) -> bool {
        self.cursor >= self.bytes.len()
    }

    #[inline]
    fn peek_byte(&self) -> Option<u8> {
        self.bytes.get(self.cursor).copied()
    }

    #[inline]
    fn peek_byte_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.cursor + offset).copied()
    }

    /// Return the current character (full Unicode) without advancing.
    fn current_char(&self) -> Option<char> {
        self.src[self.cursor..].chars().next()
    }

    /// Advance past the current character and return it.
    fn advance_char(&mut self) -> Option<char> {
        let c = self.current_char()?;
        self.cursor += c.len_utf8();
        Some(c)
    }

    fn pos(&self) -> Index {
        self.cursor as Index
    }

    fn token(&self, kind: TokenKind, from: Index, to: Index) -> Token {
        Token::new(kind, from, to)
    }

    fn push_pending(&mut self, kind: TokenKind, pos: Index) {
        self.pending.push_back(Token::new(kind, pos, pos));
    }

    fn push_error(&mut self, kind: LexErrorKind, from: Index, to: Index, msg: impl Into<String>) {
        self.errors.push(LexError::new(kind, from, to, msg));
    }

    // -- indent trigger ---------------------------------------------------

    /// Returns `true` for the three nessa format-sensitive trigger tokens:
    /// `:`, `=>`, `do`.
    fn is_indent_trigger(kind: TokenKind) -> bool {
        matches!(
            kind,
            TokenKind::Colon | TokenKind::FatArrow | TokenKind::KwDo
        )
    }

    // -- layout processing ------------------------------------------------

    /// Called after a `Newline` token has been emitted.  Scans ahead through
    /// blank lines to determine the indentation column of the next
    /// non-blank line, then emits `Indent` or `Outdent` tokens as needed.
    fn handle_newline_indent(&mut self) {
        let mut lookahead = self.cursor;
        let mut col: u32 = 0;

        // Skip through whitespace and blank lines to find the next
        // non-blank line's indentation.
        while lookahead < self.bytes.len() {
            match self.bytes[lookahead] {
                b' ' => {
                    col += 1;
                    lookahead += 1;
                }
                b'\t' => {
                    col += 4;
                    lookahead += 1;
                }
                b'\n' => {
                    // Blank line – reset column and keep going.
                    col = 0;
                    lookahead += 1;
                }
                _ => break,
            }
        }

        let current = *self.indent_stack.last().unwrap();

        if self.can_start_indent {
            self.handle_format_sensitive_indent(col, current);
        } else {
            self.handle_non_format_sensitive_indent(col, current);
        }

        self.can_start_indent = false;
        self.cursor = lookahead;
        self.line_start = self.cursor;
        self.last_emitted_was_newline = false;
    }

    /// Format-sensitive context: a trigger token preceded the newline, so a
    /// deeper indentation opens a new block.
    fn handle_format_sensitive_indent(&mut self, col: u32, current: u32) {
        if col > current {
            self.indent_stack.push(col);
            self.push_pending(TokenKind::Indent, self.pos());
        } else if col < current {
            self.emit_outdents(col);
        }
        // col == current → same level, nothing to do.
    }

    /// Non-format-sensitive context: we only handle outdents.  A deeper
    /// indentation without a trigger is legal (continuation line) but does
    /// **not** open a new indent level.
    fn handle_non_format_sensitive_indent(&mut self, col: u32, current: u32) {
        if col < current {
            self.emit_outdents(col);
        }
    }

    /// Pop indent levels until the stack matches `target_col`, emitting one
    /// `Outdent` per popped level.
    fn emit_outdents(&mut self, target_col: u32) {
        let mut matched = false;

        while self.indent_stack.len() > 1 {
            let top = *self.indent_stack.last().unwrap();
            if top <= target_col {
                if top == target_col {
                    matched = true;
                }
                break;
            }
            self.indent_stack.pop();
            self.push_pending(TokenKind::Outdent, self.pos());
            self.last_was_outdent = true;
        }

        if !matched && target_col != *self.indent_stack.last().unwrap() {
            let pos = self.pos();
            self.push_error(
                LexErrorKind::UnmatchedIndentation,
                pos,
                pos,
                "unmatched indentation level: expected one of the previous levels",
            );
            self.push_pending(TokenKind::Invalid, pos);
        }
    }

    // -- public interface -------------------------------------------------

    /// Tokenize the entire source and return the token vector.
    ///
    /// The stream always ends with an `Eof` token.  Any errors encountered
    /// during lexing are accumulated in [`Self::errors`].
    pub fn tokenize(&mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        loop {
            let tok = self.next_token();
            let is_eof = tok.kind == TokenKind::Eof;
            tokens.push(tok);
            if is_eof {
                break;
            }
        }
        tokens
    }

    /// Produce the next token.
    pub fn next_token(&mut self) -> Token {
        // After an outdent, inject a virtual newline so the parser sees a
        // statement terminator.
        if self.last_was_outdent {
            self.last_was_outdent = false;
            let nl = Token::new(TokenKind::Newline, self.pos(), self.pos());
            self.last_token_kind = Some(TokenKind::Newline);
            self.last_emitted_was_newline = true;
            self.skip_indent_handling = true;
            return nl;
        }

        // Drain pending layout tokens first.
        if let Some(tok) = self.pending.pop_front() {
            if tok.kind == TokenKind::Newline {
                self.last_emitted_was_newline = true;
            } else if tok.kind == TokenKind::Outdent {
                self.last_was_outdent = true;
            }
            self.last_token_kind = Some(tok.kind);
            return tok;
        }

        // EOF
        if self.at_end() {
            return self.handle_eof();
        }

        // Indentation processing after a newline.  This must run BEFORE
        // the physical-newline check so that blank lines are absorbed by
        // the lookahead scan instead of emitting spurious Newline tokens
        // that would reset the indent trigger.
        if self.last_emitted_was_newline && !self.skip_indent_handling {
            self.handle_newline_indent();
            if let Some(tok) = self.pending.pop_front() {
                if tok.kind != TokenKind::Newline {
                    self.last_emitted_was_newline = false;
                }
                if tok.kind == TokenKind::Outdent {
                    self.last_was_outdent = true;
                }
                self.last_token_kind = Some(tok.kind);
                return tok;
            }
        }
        self.skip_indent_handling = false;

        // Physical newline
        if self.peek_byte() == Some(b'\n') {
            return self.handle_newline();
        }

        // Skip horizontal whitespace.
        self.skip_whitespace();

        if self.at_end() {
            return self.handle_eof();
        }

        // Main token recognition loop – comments are consumed silently so
        // we may need to re-enter after skipping one.
        loop {
            if self.at_end() {
                return self.handle_eof();
            }
            if self.peek_byte() == Some(b'\n') {
                return self.handle_newline();
            }

            let start = self.pos();
            let tok = self.recognize_token(start);

            // A zero-width `.Invalid` is the sentinel for "comment was
            // consumed, try again".
            if tok.kind == TokenKind::Invalid && tok.from == tok.to {
                self.skip_whitespace();
                continue;
            }

            self.last_emitted_was_newline = false;
            self.last_token_kind = Some(tok.kind);
            return tok;
        }
    }

    // -- EOF & newline ----------------------------------------------------

    fn handle_eof(&mut self) -> Token {
        if self.indent_stack.len() > 1 {
            self.indent_stack.pop();
            self.last_was_outdent = true;
            return Token::new(TokenKind::Outdent, self.pos(), self.pos());
        }
        Token::new(TokenKind::Eof, self.pos(), self.pos())
    }

    fn handle_newline(&mut self) -> Token {
        let start = self.pos();
        self.cursor += 1; // consume '\n'
        self.line_start = self.cursor;
        self.last_emitted_was_newline = true;

        if let Some(kind) = self.last_token_kind {
            self.can_start_indent = Self::is_indent_trigger(kind);
        }

        let tok = Token::new(TokenKind::Newline, start, self.pos());
        self.last_token_kind = Some(TokenKind::Newline);
        tok
    }

    // -- whitespace & comments -------------------------------------------

    fn skip_whitespace(&mut self) {
        while let Some(b) = self.peek_byte() {
            if b == b' ' || b == b'\t' || b == b'\r' {
                self.cursor += 1;
            } else {
                break;
            }
        }
    }

    // -- token recognition ------------------------------------------------

    fn recognize_token(&mut self, start: Index) -> Token {
        let b = self.bytes[self.cursor];
        match b {
            b'.' => {
                self.cursor += 1;
                self.token(TokenKind::Dot, start, self.pos())
            }
            b'\\' => {
                self.cursor += 1;
                self.token(TokenKind::Backslash, start, self.pos())
            }
            b'(' => {
                self.cursor += 1;
                self.token(TokenKind::LParen, start, self.pos())
            }
            b')' => {
                self.cursor += 1;
                self.token(TokenKind::RParen, start, self.pos())
            }
            b'[' => {
                self.cursor += 1;
                self.token(TokenKind::LBracket, start, self.pos())
            }
            b']' => {
                self.cursor += 1;
                self.token(TokenKind::RBracket, start, self.pos())
            }
            b'}' => {
                self.cursor += 1;
                self.token(TokenKind::RBrace, start, self.pos())
            }
            b',' => {
                self.cursor += 1;
                self.token(TokenKind::Comma, start, self.pos())
            }
            b';' => {
                self.cursor += 1;
                self.token(TokenKind::Semi, start, self.pos())
            }
            b'?' => {
                self.cursor += 1;
                self.token(TokenKind::Question, start, self.pos())
            }
            b'^' => {
                self.cursor += 1;
                self.token(TokenKind::Caret, start, self.pos())
            }
            b'~' => {
                self.cursor += 1;
                self.token(TokenKind::Tilde, start, self.pos())
            }
            b'@' => {
                self.cursor += 1;
                self.token(TokenKind::At, start, self.pos())
            }
            b'$' => {
                self.cursor += 1;
                self.token(TokenKind::Dollar, start, self.pos())
            }
            b'&' => {
                self.cursor += 1;
                self.token(TokenKind::Ampersand, start, self.pos())
            }
            b'#' => {
                self.cursor += 1;
                self.token(TokenKind::Hash, start, self.pos())
            }

            // Multi-char operators
            b'|' => self.lex_pipe(start),
            b'!' => self.lex_bang(start),
            b'=' => self.lex_eq(start),
            b':' => self.lex_colon(start),
            b'+' => self.lex_plus(start),
            b'-' => self.lex_minus(start),
            b'*' => self.lex_star(start),
            b'/' => self.lex_slash(start),
            b'%' => self.lex_percent(start),
            b'<' => self.lex_lt(start),
            b'>' => self.lex_gt(start),
            b'{' => self.lex_lbrace(start),

            // Literals
            b'"' => self.lex_string(start),
            b'\'' => self.lex_quote_or_char_or_macro(start),
            b'0'..=b'9' => self.lex_number(start),
            b'`' => self.lex_arbitrary_id(start),

            // Identifiers & keywords (ASCII start)
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.lex_identifier(start),

            // Non-ASCII identifier start
            _ => {
                if let Some(c) = self.current_char() {
                    if is_id_start(c) {
                        return self.lex_identifier(start);
                    }
                    self.cursor += c.len_utf8();
                    self.push_error(
                        LexErrorKind::UnexpectedChar,
                        start,
                        self.pos(),
                        format!("unexpected character: {:?}", c),
                    );
                } else {
                    self.cursor += 1;
                }
                self.token(TokenKind::Invalid, start, self.pos())
            }
        }
    }

    // -- operator lexers --------------------------------------------------

    fn lex_pipe(&mut self, start: Index) -> Token {
        self.cursor += 1;
        if self.peek_byte() == Some(b'>') {
            self.cursor += 1;
            self.token(TokenKind::PipeGt, start, self.pos())
        } else {
            self.token(TokenKind::Pipe, start, self.pos())
        }
    }

    fn lex_bang(&mut self, start: Index) -> Token {
        self.cursor += 1;
        if self.peek_byte() == Some(b'=') {
            self.cursor += 1;
            self.token(TokenKind::BangEq, start, self.pos())
        } else {
            self.token(TokenKind::Bang, start, self.pos())
        }
    }

    fn lex_eq(&mut self, start: Index) -> Token {
        self.cursor += 1;
        match self.peek_byte() {
            Some(b'=') => {
                self.cursor += 1;
                self.token(TokenKind::EqEq, start, self.pos())
            }
            Some(b'>') => {
                self.cursor += 1;
                self.token(TokenKind::FatArrow, start, self.pos())
            }
            _ => self.token(TokenKind::Eq, start, self.pos()),
        }
    }

    fn lex_colon(&mut self, start: Index) -> Token {
        self.cursor += 1;
        self.token(TokenKind::Colon, start, self.pos())
    }

    fn lex_plus(&mut self, start: Index) -> Token {
        self.cursor += 1;
        match self.peek_byte() {
            Some(b'=') => {
                self.cursor += 1;
                self.token(TokenKind::PlusEq, start, self.pos())
            }
            Some(b'+') => {
                self.cursor += 1;
                self.token(TokenKind::PlusPlus, start, self.pos())
            }
            _ => self.token(TokenKind::Plus, start, self.pos()),
        }
    }

    fn lex_minus(&mut self, start: Index) -> Token {
        self.cursor += 1;
        match self.peek_byte() {
            Some(b'-') => {
                // Line comment: skip to end of line.
                self.cursor += 1;
                while let Some(b) = self.peek_byte() {
                    if b == b'\n' {
                        break;
                    }
                    self.cursor += 1;
                }
                // Zero-width invalid → sentinel for "retry".
                self.token(TokenKind::Invalid, start, start)
            }
            Some(b'=') => {
                self.cursor += 1;
                self.token(TokenKind::MinusEq, start, self.pos())
            }
            Some(b'>') => {
                self.cursor += 1;
                self.token(TokenKind::Arrow, start, self.pos())
            }
            _ => self.token(TokenKind::Minus, start, self.pos()),
        }
    }

    fn lex_star(&mut self, start: Index) -> Token {
        self.cursor += 1;
        if self.peek_byte() == Some(b'=') {
            self.cursor += 1;
            self.token(TokenKind::StarEq, start, self.pos())
        } else {
            self.token(TokenKind::Star, start, self.pos())
        }
    }

    fn lex_slash(&mut self, start: Index) -> Token {
        self.cursor += 1;
        if self.peek_byte() == Some(b'=') {
            self.cursor += 1;
            self.token(TokenKind::SlashEq, start, self.pos())
        } else {
            self.token(TokenKind::Slash, start, self.pos())
        }
    }

    fn lex_percent(&mut self, start: Index) -> Token {
        self.cursor += 1;
        if self.peek_byte() == Some(b'=') {
            self.cursor += 1;
            self.token(TokenKind::PercentEq, start, self.pos())
        } else {
            self.token(TokenKind::Percent, start, self.pos())
        }
    }

    fn lex_lt(&mut self, start: Index) -> Token {
        self.cursor += 1;
        if self.peek_byte() == Some(b'=') {
            self.cursor += 1;
            self.token(TokenKind::LtEq, start, self.pos())
        } else {
            self.token(TokenKind::Lt, start, self.pos())
        }
    }

    fn lex_gt(&mut self, start: Index) -> Token {
        self.cursor += 1;
        if self.peek_byte() == Some(b'=') {
            self.cursor += 1;
            self.token(TokenKind::GtEq, start, self.pos())
        } else {
            self.token(TokenKind::Gt, start, self.pos())
        }
    }

    fn lex_lbrace(&mut self, start: Index) -> Token {
        self.cursor += 1;
        if self.peek_byte() == Some(b'-') {
            // Block comment {- ... -}
            self.cursor += 1;
            let mut depth: u32 = 1;
            while depth > 0 {
                match self.peek_byte() {
                    None => break,
                    Some(b'{') => {
                        self.cursor += 1;
                        if self.peek_byte() == Some(b'-') {
                            self.cursor += 1;
                            depth += 1;
                        }
                    }
                    Some(b'-') => {
                        self.cursor += 1;
                        if self.peek_byte() == Some(b'}') {
                            self.cursor += 1;
                            depth -= 1;
                        }
                    }
                    _ => {
                        self.cursor += 1;
                    }
                }
            }
            // Zero-width invalid → retry sentinel.
            self.token(TokenKind::Invalid, start, start)
        } else {
            self.token(TokenKind::LBrace, start, self.pos())
        }
    }

    // -- literal lexers ---------------------------------------------------

    fn lex_string(&mut self, start: Index) -> Token {
        self.cursor += 1; // consume opening '"'

        // Peek ahead to decide: does this string contain any `{expr}` interpolation?
        // We do a quick scan to detect `{` that is not `{{`.
        let has_interpolation = {
            let mut i = self.cursor;
            let mut found = false;
            while i < self.bytes.len() {
                match self.bytes[i] {
                    b'"' => break,
                    b'\\' => {
                        i += 2;
                    }
                    b'{' if self.bytes.get(i + 1) != Some(&b'{') => {
                        found = true;
                        break;
                    }
                    b'\n' => break,
                    _ => {
                        i += 1;
                    }
                }
            }
            found
        };

        if !has_interpolation {
            // Plain string — scan to closing `"` and return a single String token.
            loop {
                match self.peek_byte() {
                    None => {
                        self.push_error(
                            LexErrorKind::InvalidStrLiteral,
                            start,
                            self.pos(),
                            "unterminated string literal",
                        );
                        return self.token(TokenKind::Invalid, start, self.pos());
                    }
                    Some(b'"') => {
                        self.cursor += 1;
                        return self.token(TokenKind::String, start, self.pos());
                    }
                    Some(b'\\') => {
                        self.cursor += 1;
                        match self.peek_byte() {
                            Some(b'n' | b't' | b'r' | b'\\' | b'"' | b'\'') => {
                                self.cursor += 1;
                            }
                            Some(b'x') => {
                                self.cursor += 1;
                                if !self.expect_hex_digits(2, start) {
                                    return self.token(TokenKind::Invalid, start, self.pos());
                                }
                            }
                            Some(b'u') => {
                                self.cursor += 1;
                                if !self.expect_hex_digits(4, start) {
                                    return self.token(TokenKind::Invalid, start, self.pos());
                                }
                            }
                            Some(_) => {
                                self.push_error(
                                    LexErrorKind::InvalidStrLiteral,
                                    start,
                                    self.pos(),
                                    "invalid escape sequence",
                                );
                                self.skip_to_string_end();
                                return self.token(TokenKind::Invalid, start, self.pos());
                            }
                            None => {
                                self.push_error(
                                    LexErrorKind::InvalidStrLiteral,
                                    start,
                                    self.pos(),
                                    "unterminated escape sequence",
                                );
                                return self.token(TokenKind::Invalid, start, self.pos());
                            }
                        }
                    }
                    Some(b'\n') => {
                        self.push_error(
                            LexErrorKind::InvalidStrLiteral,
                            start,
                            self.pos(),
                            "unexpected newline in string literal",
                        );
                        return self.token(TokenKind::Invalid, start, self.pos());
                    }
                    Some(_) => {
                        self.advance_char();
                    }
                }
            }
        }

        // Interpolated string — emit FStringStart, then segments and expressions.
        // We eagerly push all tokens into `self.pending` and return the first.
        self.pending
            .push_back(Token::new(TokenKind::FStringStart, start, start + 1));

        loop {
            let seg_start = self.pos();
            // Collect a literal segment up to `{`, `}`, or end of string.
            loop {
                match self.peek_byte() {
                    None | Some(b'\n') => {
                        self.push_error(
                            LexErrorKind::InvalidStrLiteral,
                            start,
                            self.pos(),
                            "unterminated string literal",
                        );
                        // Emit whatever we have and stop.
                        if self.pos() > seg_start {
                            self.pending.push_back(Token::new(
                                TokenKind::FStringLiteral,
                                seg_start,
                                self.pos(),
                            ));
                        }
                        self.pending.push_back(Token::new(
                            TokenKind::FStringEnd,
                            self.pos(),
                            self.pos(),
                        ));
                        return self.pending.pop_front().unwrap();
                    }
                    Some(b'"') => {
                        // End of string.
                        if self.pos() > seg_start {
                            self.pending.push_back(Token::new(
                                TokenKind::FStringLiteral,
                                seg_start,
                                self.pos(),
                            ));
                        }
                        self.cursor += 1;
                        let end = self.pos();
                        self.pending
                            .push_back(Token::new(TokenKind::FStringEnd, end - 1, end));
                        return self.pending.pop_front().unwrap();
                    }
                    Some(b'{') => {
                        // `{{` → literal `{`
                        if self.bytes.get(self.cursor + 1) == Some(&b'{') {
                            self.cursor += 2;
                            continue;
                        }
                        // Start of interpolated expression.
                        if self.pos() > seg_start {
                            self.pending.push_back(Token::new(
                                TokenKind::FStringLiteral,
                                seg_start,
                                self.pos(),
                            ));
                        }
                        let brace_start = self.pos();
                        self.cursor += 1; // consume `{`
                        self.pending.push_back(Token::new(
                            TokenKind::FStringExprStart,
                            brace_start,
                            self.pos(),
                        ));
                        // Now lex expression tokens until matching `}`.
                        // We use a brace depth counter and call recognize_token repeatedly.
                        let mut depth: u32 = 1;
                        while depth > 0 && !self.at_end() {
                            self.skip_whitespace();
                            if self.at_end() {
                                break;
                            }
                            match self.peek_byte() {
                                Some(b'{') => {
                                    let ts = self.pos();
                                    self.cursor += 1;
                                    depth += 1;
                                    self.pending.push_back(Token::new(
                                        TokenKind::LBrace,
                                        ts,
                                        self.pos(),
                                    ));
                                }
                                Some(b'}') => {
                                    depth -= 1;
                                    if depth == 0 {
                                        let ts = self.pos();
                                        self.cursor += 1;
                                        self.pending.push_back(Token::new(
                                            TokenKind::FStringExprEnd,
                                            ts,
                                            self.pos(),
                                        ));
                                    } else {
                                        let ts = self.pos();
                                        self.cursor += 1;
                                        self.pending.push_back(Token::new(
                                            TokenKind::RBrace,
                                            ts,
                                            self.pos(),
                                        ));
                                    }
                                }
                                Some(b'"') => {
                                    // Nested string inside interpolation — lex it.
                                    let ns = self.pos();
                                    let nested = self.lex_string(ns);
                                    // nested already pushed its tokens to pending if interpolated,
                                    // or returns a single String token.
                                    if nested.kind == TokenKind::String
                                        || nested.kind == TokenKind::FStringStart
                                        || nested.kind == TokenKind::Invalid
                                    {
                                        self.pending.push_back(nested);
                                    }
                                    // (if it was FStringStart the rest is already in pending)
                                }
                                _ => {
                                    let ts = self.pos();
                                    let tok = self.recognize_token(ts);
                                    self.pending.push_back(tok);
                                }
                            }
                        }
                        // After closing `}` we continue scanning the string body.
                        break; // break inner literal-scan loop, restart outer loop
                    }
                    Some(b'\\') => {
                        self.cursor += 1; // consume `\`
                        match self.peek_byte() {
                            Some(b'n' | b't' | b'r' | b'\\' | b'"' | b'\'') => {
                                self.cursor += 1;
                            }
                            Some(b'x') => {
                                self.cursor += 1;
                                self.expect_hex_digits(2, start);
                            }
                            Some(b'u') => {
                                self.cursor += 1;
                                self.expect_hex_digits(4, start);
                            }
                            _ => {
                                self.cursor += 1;
                            }
                        }
                    }
                    Some(_) => {
                        self.advance_char();
                    }
                }
            }
        }
    }

    /// Expect exactly `n` hex digits at the current position.  Returns
    /// `false` and emits an error on failure.
    fn expect_hex_digits(&mut self, n: usize, lit_start: Index) -> bool {
        for _ in 0..n {
            match self.peek_byte() {
                Some(b) if b.is_ascii_hexdigit() => {
                    self.cursor += 1;
                }
                _ => {
                    self.push_error(
                        LexErrorKind::InvalidStrLiteral,
                        lit_start,
                        self.pos(),
                        "invalid or incomplete hex/unicode escape sequence",
                    );
                    self.skip_to_string_end();
                    return false;
                }
            }
        }
        true
    }

    /// After a string-literal error, skip forward to the closing `"` (or
    /// newline / EOF) so the rest of the source is not mis-tokenized.
    fn skip_to_string_end(&mut self) {
        loop {
            match self.peek_byte() {
                Some(b'"') => {
                    self.cursor += 1;
                    break;
                }
                Some(b'\n') | None => break,
                Some(b'\\') => {
                    self.cursor += 1;
                    if self.peek_byte().is_some() {
                        self.cursor += 1;
                    }
                }
                _ => {
                    self.cursor += 1;
                }
            }
        }
    }

    /// `'` has been seen.  Decide if it's a quote token, a character literal,
    /// or macro content (`'{ ... }`).
    ///
    /// Parallel to `.` (always Dot for projection): `'` is the view operator
    /// token whenever the following text is not a char literal (`'x'` / `'\n'`).
    /// That lets Pratt parse `expr ' id` the same way as `expr . id`.
    fn lex_quote_or_char_or_macro(&mut self, start: Index) -> Token {
        self.cursor += 1; // consume opening '

        match self.peek_byte() {
            Some(b' ' | b'\t') => {
                // Skip whitespace, then check for macro `'{`.
                while matches!(self.peek_byte(), Some(b' ' | b'\t')) {
                    self.cursor += 1;
                }
                if self.peek_byte() == Some(b'{') {
                    return self.lex_macro_content(start);
                }
                // Just a standalone quote.
                self.token(TokenKind::Quote, start, start + 1)
            }
            Some(b'{') => self.lex_macro_content(start),
            Some(_) => {
                if self.looks_like_char_literal() {
                    self.lex_char_after_quote(start)
                } else {
                    // View operator: leave the following identifier for the next token.
                    self.token(TokenKind::Quote, start, start + 1)
                }
            }
            None => self.token(TokenKind::Quote, start, self.pos()),
        }
    }

    /// True when the bytes after the opening `'` form a char literal attempt
    /// (`'x'`, `'\n'`, or an incomplete `'x` at EOF) rather than a view op (`'id`).
    fn looks_like_char_literal(&self) -> bool {
        let s = &self.src[self.cursor..];
        if s.is_empty() {
            return false;
        }
        if s.as_bytes()[0] == b'\\' {
            // Escape forms: \n \t \r \\ \' \"  then closing '
            let rest = &s[1..];
            if rest.is_empty() {
                return true; // incomplete escape → char error path
            }
            let b = rest.as_bytes()[0];
            if matches!(b, b'n' | b't' | b'r' | b'\\' | b'"' | b'\'') {
                return true; // complete or missing closer — char path handles both
            }
            if b == b'x' || b == b'u' {
                return true;
            }
            return false;
        }
        let mut chars = s.chars();
        let Some(_) = chars.next() else {
            return false;
        };
        match chars.next() {
            Some('\'') => true, // `'x'`
            None => true,       // `'x` at EOF — unterminated char
            Some(_) => false,   // `'id…` — view operator (like `.` before an id)
        }
    }

    /// Starting right after the opening `'`, scan a character literal.
    fn lex_char_after_quote(&mut self, start: Index) -> Token {
        match self.peek_byte() {
            Some(b'\\') => {
                self.cursor += 1;
                match self.peek_byte() {
                    Some(b'n' | b't' | b'r' | b'\\' | b'"' | b'\'') => {
                        self.cursor += 1;
                    }
                    Some(_) => {
                        self.push_error(
                            LexErrorKind::InvalidCharLiteral,
                            start,
                            self.pos(),
                            "invalid escape sequence in character literal",
                        );
                        return self.token(TokenKind::Invalid, start, self.pos());
                    }
                    None => {
                        self.push_error(
                            LexErrorKind::InvalidCharLiteral,
                            start,
                            self.pos(),
                            "unterminated escape sequence in character literal",
                        );
                        return self.token(TokenKind::Invalid, start, self.pos());
                    }
                }
            }
            Some(b'\n') => {
                self.push_error(
                    LexErrorKind::InvalidCharLiteral,
                    start,
                    self.pos(),
                    "unexpected newline in character literal",
                );
                return self.token(TokenKind::Invalid, start, self.pos());
            }
            Some(b'\'') => {
                self.cursor += 1; // consume the closing quote
                self.push_error(
                    LexErrorKind::InvalidCharLiteral,
                    start,
                    self.pos(),
                    "empty character literal",
                );
                return self.token(TokenKind::Invalid, start, self.pos());
            }
            Some(_) => {
                // Consume a full Unicode codepoint.
                self.advance_char();
            }
            None => {
                self.push_error(
                    LexErrorKind::InvalidCharLiteral,
                    start,
                    self.pos(),
                    "unterminated character literal",
                );
                return self.token(TokenKind::Invalid, start, self.pos());
            }
        }

        // Expect closing quote.
        if self.peek_byte() == Some(b'\'') {
            self.cursor += 1;
            self.token(TokenKind::Char, start, self.pos())
        } else {
            self.push_error(
                LexErrorKind::InvalidCharLiteral,
                start,
                self.pos(),
                "character literal too long or missing closing quote",
            );
            self.token(TokenKind::Invalid, start, self.pos())
        }
    }

    /// Lex macro content: `'{ ... }`.  The `'` has already been consumed.
    fn lex_macro_content(&mut self, start: Index) -> Token {
        self.cursor += 1; // consume '{'
        let mut depth: u32 = 1;

        while depth > 0 {
            match self.peek_byte() {
                None => break,
                Some(b'{') => {
                    self.cursor += 1;
                    depth += 1;
                }
                Some(b'}') => {
                    self.cursor += 1;
                    depth -= 1;
                }
                _ => {
                    self.cursor += 1;
                }
            }
        }
        self.token(TokenKind::MacroContent, start, self.pos())
    }

    // -- numbers ----------------------------------------------------------

    fn lex_number(&mut self, start: Index) -> Token {
        if self.peek_byte() == Some(b'0') {
            self.cursor += 1;
            match self.peek_byte() {
                Some(b'b' | b'B') => {
                    self.cursor += 1;
                    return self.lex_bin_digits(start);
                }
                Some(b'o' | b'O') => {
                    self.cursor += 1;
                    return self.lex_oct_digits(start);
                }
                Some(b'x' | b'X') => {
                    self.cursor += 1;
                    return self.lex_hex_digits(start);
                }
                _ => {}
            }
        }

        // Decimal digits.
        while matches!(self.peek_byte(), Some(b'0'..=b'9')) {
            self.cursor += 1;
        }

        // A number after a projection dot is an integer member index. Keep
        // subsequent dots separate so nested `value.0.1` is not a real literal.
        if self.last_token_kind != Some(TokenKind::Dot)
            && self.peek_byte() == Some(b'.')
            && let Some(after_dot) = self.peek_byte_at(1)
            && after_dot.is_ascii_digit()
        {
            return self.lex_float_after_dot(start);
        }

        self.token(TokenKind::Integer, start, self.pos())
    }

    fn lex_bin_digits(&mut self, start: Index) -> Token {
        let mut has = false;
        while matches!(self.peek_byte(), Some(b'0' | b'1')) {
            has = true;
            self.cursor += 1;
        }
        if !has {
            self.push_error(
                LexErrorKind::InvalidNumber,
                start,
                self.pos(),
                "binary number literal has no digits",
            );
            return self.token(TokenKind::Invalid, start, self.pos());
        }
        self.token(TokenKind::IntBin, start, self.pos())
    }

    fn lex_oct_digits(&mut self, start: Index) -> Token {
        let mut has = false;
        while matches!(self.peek_byte(), Some(b'0'..=b'7')) {
            has = true;
            self.cursor += 1;
        }
        if !has {
            self.push_error(
                LexErrorKind::InvalidNumber,
                start,
                self.pos(),
                "octal number literal has no digits",
            );
            return self.token(TokenKind::Invalid, start, self.pos());
        }
        self.token(TokenKind::IntOct, start, self.pos())
    }

    fn lex_hex_digits(&mut self, start: Index) -> Token {
        let mut has = false;
        while matches!(self.peek_byte(), Some(b) if b.is_ascii_hexdigit()) {
            has = true;
            self.cursor += 1;
        }
        if !has {
            self.push_error(
                LexErrorKind::InvalidNumber,
                start,
                self.pos(),
                "hexadecimal number literal has no digits",
            );
            return self.token(TokenKind::Invalid, start, self.pos());
        }
        self.token(TokenKind::IntHex, start, self.pos())
    }

    fn lex_float_after_dot(&mut self, start: Index) -> Token {
        self.cursor += 1; // consume '.'

        let mut has_frac = false;
        while matches!(self.peek_byte(), Some(b'0'..=b'9')) {
            has_frac = true;
            self.cursor += 1;
        }
        if !has_frac {
            self.push_error(
                LexErrorKind::InvalidNumber,
                start,
                self.pos(),
                "float literal missing digits after decimal point",
            );
            return self.token(TokenKind::Invalid, start, self.pos());
        }

        // Scientific notation.
        if matches!(self.peek_byte(), Some(b'e' | b'E')) {
            self.cursor += 1;
            if matches!(self.peek_byte(), Some(b'+' | b'-')) {
                self.cursor += 1;
            }
            let mut has_exp = false;
            while matches!(self.peek_byte(), Some(b'0'..=b'9')) {
                has_exp = true;
                self.cursor += 1;
            }
            if !has_exp {
                self.push_error(
                    LexErrorKind::InvalidNumber,
                    start,
                    self.pos(),
                    "scientific notation missing exponent digits",
                );
                return self.token(TokenKind::Invalid, start, self.pos());
            }
            return self.token(TokenKind::RealSci, start, self.pos());
        }

        self.token(TokenKind::Real, start, self.pos())
    }

    // -- identifiers & keywords -------------------------------------------

    fn lex_identifier(&mut self, start: Index) -> Token {
        while let Some(c) = self.current_char() {
            if !is_id_continue(c) {
                break;
            }
            self.cursor += c.len_utf8();
        }

        let text = &self.src[start as usize..self.cursor];

        if let Some(kw) = lookup_keyword(text) {
            return self.token(kw, start, self.pos());
        }

        self.token(TokenKind::Id, start, self.pos())
    }

    fn lex_arbitrary_id(&mut self, start: Index) -> Token {
        self.cursor += 1; // consume opening '`'

        loop {
            match self.peek_byte() {
                Some(b'`') => {
                    self.cursor += 1;
                    return self.token(TokenKind::ArbitraryId, start, self.pos());
                }
                Some(b'\n') => {
                    self.push_error(
                        LexErrorKind::InvalidArbitraryId,
                        start,
                        self.pos(),
                        "arbitrary identifier cannot contain newlines",
                    );
                    return self.token(TokenKind::Invalid, start, self.pos());
                }
                None => {
                    self.push_error(
                        LexErrorKind::InvalidArbitraryId,
                        start,
                        self.pos(),
                        "unterminated arbitrary identifier",
                    );
                    return self.token(TokenKind::Invalid, start, self.pos());
                }
                _ => {
                    self.cursor += 1;
                }
            }
        }
    }
}

/// Convenience function: tokenize a source string in one shot.
pub fn tokenize(src: &str) -> (Vec<Token>, Vec<LexError>) {
    let mut lexer = Lexer::new(src);
    let tokens = lexer.tokenize();
    (tokens, lexer.errors)
}
