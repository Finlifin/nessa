//! Official source code formatter for the Nessa programming language (`nessa-fmt`).

pub mod bridge;
pub mod cli;
pub mod comments;
pub mod config;
pub mod diff;
pub mod directives;
pub mod doc;
pub mod exprs;
pub mod formatter;
pub mod items;
pub mod patterns;
pub mod verify;

use std::path::Path;

pub use cli::{
    ColorChoice, EXIT_CLI_OR_CONFIG_ERR, EXIT_DIFF_OR_FORMAT_ERR, EXIT_OK, EmitMode,
    PrintConfigMode, render_help, run_cli, run_cli_with_io,
};
pub use config::{
    CONFIG_FILE_NAME, ConfigError, FormatConfig, NewlineStyle, SmallHeuristics, WidthHeuristics,
};
pub use diff::{DiffLine, Mismatch, make_diff, render_check_diff};
pub use directives::{
    DIRECTIVE_SPECS, DirectiveAction, DirectiveLine, DirectiveSpec, DirectiveWarning,
    ScopedConfigStack,
};
pub use formatter::{
    FormatError, FormatOutput, FormatResult, Formatter, format_range_internal,
    format_source_internal,
};

/// Format a Nessa source buffer in memory using `config`.
pub fn format_source(src: &str, config: &FormatConfig) -> Result<FormatResult, FormatError> {
    format_source_internal(src, "<memory>", config, false)
}

/// Format a Nessa source buffer in memory with an optional file `path` for diagnostic spans.
pub fn format_source_with_path(
    src: &str,
    path: Option<&Path>,
    config: &FormatConfig,
) -> Result<FormatResult, FormatError> {
    let display = path
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<memory>".to_string());
    format_source_internal(src, &display, config, false)
}

/// Format only changes overlapping the 0-based inclusive `line_range` `(start_line, end_line)`
/// (designed for `nessa-lsp` `textDocument/rangeFormatting`), keeping import order untouched and
/// verifying the resulting source with `verify_formatted_output`.
pub fn format_range(
    src: &str,
    line_range: (u32, u32),
    config: &FormatConfig,
) -> Result<FormatResult, FormatError> {
    format_range_internal(src, "<range>", line_range, config)
}
