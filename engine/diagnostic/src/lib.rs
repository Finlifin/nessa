pub mod emitter;

use ariadne::{Color, ColorGenerator, Label, Report, ReportKind, Source};
use rustc_span::{FileNameDisplayPreference, SourceMap, Span};
use std::{
    cell::{Cell, RefCell},
    fmt, mem,
};

// 罢了, warning也用这个trait吧
pub trait NessaError {
    fn error_code(&self) -> u32;
    fn error_name(&self) -> &'static str;
    fn emit(&self, diag_ctx: &DiagnosticContext, base_pos: rustc_span::BytePos);
}

/// Diagnostic severity level
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
    Note,
    Help,
}

impl Level {
    pub fn to_ariadne_kind(&self) -> ReportKind<'_> {
        match self {
            Level::Error => ReportKind::Error,
            Level::Warning => ReportKind::Warning,
            Level::Note | Level::Help => ReportKind::Advice,
        }
    }

    pub fn color(&self) -> Color {
        match self {
            Level::Error => Color::Red,
            Level::Warning => Color::Yellow,
            Level::Note => Color::Blue,
            Level::Help => Color::Cyan,
        }
    }
}

/// A diagnostic message with location information
#[derive(Debug, Clone)]
pub struct DiagnosticMessage {
    pub span: Span,
    pub message: String,
    pub level: Level,
}

impl DiagnosticMessage {
    pub fn new(span: Span, message: String, level: Level) -> Self {
        Self {
            span,
            message,
            level,
        }
    }
}

/// A complete diagnostic with primary message and optional sub-diagnostics
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub level: Level,
    pub code: Option<u32>,
    pub message: String,
    pub primary_span: Option<Span>,
    pub labels: Vec<DiagnosticMessage>,
    pub notes: Vec<String>,
    pub helps: Vec<String>,
}

impl Diagnostic {
    pub fn new(level: Level, message: String) -> Self {
        Self {
            level,
            code: None,
            message,
            primary_span: None,
            labels: Vec::new(),
            notes: Vec::new(),
            helps: Vec::new(),
        }
    }

    pub fn error(message: String) -> Self {
        Self::new(Level::Error, message)
    }

    pub fn warning(message: String) -> Self {
        Self::new(Level::Warning, message)
    }

    pub fn note(message: String) -> Self {
        Self::new(Level::Note, message)
    }

    pub fn help(message: String) -> Self {
        Self::new(Level::Help, message)
    }
}

/// Builder for constructing diagnostics
pub struct DiagnosticBuilder {
    diagnostic: Diagnostic,
}

impl DiagnosticBuilder {
    pub fn new(level: Level, message: String) -> Self {
        Self {
            diagnostic: Diagnostic::new(level, message),
        }
    }

    pub fn error(message: String) -> Self {
        Self::new(Level::Error, message)
    }

    pub fn warning(message: String) -> Self {
        Self::new(Level::Warning, message)
    }

    pub fn note(message: String) -> Self {
        Self::new(Level::Note, message)
    }

    pub fn help(message: String) -> Self {
        Self::new(Level::Help, message)
    }

    pub fn with_code(mut self, code: u32) -> Self {
        self.diagnostic.code = Some(code);
        self
    }

    pub fn with_primary_span(mut self, span: Span) -> Self {
        self.diagnostic.primary_span = Some(span);
        self
    }

    pub fn with_label(mut self, span: Span, message: String, level: Level) -> Self {
        self.diagnostic
            .labels
            .push(DiagnosticMessage::new(span, message, level));
        self
    }

    pub fn with_error_label(self, span: Span, message: String) -> Self {
        self.with_label(span, message, Level::Error)
    }

    pub fn with_warning_label(self, span: Span, message: String) -> Self {
        self.with_label(span, message, Level::Warning)
    }

    pub fn with_note_label(self, span: Span, message: String) -> Self {
        self.with_label(span, message, Level::Note)
    }

    pub fn with_help_label(self, span: Span, message: String) -> Self {
        self.with_label(span, message, Level::Help)
    }

    pub fn with_note(mut self, note: String) -> Self {
        self.diagnostic.notes.push(note);
        self
    }

    pub fn with_help(mut self, help: String) -> Self {
        self.diagnostic.helps.push(help);
        self
    }

    pub fn build(self) -> Diagnostic {
        self.diagnostic
    }

    pub fn emit(self, context: &DiagnosticContext) {
        context.emit(self.diagnostic);
    }
}

/// Context for managing and emitting diagnostics
pub struct DiagnosticContext<'a> {
    source_map: &'a SourceMap,
    emitted_diagnostics: RefCell<Vec<Diagnostic>>,
    error_count: Cell<usize>,
    warning_count: Cell<usize>,
}

impl<'a> DiagnosticContext<'a> {
    pub fn new(source_map: &'a SourceMap) -> Self {
        Self {
            source_map,
            emitted_diagnostics: RefCell::new(Vec::new()),
            error_count: Cell::new(0),
            warning_count: Cell::new(0),
        }
    }

    pub fn source_map(&self) -> &SourceMap {
        self.source_map
    }

    pub fn emit(&self, diagnostic: Diagnostic) {
        match diagnostic.level {
            Level::Error => self.error_count.set(self.error_count.get() + 1),
            Level::Warning => self.warning_count.set(self.warning_count.get() + 1),
            _ => {}
        }

        // Emit to ariadne
        self.emit_to_ariadne(&diagnostic);

        // Store for later analysis
        self.emitted_diagnostics.borrow_mut().push(diagnostic);
    }

    pub fn error_count(&self) -> usize {
        self.error_count.get()
    }

    pub fn warning_count(&self) -> usize {
        self.warning_count.get()
    }

    pub fn has_errors(&self) -> bool {
        self.error_count.get() > 0
    }

    pub fn has_warnings(&self) -> bool {
        self.warning_count.get() > 0
    }

    pub unsafe fn diagnostics<'b>(&'b self) -> &'b [Diagnostic] {
        unsafe {
            mem::transmute::<&[Diagnostic], &'b [Diagnostic]>(
                self.emitted_diagnostics.borrow().as_ref(),
            )
        }
    }

    /// Create a new diagnostic builder
    pub fn error(&self, message: String) -> DiagnosticBuilder {
        DiagnosticBuilder::error(message)
    }

    pub fn warning(&self, message: String) -> DiagnosticBuilder {
        DiagnosticBuilder::warning(message)
    }

    pub fn note(&self, message: String) -> DiagnosticBuilder {
        DiagnosticBuilder::note(message)
    }

    pub fn help(&self, message: String) -> DiagnosticBuilder {
        DiagnosticBuilder::help(message)
    }

    /// Emit diagnostic using ariadne
    fn emit_to_ariadne(&self, diagnostic: &Diagnostic) {
        let primary_span = diagnostic.primary_span.unwrap_or_else(|| {
            // Use the first label span if no primary span is provided
            diagnostic
                .labels
                .first()
                .map(|label| label.span)
                .unwrap_or(rustc_span::DUMMY_SP)
        });

        let source_file = self.source_map.lookup_source_file(primary_span.lo());
        let mut colors = ColorGenerator::new();
        let file_id_str = format!(
            "{}",
            source_file
                .name
                .display(FileNameDisplayPreference::Local)
                .to_string_lossy()
        );

        // Convert byte positions to character positions for ariadne
        let source_content = match &source_file.src {
            Some(content) => content.as_str(),
            None => {
                eprintln!("Error: Source file content not available");
                return;
            }
        };

        let byte_start = (primary_span.lo().0 - source_file.start_pos.0) as usize;
        let byte_end = (primary_span.hi().0 - source_file.start_pos.0) as usize;

        // Convert byte indices to character indices by counting UTF-8 chars
        let char_start = source_content
            .get(..byte_start.min(source_content.len()))
            .map(|s| s.chars().count())
            .unwrap_or(0);
        let char_end = source_content
            .get(..byte_end.min(source_content.len()))
            .map(|s| s.chars().count())
            .unwrap_or(char_start);

        let mut report = Report::build(
            diagnostic.level.to_ariadne_kind(),
            (&file_id_str, char_start..char_end),
        );

        if let Some(code) = diagnostic.code {
            report = report.with_code(code);
        }

        report = report.with_message(&diagnostic.message);

        // Add labels - only from the same file for simplicity
        for label in &diagnostic.labels {
            // 检查 span 是否来自同一个文件
            let label_file = self.source_map.lookup_source_file(label.span.lo());
            if std::ptr::eq(label_file.as_ref(), source_file.as_ref()) {
                let color = colors.next();

                let label_byte_start = (label.span.lo().0 - source_file.start_pos.0) as usize;
                let label_byte_end = (label.span.hi().0 - source_file.start_pos.0) as usize;

                // Convert byte indices to character indices for label
                let label_char_start = source_content
                    .get(..label_byte_start.min(source_content.len()))
                    .map(|s| s.chars().count())
                    .unwrap_or(0);
                let label_char_end = source_content
                    .get(..label_byte_end.min(source_content.len()))
                    .map(|s| s.chars().count())
                    .unwrap_or(label_char_start);

                report = report.with_label(
                    Label::new((&file_id_str, label_char_start..label_char_end))
                        .with_message(&label.message)
                        .with_color(color),
                );
            }
        }

        // Add notes
        for note in &diagnostic.notes {
            report = report.with_note(note);
        }

        // Add helps
        for help in &diagnostic.helps {
            report = report.with_help(help);
        }

        // Print the report - use file_id as identifier
        let source_content = match &source_file.src {
            Some(content) => content.as_str(),
            None => {
                eprintln!("Error: Source file content not available");
                return;
            }
        };

        if let Err(e) = report
            .finish()
            .print((&file_id_str, Source::from(source_content)))
        {
            eprintln!("Error printing diagnostic: {}", e);
        }
    }
}

impl<'a> fmt::Debug for DiagnosticContext<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiagnosticContext")
            .field("error_count", &self.error_count)
            .field("warning_count", &self.warning_count)
            .field(
                "diagnostics_count",
                &self.emitted_diagnostics.borrow().len(),
            )
            .finish()
    }
}

/// Convenience macros for creating diagnostics
#[macro_export]
macro_rules! diag_error {
    ($ctx:expr, $msg:expr) => {
        $ctx.error($msg.to_string())
    };
    ($ctx:expr, $msg:expr, $($arg:tt)*) => {
        $ctx.error(format!($msg, $($arg)*))
    };
}

#[macro_export]
macro_rules! diag_warning {
    ($ctx:expr, $msg:expr) => {
        $ctx.warning($msg.to_string())
    };
    ($ctx:expr, $msg:expr, $($arg:tt)*) => {
        $ctx.warning(format!($msg, $($arg)*))
    };
}

#[macro_export]
macro_rules! diag_note {
    ($ctx:expr, $msg:expr) => {
        $ctx.note($msg.to_string())
    };
    ($ctx:expr, $msg:expr, $($arg:tt)*) => {
        $ctx.note(format!($msg, $($arg)*))
    };
}

#[macro_export]
macro_rules! diag_help {
    ($ctx:expr, $msg:expr) => {
        $ctx.help($msg.to_string())
    };
    ($ctx:expr, $msg:expr, $($arg:tt)*) => {
        $ctx.help(format!($msg, $($arg)*))
    };
}
