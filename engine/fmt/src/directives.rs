//! `--%` formatter directive parser, normalizer, warning collector, and scoped config stack.

use crate::config::{FormatConfig, NewlineStyle, SmallHeuristics, split_top_level_commas};

/// Metadata describing a supported `--%` directive for IDE / LSP completion and hover reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectiveSpec {
    /// Directive name with leading dot (e.g. `".skip"`, `".max_width"`).
    pub name: &'static str,
    /// Optional value syntax hint (e.g. `Some("[true|false]")`, `Some("<N>")`).
    pub value_hint: Option<&'static str>,
    /// Short human-readable summary of the directive's effect.
    pub description: &'static str,
}

/// Canonical table of all supported `--%` formatter directives.
pub const DIRECTIVE_SPECS: &[DirectiveSpec] = &[
    DirectiveSpec {
        name: ".skip",
        value_hint: Some("[true|false]"),
        description: "Skip formatting the next AST node while aligning its outer indentation.",
    },
    DirectiveSpec {
        name: ".preserve",
        value_hint: Some("[true|false]"),
        description: "Preserve user line breaks and intra-line alignment on the next AST node.",
    },
    DirectiveSpec {
        name: ".off",
        value_hint: Some("[true|false]"),
        description: "Disable formatting verbatim until `--% .on`, enclosing `}`, or EOF.",
    },
    DirectiveSpec {
        name: ".on",
        value_hint: Some("[true|false]"),
        description: "Re-enable formatting after an earlier `--% .off` directive.",
    },
    DirectiveSpec {
        name: ".max_width",
        value_hint: Some("<N>"),
        description: "Override maximum line width (`N >= 1`) for the enclosing `{ ... }` scope.",
    },
    DirectiveSpec {
        name: ".tab_spaces",
        value_hint: Some("<N>"),
        description: "Override indentation width (`N >= 1`) for the enclosing `{ ... }` scope.",
    },
    DirectiveSpec {
        name: ".newline_style",
        value_hint: Some("<.lf|.crlf|.auto>"),
        description: "Override newline style for the enclosing `{ ... }` scope.",
    },
    DirectiveSpec {
        name: ".use_small_heuristics",
        value_hint: Some("<.default|.off|.max>"),
        description: "Override sub-expression single-line width heuristics for the enclosing scope.",
    },
    DirectiveSpec {
        name: ".reorder_imports",
        value_hint: Some("[true|false]"),
        description: "Enable or disable grouping and sorting of top-of-scope `use` imports.",
    },
    DirectiveSpec {
        name: ".normalize_comments",
        value_hint: Some("[true|false]"),
        description: "Enable or disable normalizing `--text` comments to `-- text`.",
    },
];

/// A single parsed action inside a `--%` directive comment line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectiveAction {
    Skip(bool),
    Off(bool),
    On(bool),
    Preserve(bool),
    MaxWidth(usize),
    TabSpaces(usize),
    NewlineStyle(NewlineStyle),
    UseSmallHeuristics(SmallHeuristics),
    ReorderImports(bool),
    NormalizeComments(bool),
}

impl DirectiveAction {
    /// Apply configuration-override variants to `cfg`.
    pub fn apply_to_config(&self, cfg: &mut FormatConfig) {
        match *self {
            Self::MaxWidth(n) => cfg.max_width = n,
            Self::TabSpaces(n) => cfg.tab_spaces = n,
            Self::NewlineStyle(style) => cfg.newline_style = style,
            Self::UseSmallHeuristics(mode) => cfg.use_small_heuristics = mode,
            Self::ReorderImports(b) => cfg.reorder_imports = b,
            Self::NormalizeComments(b) => cfg.normalize_comments = b,
            Self::Skip(_) | Self::Off(_) | Self::On(_) | Self::Preserve(_) => {}
        }
    }

    /// Canonical Nessa directive item rendering inside `--% ...`.
    pub fn canonical_text(&self) -> String {
        match self {
            Self::Skip(true) => ".skip".to_string(),
            Self::Skip(false) => ".skip false".to_string(),
            Self::Off(true) => ".off".to_string(),
            Self::Off(false) => ".off false".to_string(),
            Self::On(true) => ".on".to_string(),
            Self::On(false) => ".on false".to_string(),
            Self::Preserve(true) => ".preserve".to_string(),
            Self::Preserve(false) => ".preserve false".to_string(),
            Self::MaxWidth(n) => format!(".max_width {n}"),
            Self::TabSpaces(n) => format!(".tab_spaces {n}"),
            Self::NewlineStyle(s) => format!(".newline_style {}", s.as_symbol_str()),
            Self::UseSmallHeuristics(m) => format!(".use_small_heuristics {}", m.as_symbol_str()),
            Self::ReorderImports(b) => format!(".reorder_imports {b}"),
            Self::NormalizeComments(b) => format!(".normalize_comments {b}"),
        }
    }
}

/// Warning emitted when a standalone `--%` line contains unknown or malformed directives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectiveWarning {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl std::fmt::Display for DirectiveWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.column, self.message)
    }
}

/// A standalone `--%` directive comment line located in the source buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectiveLine {
    /// Byte offset of the start of the line (before leading spaces/tabs).
    pub line_start: usize,
    /// Byte offset of `--%`.
    pub comment_start: usize,
    /// Byte offset of the end of the comment text (excluding `\r` / `\n`).
    pub comment_end: usize,
    /// Byte offset right after the line's trailing newline (or `src.len()` at EOF).
    pub line_end_with_newline: usize,
    /// 1-based line number.
    pub line_no: usize,
    /// 1-based column number (in Unicode scalar values) of `--%`.
    pub col_no: usize,
    /// Enclosing `{}` brace depth at the start of this directive line.
    pub brace_depth: usize,
    /// Original `--%...` text (without leading indentation or trailing newline).
    pub raw_text: String,
    /// Normalized `--% ...` text (or trimmed `raw_text` if invalid, preserving unknown directives).
    pub normalized_text: String,
    /// Parsed actions in left-to-right order (empty when invalid).
    pub actions: Vec<DirectiveAction>,
    /// True if all entries on this `--%` line were valid directives.
    pub is_valid: bool,
}

impl DirectiveLine {
    pub fn has_off(&self) -> bool {
        let mut state = None;
        for a in &self.actions {
            match *a {
                DirectiveAction::Off(true) => state = Some(true),
                DirectiveAction::Off(false) | DirectiveAction::On(true) => state = Some(false),
                _ => {}
            }
        }
        state == Some(true)
    }

    pub fn has_on(&self) -> bool {
        let mut state = None;
        for a in &self.actions {
            match *a {
                DirectiveAction::On(true) => state = Some(true),
                DirectiveAction::On(false) | DirectiveAction::Off(true) => state = Some(false),
                _ => {}
            }
        }
        state == Some(true)
    }

    pub fn skip_flag(&self) -> Option<bool> {
        let mut res = None;
        for a in &self.actions {
            if let DirectiveAction::Skip(b) = *a {
                res = Some(b);
            }
        }
        res
    }

    pub fn preserve_flag(&self) -> Option<bool> {
        let mut res = None;
        for a in &self.actions {
            if let DirectiveAction::Preserve(b) = *a {
                res = Some(b);
            }
        }
        res
    }
}

/// Parse the payload after `--%` on a standalone directive line.
pub fn parse_directive_comment(
    raw_comment: &str,
    line_no: usize,
    col_no: usize,
) -> (Vec<DirectiveAction>, String, Option<DirectiveWarning>) {
    let trimmed_right = raw_comment.trim_end_matches([' ', '\t']);
    let Some(body) = trimmed_right.strip_prefix("--%") else {
        return (Vec::new(), trimmed_right.to_string(), None);
    };
    let body_trimmed = body.trim();
    if body_trimmed.is_empty() {
        return (
            Vec::new(),
            trimmed_right.to_string(),
            Some(DirectiveWarning {
                line: line_no,
                column: col_no,
                message: "empty `--%` formatter directive".to_string(),
            }),
        );
    }

    let entries = match split_top_level_commas(body_trimmed) {
        Ok(list) => list,
        Err(err) => {
            return (
                Vec::new(),
                trimmed_right.to_string(),
                Some(DirectiveWarning {
                    line: line_no,
                    column: col_no,
                    message: format!("malformed `--%` directive: {err}"),
                }),
            );
        }
    };

    let mut actions = Vec::new();
    for raw_entry in entries {
        let entry = raw_entry.trim();
        if entry.is_empty() {
            continue;
        }
        match parse_single_directive_entry(entry) {
            Ok(action) => actions.push(action),
            Err(msg) => {
                return (
                    Vec::new(),
                    trimmed_right.to_string(),
                    Some(DirectiveWarning {
                        line: line_no,
                        column: col_no,
                        message: msg,
                    }),
                );
            }
        }
    }

    if actions.is_empty() {
        return (
            Vec::new(),
            trimmed_right.to_string(),
            Some(DirectiveWarning {
                line: line_no,
                column: col_no,
                message: "empty `--%` formatter directive".to_string(),
            }),
        );
    }

    let canonical_parts: Vec<String> = actions
        .iter()
        .map(DirectiveAction::canonical_text)
        .collect();
    let normalized = format!("--% {}", canonical_parts.join(", "));
    (actions, normalized, None)
}

fn parse_single_directive_entry(entry: &str) -> Result<DirectiveAction, String> {
    let Some(rest) = entry.strip_prefix('.') else {
        return Err(format!(
            "invalid `--%` directive `{entry}`: directives must start with `.` (e.g. `--% .skip`)"
        ));
    };
    let rest = rest.trim();
    let split_idx = rest
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(rest.len());
    let key = &rest[..split_idx];
    let mut val = rest[split_idx..].trim();
    if let Some(after_eq) = val.strip_prefix('=') {
        val = after_eq.trim();
    }

    match key {
        "skip" => Ok(DirectiveAction::Skip(parse_optional_bool(val, key)?)),
        "off" => Ok(DirectiveAction::Off(parse_optional_bool(val, key)?)),
        "on" => Ok(DirectiveAction::On(parse_optional_bool(val, key)?)),
        "preserve" => Ok(DirectiveAction::Preserve(parse_optional_bool(val, key)?)),
        "reorder_imports" => Ok(DirectiveAction::ReorderImports(parse_optional_bool(
            val, key,
        )?)),
        "normalize_comments" => Ok(DirectiveAction::NormalizeComments(parse_optional_bool(
            val, key,
        )?)),
        "max_width" => {
            if val.is_empty() {
                return Err("directive `.max_width` requires a positive integer".to_string());
            }
            let cleaned: String = val.chars().filter(|&c| c != '_').collect();
            let n: usize = cleaned.parse().ok().filter(|&n| n >= 1).ok_or_else(|| {
                format!("directive `.max_width` expects a positive integer, found `{val}`")
            })?;
            Ok(DirectiveAction::MaxWidth(n))
        }
        "tab_spaces" => {
            if val.is_empty() {
                return Err("directive `.tab_spaces` requires a positive integer".to_string());
            }
            let cleaned: String = val.chars().filter(|&c| c != '_').collect();
            let n: usize = cleaned.parse().ok().filter(|&n| n >= 1).ok_or_else(|| {
                format!("directive `.tab_spaces` expects a positive integer, found `{val}`")
            })?;
            Ok(DirectiveAction::TabSpaces(n))
        }
        "newline_style" => {
            let style = NewlineStyle::parse_value(val).ok_or_else(|| {
                format!(
                    "directive `.newline_style` expects `.lf`, `.crlf`, or `.auto`, found `{val}`"
                )
            })?;
            Ok(DirectiveAction::NewlineStyle(style))
        }
        "use_small_heuristics" => {
            let mode = SmallHeuristics::parse_value(val).ok_or_else(|| {
                format!(
                    "directive `.use_small_heuristics` expects `.default`, `.off`, or `.max`, found `{val}`"
                )
            })?;
            Ok(DirectiveAction::UseSmallHeuristics(mode))
        }
        "" => Err("empty `--%` directive key".to_string()),
        other => Err(format!("unknown `--%` formatter directive `.{other}`")),
    }
}

fn parse_optional_bool(val: &str, key: &str) -> Result<bool, String> {
    match val {
        "" | "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!(
            "directive `.{key}` expects `true`, `false`, or no argument, found `{other}`"
        )),
    }
}

/// Block-scoped configuration stack driven by `{ ... }` depth and `--%` overrides.
#[derive(Debug, Clone)]
pub struct ScopedConfigStack {
    frames: Vec<(usize, FormatConfig)>,
}

impl ScopedConfigStack {
    pub fn new(root: FormatConfig) -> Self {
        Self {
            frames: vec![(0, root)],
        }
    }

    pub fn current(&self) -> &FormatConfig {
        &self.frames.last().expect("root config frame").1
    }

    pub fn apply_at_depth(&mut self, depth: usize, actions: &[DirectiveAction]) {
        self.pop_above(depth);
        let mut next = self.current().clone();
        let mut changed = false;
        for action in actions {
            match action {
                DirectiveAction::MaxWidth(_)
                | DirectiveAction::TabSpaces(_)
                | DirectiveAction::NewlineStyle(_)
                | DirectiveAction::UseSmallHeuristics(_)
                | DirectiveAction::ReorderImports(_)
                | DirectiveAction::NormalizeComments(_) => {
                    action.apply_to_config(&mut next);
                    changed = true;
                }
                _ => {}
            }
        }
        if !changed {
            return;
        }
        if let Some((d, slot)) = self.frames.last_mut()
            && *d == depth
        {
            *slot = next;
        } else {
            self.frames.push((depth, next));
        }
    }

    pub fn pop_above(&mut self, depth: usize) {
        while self.frames.len() > 1 && self.frames.last().is_some_and(|(d, _)| *d > depth) {
            self.frames.pop();
        }
    }
}
