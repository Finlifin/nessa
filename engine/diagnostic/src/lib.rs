pub mod emitter;
pub use source_span::{
    LocatedSourceSpan, RawByteRange, SourceMapping, SourceSpanError, SourceSpanMapper,
};

use ariadne::{Color, ColorGenerator, Label, Report, ReportKind, Source};
use rustc_span::{BytePos, FileNameDisplayPreference, SourceMap, Span};
use std::{
    cell::{Cell, Ref, RefCell},
    fmt,
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
    pub source_start: Option<BytePos>,
    pub message: String,
    pub level: Level,
}

impl DiagnosticMessage {
    pub fn new(span: Span, message: String, level: Level) -> Self {
        Self {
            span,
            source_start: None,
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
    pub primary_source_start: Option<BytePos>,
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
            primary_source_start: None,
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
        self.diagnostic.primary_source_start = None;
        self
    }

    /// Attach an authenticated owning file, including genuine zero-width spans.
    pub fn with_primary_source_span(mut self, location: LocatedSourceSpan) -> Self {
        self.diagnostic.primary_span = Some(location.span);
        self.diagnostic.primary_source_start = location.file_start;
        self
    }

    pub fn with_error_source_span(mut self, location: LocatedSourceSpan, message: String) -> Self {
        let mut label = DiagnosticMessage::new(location.span, message, Level::Error);
        label.source_start = location.file_start;
        self.diagnostic.labels.push(label);
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

    /// Borrow stored diagnostics while retaining the RefCell's borrow guard.
    /// Clone the slice before emitting more diagnostics into this context.
    pub fn diagnostics(&self) -> Ref<'_, [Diagnostic]> {
        Ref::map(self.emitted_diagnostics.borrow(), |diagnostics| {
            diagnostics.as_slice()
        })
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

    /// Match raw bytes against the registered file, or retain legacy local coordinates.
    pub fn source_span_mapper(
        &self,
        base: BytePos,
        raw: &str,
    ) -> Result<SourceSpanMapper, SourceSpanError> {
        if let Some(file) = self
            .source_map
            .files()
            .into_iter()
            .find(|file| file.start_pos == base)
        {
            SourceSpanMapper::for_source(&file, raw)
        } else {
            SourceSpanMapper::raw_local(base, raw)
        }
    }

    pub fn mapped_raw_span(
        &self,
        base: BytePos,
        range: RawByteRange,
    ) -> Result<LocatedSourceSpan, SourceSpanError> {
        if let Some(file) = self
            .source_map
            .files()
            .into_iter()
            .find(|file| file.start_pos == base)
        {
            SourceSpanMapper::from_file(&file)?.map_range(range)
        } else {
            SourceMapping::raw_local(base, range.end as usize)?.map_range(range)
        }
    }

    #[cfg(test)]
    fn source_for_span(
        &self,
        span: Span,
    ) -> Option<impl std::ops::Deref<Target = rustc_span::SourceFile> + AsRef<rustc_span::SourceFile>>
    {
        self.source_for_location(span, None)
    }

    pub(crate) fn source_for_location(
        &self,
        span: Span,
        source_start: Option<BytePos>,
    ) -> Option<impl std::ops::Deref<Target = rustc_span::SourceFile> + AsRef<rustc_span::SourceFile>>
    {
        if span.is_dummy() && source_start.is_none() {
            return None;
        }
        let file = self
            .source_map
            .files()
            .into_iter()
            .filter(|file| {
                source_start.map_or(file.start_pos <= span.lo(), |base| file.start_pos == base)
            })
            .max_by_key(|file| file.start_pos)?;
        if span.lo() < file.start_pos || span.hi() < span.lo() || span.hi() > file.end_pos {
            return None;
        }
        let start = span.lo().0.checked_sub(file.start_pos.0)? as usize;
        let end = span.hi().0.checked_sub(file.start_pos.0)? as usize;
        file.src.as_ref()?.get(start..end)?;
        Some(file)
    }

    /// Emit diagnostic using ariadne, or plain text without a real source span.
    fn emit_to_ariadne(&self, diagnostic: &Diagnostic) {
        let span = diagnostic
            .primary_span
            .map(|span| (span, diagnostic.primary_source_start))
            .or_else(|| {
                diagnostic
                    .labels
                    .first()
                    .map(|label| (label.span, label.source_start))
            });
        let Some((primary_span, source_file)) = span.and_then(|(span, anchor)| {
            self.source_for_location(span, anchor)
                .map(|file| (span, file))
        }) else {
            emit_plain_diagnostic(diagnostic);
            return;
        };
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
        let mut has_source_label = false;
        for label in &diagnostic.labels {
            // 检查 span 是否来自同一个文件
            if let Some(label_file) = self.source_for_location(label.span, label.source_start)
                && std::ptr::eq(label_file.as_ref(), source_file.as_ref())
            {
                has_source_label = true;
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

        // Ariadne only renders source locations with a label. Preserve the exact
        // primary range when no valid same-file label provides that location.
        if !has_source_label {
            report = report.with_label(
                Label::new((&file_id_str, char_start..char_end))
                    .with_color(diagnostic.level.color()),
            );
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
            .eprint((&file_id_str, Source::from(source_content)))
        {
            eprintln!("Error printing diagnostic: {}", e);
        }
    }
}

pub(crate) fn emit_plain_diagnostic(diagnostic: &Diagnostic) {
    if let Some(code) = diagnostic.code {
        eprintln!("[{code}] {:?}: {}", diagnostic.level, diagnostic.message);
    } else {
        eprintln!("{:?}: {}", diagnostic.level, diagnostic.message);
    }
    for label in &diagnostic.labels {
        eprintln!("{:?}: {}", label.level, label.message);
    }
    for note in &diagnostic.notes {
        eprintln!("Note: {note}");
    }
    for help in &diagnostic.helps {
        eprintln!("Help: {help}");
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

#[cfg(test)]
mod tests {
    use rustc_span::{BytePos, FileName, source_map::FilePathMapping};

    use super::*;

    #[test]
    fn unmapped_diagnostics_are_retained_and_both_emitters_do_not_panic() {
        let map = SourceMap::new(FilePathMapping::empty());
        let context = DiagnosticContext::new(&map);
        let diagnostic = Diagnostic::error("source contents are unavailable".into());
        context.emit(diagnostic.clone());
        assert_eq!(context.error_count(), 1);
        assert_eq!(context.diagnostics().len(), 1);
        assert_eq!(context.diagnostics()[0].message, diagnostic.message);
        emitter::AriadneEmitter::new_default().emit_diagnostic(&diagnostic, &context);
        let invalid = Span::new(BytePos(5), BytePos(7));
        context
            .error("unmapped span".into())
            .with_primary_span(invalid)
            .emit(&context);
        emitter::AriadneEmitter::new_default().emit_diagnostic(&context.diagnostics()[1], &context);
        assert_eq!(context.error_count(), 2);
        assert_eq!(context.diagnostics()[1].primary_span, Some(invalid));
    }

    #[test]
    fn checked_diagnostic_source_requires_actual_source_boundaries() {
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("actual.ns".into()), "let value=42".into());
        let context = DiagnosticContext::new(&map);
        assert!(
            context
                .source_for_span(Span::new(file.start_pos, file.end_pos))
                .is_some()
        );
        assert!(
            context
                .source_for_span(Span::new(file.end_pos, BytePos(file.end_pos.0 + 1)))
                .is_none()
        );
        assert!(context.source_for_span(rustc_span::DUMMY_SP).is_none());
        context.warning("no source location".into()).emit(&context);
        assert_eq!(context.warning_count(), 1);
        assert_eq!(context.diagnostics()[0].primary_span, None);
    }
}
