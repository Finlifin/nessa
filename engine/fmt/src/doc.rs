//! Wadler-Lindig width-aware document intermediate representation and layout printer.

use std::rc::Rc;

pub use crate::config::NewlineStyle;

/// Pretty-printing document IR.
#[derive(Debug, Clone)]
pub enum Doc {
    Nil,
    /// Single-line literal text (must not contain `\n`).
    Text(String),
    /// Multi-line or raw text emitted as-is (`\n` converted to target newline style).
    Verbatim(String),
    /// Space in `Flat` mode, newline + current indent in `Break` mode.
    Line,
    /// Empty in `Flat` mode, newline + current indent in `Break` mode.
    SoftLine,
    /// Unconditional newline + current indent; forces enclosing `Group`s to break.
    HardLine,
    /// Blank line: emits two newlines and schedules current indent for the next non-empty line.
    BlankLine,
    /// Increase indentation by `usize` spaces for line breaks inside the child `Doc`.
    Indent(usize, Rc<Doc>),
    /// Reset indentation to column 0 for line breaks inside the child `Doc` (used for `.off` / `.skip`).
    DedentToZero(Rc<Doc>),
    /// Scoped `max_width` override (used by `--% .max_width N`).
    WithMaxWidth(usize, Rc<Doc>),
    /// Pad with spaces to at least column `target_col` (minimum 1 space), used for aligned trailing comments.
    PadToCol(usize),
    /// Choose `broken` when the nearest enclosing `Group` breaks, otherwise `flat`.
    IfBreak {
        broken: Rc<Doc>,
        flat: Rc<Doc>,
    },
    /// Try rendering `body` in `Flat` mode if it contains no hard line breaks, fits within
    /// `max_width - current_col`, and (when `max_flat_width` is `Some(limit)`) has flat width `<= limit`.
    Group {
        body: Rc<Doc>,
        max_flat_width: Option<usize>,
    },
    /// Sequence of documents.
    Concat(Vec<Doc>),
}

impl Doc {
    pub fn text(s: impl Into<String>) -> Self {
        let s = s.into();
        if s.is_empty() {
            Self::Nil
        } else {
            Self::Text(s)
        }
    }

    pub fn str(s: &str) -> Self {
        if s.is_empty() {
            Self::Nil
        } else {
            Self::Text(s.to_string())
        }
    }

    pub fn space() -> Self {
        Self::Text(" ".to_string())
    }

    pub fn verbatim(s: impl Into<String>) -> Self {
        let s = s.into();
        if s.is_empty() {
            Self::Nil
        } else {
            Self::Verbatim(s)
        }
    }

    pub fn indent(spaces: usize, doc: Doc) -> Self {
        if matches!(doc, Doc::Nil) {
            Doc::Nil
        } else {
            Doc::Indent(spaces, Rc::new(doc))
        }
    }

    pub fn dedent_to_zero(doc: Doc) -> Self {
        if matches!(doc, Doc::Nil) {
            Doc::Nil
        } else {
            Doc::DedentToZero(Rc::new(doc))
        }
    }

    pub fn with_max_width(max_width: usize, doc: Doc) -> Self {
        if matches!(doc, Doc::Nil) {
            Doc::Nil
        } else {
            Doc::WithMaxWidth(max_width, Rc::new(doc))
        }
    }

    pub fn group(doc: Doc) -> Self {
        Doc::Group {
            body: Rc::new(doc),
            max_flat_width: None,
        }
    }

    pub fn group_with_budget(budget: usize, doc: Doc) -> Self {
        Doc::Group {
            body: Rc::new(doc),
            max_flat_width: Some(budget),
        }
    }

    pub fn if_break(broken: Doc, flat: Doc) -> Self {
        Doc::IfBreak {
            broken: Rc::new(broken),
            flat: Rc::new(flat),
        }
    }

    pub fn trailing_comma() -> Self {
        Self::if_break(Doc::str(","), Doc::Nil)
    }

    pub fn concat(items: impl IntoIterator<Item = Doc>) -> Self {
        let mut flat = Vec::new();
        for item in items {
            match item {
                Doc::Nil => {}
                Doc::Concat(sub) => flat.extend(sub),
                other => flat.push(other),
            }
        }
        match flat.len() {
            0 => Doc::Nil,
            1 => flat.pop().unwrap_or(Doc::Nil),
            _ => Doc::Concat(flat),
        }
    }

    pub fn join(docs: impl IntoIterator<Item = Doc>, sep: Doc) -> Self {
        let mut iter = docs.into_iter();
        let Some(first) = iter.next() else {
            return Doc::Nil;
        };
        let mut out = vec![first];
        for d in iter {
            out.push(sep.clone());
            out.push(d);
        }
        Self::concat(out)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Flat,
    Break,
}

/// Render `doc` to a `String` using `max_width` and `newline` (`"\n"` or `"\r\n"`).
pub fn print_doc(doc: &Doc, max_width: usize, newline: &str) -> String {
    let mut out = String::new();
    let mut col = 0usize;
    let mut pending_indent: Option<usize> = None;
    let mut stack: Vec<(usize, usize, Mode, &Doc)> = vec![(0, max_width, Mode::Break, doc)];

    while let Some((indent, cur_max_width, mode, cur)) = stack.pop() {
        match cur {
            Doc::Nil => {}
            Doc::Text(s) => {
                if let Some(ind) = pending_indent.take() {
                    for _ in 0..ind {
                        out.push(' ');
                    }
                    col = ind;
                }
                out.push_str(s);
                col += s.chars().count();
            }
            Doc::Verbatim(raw) => {
                let normalized = raw.replace("\r\n", "\n");
                let segments: Vec<&str> = normalized.split('\n').collect();
                for (idx, seg) in segments.iter().enumerate() {
                    if idx > 0 {
                        out.push_str(newline);
                        col = 0;
                        pending_indent = None;
                    }
                    if !seg.is_empty() {
                        if let Some(ind) = pending_indent.take() {
                            for _ in 0..ind {
                                out.push(' ');
                            }
                            col = ind;
                        }
                        out.push_str(seg);
                        col += seg.chars().count();
                    } else if idx == 0 {
                        pending_indent = None;
                    }
                }
            }
            Doc::PadToCol(target_col) => {
                if let Some(ind) = pending_indent.take() {
                    for _ in 0..ind {
                        out.push(' ');
                    }
                    col = ind;
                }
                let spaces = if *target_col > col {
                    *target_col - col
                } else {
                    1
                };
                for _ in 0..spaces {
                    out.push(' ');
                }
                col += spaces;
            }
            Doc::Line => match mode {
                Mode::Flat => {
                    if let Some(ind) = pending_indent.take() {
                        for _ in 0..ind {
                            out.push(' ');
                        }
                        col = ind;
                    }
                    out.push(' ');
                    col += 1;
                }
                Mode::Break => {
                    trim_trailing_horizontal_ws(&mut out);
                    out.push_str(newline);
                    col = 0;
                    pending_indent = Some(indent);
                }
            },
            Doc::SoftLine => match mode {
                Mode::Flat => {}
                Mode::Break => {
                    trim_trailing_horizontal_ws(&mut out);
                    out.push_str(newline);
                    col = 0;
                    pending_indent = Some(indent);
                }
            },
            Doc::HardLine => {
                trim_trailing_horizontal_ws(&mut out);
                out.push_str(newline);
                col = 0;
                pending_indent = Some(indent);
            }
            Doc::BlankLine => {
                trim_trailing_horizontal_ws(&mut out);
                out.push_str(newline);
                out.push_str(newline);
                col = 0;
                pending_indent = Some(indent);
            }
            Doc::Indent(extra, inner) => {
                stack.push((indent + extra, cur_max_width, mode, inner));
            }
            Doc::DedentToZero(inner) => {
                if pending_indent.is_some() {
                    pending_indent = Some(0);
                }
                stack.push((0, cur_max_width, mode, inner));
            }
            Doc::WithMaxWidth(mw, inner) => {
                stack.push((indent, *mw, mode, inner));
            }
            Doc::IfBreak { broken, flat } => match mode {
                Mode::Break => stack.push((indent, cur_max_width, mode, broken)),
                Mode::Flat => stack.push((indent, cur_max_width, mode, flat)),
            },
            Doc::Group {
                body,
                max_flat_width,
            } => {
                let effective_col = pending_indent.unwrap_or(col);
                let rem_line = cur_max_width.saturating_sub(effective_col);
                let fits = if let Some(budget) = max_flat_width {
                    if *budget == 0 {
                        flat_width_only(body, Some(0)).is_some()
                    } else {
                        flat_width_only(body, Some(*budget)).is_some()
                            && fits(
                                rem_line as isize,
                                effective_col,
                                &[(indent, cur_max_width, Mode::Flat, body.as_ref())],
                                &stack,
                            )
                    }
                } else {
                    fits(
                        rem_line as isize,
                        effective_col,
                        &[(indent, cur_max_width, Mode::Flat, body.as_ref())],
                        &stack,
                    )
                };
                let chosen = if fits { Mode::Flat } else { Mode::Break };
                stack.push((indent, cur_max_width, chosen, body));
            }
            Doc::Concat(items) => {
                for item in items.iter().rev() {
                    stack.push((indent, cur_max_width, mode, item));
                }
            }
        }
    }

    trim_trailing_horizontal_ws(&mut out);
    if !out.is_empty() && !out.ends_with(newline) {
        out.push_str(newline);
    }
    out
}

fn trim_trailing_horizontal_ws(buf: &mut String) {
    while buf.ends_with(' ') || buf.ends_with('\t') {
        buf.pop();
    }
}

/// Compute flat width of `doc` alone if `<= limit` and free of `HardLine` / `BlankLine` / multi-line `Verbatim`.
fn flat_width_only(doc: &Doc, limit: Option<usize>) -> Option<usize> {
    let mut total = 0usize;
    let mut stack = vec![doc];
    while let Some(cur) = stack.pop() {
        match cur {
            Doc::Nil | Doc::SoftLine => {}
            Doc::Text(s) => {
                total += s.chars().count();
            }
            Doc::Verbatim(s) => {
                if s.contains('\n') {
                    return None;
                }
                total += s.chars().count();
            }
            Doc::PadToCol(_) | Doc::Line => {
                total += 1;
            }
            Doc::HardLine | Doc::BlankLine => return None,
            Doc::Indent(_, inner)
            | Doc::DedentToZero(inner)
            | Doc::WithMaxWidth(_, inner)
            | Doc::Group { body: inner, .. } => {
                stack.push(inner);
            }
            Doc::IfBreak { flat, .. } => {
                stack.push(flat);
            }
            Doc::Concat(items) => {
                for item in items.iter().rev() {
                    stack.push(item);
                }
            }
        }
        if let Some(lim) = limit
            && total > lim
        {
            return None;
        }
    }
    Some(total)
}

/// Wadler-Lindig lookahead fit check up to the next line break or width overflow.
pub(crate) fn fits(
    mut rem: isize,
    mut col: usize,
    initial: &[(usize, usize, Mode, &Doc)],
    rest_stack: &[(usize, usize, Mode, &Doc)],
) -> bool {
    if rem < 0 {
        return false;
    }
    let mut work: Vec<(usize, usize, Mode, &Doc)> = Vec::with_capacity(initial.len() + 8);
    work.extend_from_slice(initial);
    let mut rest_idx = rest_stack.len();

    loop {
        let next_item = if let Some(item) = work.pop() {
            item
        } else if rest_idx > 0 {
            rest_idx -= 1;
            rest_stack[rest_idx]
        } else {
            return true;
        };

        let (indent, mw, mode, cur) = next_item;
        match cur {
            Doc::Nil => {}
            Doc::Text(s) => {
                let w = s.chars().count();
                rem -= w as isize;
                col += w;
                if rem < 0 {
                    return false;
                }
            }
            Doc::Verbatim(s) => {
                if let Some(nl_pos) = s.find('\n') {
                    if mode == Mode::Flat {
                        return false;
                    }
                    let first_seg = s[..nl_pos].trim_end_matches('\r');
                    let w = first_seg.chars().count();
                    return rem - (w as isize) >= 0;
                }
                let w = s.chars().count();
                rem -= w as isize;
                col += w;
                if rem < 0 {
                    return false;
                }
            }
            Doc::PadToCol(target) => {
                let w = if *target > col { *target - col } else { 1 };
                rem -= w as isize;
                col += w;
                if rem < 0 {
                    return false;
                }
            }
            Doc::Line => match mode {
                Mode::Flat => {
                    rem -= 1;
                    col += 1;
                    if rem < 0 {
                        return false;
                    }
                }
                Mode::Break => return true,
            },
            Doc::SoftLine => match mode {
                Mode::Flat => {}
                Mode::Break => return true,
            },
            Doc::HardLine | Doc::BlankLine => {
                return mode == Mode::Break;
            }
            Doc::Indent(extra, inner) => {
                work.push((indent + extra, mw, mode, inner));
            }
            Doc::DedentToZero(inner) => {
                work.push((0, mw, mode, inner));
            }
            Doc::WithMaxWidth(inner_mw, inner) => {
                rem = (*inner_mw as isize) - (col as isize);
                if rem < 0 {
                    return false;
                }
                work.push((indent, *inner_mw, mode, inner));
            }
            Doc::IfBreak { broken, flat } => match mode {
                Mode::Break => work.push((indent, mw, mode, broken)),
                Mode::Flat => work.push((indent, mw, mode, flat)),
            },
            Doc::Group {
                body,
                max_flat_width,
            } => {
                if let Some(budget) = max_flat_width
                    && flat_width_only(body, Some(*budget)).is_none()
                {
                    work.push((indent, mw, Mode::Break, body));
                    continue;
                }
                work.push((indent, mw, Mode::Flat, body));
            }
            Doc::Concat(items) => {
                for item in items.iter().rev() {
                    work.push((indent, mw, mode, item));
                }
            }
        }
    }
}
