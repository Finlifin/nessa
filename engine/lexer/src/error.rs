use std::fmt;

use diagnostic::{DiagnosticContext, NessaError};
use rustc_span::{BytePos, Span};

use crate::token::Index;

const LEX_ERROR_BASE: u32 = 1000;

/// Error kinds that the nessa lexer can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexErrorKind {
    InvalidStrLiteral,
    InvalidCharLiteral,
    InvalidNumber,
    InvalidArbitraryId,
    UnmatchedIndentation,
    UnexpectedChar,
}

impl fmt::Display for LexErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidStrLiteral => write!(f, "InvalidStrLiteral"),
            Self::InvalidCharLiteral => write!(f, "InvalidCharLiteral"),
            Self::InvalidNumber => write!(f, "InvalidNumber"),
            Self::InvalidArbitraryId => write!(f, "InvalidArbitraryId"),
            Self::UnmatchedIndentation => write!(f, "UnmatchedIndentation"),
            Self::UnexpectedChar => write!(f, "UnexpectedChar"),
        }
    }
}

/// A single lexer error with contextual information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub kind: LexErrorKind,
    pub message: String,
    pub from: Index,
    pub to: Index,
}

impl LexError {
    pub fn new(kind: LexErrorKind, from: Index, to: Index, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            from,
            to,
        }
    }

    pub fn span(&self, base_pos: BytePos) -> Span {
        Span::new(
            BytePos(base_pos.0 + self.from),
            BytePos(base_pos.0 + self.to),
        )
    }
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at {}..{}: {}",
            self.kind, self.from, self.to, self.message
        )
    }
}

impl std::error::Error for LexError {}

impl NessaError for LexError {
    fn error_code(&self) -> u32 {
        LEX_ERROR_BASE
            + match self.kind {
                LexErrorKind::InvalidStrLiteral => 1,
                LexErrorKind::InvalidCharLiteral => 2,
                LexErrorKind::InvalidNumber => 3,
                LexErrorKind::InvalidArbitraryId => 4,
                LexErrorKind::UnmatchedIndentation => 5,
                LexErrorKind::UnexpectedChar => 6,
            }
    }

    fn error_name(&self) -> &'static str {
        match self.kind {
            LexErrorKind::InvalidStrLiteral => "InvalidStrLiteral",
            LexErrorKind::InvalidCharLiteral => "InvalidCharLiteral",
            LexErrorKind::InvalidNumber => "InvalidNumber",
            LexErrorKind::InvalidArbitraryId => "InvalidArbitraryId",
            LexErrorKind::UnmatchedIndentation => "UnmatchedIndentation",
            LexErrorKind::UnexpectedChar => "UnexpectedChar",
        }
    }

    fn emit(&self, diag_ctx: &DiagnosticContext, base_pos: BytePos) {
        let span = self.span(base_pos);
        diag_ctx
            .error(self.message.clone())
            .with_code(self.error_code())
            .with_primary_span(span)
            .with_error_label(span, format!("{}", self.kind))
            .emit(diag_ctx);
    }
}
