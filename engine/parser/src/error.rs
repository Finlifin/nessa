use diagnostic::{DiagnosticContext, NessaError};
use rustc_span::{BytePos, Span};

use ast::NodeIndex;

const PARSE_ERROR_BASE: u32 = 2000;

/// Parsing error kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseErrorKind {
    UnexpectedToken,
    InvalidSyntax,
    InvalidIndentation,
    ControlMeetExtendedCall,
    InvalidBlockPrefix,
}

/// A single parse error with span and message.
#[derive(Debug, Clone)]
pub struct ParseError {
    pub kind: ParseErrorKind,
    pub message: String,
    pub span: Span,
    pub label: Option<ParseErrorLabel>,
}

#[derive(Debug, Clone)]
pub struct ParseErrorLabel {
    pub span: Span,
    pub message: String,
}

impl ParseError {
    pub fn new(kind: ParseErrorKind, span: Span, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            span,
            label: None,
        }
    }

    pub fn with_label(
        kind: ParseErrorKind,
        span: Span,
        message: impl Into<String>,
        label_span: Span,
        label_message: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            span,
            label: Some(ParseErrorLabel {
                span: label_span,
                message: label_message.into(),
            }),
        }
    }
}

impl NessaError for ParseError {
    fn error_code(&self) -> u32 {
        PARSE_ERROR_BASE
            + match self.kind {
                ParseErrorKind::UnexpectedToken => 1,
                ParseErrorKind::InvalidSyntax => 2,
                ParseErrorKind::InvalidIndentation => 3,
                ParseErrorKind::ControlMeetExtendedCall => 4,
                ParseErrorKind::InvalidBlockPrefix => 5,
            }
    }

    fn error_name(&self) -> &'static str {
        match self.kind {
            ParseErrorKind::UnexpectedToken => "UnexpectedToken",
            ParseErrorKind::InvalidSyntax => "InvalidSyntax",
            ParseErrorKind::InvalidIndentation => "InvalidIndentation",
            ParseErrorKind::ControlMeetExtendedCall => "ControlMeetExtendedCall",
            ParseErrorKind::InvalidBlockPrefix => "InvalidBlockPrefix",
        }
    }

    fn emit(&self, diag_ctx: &DiagnosticContext, _base_pos: BytePos) {
        let mut builder = diag_ctx
            .error(self.message.clone())
            .with_code(self.error_code())
            .with_primary_span(self.span);

        if let Some(lbl) = &self.label {
            builder = builder.with_error_label(lbl.span, lbl.message.clone());
        }

        builder.emit(diag_ctx);
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for ParseError {}

/// The result type used throughout the parser.
/// - `Ok(NodeIndex)` where `NodeIndex::NULL` means "no match" (backtrack)
///   and non-null means a successfully parsed node.
/// - `Err(ParseErrorKind)` for hard errors that should stop current parse path.
pub type ParseResult = Result<NodeIndex, ParseErrorKind>;
