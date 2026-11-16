use diagnostic::{DiagnosticContext, LocatedSourceSpan, NessaError};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseLabelStyle {
    Error,
    Note,
}

#[derive(Debug, Clone)]
pub struct ParseErrorLabel {
    pub span: Span,
    pub message: String,
    pub style: ParseLabelStyle,
}

/// A single parse error with span, secondary labels, notes, and actionable help.
#[derive(Debug, Clone)]
pub struct ParseError {
    pub kind: ParseErrorKind,
    pub message: String,
    pub span: Span,
    pub label: Option<ParseErrorLabel>,
    pub extra_labels: Vec<ParseErrorLabel>,
    pub notes: Vec<String>,
    pub helps: Vec<String>,
    pub source_start: Option<BytePos>,
}

impl ParseError {
    pub fn new(kind: ParseErrorKind, span: Span, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            span,
            label: None,
            extra_labels: Vec::new(),
            notes: Vec::new(),
            helps: Vec::new(),
            source_start: None,
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
                style: ParseLabelStyle::Error,
            }),
            extra_labels: Vec::new(),
            notes: Vec::new(),
            helps: Vec::new(),
            source_start: None,
        }
    }

    pub fn add_error_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.extra_labels.push(ParseErrorLabel {
            span,
            message: message.into(),
            style: ParseLabelStyle::Error,
        });
        self
    }

    pub fn add_note_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.extra_labels.push(ParseErrorLabel {
            span,
            message: message.into(),
            style: ParseLabelStyle::Note,
        });
        self
    }

    pub fn add_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn add_help(mut self, help: impl Into<String>) -> Self {
        self.helps.push(help.into());
        self
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
            .with_primary_source_span(LocatedSourceSpan {
                span: self.span,
                file_start: self.source_start,
            });

        if let Some(lbl) = &self.label {
            let loc = LocatedSourceSpan {
                span: lbl.span,
                file_start: self.source_start,
            };
            builder = match lbl.style {
                ParseLabelStyle::Error => builder.with_error_source_span(loc, lbl.message.clone()),
                ParseLabelStyle::Note => {
                    builder.with_source_label(loc, lbl.message.clone(), diagnostic::Level::Note)
                }
            };
        }

        for lbl in &self.extra_labels {
            let loc = LocatedSourceSpan {
                span: lbl.span,
                file_start: self.source_start,
            };
            builder = match lbl.style {
                ParseLabelStyle::Error => builder.with_error_source_span(loc, lbl.message.clone()),
                ParseLabelStyle::Note => {
                    builder.with_source_label(loc, lbl.message.clone(), diagnostic::Level::Note)
                }
            };
        }

        for note in &self.notes {
            builder = builder.with_note(note.clone());
        }

        for help in &self.helps {
            builder = builder.with_help(help.clone());
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
