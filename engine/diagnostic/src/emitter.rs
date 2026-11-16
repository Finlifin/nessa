//! Ariadne wrapper for pretty-printing diagnostics across one or more source files.

use super::{Diagnostic, DiagnosticContext, Level, byte_span_to_char_range};
use ariadne::{Color, Config, Label, Report, sources};
use rustc_span::FileNameDisplayPreference;

/// Configuration for diagnostic emission
#[derive(Debug, Clone)]
pub struct EmitterConfig {
    /// Whether to use colors in output
    pub use_colors: bool,
    /// Maximum number of lines to show in context
    pub context_lines: usize,
    /// Whether to show line numbers
    pub show_line_numbers: bool,
}

impl Default for EmitterConfig {
    fn default() -> Self {
        Self {
            use_colors: true,
            context_lines: 3,
            show_line_numbers: true,
        }
    }
}

/// Ariadne-based diagnostic emitter supporting multi-file spans and in-memory rendering.
pub struct AriadneEmitter {
    config: EmitterConfig,
}

impl AriadneEmitter {
    pub fn new(config: EmitterConfig) -> Self {
        Self { config }
    }

    pub fn new_default() -> Self {
        Self::new(EmitterConfig::default())
    }

    /// Format a single diagnostic into an owned `String` (honoring `config.use_colors`
    /// and rendering cross-file labels when present).
    pub fn render_to_string(&self, diagnostic: &Diagnostic, context: &DiagnosticContext) -> String {
        let mut buf = Vec::new();
        self.write_diagnostic(diagnostic, context, &mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    }

    /// Emit a single diagnostic to stderr.
    pub fn emit_diagnostic(&self, diagnostic: &Diagnostic, context: &DiagnosticContext) {
        let rendered = self.render_to_string(diagnostic, context);
        if !rendered.is_empty() {
            eprint!("{rendered}");
        }
    }

    fn write_diagnostic(
        &self,
        diagnostic: &Diagnostic,
        context: &DiagnosticContext,
        out: &mut impl std::io::Write,
    ) {
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
            context
                .source_for_location(span, anchor)
                .map(|file| (span, file))
        }) else {
            let _ = write_plain_diagnostic(diagnostic, out);
            return;
        };

        let file_name = source_file
            .name
            .display(FileNameDisplayPreference::Local)
            .to_string_lossy()
            .into_owned();

        let Some(source_content) = source_file.src.as_deref() else {
            let _ = writeln!(out, "Error: Source file content not available");
            return;
        };

        let (char_start, char_end) = byte_span_to_char_range(&source_file, primary_span);

        let mut report = Report::build(
            diagnostic.level.to_ariadne_kind(),
            (file_name.clone(), char_start..char_end),
        )
        .with_config(Config::default().with_color(self.config.use_colors));

        if let Some(code) = diagnostic.code {
            report = report.with_code(code);
        }

        report = report.with_message(&diagnostic.message);

        let mut source_entries: Vec<(String, String)> =
            vec![(file_name.clone(), source_content.to_owned())];
        let mut has_primary_file_label = false;

        for label in &diagnostic.labels {
            if let Some(label_file) = context.source_for_location(label.span, label.source_start)
                && let Some(label_src) = label_file.src.as_deref()
            {
                let label_file_id = label_file
                    .name
                    .display(FileNameDisplayPreference::Local)
                    .to_string_lossy()
                    .into_owned();
                if std::ptr::eq(label_file.as_ref(), source_file.as_ref()) {
                    has_primary_file_label = true;
                } else if !source_entries.iter().any(|(id, _)| id == &label_file_id) {
                    source_entries.push((label_file_id.clone(), label_src.to_owned()));
                }

                let color = match label.level {
                    Level::Error => Color::Red,
                    Level::Warning => Color::Yellow,
                    Level::Note => Color::Blue,
                    Level::Help => Color::Cyan,
                };
                let (l_start, l_end) = byte_span_to_char_range(&label_file, label.span);

                report = report.with_label(
                    Label::new((label_file_id, l_start..l_end))
                        .with_message(&label.message)
                        .with_color(color),
                );
            }
        }

        if !has_primary_file_label {
            report = report.with_label(
                Label::new((file_name, char_start..char_end)).with_color(diagnostic.level.color()),
            );
        }

        for note in &diagnostic.notes {
            report = report.with_note(note);
        }

        for help in &diagnostic.helps {
            report = report.with_help(help);
        }

        let cache = sources(source_entries);
        let _ = report.finish().write(cache, out);
    }

    /// Emit all diagnostics from a context
    pub fn emit_all(&self, context: &DiagnosticContext) {
        for diagnostic in context.diagnostics().iter() {
            self.emit_diagnostic(diagnostic, context);
        }
    }
}

fn write_plain_diagnostic(
    diagnostic: &Diagnostic,
    out: &mut impl std::io::Write,
) -> std::io::Result<()> {
    if let Some(code) = diagnostic.code {
        writeln!(
            out,
            "[{code}] {:?}: {}",
            diagnostic.level, diagnostic.message
        )?;
    } else {
        writeln!(out, "{:?}: {}", diagnostic.level, diagnostic.message)?;
    }
    for label in &diagnostic.labels {
        writeln!(out, "{:?}: {}", label.level, label.message)?;
    }
    for note in &diagnostic.notes {
        writeln!(out, "Note: {note}")?;
    }
    for help in &diagnostic.helps {
        writeln!(out, "Help: {help}")?;
    }
    Ok(())
}

impl Level {
    pub fn name(&self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warning => "warning",
            Level::Note => "note",
            Level::Help => "help",
        }
    }
}
