use ast::{Ast, NodeIndex};
use diagnostic::{DiagnosticContext, NessaError, SourceSpanError, SourceSpanMapper};
use lexer::token::{Token, TokenKind};
use rustc_span::{BytePos, Span};

use crate::error::{ParseError, ParseErrorKind};

/// The nessa PEG parser.
///
/// Each file gets its own `Parser` instance.  The `file_base_pos` offset is
/// added to every span so that positions are absolute within the `SourceMap`.
pub struct Parser<'a> {
    tokens: &'a [Token],
    source: &'a str,
    source_mapper: Result<SourceSpanMapper, SourceSpanError>,
    ast: Ast,
    diag_ctx: &'a DiagnosticContext<'a>,

    /// Byte offset of the file within the SourceMap.  Added to token positions
    /// to produce absolute spans.
    file_base_pos: BytePos,

    /// Current cursor position in the token stream.
    cursor: usize,
    /// A wildcard consumed as an import path terminator can end a statement;
    /// the same token in an arithmetic expression cannot.
    import_glob_end: Option<usize>,

    /// Stack of saved cursor positions (for span tracking per parse rule).
    cursor_stack: Vec<usize>,

    /// Accumulated parse errors.
    errors: Vec<ParseError>,
}

impl<'a> Parser<'a> {
    pub fn new(
        tokens: &'a [Token],
        source: &'a str,
        diag_ctx: &'a DiagnosticContext<'a>,
        file_base_pos: BytePos,
    ) -> Self {
        Self {
            tokens,
            source,
            source_mapper: diag_ctx.source_span_mapper(file_base_pos, source),
            ast: Ast::new(),
            diag_ctx,
            file_base_pos,
            cursor: 0,
            import_glob_end: None,
            cursor_stack: vec![0],
            errors: Vec::new(),
        }
    }

    /// Run the parser and consume it, returning the built AST.
    ///
    /// Parse errors (if any) are emitted to the diagnostic context before
    /// returning.  Callers that need to inspect errors programmatically should
    /// use [`finish`] instead.
    pub fn parse(mut self) -> Ast {
        if self.source_mapper.is_ok()
            && let Ok(root) = crate::basic::try_file_scope(&mut self)
        {
            self.ast.root = root;
        }
        let context = self.diag_ctx;
        let base = self.file_base_pos;
        let (ast, errors) = self.finish();
        for error in errors {
            error.emit(context, base);
        }
        ast
    }

    /// Convert raw parser positions once while retaining original source bytes.
    pub fn finish(mut self) -> (Ast, Vec<ParseError>) {
        let mapped = (|| -> Result<(), SourceSpanError> {
            let mapper = self.source_mapper.as_ref().map_err(|error| *error)?;
            let mut errors = self.errors.clone();
            for error in &mut errors {
                let location = mapper.map_raw_absolute(error.span)?;
                error.span = location.span;
                error.source_start = location.file_start;
                if let Some(label) = &mut error.label {
                    label.span = mapper.map_raw_absolute(label.span)?.span;
                }
            }
            self.ast.attach_source(self.source.to_owned(), mapper)?;
            self.errors = errors;
            Ok(())
        })();
        if let Err(error) = mapped {
            self.ast.source = Some(self.source.to_owned());
            self.ast.root = NodeIndex::NULL;
            self.errors = vec![ParseError::new(
                ParseErrorKind::InvalidSyntax,
                rustc_span::DUMMY_SP,
                format!("Invalid source coordinates: {error}"),
            )];
        }
        (self.ast, self.errors)
    }

    // -- Cursor stack (span tracking) ----------------------------------------

    /// Push the current cursor position and return a guard that pops it on drop.
    ///
    /// The returned [`ParseGuard`] holds a raw pointer to `cursor_stack` so
    /// that parsing can continue through `&mut self` while the guard exists.
    ///
    /// # Safety (internal)
    ///
    /// The raw pointer aliases the `cursor_stack` field while the caller keeps
    /// using `&mut Parser`.  This is sound because:
    ///
    /// - `cursor_stack` is only ever modified by `enter` (push) and the
    ///   guard's `Drop` (pop) — **never** by the parsing methods that touch
    ///   `cursor`, `ast`, or `errors`.
    /// - The `Parser` is pinned behind `&mut` for the entire call tree, so the
    ///   pointer stays valid until the guard is dropped.
    /// - Every inner `enter`/`drop` pair is fully nested, so the pop in `Drop`
    ///   always removes exactly the entry that was pushed here.
    #[inline]
    pub(crate) fn enter(&mut self) -> ParseGuard {
        self.cursor_stack.push(self.cursor);
        ParseGuard {
            cursor_stack: &mut self.cursor_stack as *mut Vec<usize>,
        }
    }

    // -- Token access ---------------------------------------------------------

    pub(crate) fn mark_import_glob_end(&mut self) {
        self.import_glob_end = Some(self.cursor);
    }

    /// Peek at the next meaningful token (skipping over non-semantic newlines),
    /// without consuming it.
    pub(crate) fn peek_token(&self) -> Token {
        if self.cursor >= self.tokens.len() {
            return Token::EOF;
        }
        self.next_token_inner(self.cursor).token
    }

    /// Check if the next sequence of tokens matches the given kinds.
    pub(crate) fn peek(&self, expected: &[TokenKind]) -> bool {
        if self.cursor >= self.tokens.len() {
            return false;
        }
        let mut state = self.next_token_inner(self.cursor);
        for (i, &kind) in expected.iter().enumerate() {
            if state.token.kind != kind {
                return false;
            }
            if i + 1 < expected.len() {
                if state.cursor >= self.tokens.len() {
                    return false;
                }
                state = self.next_token_inner(state.cursor);
            }
        }
        true
    }

    /// Consume and return the next meaningful token.
    pub(crate) fn next_token(&mut self) -> Token {
        if self.cursor >= self.tokens.len() {
            return Token::EOF;
        }
        let result = self.next_token_inner(self.cursor);
        self.cursor = result.cursor;
        result.token
    }

    /// Consume a token if it matches the expected kind.  Returns `true` if consumed.
    pub(crate) fn eat_token(&mut self, kind: TokenKind) -> bool {
        if self.cursor >= self.tokens.len() {
            return false;
        }
        let state = self.next_token_inner(self.cursor);
        if state.token.kind == kind {
            self.cursor = state.cursor;
            true
        } else {
            false
        }
    }

    /// Skip `count` tokens (consuming them one by one).
    pub(crate) fn eat_tokens(&mut self, count: usize) {
        for _ in 0..count {
            self.next_token();
        }
    }

    /// Consume a token if it matches, otherwise emit an error and return Err.
    pub(crate) fn expect_token(&mut self, expected: TokenKind) -> Result<(), ParseErrorKind> {
        if self.eat_token(expected) {
            Ok(())
        } else {
            self.err(
                ParseErrorKind::UnexpectedToken,
                self.next_token_span(),
                format!(
                    "Expected `{}`, found `{}`",
                    expected.lexeme(),
                    self.peek_token().kind.lexeme()
                ),
            )
        }
    }

    /// Virtual newline-to-semicolon conversion logic.
    ///
    /// When a newline appears between a token that can-end-statement and a
    /// token that can-start-statement, the newline is reinterpreted as a
    /// semicolon.  Otherwise it's silently skipped.
    fn next_token_inner(&self, current: usize) -> TokenState {
        let mut cursor = current;
        let mut token = self.tokens[cursor];

        while token.kind == TokenKind::Newline && cursor < self.tokens.len() {
            if cursor > 0 && cursor < self.tokens.len() - 1 {
                let prev = self.tokens[cursor - 1].kind;
                let next = self.tokens[cursor + 1].kind;
                if (prev.can_end_statement() || self.import_glob_end == Some(cursor))
                    && next.can_start_statement()
                {
                    // Treat newline as semicolon
                    token.kind = TokenKind::Semi;
                    break;
                } else {
                    // Skip this newline
                    cursor += 1;
                    if cursor < self.tokens.len() {
                        token = self.tokens[cursor];
                    } else {
                        return TokenState {
                            cursor: cursor + 1,
                            token: Token::EOF,
                        };
                    }
                    continue;
                }
            }
            cursor += 1;
            if cursor < self.tokens.len() {
                token = self.tokens[cursor];
            }
        }

        TokenState {
            cursor: cursor + 1,
            token,
        }
    }

    // -- Span helpers ---------------------------------------------------------

    /// Get the source text of the next token (before consuming it).
    pub(crate) fn next_token_text(&self) -> &str {
        if self.cursor >= self.tokens.len() {
            return "";
        }
        let token = self.next_token_inner(self.cursor).token;
        token.text(self.source)
    }

    /// Span of the next token (for error reporting), adjusted by file base pos.
    pub(crate) fn next_token_span(&self) -> Span {
        if self.cursor >= self.tokens.len() {
            return u32::try_from(self.source.len())
                .ok()
                .and_then(|length| self.file_base_pos.0.checked_add(length))
                .map_or(rustc_span::DUMMY_SP, |end| {
                    Span::new(BytePos(end), BytePos(end))
                });
        }
        let token = self.next_token_inner(self.cursor).token;
        let lo = BytePos(self.file_base_pos.0 + token.from);
        let hi = BytePos(self.file_base_pos.0 + token.to);
        Span::new(lo, hi)
    }

    /// Span from the cursor-stack start to the current cursor position,
    /// adjusted by file base pos.
    pub(crate) fn current_span(&self) -> Span {
        let start = *self.cursor_stack.last().unwrap_or(&0);
        let end = self.cursor.min(self.tokens.len().saturating_sub(1));
        let lo = BytePos(self.file_base_pos.0 + self.tokens[start].from);
        let hi = BytePos(self.file_base_pos.0 + self.tokens[end].to);
        Span::new(lo, hi)
    }

    // -- AST access -----------------------------------------------------------

    /// Mutable access to the AST being built.
    #[inline]
    pub(crate) fn ast(&mut self) -> &mut Ast {
        &mut self.ast
    }

    // -- Error helpers --------------------------------------------------------

    /// Record a parse error and return `Err(kind)`.
    pub(crate) fn err(
        &mut self,
        kind: ParseErrorKind,
        span: Span,
        message: impl Into<String>,
    ) -> Result<(), ParseErrorKind> {
        self.errors.push(ParseError::new(kind, span, message));
        Err(kind)
    }

    /// Record a parse error with label and return `Err(kind)`.
    pub(crate) fn err_with_label(
        &mut self,
        kind: ParseErrorKind,
        span: Span,
        message: impl Into<String>,
        label_span: Span,
        label_message: impl Into<String>,
    ) -> Result<(), ParseErrorKind> {
        self.errors.push(ParseError::with_label(
            kind,
            span,
            message,
            label_span,
            label_message,
        ));
        Err(kind)
    }
}

/// Internal state returned by `next_token_inner`.
struct TokenState {
    cursor: usize,
    token: Token,
}

/// Null sentinel for `NodeIndex`.
pub(crate) const NULL: NodeIndex = NodeIndex::NULL;

/// RAII guard returned by [`Parser::enter`].
///
/// Pops the cursor-stack entry on drop so that `exit` is never forgotten,
/// even across early returns and `?` propagation.
pub(crate) struct ParseGuard {
    cursor_stack: *mut Vec<usize>,
}

impl Drop for ParseGuard {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: see the safety comment on `Parser::enter`.
        unsafe {
            (*self.cursor_stack).pop();
        }
    }
}
