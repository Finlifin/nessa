//! Lua 5.4 byte/UTF-8 pattern matching (`__pat_find`, `__pat_gsub`) and radix/UTF-8 string helpers.

use interpreter::{BuiltinCtx, NativeValue, VmError};
use runtime::Number;

const MAX_CAPTURES: usize = 32;
const MAX_MATCH_DEPTH: usize = 200;

/// `580` `__pat_find(s: String, pattern: String, init: i64, plain: bool) -> (i64, i64, List, String)`
pub fn pat_find(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(4)?;
    let s = ctx.arg_string(0)?;
    let pattern = ctx.arg_string(1)?;
    let init = ctx.arg_i64(2)?;
    let plain = ctx.arg_bool(3)?;

    let len = s.len() as i64;
    let start_i64 = if init < 0 { (len + init).max(0) } else { init };

    if plain {
        if start_i64 > len {
            return return_find_tuple(ctx, -1, -1, Vec::new(), "");
        }
        let start = start_i64 as usize;
        let hay = &s.as_bytes()[start..];
        let needle = pattern.as_bytes();
        if needle.is_empty() {
            return return_find_tuple(ctx, start as i64, start as i64, Vec::new(), "");
        }
        if let Some(rel) = find_subslice(hay, needle) {
            let m_start = start + rel;
            let m_end = m_start + needle.len();
            return return_find_tuple(ctx, m_start as i64, m_end as i64, Vec::new(), "");
        }
        return return_find_tuple(ctx, -1, -1, Vec::new(), "");
    }

    // Validate pattern syntax even when `init > s.len()`.
    let compiled = match compile_pattern(pattern.as_bytes()) {
        Ok(c) => c,
        Err(err_msg) => return return_find_tuple(ctx, -1, -1, Vec::new(), &err_msg),
    };

    if start_i64 > len {
        return return_find_tuple(ctx, -1, -1, Vec::new(), "");
    }

    let subject = s.as_bytes();
    let start = start_i64 as usize;
    let mut pos = start;
    loop {
        match run_match(&compiled, subject, pos) {
            Ok(Some((end_pos, caps))) => {
                let mut cap_values = Vec::with_capacity(caps.len());
                for cap in caps {
                    match cap {
                        CaptureResult::Position(p) => cap_values.push(NativeValue::I64(p as i64)),
                        CaptureResult::Slice(a, b) => {
                            let text = bytes_to_utf8_lossy(&subject[a..b]);
                            cap_values.push(NativeValue::Str(text));
                        }
                    }
                }
                return return_find_tuple(ctx, pos as i64, end_pos as i64, cap_values, "");
            }
            Ok(None) => {}
            Err(err_msg) => return return_find_tuple(ctx, -1, -1, Vec::new(), &err_msg),
        }
        if compiled.anchored || pos >= subject.len() {
            break;
        }
        pos += 1;
    }

    return_find_tuple(ctx, -1, -1, Vec::new(), "")
}

fn return_find_tuple(
    ctx: &mut BuiltinCtx<'_>,
    start: i64,
    end: i64,
    captures: Vec<NativeValue>,
    err: &str,
) -> Result<(), VmError> {
    ctx.return_native(NativeValue::Tuple(vec![
        NativeValue::I64(start),
        NativeValue::I64(end),
        NativeValue::List(captures),
        NativeValue::Str(err.to_string()),
    ]))
}

/// `581` `__pat_gsub(s: String, pattern: String, repl: String, max_n: i64) -> (String, i64, String)`
pub fn pat_gsub(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(4)?;
    let s = ctx.arg_string(0)?;
    let pattern = ctx.arg_string(1)?;
    let repl = ctx.arg_string(2)?;
    let max_n = ctx.arg_i64(3)?;

    let compiled = match compile_pattern(pattern.as_bytes()) {
        Ok(c) => c,
        Err(err_msg) => return return_gsub_tuple(ctx, "", 0, &err_msg),
    };

    let limit = if max_n < 0 {
        None
    } else {
        Some(usize::try_from(max_n).unwrap_or(usize::MAX))
    };

    let subject = s.as_bytes();
    let repl_bytes = repl.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(subject.len());
    let mut pos = 0usize;
    let mut last_match_end: Option<usize> = None;
    let mut replacements: i64 = 0;

    while limit.is_none_or(|lim| (replacements as usize) < lim) && pos <= subject.len() {
        let matched = match run_match(&compiled, subject, pos) {
            Ok(res) => res,
            Err(err_msg) => return return_gsub_tuple(ctx, "", 0, &err_msg),
        };

        if let Some((end_pos, caps)) = matched
            && Some(end_pos) != last_match_end
        {
            replacements += 1;
            let repl_match = ReplacementMatch {
                subject,
                range: (pos, end_pos),
                captures: &caps,
                total_captures: compiled.capture_count,
            };
            if let Err(err_msg) = expand_replacement(ctx, &mut out, repl_bytes, &repl_match)? {
                return return_gsub_tuple(ctx, "", 0, &err_msg);
            }
            pos = end_pos;
            last_match_end = Some(end_pos);
            if compiled.anchored {
                break;
            }
            continue;
        }

        if pos < subject.len() {
            // Advance by full UTF-8 scalar when aligned on a char boundary so
            // 0-length matches never split multibyte UTF-8 characters.
            let step = if s.is_char_boundary(pos) {
                s[pos..].chars().next().map_or(1, char::len_utf8)
            } else {
                1
            };
            let next_len = out
                .len()
                .checked_add(step)
                .ok_or(VmError::DisplaySizeExceeded)?;
            ctx.check_string_size(next_len)?;
            out.extend_from_slice(&subject[pos..pos + step]);
            pos += step;
        } else {
            break;
        }

        if compiled.anchored {
            break;
        }
    }

    if pos < subject.len() {
        let tail = &subject[pos..];
        let next_len = out
            .len()
            .checked_add(tail.len())
            .ok_or(VmError::DisplaySizeExceeded)?;
        ctx.check_string_size(next_len)?;
        out.extend_from_slice(tail);
    }

    let rendered = bytes_to_utf8_lossy(&out);
    ctx.check_string_size(rendered.len())?;
    return_gsub_tuple(ctx, &rendered, replacements, "")
}

fn return_gsub_tuple(
    ctx: &mut BuiltinCtx<'_>,
    result: &str,
    count: i64,
    err: &str,
) -> Result<(), VmError> {
    ctx.return_native(NativeValue::Tuple(vec![
        NativeValue::Str(result.to_string()),
        NativeValue::I64(count),
        NativeValue::Str(err.to_string()),
    ]))
}

struct ReplacementMatch<'a> {
    subject: &'a [u8],
    range: (usize, usize),
    captures: &'a [CaptureResult],
    total_captures: usize,
}

fn expand_replacement(
    ctx: &BuiltinCtx<'_>,
    out: &mut Vec<u8>,
    repl: &[u8],
    matched: &ReplacementMatch<'_>,
) -> Result<Result<(), String>, VmError> {
    let (m_start, m_end) = matched.range;
    let subject = matched.subject;
    let caps = matched.captures;
    let total_captures = matched.total_captures;
    let mut i = 0usize;
    while i < repl.len() {
        if repl[i] != b'%' {
            let start = i;
            i += 1;
            while i < repl.len() && repl[i] != b'%' {
                i += 1;
            }
            let slice = &repl[start..i];
            let next_len = out
                .len()
                .checked_add(slice.len())
                .ok_or(VmError::DisplaySizeExceeded)?;
            ctx.check_string_size(next_len)?;
            out.extend_from_slice(slice);
            continue;
        }
        i += 1;
        if i >= repl.len() {
            return Ok(Err("invalid use of '%' in replacement string".to_string()));
        }
        let esc = repl[i];
        i += 1;
        if esc == b'%' {
            let next_len = out
                .len()
                .checked_add(1)
                .ok_or(VmError::DisplaySizeExceeded)?;
            ctx.check_string_size(next_len)?;
            out.push(b'%');
        } else if esc == b'0' {
            let whole = &subject[m_start..m_end];
            let next_len = out
                .len()
                .checked_add(whole.len())
                .ok_or(VmError::DisplaySizeExceeded)?;
            ctx.check_string_size(next_len)?;
            out.extend_from_slice(whole);
        } else if esc.is_ascii_digit() {
            let idx = usize::from(esc - b'1');
            if total_captures == 0 && idx == 0 {
                let whole = &subject[m_start..m_end];
                let next_len = out
                    .len()
                    .checked_add(whole.len())
                    .ok_or(VmError::DisplaySizeExceeded)?;
                ctx.check_string_size(next_len)?;
                out.extend_from_slice(whole);
            } else if let Some(cap) = caps.get(idx) {
                match *cap {
                    CaptureResult::Position(p) => {
                        let s = p.to_string();
                        let next_len = out
                            .len()
                            .checked_add(s.len())
                            .ok_or(VmError::DisplaySizeExceeded)?;
                        ctx.check_string_size(next_len)?;
                        out.extend_from_slice(s.as_bytes());
                    }
                    CaptureResult::Slice(a, b) => {
                        let piece = &subject[a..b];
                        let next_len = out
                            .len()
                            .checked_add(piece.len())
                            .ok_or(VmError::DisplaySizeExceeded)?;
                        ctx.check_string_size(next_len)?;
                        out.extend_from_slice(piece);
                    }
                }
            } else {
                return Ok(Err(format!("invalid capture index %{}", esc as char)));
            }
        } else {
            return Ok(Err("invalid use of '%' in replacement string".to_string()));
        }
    }
    Ok(Ok(()))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn bytes_to_utf8_lossy(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

// ---------------------------------------------------------------------------
// Lua 5.4 Pattern Compiler & Backtracking Matcher
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct CompiledPattern {
    anchored: bool,
    items: Vec<PatItem>,
    capture_count: usize,
}

#[derive(Clone)]
enum PatItem {
    Atom(ByteMatcher, Quantifier),
    EndAnchor,
    CaptureStart { slot: usize, is_position: bool },
    CaptureEnd { slot: usize },
    Balanced(u8, u8),
    Frontier(Box<[bool; 256]>),
    Backref(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quantifier {
    Once,
    GreedyZeroOrMore, // *
    GreedyOneOrMore,  // +
    LazyZeroOrMore,   // -
    Optional,         // ?
}

#[derive(Clone)]
enum ByteMatcher {
    Any,
    Literal(u8),
    Class(u8),
    Set(Box<[bool; 256]>),
}

impl ByteMatcher {
    fn matches(&self, b: u8) -> bool {
        match self {
            Self::Any => true,
            Self::Literal(expected) => b == *expected,
            Self::Class(cls) => match_lua_class(*cls, b),
            Self::Set(table) => table[usize::from(b)],
        }
    }
}

fn is_class_letter(c: u8) -> bool {
    matches!(
        c.to_ascii_lowercase(),
        b'a' | b'c' | b'd' | b'g' | b'l' | b'p' | b's' | b'u' | b'w' | b'x' | b'z'
    )
}

fn match_lua_class(cls: u8, b: u8) -> bool {
    let base = match cls.to_ascii_lowercase() {
        b'a' => b.is_ascii_alphabetic(),
        b'c' => b.is_ascii_control(),
        b'd' => b.is_ascii_digit(),
        b'g' => b.is_ascii_graphic(),
        b'l' => b.is_ascii_lowercase(),
        b'p' => b.is_ascii_punctuation(),
        b's' => matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c),
        b'u' => b.is_ascii_uppercase(),
        b'w' => b.is_ascii_alphanumeric(),
        b'x' => b.is_ascii_hexdigit(),
        b'z' => b == 0,
        _ => b == cls,
    };
    if cls.is_ascii_uppercase() {
        !base
    } else {
        base
    }
}

fn compile_pattern(pat: &[u8]) -> Result<CompiledPattern, String> {
    let mut i = 0usize;
    let anchored = if pat.first() == Some(&b'^') {
        i = 1;
        true
    } else {
        false
    };

    let mut items = Vec::new();
    let mut open_stack: Vec<usize> = Vec::new();
    let mut closed = [false; MAX_CAPTURES];
    let mut next_slot = 0usize;

    while i < pat.len() {
        match pat[i] {
            b'$' if i + 1 == pat.len() => {
                items.push(PatItem::EndAnchor);
                i += 1;
            }
            b'(' => {
                if next_slot >= MAX_CAPTURES {
                    return Err("too many captures".to_string());
                }
                let slot = next_slot;
                next_slot += 1;
                if pat.get(i + 1) == Some(&b')') {
                    items.push(PatItem::CaptureStart {
                        slot,
                        is_position: true,
                    });
                    closed[slot] = true;
                    i += 2;
                } else {
                    items.push(PatItem::CaptureStart {
                        slot,
                        is_position: false,
                    });
                    open_stack.push(slot);
                    i += 1;
                }
            }
            b')' => {
                let Some(slot) = open_stack.pop() else {
                    return Err("invalid pattern capture".to_string());
                };
                closed[slot] = true;
                items.push(PatItem::CaptureEnd { slot });
                i += 1;
            }
            b'%' => {
                i += 1;
                if i >= pat.len() {
                    return Err("malformed pattern (ends with '%')".to_string());
                }
                let esc = pat[i];
                match esc {
                    b'b' => {
                        if i + 2 >= pat.len() {
                            return Err("malformed pattern (missing arguments to '%b')".to_string());
                        }
                        items.push(PatItem::Balanced(pat[i + 1], pat[i + 2]));
                        i += 3;
                    }
                    b'B' => {
                        let byte = parse_hex_byte_escape(pat, i + 1)?;
                        i += 3;
                        let quant = parse_quantifier(pat, &mut i);
                        items.push(PatItem::Atom(ByteMatcher::Literal(byte), quant));
                    }
                    b'f' => {
                        i += 1;
                        if pat.get(i) != Some(&b'[') {
                            return Err("missing '[' after '%f' in pattern".to_string());
                        }
                        let (set, next_i) = parse_bracket_set(pat, i)?;
                        items.push(PatItem::Frontier(set));
                        i = next_i;
                    }
                    b'0' => {
                        return Err("invalid capture index %0".to_string());
                    }
                    b'1'..=b'9' => {
                        let idx = usize::from(esc - b'1');
                        if idx >= next_slot || !closed[idx] {
                            return Err(format!("invalid capture index %{}", esc as char));
                        }
                        items.push(PatItem::Backref(idx));
                        i += 1;
                    }
                    c if is_class_letter(c) => {
                        i += 1;
                        let quant = parse_quantifier(pat, &mut i);
                        items.push(PatItem::Atom(ByteMatcher::Class(c), quant));
                    }
                    c if c.is_ascii_alphanumeric() => {
                        return Err(format!("invalid escape '%{}'", c as char));
                    }
                    c => {
                        i += 1;
                        let quant = parse_quantifier(pat, &mut i);
                        items.push(PatItem::Atom(ByteMatcher::Literal(c), quant));
                    }
                }
            }
            b'[' => {
                let (set, next_i) = parse_bracket_set(pat, i)?;
                i = next_i;
                let quant = parse_quantifier(pat, &mut i);
                items.push(PatItem::Atom(ByteMatcher::Set(set), quant));
            }
            b'.' => {
                i += 1;
                let quant = parse_quantifier(pat, &mut i);
                items.push(PatItem::Atom(ByteMatcher::Any, quant));
            }
            lit => {
                i += 1;
                let quant = parse_quantifier(pat, &mut i);
                items.push(PatItem::Atom(ByteMatcher::Literal(lit), quant));
            }
        }
    }

    if !open_stack.is_empty() {
        return Err("unfinished capture".to_string());
    }

    Ok(CompiledPattern {
        anchored,
        items,
        capture_count: next_slot,
    })
}

fn parse_quantifier(pat: &[u8], i: &mut usize) -> Quantifier {
    match pat.get(*i) {
        Some(b'*') => {
            *i += 1;
            Quantifier::GreedyZeroOrMore
        }
        Some(b'+') => {
            *i += 1;
            Quantifier::GreedyOneOrMore
        }
        Some(b'-') => {
            *i += 1;
            Quantifier::LazyZeroOrMore
        }
        Some(b'?') => {
            *i += 1;
            Quantifier::Optional
        }
        _ => Quantifier::Once,
    }
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn parse_hex_byte_escape(pat: &[u8], hex_start: usize) -> Result<u8, String> {
    let (Some(&hi_ch), Some(&lo_ch)) = (pat.get(hex_start), pat.get(hex_start + 1)) else {
        return Err("malformed pattern (expected two hex digits after '%B')".to_string());
    };
    let (Some(hi), Some(lo)) = (hex_nibble(hi_ch), hex_nibble(lo_ch)) else {
        return Err("malformed pattern (expected two hex digits after '%B')".to_string());
    };
    Ok((hi << 4) | lo)
}

enum BracketAtom {
    RangeableByte(u8),
    EscapedLiteral(u8),
    Class(u8),
}

fn read_bracket_atom(pat: &[u8], i: &mut usize) -> Result<BracketAtom, String> {
    if pat[*i] == b'%' {
        *i += 1;
        if *i >= pat.len() {
            return Err("malformed pattern (ends with '%')".to_string());
        }
        let esc = pat[*i];
        if esc == b'B' {
            let byte = parse_hex_byte_escape(pat, *i + 1)?;
            *i += 3;
            Ok(BracketAtom::RangeableByte(byte))
        } else if is_class_letter(esc) {
            *i += 1;
            Ok(BracketAtom::Class(esc))
        } else if esc.is_ascii_alphanumeric() {
            Err(format!("invalid escape '%{}'", esc as char))
        } else {
            *i += 1;
            Ok(BracketAtom::EscapedLiteral(esc))
        }
    } else {
        let b = pat[*i];
        *i += 1;
        Ok(BracketAtom::RangeableByte(b))
    }
}

fn parse_bracket_set(
    pat: &[u8],
    start_bracket: usize,
) -> Result<(Box<[bool; 256]>, usize), String> {
    let mut i = start_bracket + 1;
    let invert = if pat.get(i) == Some(&b'^') {
        i += 1;
        true
    } else {
        false
    };

    let mut table = Box::new([false; 256]);
    // A ']' immediately following '[' or '[^' is part of the set rather than closing it.
    if pat.get(i) == Some(&b']') {
        table[usize::from(b']')] = true;
        i += 1;
    }

    while i < pat.len() && pat[i] != b']' {
        match read_bracket_atom(pat, &mut i)? {
            BracketAtom::Class(esc) => {
                for b in 0u8..=255u8 {
                    if match_lua_class(esc, b) {
                        table[usize::from(b)] = true;
                    }
                }
            }
            BracketAtom::EscapedLiteral(esc) => {
                table[usize::from(esc)] = true;
            }
            BracketAtom::RangeableByte(first) => {
                if pat.get(i) == Some(&b'-') && pat.get(i + 1).is_some_and(|&next| next != b']') {
                    let mut probe = i + 1;
                    if let BracketAtom::RangeableByte(last) = read_bracket_atom(pat, &mut probe)? {
                        i = probe;
                        if first <= last {
                            for b in first..=last {
                                table[usize::from(b)] = true;
                            }
                        }
                        continue;
                    }
                }
                table[usize::from(first)] = true;
            }
        }
    }

    if pat.get(i) != Some(&b']') {
        return Err("malformed pattern (missing ']')".to_string());
    }
    if invert {
        for entry in table.iter_mut() {
            *entry = !*entry;
        }
    }
    Ok((table, i + 1))
}

#[derive(Clone, Copy)]
enum CaptureState {
    Unset,
    Open(usize),
    Closed(usize, usize),
    Position(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureResult {
    Position(usize),
    Slice(usize, usize),
}

struct MatchEnv<'a> {
    subject: &'a [u8],
    items: &'a [PatItem],
    captures: [CaptureState; MAX_CAPTURES],
    capture_count: usize,
}

fn run_match(
    compiled: &CompiledPattern,
    subject: &[u8],
    start_pos: usize,
) -> Result<Option<(usize, Vec<CaptureResult>)>, String> {
    let mut env = MatchEnv {
        subject,
        items: &compiled.items,
        captures: [CaptureState::Unset; MAX_CAPTURES],
        capture_count: compiled.capture_count,
    };
    match match_step(&mut env, 0, start_pos, 0)? {
        Some(end_pos) => {
            let mut results = Vec::with_capacity(env.capture_count);
            for slot in 0..env.capture_count {
                match env.captures[slot] {
                    CaptureState::Position(p) => results.push(CaptureResult::Position(p)),
                    CaptureState::Closed(a, b) => results.push(CaptureResult::Slice(a, b)),
                    CaptureState::Open(_) | CaptureState::Unset => {
                        return Err("unfinished capture".to_string());
                    }
                }
            }
            Ok(Some((end_pos, results)))
        }
        None => Ok(None),
    }
}

fn match_step(
    env: &mut MatchEnv<'_>,
    mut item_idx: usize,
    mut pos: usize,
    depth: usize,
) -> Result<Option<usize>, String> {
    if depth > MAX_MATCH_DEPTH {
        return Err("pattern too complex".to_string());
    }

    loop {
        let Some(item) = env.items.get(item_idx) else {
            return Ok(Some(pos));
        };
        match item {
            PatItem::Atom(matcher, Quantifier::Once) => {
                let Some(&b) = env.subject.get(pos) else {
                    return Ok(None);
                };
                if !matcher.matches(b) {
                    return Ok(None);
                }
                pos += 1;
                item_idx += 1;
            }
            PatItem::Atom(matcher, Quantifier::Optional) => {
                if let Some(&b) = env.subject.get(pos)
                    && matcher.matches(b)
                {
                    let saved = env.captures;
                    if let Some(end) = match_step(env, item_idx + 1, pos + 1, depth + 1)? {
                        return Ok(Some(end));
                    }
                    env.captures = saved;
                }
                item_idx += 1;
            }
            PatItem::Atom(matcher, Quantifier::GreedyZeroOrMore) => {
                return match_greedy(env, matcher, item_idx + 1, pos, depth + 1);
            }
            PatItem::Atom(matcher, Quantifier::GreedyOneOrMore) => {
                let Some(&b) = env.subject.get(pos) else {
                    return Ok(None);
                };
                if !matcher.matches(b) {
                    return Ok(None);
                }
                return match_greedy(env, matcher, item_idx + 1, pos + 1, depth + 1);
            }
            PatItem::Atom(matcher, Quantifier::LazyZeroOrMore) => {
                return match_lazy(env, matcher, item_idx + 1, pos, depth + 1);
            }
            PatItem::EndAnchor => {
                return Ok((pos == env.subject.len()).then_some(pos));
            }
            PatItem::CaptureStart { slot, is_position } => {
                let slot = *slot;
                let prev = env.captures[slot];
                env.captures[slot] = if *is_position {
                    CaptureState::Position(pos)
                } else {
                    CaptureState::Open(pos)
                };
                if let Some(end) = match_step(env, item_idx + 1, pos, depth + 1)? {
                    return Ok(Some(end));
                }
                env.captures[slot] = prev;
                return Ok(None);
            }
            PatItem::CaptureEnd { slot } => {
                let slot = *slot;
                let prev = env.captures[slot];
                let CaptureState::Open(start) = prev else {
                    return Err("invalid pattern capture".to_string());
                };
                env.captures[slot] = CaptureState::Closed(start, pos);
                if let Some(end) = match_step(env, item_idx + 1, pos, depth + 1)? {
                    return Ok(Some(end));
                }
                env.captures[slot] = prev;
                return Ok(None);
            }
            PatItem::Balanced(open_b, close_b) => {
                if env.subject.get(pos) != Some(open_b) {
                    return Ok(None);
                }
                let mut count = 1usize;
                let mut p = pos + 1;
                let mut found = None;
                while p < env.subject.len() {
                    let cur = env.subject[p];
                    if cur == *close_b {
                        count -= 1;
                        if count == 0 {
                            found = Some(p + 1);
                            break;
                        }
                    } else if cur == *open_b {
                        count += 1;
                    }
                    p += 1;
                }
                let Some(next_pos) = found else {
                    return Ok(None);
                };
                pos = next_pos;
                item_idx += 1;
            }
            PatItem::Frontier(set) => {
                let prev_b = if pos == 0 { 0u8 } else { env.subject[pos - 1] };
                let curr_b = env.subject.get(pos).copied().unwrap_or(0u8);
                if set[usize::from(prev_b)] || !set[usize::from(curr_b)] {
                    return Ok(None);
                }
                item_idx += 1;
            }
            PatItem::Backref(slot) => {
                let CaptureState::Closed(a, b) = env.captures[*slot] else {
                    return Err(format!("invalid capture index %{}", slot + 1));
                };
                let len = b - a;
                if pos + len > env.subject.len() {
                    return Ok(None);
                }
                if env.subject[pos..pos + len] != env.subject[a..b] {
                    return Ok(None);
                }
                pos += len;
                item_idx += 1;
            }
        }
    }
}

fn match_greedy(
    env: &mut MatchEnv<'_>,
    matcher: &ByteMatcher,
    next_item: usize,
    pos: usize,
    depth: usize,
) -> Result<Option<usize>, String> {
    let mut count = 0usize;
    while let Some(&b) = env.subject.get(pos + count) {
        if !matcher.matches(b) {
            break;
        }
        count += 1;
    }
    let saved = env.captures;
    loop {
        if let Some(end) = match_step(env, next_item, pos + count, depth)? {
            return Ok(Some(end));
        }
        env.captures = saved;
        if count == 0 {
            return Ok(None);
        }
        count -= 1;
    }
}

fn match_lazy(
    env: &mut MatchEnv<'_>,
    matcher: &ByteMatcher,
    next_item: usize,
    mut pos: usize,
    depth: usize,
) -> Result<Option<usize>, String> {
    let saved = env.captures;
    loop {
        if let Some(end) = match_step(env, next_item, pos, depth)? {
            return Ok(Some(end));
        }
        env.captures = saved;
        let Some(&b) = env.subject.get(pos) else {
            return Ok(None);
        };
        if !matcher.matches(b) {
            return Ok(None);
        }
        pos += 1;
    }
}

// ---------------------------------------------------------------------------
// 585..=587: Radix int parsing, str_from_codes, str_utf8_offset
// ---------------------------------------------------------------------------

/// `585` `__str_parse_int_radix(s: String, base: i64) -> ?i64`
pub fn str_parse_int_radix(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let s = ctx.arg_string(0)?;
    let base = ctx.arg_i64(1)?;
    if base != 0 && !(2..=36).contains(&base) {
        return Err(VmError::InvalidArgument(
            "radix base must be 0 or in 2..=36",
        ));
    }

    let trimmed = s.trim();
    if trimmed.is_empty() {
        ctx.return_null();
        return Ok(());
    }

    let (neg, mut rest) = if let Some(r) = trimmed.strip_prefix('-') {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix('+') {
        (false, r)
    } else {
        (false, trimmed)
    };

    let radix: u32 = if base == 0 {
        if let Some(r) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
            rest = r;
            16
        } else if let Some(r) = rest.strip_prefix("0b").or_else(|| rest.strip_prefix("0B")) {
            rest = r;
            2
        } else if let Some(r) = rest.strip_prefix("0o").or_else(|| rest.strip_prefix("0O")) {
            rest = r;
            8
        } else {
            10
        }
    } else {
        let r_u32 = base as u32;
        if r_u32 == 16
            && let Some(r) = rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X"))
            && !r.is_empty()
        {
            rest = r;
        } else if r_u32 == 2
            && let Some(r) = rest.strip_prefix("0b").or_else(|| rest.strip_prefix("0B"))
            && !r.is_empty()
        {
            rest = r;
        } else if r_u32 == 8
            && let Some(r) = rest.strip_prefix("0o").or_else(|| rest.strip_prefix("0O"))
            && !r.is_empty()
        {
            rest = r;
        }
        r_u32
    };

    if rest.is_empty() {
        ctx.return_null();
        return Ok(());
    }

    let mut acc: i64 = 0;
    for ch in rest.chars() {
        let Some(digit) = ch.to_digit(radix) else {
            ctx.return_null();
            return Ok(());
        };
        let d_i64 = i64::from(digit);
        acc = match acc.checked_mul(i64::from(radix)) {
            Some(prod) => {
                if neg {
                    match prod.checked_sub(d_i64) {
                        Some(v) => v,
                        None => {
                            ctx.return_null();
                            return Ok(());
                        }
                    }
                } else {
                    match prod.checked_add(d_i64) {
                        Some(v) => v,
                        None => {
                            ctx.return_null();
                            return Ok(());
                        }
                    }
                }
            }
            None => {
                ctx.return_null();
                return Ok(());
            }
        };
    }

    ctx.return_i64(acc)
}

/// `586` `str_from_codes(codes: List) -> ?String`
pub fn str_from_codes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let list_root = ctx.arg_rooted(0)?;
    let list_val = ctx.reload_root(&list_root)?;
    let len = ctx.list_len(list_val)?;

    let mut out = String::with_capacity(len);
    let mut out_of_range = false;

    for idx in 0..len {
        let elem_root = ctx.list_get_rooted(&list_root, idx)?;
        let elem = ctx.reload_root(&elem_root)?;
        let num = ctx.number_of(elem)?;
        let code_u32 = match num {
            Number::I64(v) => u32::try_from(v).ok(),
            Number::U64(v) => u32::try_from(v).ok(),
            Number::I128(v) => u32::try_from(v).ok(),
            Number::U128(v) => u32::try_from(v).ok(),
            Number::F64(_) => return Err(VmError::TypeError),
        };
        if !out_of_range {
            if let Some(ch) = code_u32.and_then(char::from_u32) {
                let next_len = out
                    .len()
                    .checked_add(ch.len_utf8())
                    .ok_or(VmError::DisplaySizeExceeded)?;
                ctx.check_string_size(next_len)?;
                out.push(ch);
            } else {
                out_of_range = true;
            }
        }
    }

    if out_of_range {
        ctx.return_null();
        Ok(())
    } else {
        ctx.return_string(&out)
    }
}

/// `587` `str_utf8_offset(s: String, n: i64, init_byte: i64) -> ?i64`
pub fn str_utf8_offset(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let s = ctx.arg_string(0)?;
    let n = ctx.arg_i64(1)?;
    let init_byte = ctx.arg_i64(2)?;

    let len_i64 = s.len() as i64;
    let resolved = if init_byte < 0 {
        len_i64.checked_add(init_byte)
    } else {
        Some(init_byte)
    };
    let Some(pos_i64) = resolved else {
        ctx.return_null();
        return Ok(());
    };
    if pos_i64 < 0 || pos_i64 > len_i64 {
        ctx.return_null();
        return Ok(());
    }
    let mut pos = pos_i64 as usize;

    if n == 0 {
        while pos > 0 && !s.is_char_boundary(pos) {
            pos -= 1;
        }
        return ctx.return_i64(pos as i64);
    }

    if !s.is_char_boundary(pos) {
        ctx.return_null();
        return Ok(());
    }

    if n > 0 {
        let steps = (n - 1) as u64;
        for _ in 0..steps {
            if pos >= s.len() {
                ctx.return_null();
                return Ok(());
            }
            let ch = s[pos..]
                .chars()
                .next()
                .expect("pos < s.len() on char boundary");
            pos += ch.len_utf8();
        }
        ctx.return_i64(pos as i64)
    } else {
        let steps = n.unsigned_abs();
        for _ in 0..steps {
            if pos == 0 {
                ctx.return_null();
                return Ok(());
            }
            pos -= 1;
            while pos > 0 && !s.is_char_boundary(pos) {
                pos -= 1;
            }
        }
        ctx.return_i64(pos as i64)
    }
}
