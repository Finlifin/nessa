//! Comment normalization, aligned trailing `--` comment detection, and monotonic trivia cursor.

use std::collections::HashMap;

use crate::bridge::{CommentItem, CommentKind, ParsedSource};
use crate::config::FormatConfig;
use crate::directives::ScopedConfigStack;
use crate::doc::Doc;

/// Format a `-- ...` line comment according to `.normalize_comments` and directive rules.
pub fn format_line_comment(
    parsed: &ParsedSource,
    comment: &CommentItem,
    normalize_comments: bool,
) -> String {
    if let Some(d_idx) = comment.directive_index {
        return parsed.directives[d_idx].normalized_text.clone();
    }
    let raw = &parsed.original_src[comment.byte_start..comment.byte_end];
    let trimmed = raw.trim_end_matches([' ', '\t']);
    if !normalize_comments {
        return trimmed.to_string();
    }
    normalize_single_line_comment_str(trimmed)
}

/// Normalize a `--...` comment string if `normalize_comments` is enabled:
/// `--foo` -> `-- foo`, while keeping `--`, `---...`, `--%...`, and `-- ...` unchanged.
pub fn normalize_single_line_comment_str(trimmed: &str) -> String {
    let Some(after_dashes) = trimmed.strip_prefix("--") else {
        return trimmed.to_string();
    };
    if after_dashes.is_empty() {
        return "--".to_string();
    }
    if after_dashes.starts_with('-')
        || after_dashes.starts_with('%')
        || after_dashes.starts_with(' ')
    {
        return trimmed.to_string();
    }
    if let Some(after_tab) = after_dashes.strip_prefix('\t') {
        return format!("-- {after_tab}");
    }
    format!("-- {after_dashes}")
}

/// Format a `{- ... -}` block comment, normalizing internal relative indentation to `target_indent`.
pub fn format_block_comment(
    parsed: &ParsedSource,
    comment: &CommentItem,
    target_indent: usize,
    tab_spaces: usize,
) -> Doc {
    let raw = &parsed.original_src[comment.byte_start..comment.byte_end];
    if !raw.contains('\n') {
        return Doc::text(raw.to_string());
    }
    let orig_base_col = parsed.visual_column(comment.byte_start, tab_spaces);
    let normalized = raw.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.split('\n').collect();
    let mut out_lines = Vec::with_capacity(lines.len());
    for (idx, line) in lines.iter().enumerate() {
        let stripped_right = line.trim_end_matches([' ', '\t']);
        if idx == 0 {
            out_lines.push(stripped_right.to_string());
            continue;
        }
        if stripped_right.is_empty() {
            out_lines.push(String::new());
            continue;
        }
        let (line_col, content_start) = measure_leading_ws(stripped_right, tab_spaces);
        let content = &stripped_right[content_start..];
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

pub fn measure_leading_ws(line: &str, tab_spaces: usize) -> (usize, usize) {
    let mut col = 0usize;
    let mut byte_idx = 0usize;
    for (idx, ch) in line.char_indices() {
        match ch {
            ' ' => {
                col += 1;
                byte_idx = idx + 1;
            }
            '\t' => {
                col += tab_spaces - (col % tab_spaces);
                byte_idx = idx + 1;
            }
            _ => break,
        }
    }
    (col, byte_idx)
}

/// Precompute aligned trailing `--` comment groups across consecutive lines.
/// Maps `comment_idx -> original_visual_column` when 2+ adjacent lines share the same trailing `--` column.
pub fn compute_aligned_trailing_comments(
    parsed: &ParsedSource,
    tab_spaces: usize,
) -> HashMap<usize, usize> {
    let mut aligned = HashMap::new();
    let mut i = 0usize;
    while i < parsed.comments.len() {
        let c = &parsed.comments[i];
        if c.kind != CommentKind::Line || !c.has_code_before_on_line {
            i += 1;
            continue;
        }
        let base_col = parsed.visual_column(c.byte_start, tab_spaces);
        let mut j = i + 1;
        let mut prev_line = c.line_no;
        while j < parsed.comments.len() {
            let next = &parsed.comments[j];
            if next.kind == CommentKind::Line
                && next.has_code_before_on_line
                && next.line_no == prev_line + 1
                && parsed.visual_column(next.byte_start, tab_spaces) == base_col
            {
                prev_line = next.line_no;
                j += 1;
            } else {
                break;
            }
        }
        if j - i >= 2 {
            for idx in i..j {
                aligned.insert(idx, base_col);
            }
        }
        i = j;
    }
    aligned
}

/// Pending per-node directive flags accumulated immediately before an AST node.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NodeDirectiveFlags {
    pub skip: bool,
    pub preserve: bool,
}

/// Monotonic cursor over `parsed.comments` ensuring every comment is emitted exactly once.
pub struct CommentCursor<'a> {
    pub parsed: &'a ParsedSource,
    pub next_idx: usize,
    pub aligned_trailing: HashMap<usize, usize>,
}

impl<'a> CommentCursor<'a> {
    pub fn new(parsed: &'a ParsedSource, tab_spaces: usize) -> Self {
        let aligned_trailing = compute_aligned_trailing_comments(parsed, tab_spaces);
        Self {
            parsed,
            next_idx: 0,
            aligned_trailing,
        }
    }

    pub fn peek(&self) -> Option<(usize, &'a CommentItem)> {
        self.parsed
            .comments
            .get(self.next_idx)
            .map(|c| (self.next_idx, c))
    }

    /// Skip comments inside `[0 .. up_to_byte)` when emitting a verbatim `.off` or `.skip` slice
    /// that already includes those exact comment bytes.
    pub fn advance_past_verbatim(
        &mut self,
        up_to_byte: usize,
        brace_depth: usize,
        cfg_stack: &mut ScopedConfigStack,
    ) {
        while let Some((_, c)) = self.peek() {
            if c.byte_start >= up_to_byte {
                break;
            }
            if let Some(d_idx) = c.directive_index {
                let dir = &self.parsed.directives[d_idx];
                if dir.is_valid {
                    cfg_stack.apply_at_depth(brace_depth, &dir.actions);
                }
            }
            self.next_idx += 1;
        }
    }

    /// Check whether a same-line trailing `--` line comment exists on `line_no` before `limit_byte`.
    pub fn has_same_line_line_comment(&self, line_no: usize, limit_byte: usize) -> bool {
        let mut idx = self.next_idx;
        while let Some(c) = self.parsed.comments.get(idx) {
            if c.byte_start >= limit_byte || c.line_no != line_no || !c.has_code_before_on_line {
                break;
            }
            if c.kind == CommentKind::Line {
                return true;
            }
            idx += 1;
        }
        false
    }

    /// Consume trailing comment(s) on the SAME line `line_no` before `limit_byte`, if present.
    pub fn take_same_line_trailing(
        &mut self,
        line_no: usize,
        limit_byte: usize,
        indent_spaces: usize,
        cfg: &FormatConfig,
    ) -> Doc {
        let mut docs = Vec::new();
        while let Some((c_idx, c)) = self.peek() {
            if c.byte_start >= limit_byte || c.line_no != line_no || !c.has_code_before_on_line {
                break;
            }
            self.next_idx += 1;
            match c.kind {
                CommentKind::Line => {
                    let text = format_line_comment(self.parsed, c, cfg.normalize_comments);
                    if let Some(&orig_col) = self.aligned_trailing.get(&c_idx) {
                        docs.push(Doc::PadToCol(orig_col));
                    } else {
                        docs.push(Doc::space());
                    }
                    docs.push(Doc::text(text));
                }
                CommentKind::Block => {
                    docs.push(Doc::space());
                    docs.push(format_block_comment(
                        self.parsed,
                        c,
                        indent_spaces,
                        cfg.tab_spaces,
                    ));
                }
            }
        }
        Doc::concat(docs)
    }

    /// Consume leading comments strictly before `before_byte`, updating `cfg_stack` and returning
    /// `(leading_doc, prev_end_byte, pending_node_flags)`.
    /// Also handles any `--% .off ... --% .on` verbatim blocks encountered in the trivia gap.
    pub fn take_leading_before(
        &mut self,
        mut prev_end_byte: usize,
        before_byte: usize,
        indent_spaces: usize,
        brace_depth: usize,
        allow_initial_blank: bool,
        cfg_stack: &mut ScopedConfigStack,
    ) -> (Doc, usize, NodeDirectiveFlags) {
        let mut parts: Vec<Doc> = Vec::new();
        let mut flags = NodeDirectiveFlags::default();
        let mut emitted_anything = false;

        while let Some((_, c)) = self.peek() {
            if c.byte_start >= before_byte {
                break;
            }
            self.next_idx += 1;

            let blanks = self.parsed.blank_lines_in_gap(prev_end_byte, c.byte_start);
            if blanks > 0 && (emitted_anything || allow_initial_blank) {
                if let Some(Doc::HardLine) = parts.last() {
                    if let Some(last) = parts.last_mut() {
                        *last = Doc::BlankLine;
                    }
                } else if parts.is_empty() {
                    parts.push(Doc::HardLine);
                }
            }

            if let Some(d_idx) = c.directive_index {
                let dir = &self.parsed.directives[d_idx];
                if dir.is_valid {
                    cfg_stack.apply_at_depth(brace_depth, &dir.actions);
                    if let Some(s) = dir.skip_flag() {
                        flags.skip = s;
                    }
                    if let Some(p) = dir.preserve_flag() {
                        flags.preserve = p;
                    }
                }
                parts.push(Doc::text(dir.normalized_text.clone()));
                parts.push(Doc::HardLine);
                prev_end_byte = c.byte_end;
                emitted_anything = true;

                // If this directive was `--% .off` AND its entire `.off` region lies before `before_byte`,
                // emit the `.off` region verbatim right now.
                if let Some(region) = self
                    .parsed
                    .off_regions
                    .iter()
                    .find(|r| r.off_directive_idx == d_idx && r.body_end <= before_byte)
                    && region.body_start < region.body_end
                {
                    let raw_slice = &self.parsed.original_src[region.body_start..region.body_end];
                    let trimmed_nl = raw_slice.strip_suffix('\n').unwrap_or(raw_slice);
                    let trimmed_nl = trimmed_nl.strip_suffix('\r').unwrap_or(trimmed_nl);
                    parts.push(Doc::dedent_to_zero(Doc::verbatim(trimmed_nl.to_string())));
                    parts.push(Doc::HardLine);
                    self.advance_past_verbatim(region.body_end, brace_depth, cfg_stack);
                    prev_end_byte = region.body_end;
                }
                continue;
            }

            let cfg = cfg_stack.current();
            match c.kind {
                CommentKind::Line => {
                    let text = format_line_comment(self.parsed, c, cfg.normalize_comments);
                    parts.push(Doc::text(text));
                    parts.push(Doc::HardLine);
                }
                CommentKind::Block => {
                    parts.push(format_block_comment(
                        self.parsed,
                        c,
                        indent_spaces,
                        cfg.tab_spaces,
                    ));
                    if c.has_code_after_on_line && c.byte_end <= before_byte {
                        parts.push(Doc::space());
                    } else {
                        parts.push(Doc::HardLine);
                    }
                }
            }
            prev_end_byte = c.byte_end;
            emitted_anything = true;
        }

        (Doc::concat(parts), prev_end_byte, flags)
    }

    /// Consume inline comments inside an expression/parameter/arm gap `[_from_byte .. before_byte)`.
    pub fn take_inline_between(
        &mut self,
        _from_byte: usize,
        before_byte: usize,
        indent_spaces: usize,
        cfg: &FormatConfig,
    ) -> Doc {
        let mut parts = Vec::new();
        while let Some((_, c)) = self.peek() {
            if c.byte_start >= before_byte {
                break;
            }
            self.next_idx += 1;
            match c.kind {
                CommentKind::Line => {
                    let text = format_line_comment(self.parsed, c, cfg.normalize_comments);
                    parts.push(Doc::space());
                    parts.push(Doc::text(text));
                    parts.push(Doc::HardLine);
                }
                CommentKind::Block => {
                    parts.push(Doc::space());
                    parts.push(format_block_comment(
                        self.parsed,
                        c,
                        indent_spaces,
                        cfg.tab_spaces,
                    ));
                    if c.has_code_after_on_line {
                        parts.push(Doc::space());
                    } else {
                        parts.push(Doc::Line);
                    }
                }
            }
        }
        Doc::concat(parts)
    }
}
