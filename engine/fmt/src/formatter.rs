//! Core `Formatter` state, token-slot navigation, container layout, import sorting, and range formatting.

use std::cmp::Ordering;

use ast::{Ast, NodeIndex, NodeKind};
use lexer::TokenKind;

use crate::bridge::{BridgedConstruct, ParseFailure, ParsedSource, parse_and_bridge};
use crate::comments::{CommentCursor, measure_leading_ws, normalize_single_line_comment_str};
use crate::config::{ConfigError, FormatConfig};
use crate::diff::make_diff;
use crate::directives::{DirectiveWarning, ScopedConfigStack};
use crate::doc::{Doc, print_doc};
use crate::verify::verify_formatted_output;

/// Output of a successful formatting run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatResult {
    pub formatted: String,
    pub changed: bool,
    pub warnings: Vec<DirectiveWarning>,
}

/// Alias for `FormatResult` for callers expecting `FormatOutput`.
pub type FormatOutput = FormatResult;

/// Failure during source formatting.
#[derive(Debug, Clone)]
pub enum FormatError {
    Parse(ParseFailure),
    Verification(String),
    Config(ConfigError),
    Io(String),
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(p) => f.write_str(&p.rendered_diagnostics),
            Self::Verification(msg) => write!(f, "formatter verification error: {msg}"),
            Self::Config(err) => write!(f, "configuration error: {err}"),
            Self::Io(msg) => write!(f, "I/O error: {msg}"),
        }
    }
}

impl std::error::Error for FormatError {}

impl From<ConfigError> for FormatError {
    fn from(err: ConfigError) -> Self {
        Self::Config(err)
    }
}

/// Internal entry point with explicit `display_name` and `emit_diagnostics` control.
pub fn format_source_internal(
    src: &str,
    display_name: &str,
    config: &FormatConfig,
    emit_diagnostics: bool,
) -> Result<FormatResult, FormatError> {
    let parsed =
        parse_and_bridge(src, display_name, emit_diagnostics).map_err(FormatError::Parse)?;
    let warnings = parsed.warnings.clone();
    let newline = config.newline_style.resolve(src);

    let root_items = if parsed.ast.root.is_null() {
        &[][..]
    } else {
        parsed.ast.multi_children(parsed.ast.root)
    };
    if root_items.is_empty() && parsed.comments.is_empty() {
        return Ok(FormatResult {
            formatted: String::new(),
            changed: !src.is_empty(),
            warnings,
        });
    }

    let mut fmt = Formatter::new(&parsed, config.clone());
    let doc = fmt.format_file_scope();
    let rendered = print_doc(&doc, config.max_width, newline);

    verify_formatted_output(&parsed, &rendered, display_name).map_err(FormatError::Verification)?;

    let changed = rendered != src;
    Ok(FormatResult {
        formatted: rendered,
        changed,
        warnings,
    })
}

/// Format only changes overlapping `line_range` (`(start_line, end_line)`, 0-based inclusive, as used by
/// LSP `textDocument/rangeFormatting`, while clamping gracefully to file bounds).
///
/// Formats the buffer with `reorder_imports: false`, splices only diff hunks overlapping `line_range`,
/// and verifies the spliced output with `verify_formatted_output` (falling back to full-buffer result
/// only if a partial hunk splice would break cross-line syntax).
pub fn format_range_internal(
    src: &str,
    display_name: &str,
    line_range: (u32, u32),
    config: &FormatConfig,
) -> Result<FormatResult, FormatError> {
    let parsed = parse_and_bridge(src, display_name, false).map_err(FormatError::Parse)?;
    let warnings = parsed.warnings.clone();
    let newline = config.newline_style.resolve(src);

    let root_items = if parsed.ast.root.is_null() {
        &[][..]
    } else {
        parsed.ast.multi_children(parsed.ast.root)
    };
    if root_items.is_empty() && parsed.comments.is_empty() {
        return Ok(FormatResult {
            formatted: String::new(),
            changed: !src.is_empty(),
            warnings,
        });
    }

    let mut range_cfg = config.clone();
    range_cfg.reorder_imports = false;

    let mut fmt = Formatter::new(&parsed, range_cfg);
    let doc = fmt.format_file_scope();
    let full_formatted = print_doc(&doc, config.max_width, newline);

    if full_formatted == src {
        return Ok(FormatResult {
            formatted: src.to_string(),
            changed: false,
            warnings,
        });
    }

    // Convert 0-based inclusive `(start_line, end_line)` to 1-based inclusive `[req_start ..= req_end]`.
    let req_start = (line_range.0 as usize) + 1;
    let req_end = (line_range.1.max(line_range.0) as usize) + 1;

    let hunks = make_diff(src, &full_formatted, 0);
    if hunks.is_empty() {
        return Ok(FormatResult {
            formatted: src.to_string(),
            changed: false,
            warnings,
        });
    }

    let orig_lines = split_lines_raw(src);
    let mut out_lines: Vec<String> = Vec::with_capacity(orig_lines.len() + 16);
    let mut orig_cursor = 0usize; // 0-based index into `orig_lines`
    let mut any_applied = false;

    for hunk in &hunks {
        let hunk_start_1 = hunk.line_number_orig; // 1-based
        let del_count = hunk
            .lines
            .iter()
            .filter(|l| matches!(l, crate::diff::DiffLine::Resulting(_)))
            .count();
        // Inclusive 1-based range in `orig` touched by this hunk:
        let hunk_end_1 = if del_count == 0 {
            hunk_start_1
        } else {
            hunk_start_1 + del_count - 1
        };

        let overlaps = hunk_start_1 <= req_end && hunk_end_1 >= req_start;
        let hunk_start_0 = hunk_start_1.saturating_sub(1).min(orig_lines.len());

        while orig_cursor < hunk_start_0 {
            out_lines.push(orig_lines[orig_cursor].to_string());
            orig_cursor += 1;
        }

        if overlaps {
            any_applied = true;
            for line in &hunk.lines {
                if let crate::diff::DiffLine::Expected(s) = line {
                    out_lines.push(s.trim_end_matches('\r').to_string());
                }
            }
            orig_cursor = (hunk_start_0 + del_count).min(orig_lines.len());
        } else {
            let end_0 = (hunk_start_0 + del_count).min(orig_lines.len());
            while orig_cursor < end_0 {
                out_lines.push(orig_lines[orig_cursor].to_string());
                orig_cursor += 1;
            }
        }
    }

    while orig_cursor < orig_lines.len() {
        out_lines.push(orig_lines[orig_cursor].to_string());
        orig_cursor += 1;
    }

    if !any_applied {
        return Ok(FormatResult {
            formatted: src.to_string(),
            changed: false,
            warnings,
        });
    }

    let mut spliced = out_lines.join(newline);
    if (src.ends_with('\n') || full_formatted.ends_with('\n')) && !spliced.is_empty() {
        spliced.push_str(newline);
    }

    if verify_formatted_output(&parsed, &spliced, display_name).is_ok() {
        let changed = spliced != src;
        return Ok(FormatResult {
            formatted: spliced,
            changed,
            warnings,
        });
    }

    // If a partial hunk split a multi-line construct, verify & return the full non-reordered result.
    verify_formatted_output(&parsed, &full_formatted, display_name)
        .map_err(FormatError::Verification)?;
    let changed = full_formatted != src;
    Ok(FormatResult {
        formatted: full_formatted,
        changed,
        warnings,
    })
}

fn split_lines_raw(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    if text.ends_with('\n') && lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

/// Inclusive range `[first ..= last]` in `ParsedSource::sig_tokens`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenSlot {
    pub first: usize,
    pub last: usize,
}

impl TokenSlot {
    pub fn new(first: usize, last: usize) -> Self {
        Self { first, last }
    }

    pub fn from_node(parsed: &ParsedSource, node: NodeIndex, floor: usize) -> Self {
        let (f, l) = parsed.exact_token_bounds(node, floor.max(1));
        Self { first: f, last: l }
    }

    /// Unwrap explicit `( ... )` parenthesis pairs wrapping `node` inside `self`.
    pub fn unwrap_parens(self, parsed: &ParsedSource, node: NodeIndex) -> (usize, Self) {
        let (count, inner_f, inner_l) = parsed.count_outer_parens(node, self.first, self.last);
        (
            count,
            Self {
                first: inner_f,
                last: inner_l,
            },
        )
    }

    /// Resolve the balanced token slot `[start_tok ..= end_tok]` for `child` inside `self`,
    /// including any explicit outer `(...)` around `child`.
    pub fn child_slot_from(
        self,
        parsed: &ParsedSource,
        start_tok: usize,
        child: NodeIndex,
    ) -> Self {
        let max_real = parsed.sig_tokens.len().saturating_sub(2);
        if max_real == 0 || child.is_null() {
            return Self::new(0, 0);
        }
        let start = start_tok.clamp(1, self.last.max(1).min(max_real));
        let (child_raw_first, raw_last) = parsed.exact_token_bounds(child, start);

        // Include any leading `(` before `child_raw_first` (at `>= start`) that wraps `child`.
        let mut actual_start = start;
        while actual_start < child_raw_first
            && parsed.sig_tokens[actual_start].kind != TokenKind::LParen
        {
            actual_start += 1;
        }
        // Only keep leading `(` that actually close at or before `self.last` around `child`:
        while actual_start < child_raw_first
            && parsed.sig_tokens[actual_start].kind == TokenKind::LParen
        {
            if let Some(close_idx) =
                crate::bridge::matching_paren_at(&parsed.sig_tokens, actual_start)
                && close_idx >= raw_last
                && close_idx <= self.last.min(max_real)
            {
                break;
            }
            actual_start += 1;
        }
        while actual_start < child_raw_first
            && parsed.sig_tokens[actual_start].kind != TokenKind::LParen
        {
            actual_start += 1;
        }

        let mut end = raw_last.clamp(actual_start, self.last.max(actual_start).min(max_real));
        let mut depth = 0isize;
        for idx in actual_start..=end {
            match parsed.sig_tokens[idx].kind {
                TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::LBrace
                | TokenKind::FStringExprStart => depth += 1,
                TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::RBrace
                | TokenKind::FStringExprEnd => {
                    depth = (depth - 1).max(0);
                }
                _ => {}
            }
        }
        let limit = self.last.min(max_real);
        while depth > 0 && end < limit {
            end += 1;
            match parsed.sig_tokens[end].kind {
                TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::LBrace
                | TokenKind::FStringExprStart => depth += 1,
                TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::RBrace
                | TokenKind::FStringExprEnd => depth -= 1,
                _ => {}
            }
        }
        Self::new(actual_start, end)
    }
}

/// Kind of container sequence for blank-line, comma, and semicolon rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerKind {
    /// Top-level file scope.
    FileScope,
    /// `mod name { ... }` body.
    ModuleBody,
    /// `struct` / `enum` body (fields/variants + methods/defs).
    AdtBody,
    /// `trait` / `impl` / `extend` body.
    TraitOrImplBody,
    /// Value/statement `{ ... }` block (`fn`, `if`, `else`, `while`, `for`, `do`, `match` arm, etc.).
    ValueBlock,
}

/// Member group category inside mixed containers (`struct`, `enum`, `mod`, `trait`, `impl`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemberCategory {
    FieldOrVariant,
    AssocItem,
    UseImport,
    StatementOrDef,
}

/// Nessa 3-group import classification per `docs/code-style.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ImportGroup {
    /// Group 1: `use std...`
    Std = 0,
    /// Group 2: `use external_pkg...`
    External = 1,
    /// Group 3: `use @...` (`PackagePath`) and `use ....` (`SuperPath`)
    IntraPackage = 2,
}

/// Stateful AST -> `Doc` formatter.
pub struct Formatter<'a> {
    pub parsed: &'a ParsedSource,
    pub comments: CommentCursor<'a>,
    pub cfg_stack: ScopedConfigStack,
    pub indent_spaces: usize,
    pub brace_depth: usize,
}

impl<'a> Formatter<'a> {
    pub fn new(parsed: &'a ParsedSource, root_cfg: FormatConfig) -> Self {
        let tab_spaces = root_cfg.tab_spaces;
        Self {
            parsed,
            comments: CommentCursor::new(parsed, tab_spaces),
            cfg_stack: ScopedConfigStack::new(root_cfg),
            indent_spaces: 0,
            brace_depth: 0,
        }
    }

    pub fn cfg(&self) -> FormatConfig {
        self.cfg_stack.current().clone()
    }

    pub fn tab_spaces(&self) -> usize {
        self.cfg_stack.current().tab_spaces
    }

    /// Format an `Id` node, preserving backtick `` `...` `` quoting if originally backtick-quoted
    /// or if the identifier requires backticks (keyword / non-identifier characters).
    pub fn format_ident(&self, node: NodeIndex, floor_tok: usize) -> String {
        if node.is_null() {
            return String::new();
        }
        let (f, l) = self.parsed.exact_token_bounds(node, floor_tok.max(1));
        if f > 0
            && f == l
            && let Some(tok) = self.parsed.sig_tokens.get(f)
            && tok.kind == TokenKind::ArbitraryId
        {
            let lo = tok.from as usize;
            let hi = tok.to as usize;
            if let Some(raw) = self.parsed.original_src.get(lo..hi) {
                return raw.to_string();
            }
        }
        let text = str_interner::get(self.parsed.ast.node(node).str_id);
        if needs_backticks(&text) {
            format!("`{text}`")
        } else {
            text.to_string()
        }
    }

    /// Raw unquoted identifier string for sorting/classification.
    pub fn raw_ident_name(&self, node: NodeIndex) -> String {
        if node.is_null() {
            return String::new();
        }
        str_interner::get(self.parsed.ast.node(node).str_id).to_string()
    }

    /// Exact slice of `original_src` for a leaf/literal node.
    pub fn literal_slice(&self, node: NodeIndex, floor_tok: usize) -> &'a str {
        if let Some(orig) = self.parsed.ast.original_node_text(node)
            && is_exact_atomic_node(&self.parsed.ast, node)
        {
            return orig;
        }
        let (lo, hi) = self.parsed.exact_byte_range(node, floor_tok.max(1));
        self.parsed.original_src.get(lo..hi).unwrap_or("")
    }

    /// Check if `node` is a bridged `--% .skip` placeholder construct.
    pub fn bridged_at(&self, node: NodeIndex, floor_tok: usize) -> Option<&'a BridgedConstruct> {
        let (raw_lo, _) = self.parsed.raw_recorded_range(node);
        if let Some(b) = self.parsed.bridge.constructs_by_start.get(&raw_lo) {
            return Some(b);
        }
        let (exact_lo, _) = self.parsed.exact_byte_range(node, floor_tok.max(1));
        self.parsed.bridge.constructs_by_start.get(&exact_lo)
    }

    /// Format a bridged construct (`SkippedUnparsed`).
    pub fn format_bridged(&mut self, bridged: &BridgedConstruct) -> Doc {
        match bridged {
            BridgedConstruct::SkippedUnparsed { start, end } => {
                self.format_skipped_bytes(*start, *end)
            }
        }
    }

    /// Format `[lo..hi]` under `--% .skip`: align line 1 to current indent and shift lines 2..N
    /// preserving relative indentation, spacing, and line breaks verbatim.
    pub fn format_skipped_bytes(&mut self, lo: usize, hi: usize) -> Doc {
        self.comments
            .advance_past_verbatim(hi, self.brace_depth, &mut self.cfg_stack);
        let raw = self.parsed.original_src.get(lo..hi).unwrap_or("");
        if !raw.contains('\n') {
            return Doc::text(raw.trim_end_matches([' ', '\t']).to_string());
        }
        let tab_spaces = self.tab_spaces();
        let orig_base_col = self.parsed.visual_column(lo, tab_spaces);
        let target_indent = self.indent_spaces;

        let normalized = raw.replace("\r\n", "\n");
        let lines: Vec<&str> = normalized.split('\n').collect();
        let mut out_lines = Vec::with_capacity(lines.len());

        for (idx, line) in lines.iter().enumerate() {
            let stripped = line.trim_end_matches([' ', '\t']);
            if idx == 0 {
                out_lines.push(stripped.to_string());
                continue;
            }
            if stripped.is_empty() {
                out_lines.push(String::new());
                continue;
            }
            let (line_col, content_idx) = measure_leading_ws(stripped, tab_spaces);
            let content = &stripped[content_idx..];
            let new_indent = if line_col >= orig_base_col {
                target_indent + (line_col - orig_base_col)
            } else {
                line_col
            };
            let mut rebuilt = String::with_capacity(new_indent + content.len());
            for _ in 0..new_indent {
                rebuilt.push(' ');
            }
            rebuilt.push_str(content);
            out_lines.push(rebuilt);
        }

        let first = out_lines[0].clone();
        let rest = out_lines[1..].join("\n");
        Doc::concat([
            Doc::text(first),
            Doc::dedent_to_zero(Doc::verbatim(format!("\n{rest}"))),
        ])
    }

    /// Format `[lo..hi]` under `--% .preserve`: preserve user line breaks and intra-line alignment
    /// spaces while normalizing outer block indentation and `--` comment spacing.
    pub fn format_preserved_bytes(&mut self, lo: usize, hi: usize) -> Doc {
        let normalize_comments = self.cfg_stack.current().normalize_comments;
        self.comments
            .advance_past_verbatim(hi, self.brace_depth, &mut self.cfg_stack);
        let raw = self.parsed.original_src.get(lo..hi).unwrap_or("");
        let tab_spaces = self.tab_spaces();
        let orig_base_col = self.parsed.visual_column(lo, tab_spaces);
        let target_indent = self.indent_spaces;

        let normalized = raw.replace("\r\n", "\n");
        let lines: Vec<&str> = normalized.split('\n').collect();
        if lines.len() == 1 {
            let line = normalize_line_comment_suffix(
                lines[0].trim_end_matches([' ', '\t']),
                normalize_comments,
            );
            return Doc::text(line);
        }

        let mut out_lines = Vec::with_capacity(lines.len());
        for (idx, line) in lines.iter().enumerate() {
            let stripped = line.trim_end_matches([' ', '\t']);
            if idx == 0 {
                out_lines.push(normalize_line_comment_suffix(stripped, normalize_comments));
                continue;
            }
            if stripped.is_empty() {
                out_lines.push(String::new());
                continue;
            }
            let (line_col, content_idx) = measure_leading_ws(stripped, tab_spaces);
            let content =
                normalize_line_comment_suffix(&stripped[content_idx..], normalize_comments);
            let new_indent = if line_col >= orig_base_col {
                target_indent + (line_col - orig_base_col)
            } else {
                line_col
            };
            let mut rebuilt = String::with_capacity(new_indent + content.len());
            for _ in 0..new_indent {
                rebuilt.push(' ');
            }
            rebuilt.push_str(&content);
            out_lines.push(rebuilt);
        }

        let first = out_lines[0].clone();
        let rest = out_lines[1..].join("\n");
        Doc::concat([
            Doc::text(first),
            Doc::dedent_to_zero(Doc::verbatim(format!("\n{rest}"))),
        ])
    }

    /// Format top-level `FileScope`.
    pub fn format_file_scope(&mut self) -> Doc {
        let root = self.parsed.ast.root;
        let items = if root.is_null() {
            &[][..]
        } else {
            self.parsed.ast.multi_children(root)
        };
        let max_real = self.parsed.sig_tokens.len().saturating_sub(2);
        let eof_byte = self.parsed.original_src.len();
        self.format_container_elements(items, 1, max_real, 0, eof_byte, ContainerKind::FileScope)
    }

    /// Format a sequence of elements inside a file, module, ADT, trait/impl, or `{ ... }` block.
    pub fn format_container_elements(
        &mut self,
        items: &[NodeIndex],
        container_first_tok: usize,
        container_last_tok: usize,
        open_end_byte: usize,
        close_start_byte: usize,
        kind: ContainerKind,
    ) -> Doc {
        if items.is_empty() {
            let (trailing_comments, _, _) = self.comments.take_leading_before(
                open_end_byte,
                close_start_byte,
                self.indent_spaces,
                self.brace_depth,
                false,
                &mut self.cfg_stack,
            );
            return trim_trailing_hardline(trailing_comments);
        }

        // Precompute exact token bounds & byte ranges for each element monotonically.
        let mut elem_infos = Vec::with_capacity(items.len());
        let mut floor_tok = container_first_tok.max(1);
        for (idx, &node) in items.iter().enumerate() {
            let (raw_f, raw_l) = self.parsed.exact_token_bounds(node, floor_tok);
            let slot_start = floor_tok;
            let mut f = raw_f;
            while f > slot_start && self.parsed.sig_tokens[f - 1].kind == TokenKind::LParen {
                if let Some(close_p) =
                    crate::bridge::matching_paren_at(&self.parsed.sig_tokens, f - 1)
                    && close_p >= raw_l
                    && close_p <= container_last_tok.max(raw_l)
                {
                    f -= 1;
                } else {
                    break;
                }
            }
            let bound = TokenSlot::new(f, container_last_tok.max(raw_l));
            let full_slot = bound.child_slot_from(self.parsed, f, node);
            let lo = self.parsed.sig_tokens[full_slot.first].from as usize;
            let mut hi = self.parsed.sig_tokens[full_slot.last].to as usize;
            if let Some(b) = self.parsed.bridge.constructs_by_start.get(&lo) {
                hi = hi.max(b.span_bytes().1);
            }
            let mut sep_end_byte = hi;
            let mut after_sep_tok = full_slot.last + 1;
            while after_sep_tok <= container_last_tok
                && after_sep_tok < self.parsed.sig_tokens.len()
                && matches!(
                    self.parsed.sig_tokens[after_sep_tok].kind,
                    TokenKind::Semi | TokenKind::Comma
                )
            {
                sep_end_byte = self.parsed.sig_tokens[after_sep_tok].to as usize;
                after_sep_tok += 1;
            }
            let end_line = self.parsed.line_of_byte(sep_end_byte.saturating_sub(1));
            elem_infos.push(ElemInfo {
                index: idx,
                node,
                slot: full_slot,
                lo,
                hi,
                sep_end_byte,
                end_line,
            });
            floor_tok = after_sep_tok;
        }

        let mut parts: Vec<Doc> = Vec::new();
        let mut prev_end_byte = open_end_byte;
        let mut prev_category: Option<MemberCategory> = None;
        let mut prev_was_multiline_def = false;
        let mut idx = 0usize;

        while idx < elem_infos.len() {
            let info = elem_infos[idx];

            // Handle `--% .off` region covering one or more elements!
            if let Some(region) = self.parsed.enclosing_off_region(info.lo, info.hi) {
                let (lead, _, _) = self.comments.take_leading_before(
                    prev_end_byte,
                    region.body_start,
                    self.indent_spaces,
                    self.brace_depth,
                    !parts.is_empty(),
                    &mut self.cfg_stack,
                );
                if !matches!(lead, Doc::Nil) {
                    if !parts.is_empty() {
                        parts.push(Doc::HardLine);
                    }
                    parts.push(trim_trailing_hardline(lead));
                }
                let raw_body = &self.parsed.original_src[region.body_start..region.body_end];
                let trimmed_nl = raw_body.strip_suffix('\n').unwrap_or(raw_body);
                let trimmed_nl = trimmed_nl.strip_suffix('\r').unwrap_or(trimmed_nl);
                if !trimmed_nl.is_empty() {
                    if !parts.is_empty() {
                        parts.push(Doc::HardLine);
                    }
                    parts.push(Doc::dedent_to_zero(Doc::verbatim(trimmed_nl.to_string())));
                }
                self.comments.advance_past_verbatim(
                    region.body_end,
                    self.brace_depth,
                    &mut self.cfg_stack,
                );
                prev_end_byte = region.body_end;
                while idx < elem_infos.len() && elem_infos[idx].hi <= region.body_end {
                    prev_category = Some(categorize_member(&self.parsed.ast, elem_infos[idx].node));
                    prev_was_multiline_def = false;
                    idx += 1;
                }
                continue;
            }

            let (lead_doc, gap_cursor, dir_flags) = self.comments.take_leading_before(
                prev_end_byte,
                info.lo,
                self.indent_spaces,
                self.brace_depth,
                !parts.is_empty(),
                &mut self.cfg_stack,
            );

            let cur_cat = categorize_member(&self.parsed.ast, info.node);
            let cur_is_multiline_def = is_multiline_item_def(&self.parsed.ast, info.node);

            if !parts.is_empty() {
                let user_had_blank = self.parsed.blank_lines_in_gap(prev_end_byte, info.lo) > 0
                    || self.parsed.blank_lines_in_gap(gap_cursor, info.lo) > 0;
                let group_transition = matches!(
                    (kind, prev_category, cur_cat),
                    (
                        ContainerKind::AdtBody
                            | ContainerKind::ModuleBody
                            | ContainerKind::TraitOrImplBody
                            | ContainerKind::FileScope,
                        Some(prev_c),
                        cur_c,
                    ) if prev_c != cur_c
                );
                let top_def_spacing = matches!(
                    kind,
                    ContainerKind::FileScope
                        | ContainerKind::ModuleBody
                        | ContainerKind::TraitOrImplBody
                ) && (prev_was_multiline_def || cur_is_multiline_def);

                let lead_starts_blank = doc_starts_with_line_break(&lead_doc);
                if (user_had_blank || group_transition || top_def_spacing) && !lead_starts_blank {
                    parts.push(Doc::BlankLine);
                } else {
                    parts.push(Doc::HardLine);
                }
            }

            // Reorder contiguous top-of-scope `use` imports into Nessa's 3 groups when enabled!
            if self.cfg_stack.current().reorder_imports
                && !dir_flags.skip
                && !dir_flags.preserve
                && is_sortable_use_item(&self.parsed.ast, info.node)
            {
                let group_end = self.find_contiguous_use_run_end(&elem_infos, idx);
                if group_end > idx {
                    if !matches!(lead_doc, Doc::Nil) {
                        parts.push(lead_doc);
                    }
                    let next_after_group = elem_infos.get(group_end).copied();
                    let group_doc = self.format_sorted_use_run(
                        &elem_infos[idx..group_end],
                        next_after_group,
                        close_start_byte,
                        kind,
                    );
                    parts.push(group_doc);
                    let last_in_group = elem_infos[group_end - 1];
                    prev_end_byte = last_in_group
                        .sep_end_byte
                        .max(self.last_consumed_comment_end());
                    prev_category = Some(MemberCategory::UseImport);
                    prev_was_multiline_def = false;
                    idx = group_end;
                    continue;
                }
            }

            if !matches!(lead_doc, Doc::Nil) {
                parts.push(lead_doc);
            }

            let is_last = idx + 1 == elem_infos.len();
            let next_info = elem_infos.get(idx + 1).copied();
            let next_limit_byte = next_info.map_or(close_start_byte, |n| n.lo);

            let elem_doc = if dir_flags.skip {
                let body = self.format_skipped_bytes(info.lo, info.hi);
                let term = self.element_terminator(info, is_last, next_info, kind, false);
                Doc::concat([body, term])
            } else if dir_flags.preserve {
                let body = self.format_preserved_bytes(info.lo, info.hi);
                let term = self.element_terminator(info, is_last, next_info, kind, false);
                Doc::concat([body, term])
            } else {
                let active_mw = self.cfg_stack.current().max_width;
                let (body, formatted_ends_rbrace) =
                    self.format_container_member(info.node, info.slot, is_last, kind);
                let term =
                    self.element_terminator(info, is_last, next_info, kind, formatted_ends_rbrace);
                Doc::with_max_width(active_mw, Doc::concat([body, term]))
            };

            let cfg_now = self.cfg();
            let trailing_comment = self.comments.take_same_line_trailing(
                info.end_line,
                next_limit_byte,
                self.indent_spaces,
                &cfg_now,
            );
            parts.push(elem_doc);
            if !matches!(trailing_comment, Doc::Nil) {
                parts.push(trailing_comment);
            }

            prev_end_byte = info.sep_end_byte.max(self.last_consumed_comment_end());
            prev_category = Some(cur_cat);
            prev_was_multiline_def = cur_is_multiline_def;
            idx += 1;
        }

        let (end_comments, _, _) = self.comments.take_leading_before(
            prev_end_byte,
            close_start_byte,
            self.indent_spaces,
            self.brace_depth,
            true,
            &mut self.cfg_stack,
        );
        if !matches!(end_comments, Doc::Nil) {
            if !parts.is_empty() {
                let had_blank = self
                    .parsed
                    .blank_lines_in_gap(prev_end_byte, close_start_byte)
                    > 0;
                if had_blank && !doc_starts_with_line_break(&end_comments) {
                    parts.push(Doc::BlankLine);
                } else {
                    parts.push(Doc::HardLine);
                }
            }
            parts.push(trim_trailing_hardline(end_comments));
        }

        Doc::concat(parts)
    }

    fn last_consumed_comment_end(&self) -> usize {
        if self.comments.next_idx == 0 {
            0
        } else {
            self.parsed.comments[self.comments.next_idx - 1].byte_end
        }
    }

    /// Determine trailing `,` or `;` for a container member in Nessa:
    /// - `StructField` and `EnumVariant` inside `AdtBody` use `,`.
    /// - Statements and definitions omit `;` UNLESS required to prevent newline continuation
    ///   ambiguity in `Parser::next_token_inner`.
    pub(crate) fn element_terminator(
        &self,
        info: ElemInfo,
        is_last: bool,
        next_info: Option<ElemInfo>,
        container_kind: ContainerKind,
        formatted_ends_rbrace: bool,
    ) -> Doc {
        let base_kind = unwrap_modifiers_kind(&self.parsed.ast, info.node);

        if container_kind == ContainerKind::AdtBody
            && matches!(
                base_kind,
                Some(NodeKind::StructField | NodeKind::EnumVariant)
            )
        {
            return Doc::str(",");
        }

        if is_last {
            return Doc::Nil;
        }

        let Some(next) = next_info else {
            return Doc::Nil;
        };

        if self.needs_semicolon_before_next_line(info, formatted_ends_rbrace, next) {
            Doc::str(";")
        } else {
            Doc::Nil
        }
    }

    /// Check `Parser::next_token_inner` (`engine/parser/src/parser.rs:396-426`):
    /// A newline `\n` is NOT converted to a virtual `TokenKind::Semi` when:
    /// 1. `prev` (the last emitted token of `cur`) has `!prev.can_end_statement()` and is not
    ///    `use ...*` (`PathProjectionAll`), e.g. `RangeFull` (`..`) or `RangeFrom` (`x..`) ending in `.`!
    /// 2. `next` (the first emitted token of `next`) has `!next.can_start_statement()` (e.g. `.symbol`,
    ///    `..`, `|x| body`, `not expr`), OR starts with `# {` / `! {` (`next_is_postfix_elim`).
    fn needs_semicolon_before_next_line(
        &self,
        cur: ElemInfo,
        formatted_ends_rbrace: bool,
        next: ElemInfo,
    ) -> bool {
        let prev_ended = if formatted_ends_rbrace || is_glob_use_item(&self.parsed.ast, cur.node) {
            true
        } else if let Some(last_tok) = self.parsed.sig_tokens.get(cur.slot.last) {
            last_tok.kind.can_end_statement()
        } else {
            true
        };
        if !prev_ended {
            return true;
        }

        let Some(first_next) = self.parsed.sig_tokens.get(next.slot.first) else {
            return false;
        };
        if !first_next.kind.can_start_statement() {
            return true;
        }
        // `next_is_postfix_elim`: `# {` or `! {` at the start of `next`.
        if matches!(first_next.kind, TokenKind::Hash | TokenKind::Bang)
            && self
                .parsed
                .sig_tokens
                .get(next.slot.first + 1)
                .is_some_and(|t2| t2.kind == TokenKind::LBrace)
        {
            return true;
        }
        false
    }

    /// Find the end of a contiguous `use` run sharing the same visibility (`use` vs `pub use` vs `private use`)
    /// with no interleaved `.off`/`.skip` regions or standalone comments/directives.
    /// Blank lines between `use` statements ARE permitted inside the run so 3-group organization is idempotent!
    fn find_contiguous_use_run_end(&self, infos: &[ElemInfo], start_idx: usize) -> usize {
        let first_vis = use_visibility_tag(&self.parsed.ast, infos[start_idx].node);
        let mut end = start_idx + 1;
        while end < infos.len() {
            let prev = infos[end - 1];
            let cur = infos[end];
            if !is_sortable_use_item(&self.parsed.ast, cur.node) {
                break;
            }
            if use_visibility_tag(&self.parsed.ast, cur.node) != first_vis {
                break;
            }
            if self.parsed.enclosing_off_region(cur.lo, cur.hi).is_some() {
                break;
            }
            let has_intervening_standalone_comment = self.parsed.comments.iter().any(|c| {
                c.byte_start >= prev.sep_end_byte
                    && c.byte_start < cur.lo
                    && !(c.line_no == prev.end_line && c.has_code_before_on_line)
            });
            if has_intervening_standalone_comment {
                break;
            }
            end += 1;
        }
        end
    }

    fn format_sorted_use_run(
        &mut self,
        run: &[ElemInfo],
        next_after: Option<ElemInfo>,
        close_start_byte: usize,
        container_kind: ContainerKind,
    ) -> Doc {
        struct RenderedUse {
            group: ImportGroup,
            sort_key: String,
            body: Doc,
            trailing_comment: Doc,
            orig_info: ElemInfo,
        }

        let cfg_now = self.cfg();
        let mut entries = Vec::with_capacity(run.len());
        for (i, &info) in run.iter().enumerate() {
            let limit_byte = run
                .get(i + 1)
                .map(|n| n.lo)
                .or_else(|| next_after.map(|n| n.lo))
                .unwrap_or(close_start_byte);
            let (body, group, sort_key) = self.format_use_item_with_group_key(info.node, info.slot);
            let trailing_comment = self.comments.take_same_line_trailing(
                info.end_line,
                limit_byte,
                self.indent_spaces,
                &cfg_now,
            );
            entries.push(RenderedUse {
                group,
                sort_key,
                body,
                trailing_comment,
                orig_info: info,
            });
        }

        entries.sort_by(|a, b| {
            a.group
                .cmp(&b.group)
                .then_with(|| compare_import_keys(&a.sort_key, &b.sort_key))
        });

        let mut docs = Vec::with_capacity(entries.len() * 3);
        let len = entries.len();
        let mut prev_group: Option<ImportGroup> = None;

        for (idx, entry) in entries.into_iter().enumerate() {
            if let Some(pg) = prev_group {
                if pg != entry.group {
                    docs.push(Doc::BlankLine);
                } else {
                    docs.push(Doc::HardLine);
                }
            }
            prev_group = Some(entry.group);

            let is_very_last = idx + 1 == len && next_after.is_none();
            let term = if idx + 1 == len {
                self.element_terminator(
                    entry.orig_info,
                    is_very_last,
                    next_after,
                    container_kind,
                    false,
                )
            } else {
                Doc::Nil
            };
            docs.push(entry.body);
            docs.push(term);
            if !matches!(entry.trailing_comment, Doc::Nil) {
                docs.push(entry.trailing_comment);
            }
        }
        Doc::concat(docs)
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ElemInfo {
    #[allow(dead_code)]
    pub index: usize,
    pub node: NodeIndex,
    pub slot: TokenSlot,
    pub lo: usize,
    pub hi: usize,
    pub sep_end_byte: usize,
    pub end_line: usize,
}

pub(crate) fn unwrap_modifiers_kind(ast: &Ast, mut node: NodeIndex) -> Option<NodeKind> {
    while !node.is_null() {
        let kind = ast.node(node).kind;
        match kind {
            NodeKind::PubDef | NodeKind::PrivateDef => {
                node = ast.fixed_children(node)[0];
            }
            other => return Some(other),
        }
    }
    None
}

fn categorize_member(ast: &Ast, node: NodeIndex) -> MemberCategory {
    match unwrap_modifiers_kind(ast, node) {
        Some(NodeKind::StructField | NodeKind::EnumVariant) => MemberCategory::FieldOrVariant,
        Some(NodeKind::AssocDecl) => MemberCategory::AssocItem,
        Some(NodeKind::UseStatement) => MemberCategory::UseImport,
        _ => MemberCategory::StatementOrDef,
    }
}

fn is_multiline_item_def(ast: &Ast, mut node: NodeIndex) -> bool {
    while !node.is_null() {
        match ast.node(node).kind {
            NodeKind::PubDef | NodeKind::PrivateDef => {
                node = ast.fixed_children(node)[0];
            }
            NodeKind::StructDef
            | NodeKind::EnumDef
            | NodeKind::TraitDef
            | NodeKind::ImplDef
            | NodeKind::ImplTraitDef
            | NodeKind::ExtendDef
            | NodeKind::ExtendTraitDef => return true,
            NodeKind::ModuleDef => {
                return !ast.multi_children(node).is_empty();
            }
            NodeKind::FunctionDef | NodeKind::TraitDeriveFn => {
                let body = ast.fixed_children(node)[2];
                return !body.is_null() && ast.node(body).kind == NodeKind::Block;
            }
            NodeKind::HandlesStatement => {
                let body = ast.fixed_children(node)[2];
                return !body.is_null() && ast.node(body).kind == NodeKind::Block;
            }
            _ => return false,
        }
    }
    false
}

fn is_sortable_use_item(ast: &Ast, mut node: NodeIndex) -> bool {
    while !node.is_null() {
        match ast.node(node).kind {
            NodeKind::PubDef | NodeKind::PrivateDef => {
                node = ast.fixed_children(node)[0];
            }
            NodeKind::UseStatement => return true,
            _ => return false,
        }
    }
    false
}

fn use_visibility_tag(ast: &Ast, mut node: NodeIndex) -> u8 {
    let mut vis = 0u8;
    while !node.is_null() {
        match ast.node(node).kind {
            NodeKind::PubDef => {
                vis = 1;
                node = ast.fixed_children(node)[0];
            }
            NodeKind::PrivateDef => {
                vis = 2;
                node = ast.fixed_children(node)[0];
            }
            _ => break,
        }
    }
    vis
}

fn is_glob_use_item(ast: &Ast, mut node: NodeIndex) -> bool {
    while !node.is_null() {
        match ast.node(node).kind {
            NodeKind::PubDef | NodeKind::PrivateDef => {
                node = ast.fixed_children(node)[0];
            }
            NodeKind::UseStatement => {
                let mut p = ast.fixed_children(node)[0];
                if !p.is_null()
                    && matches!(
                        ast.node(p).kind,
                        NodeKind::SuperPath | NodeKind::PackagePath
                    )
                {
                    p = ast.fixed_children(p)[0];
                }
                return !p.is_null() && ast.node(p).kind == NodeKind::PathProjectionAll;
            }
            _ => return false,
        }
    }
    false
}

fn is_exact_atomic_node(ast: &Ast, node: NodeIndex) -> bool {
    if node.is_null() {
        return false;
    }
    matches!(
        ast.node(node).kind,
        NodeKind::Int | NodeKind::Real | NodeKind::Str | NodeKind::Char | NodeKind::Bool
    )
}

fn needs_backticks(s: &str) -> bool {
    if s.is_empty() || lexer::token::lookup_keyword(s).is_some() {
        return true;
    }
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return true;
    };
    if !(first.is_alphabetic() || first == '_') {
        return true;
    }
    !chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// Lexicographical comparator for Nessa import sort keys where `@` (`PackagePath`) sorts before
/// `.` (`SuperPath`) at position 0 per `docs/code-style.md` (`use @config.Settings` before `use .utils.validate`).
pub fn compare_import_keys(a: &str, b: &str) -> Ordering {
    match (
        a.starts_with('@'),
        b.starts_with('.'),
        a.starts_with('.'),
        b.starts_with('@'),
    ) {
        (true, true, _, _) => return Ordering::Less,
        (_, _, true, true) => return Ordering::Greater,
        _ => {}
    }
    a.cmp(b)
}

fn normalize_line_comment_suffix(line: &str, normalize_comments: bool) -> String {
    if !normalize_comments {
        return line.to_string();
    }
    // Scan outside strings/chars/backticks for a `--` line comment suffix.
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    while i < len {
        match bytes[i] {
            b'"' => {
                i = crate::bridge::skip_nessa_string(line, i);
                continue;
            }
            b'`' => {
                i += 1;
                while i < len && bytes[i] != b'`' {
                    i += 1;
                }
            }
            b'\'' => {
                i = crate::bridge::skip_quote_or_char_or_macro(line, i);
                continue;
            }
            b'-' if i + 1 < len && bytes[i + 1] == b'-' => {
                let before = &line[..i];
                let comment = &line[i..];
                return format!("{before}{}", normalize_single_line_comment_str(comment));
            }
            _ => {}
        }
        if let Some(ch) = line[i..].chars().next() {
            i += ch.len_utf8();
        } else {
            break;
        }
    }
    line.to_string()
}

pub(crate) fn trim_trailing_hardline(doc: Doc) -> Doc {
    match doc {
        Doc::HardLine => Doc::Nil,
        Doc::Concat(mut items) => {
            if matches!(items.last(), Some(Doc::HardLine)) {
                items.pop();
            }
            Doc::concat(items)
        }
        other => other,
    }
}

pub(crate) fn doc_starts_with_line_break(doc: &Doc) -> bool {
    match doc {
        Doc::HardLine | Doc::BlankLine => true,
        Doc::Concat(items) => items.first().is_some_and(doc_starts_with_line_break),
        _ => false,
    }
}
