//! Line-based Myers diff computation and `rustfmt`-style 3-line unified hunk formatting for `--check`.

use std::fmt::Write as _;

const CONTEXT_LINES: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine<'a> {
    Context(&'a str),
    Expected(&'a str),  // `+` (formatted)
    Resulting(&'a str), // `-` (actual on disk / input)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch<'a> {
    /// 1-based starting line number in the original (unformatted) text.
    pub line_number_orig: usize,
    /// 1-based starting line number in the formatted text.
    pub line_number_fmt: usize,
    pub lines: Vec<DiffLine<'a>>,
}

/// Compute diff hunks between `orig` (current input) and `formatted` (expected output).
/// Returns an empty `Vec` when `orig == formatted`.
pub fn make_diff<'a>(orig: &'a str, formatted: &'a str, context_size: usize) -> Vec<Mismatch<'a>> {
    if orig == formatted {
        return Vec::new();
    }

    let orig_lines = split_lines_preserving_cr_marker(orig);
    let fmt_lines = split_lines_preserving_cr_marker(formatted);

    let mut prefix = 0usize;
    let min_len = orig_lines.len().min(fmt_lines.len());
    while prefix < min_len && orig_lines[prefix] == fmt_lines[prefix] {
        prefix += 1;
    }

    let mut orig_end = orig_lines.len();
    let mut fmt_end = fmt_lines.len();
    while orig_end > prefix
        && fmt_end > prefix
        && orig_lines[orig_end - 1] == fmt_lines[fmt_end - 1]
    {
        orig_end -= 1;
        fmt_end -= 1;
    }

    let ops = diff_slice(&orig_lines[prefix..orig_end], &fmt_lines[prefix..fmt_end]);

    let ctx_before_start = prefix.saturating_sub(context_size);
    let ctx_after_end = (orig_end + context_size).min(orig_lines.len());

    let mut full_ops: Vec<EditOp<'a>> =
        Vec::with_capacity((prefix - ctx_before_start) + ops.len() + (ctx_after_end - orig_end));
    for &line in &orig_lines[ctx_before_start..prefix] {
        full_ops.push(EditOp::Equal(line));
    }
    full_ops.extend(ops);
    for &line in &orig_lines[orig_end..ctx_after_end] {
        full_ops.push(EditOp::Equal(line));
    }

    build_mismatches(
        &full_ops,
        ctx_before_start + 1,
        ctx_before_start + 1,
        context_size,
    )
}

/// Render `rustfmt`-style `--check` diff output for a single file.
pub fn render_check_diff(
    display_path: &str,
    orig: &str,
    formatted: &str,
    use_color: bool,
) -> String {
    let mismatches = make_diff(orig, formatted, CONTEXT_LINES);
    let mut out = String::new();

    for m in mismatches {
        if use_color {
            let _ = writeln!(
                out,
                "\x1b[1mDiff in {} at line {}:\x1b[0m",
                display_path, m.line_number_orig
            );
        } else {
            let _ = writeln!(
                out,
                "Diff in {} at line {}:",
                display_path, m.line_number_orig
            );
        }

        for line in m.lines {
            match line {
                DiffLine::Context(s) => {
                    let vis = format_visible_cr(s);
                    let _ = writeln!(out, " {vis}");
                }
                DiffLine::Resulting(s) => {
                    let vis = format_visible_cr(s);
                    if use_color {
                        let _ = writeln!(out, "\x1b[31m-{vis}\x1b[0m");
                    } else {
                        let _ = writeln!(out, "-{vis}");
                    }
                }
                DiffLine::Expected(s) => {
                    let vis = format_visible_cr(s);
                    if use_color {
                        let _ = writeln!(out, "\x1b[32m+{vis}\x1b[0m");
                    } else {
                        let _ = writeln!(out, "+{vis}");
                    }
                }
            }
        }
    }

    out
}

fn format_visible_cr(line: &str) -> String {
    if let Some(stripped) = line.strip_suffix('\r') {
        format!("{stripped}\u{240d}")
    } else {
        line.to_string()
    }
}

fn split_lines_preserving_cr_marker(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditOp<'a> {
    Equal(&'a str),
    Delete(&'a str),
    Insert(&'a str),
}

/// Myers O(ND) shortest-edit-script over line slices, with safe fallback for huge diffs.
fn diff_slice<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<EditOp<'a>> {
    if a.is_empty() {
        return b.iter().map(|&s| EditOp::Insert(s)).collect();
    }
    if b.is_empty() {
        return a.iter().map(|&s| EditOp::Delete(s)).collect();
    }

    let n = a.len();
    let m = b.len();
    let max_d = n + m;

    if max_d > 8192 {
        let mut fallback = Vec::with_capacity(n + m);
        for &s in a {
            fallback.push(EditOp::Delete(s));
        }
        for &s in b {
            fallback.push(EditOp::Insert(s));
        }
        return fallback;
    }

    let offset = max_d as isize;
    let v_len = 2 * max_d + 1;
    let mut v = vec![0usize; v_len];
    let mut trace: Vec<Vec<usize>> = Vec::new();

    'outer: for d in 0..=max_d {
        trace.push(v.clone());
        let d_isize = d as isize;
        let mut k = -d_isize;
        while k <= d_isize {
            let k_idx = (k + offset) as usize;
            let mut x = if k == -d_isize
                || (k != d_isize && v[(k - 1 + offset) as usize] < v[(k + 1 + offset) as usize])
            {
                v[(k + 1 + offset) as usize]
            } else {
                v[(k - 1 + offset) as usize] + 1
            };
            let mut y = (x as isize - k) as usize;
            while x < n && y < m && a[x] == b[y] {
                x += 1;
                y += 1;
            }
            v[k_idx] = x;
            if x >= n && y >= m {
                break 'outer;
            }
            k += 2;
        }
    }

    let mut ops = Vec::with_capacity(n + m);
    let mut x = n;
    let mut y = m;
    for d in (1..trace.len()).rev() {
        let prev_v = &trace[d];
        let d_isize = d as isize;
        let k = x as isize - y as isize;
        let prev_k = if k == -d_isize
            || (k != d_isize
                && prev_v[(k - 1 + offset) as usize] < prev_v[(k + 1 + offset) as usize])
        {
            k + 1
        } else {
            k - 1
        };
        let prev_x = prev_v[(prev_k + offset) as usize];
        let prev_y = (prev_x as isize - prev_k) as usize;

        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            ops.push(EditOp::Equal(a[x]));
        }
        if x == prev_x {
            y -= 1;
            ops.push(EditOp::Insert(b[y]));
        } else {
            x -= 1;
            ops.push(EditOp::Delete(a[x]));
        }
    }
    while x > 0 && y > 0 {
        x -= 1;
        y -= 1;
        ops.push(EditOp::Equal(a[x]));
    }
    while x > 0 {
        x -= 1;
        ops.push(EditOp::Delete(a[x]));
    }
    while y > 0 {
        y -= 1;
        ops.push(EditOp::Insert(b[y]));
    }

    ops.reverse();
    ops
}

fn build_mismatches<'a>(
    ops: &[EditOp<'a>],
    start_orig_line: usize,
    start_fmt_line: usize,
    context_size: usize,
) -> Vec<Mismatch<'a>> {
    let change_indices: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter_map(|(idx, op)| (!matches!(op, EditOp::Equal(_))).then_some(idx))
        .collect();
    if change_indices.is_empty() {
        return Vec::new();
    }

    let mut groups: Vec<(usize, usize)> = Vec::new();
    let mut cur_first = change_indices[0];
    let mut cur_last = change_indices[0];
    for &idx in &change_indices[1..] {
        let eq_between = idx - cur_last - 1;
        if eq_between <= 2 * context_size {
            cur_last = idx;
        } else {
            groups.push((cur_first, cur_last));
            cur_first = idx;
            cur_last = idx;
        }
    }
    groups.push((cur_first, cur_last));

    let mut line_at = Vec::with_capacity(ops.len() + 1);
    let mut o_line = start_orig_line;
    let mut f_line = start_fmt_line;
    for op in ops {
        line_at.push((o_line, f_line));
        match op {
            EditOp::Equal(_) => {
                o_line += 1;
                f_line += 1;
            }
            EditOp::Delete(_) => {
                o_line += 1;
            }
            EditOp::Insert(_) => {
                f_line += 1;
            }
        }
    }

    let mut mismatches = Vec::with_capacity(groups.len());
    for (first_chg, last_chg) in groups {
        let win_start = first_chg.saturating_sub(context_size);
        let win_end = (last_chg + 1 + context_size).min(ops.len());
        let (hunk_orig_line, hunk_fmt_line) = line_at[win_start];

        let mut lines = Vec::with_capacity(win_end - win_start);
        for op in &ops[win_start..win_end] {
            match op {
                EditOp::Equal(s) => lines.push(DiffLine::Context(s)),
                EditOp::Delete(s) => lines.push(DiffLine::Resulting(s)),
                EditOp::Insert(s) => lines.push(DiffLine::Expected(s)),
            }
        }
        mismatches.push(Mismatch {
            line_number_orig: hunk_orig_line,
            line_number_fmt: hunk_fmt_line,
            lines,
        });
    }

    mismatches
}
