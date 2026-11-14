//! String builtins.

use interpreter::{BuiltinCtx, VmError};
use runtime::{Number, TaggedValue};

pub fn str_len(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let len = ctx.arg_string(0)?.len() as i64;
    ctx.return_i64(len)?;
    Ok(())
}

pub fn str_concat(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let mut s = ctx.arg_string(0)?;
    let right = ctx.arg_string(1)?;
    let bytes = s
        .len()
        .checked_add(right.len())
        .ok_or(VmError::DisplaySizeExceeded)?;
    ctx.check_display_size(bytes)?;
    s.push_str(&right);
    ctx.return_string(&s)
}

fn string_predicate(
    ctx: &mut BuiltinCtx<'_>,
    predicate: fn(&str, &str) -> bool,
) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let pattern = ctx.arg_string(1)?;
    ctx.return_bool(predicate(&text, &pattern));
    Ok(())
}

fn string_transform(
    ctx: &mut BuiltinCtx<'_>,
    transform: fn(&str) -> String,
) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let text = ctx.arg_string(0)?;
    ctx.return_string(&transform(&text))
}

pub fn str_contains(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    string_predicate(ctx, |text, pattern| text.contains(pattern))
}

pub fn str_starts_with(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    string_predicate(ctx, |text, pattern| text.starts_with(pattern))
}

pub fn str_ends_with(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    string_predicate(ctx, |text, pattern| text.ends_with(pattern))
}

/// Trimming removes Unicode White_Space, matching Rust's `str::trim`.
pub fn str_trim(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    string_transform(ctx, |text| text.trim().to_owned())
}

pub fn str_trim_start(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    string_transform(ctx, |text| text.trim_start().to_owned())
}

pub fn str_trim_end(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    string_transform(ctx, |text| text.trim_end().to_owned())
}

pub fn str_trim_ascii(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    string_transform(ctx, |text| text.trim_ascii().to_owned())
}

pub fn str_trim_ascii_start(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    string_transform(ctx, |text| text.trim_ascii_start().to_owned())
}

pub fn str_trim_ascii_end(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    string_transform(ctx, |text| text.trim_ascii_end().to_owned())
}

pub fn str_eq_ignore_ascii_case(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    string_predicate(ctx, str::eq_ignore_ascii_case)
}

pub fn str_split_ascii_whitespace(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let text = ctx.arg_string(0)?;
    let pieces: Vec<&str> = text.split_ascii_whitespace().collect();
    ctx.return_string_list(&pieces)
}

/// Full Unicode case mapping; a result may be longer than its input.
pub fn str_to_upper(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    string_transform(ctx, str::to_uppercase)
}

pub fn str_to_lower(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    string_transform(ctx, str::to_lowercase)
}

/// ASCII letters only. Non-ASCII scalars, including `é`, stay unchanged.
pub fn str_to_ascii_uppercase(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    string_transform(ctx, str::to_ascii_uppercase)
}

pub fn str_to_ascii_lowercase(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    string_transform(ctx, str::to_ascii_lowercase)
}

/// Unicode White_Space, the same set `trim` removes. Empty pieces are omitted,
/// so an empty or whitespace-only string returns an empty list.
pub fn str_split_whitespace(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let text = ctx.arg_string(0)?;
    let pieces: Vec<&str> = text.split_whitespace().collect();
    ctx.return_string_list(&pieces)
}

/// Width is a Unicode scalar count. A string that is already long enough is
/// returned unchanged. The fill is cycled and cut so the result has exactly
/// `width` scalars.
fn plan_pad(text: &str, width: i64, fill: &str) -> Result<Option<(usize, usize, usize)>, VmError> {
    if width < 0 {
        return Err(VmError::InvalidArgument("pad width must not be negative"));
    }
    let width = usize::try_from(width).map_err(|_| VmError::ObjectTooLarge)?;
    let current = text.chars().count();
    if current >= width {
        return Ok(None);
    }
    if fill.is_empty() {
        return Err(VmError::InvalidArgument("pad fill must not be empty"));
    }
    let missing = width - current;
    let fill_scalars = fill.chars().count();
    let full = missing / fill_scalars;
    let remainder = missing % fill_scalars;
    let mut extra_bytes = fill
        .len()
        .checked_mul(full)
        .ok_or(VmError::ObjectTooLarge)?;
    for scalar in fill.chars().take(remainder) {
        extra_bytes = extra_bytes
            .checked_add(scalar.len_utf8())
            .ok_or(VmError::ObjectTooLarge)?;
    }
    let bytes = text
        .len()
        .checked_add(extra_bytes)
        .ok_or(VmError::ObjectTooLarge)?;
    Ok(Some((full, remainder, bytes)))
}

fn apply_pad(text: &str, fill: &str, full: usize, remainder: usize, at_start: bool) -> String {
    let mut extra = fill.repeat(full);
    extra.extend(fill.chars().take(remainder));
    if at_start {
        extra.push_str(text);
        extra
    } else {
        let mut result = String::with_capacity(text.len() + extra.len());
        result.push_str(text);
        result.push_str(&extra);
        result
    }
}

fn str_pad(ctx: &mut BuiltinCtx<'_>, at_start: bool) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let text = ctx.arg_string(0)?;
    let width = ctx.arg_i64(1)?;
    let fill = ctx.arg_string(2)?;
    match plan_pad(&text, width, &fill)? {
        None => ctx.return_string(&text),
        Some((full, remainder, bytes)) => {
            ctx.check_string_size(bytes)?;
            ctx.return_string(&apply_pad(&text, &fill, full, remainder, at_start))
        }
    }
}

pub fn str_pad_start(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    str_pad(ctx, true)
}

pub fn str_pad_end(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    str_pad(ctx, false)
}

pub fn str_repeat(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let count = usize::try_from(ctx.arg_i64(1)?)
        .map_err(|_| VmError::InvalidArgument("repeat count must not be negative"))?;
    let bytes = text
        .len()
        .checked_mul(count)
        .ok_or(VmError::ObjectTooLarge)?;
    ctx.check_string_size(bytes)?;
    ctx.return_string(&text.repeat(count))
}

/// Replace every non-overlapping match; an empty pattern matches at each
/// character boundary, as in Rust's `str::replace`.
pub fn str_replace(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let text = ctx.arg_string(0)?;
    let from = ctx.arg_string(1)?;
    let to = ctx.arg_string(2)?;
    let matches = text.match_indices(from.as_str()).count();
    let bytes = matches
        .checked_mul(to.len())
        .and_then(|inserted| (text.len() - matches * from.len()).checked_add(inserted))
        .ok_or(VmError::ObjectTooLarge)?;
    ctx.check_string_size(bytes)?;
    ctx.return_string(&text.replace(from.as_str(), &to))
}

pub fn str_split(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let separator = ctx.arg_string(1)?;
    if separator.is_empty() {
        return Err(VmError::InvalidArgument(
            "split separator must not be empty",
        ));
    }
    let pieces: Vec<&str> = text.split(separator.as_str()).collect();
    ctx.return_string_list(&pieces)
}

/// An empty pattern matches without removing characters, so the original
/// string is returned instead of looping.
fn str_trim_matches(ctx: &mut BuiltinCtx<'_>, which: TrimMatches) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let pattern = ctx.arg_string(1)?;
    if pattern.is_empty() {
        return ctx.return_string(&text);
    }
    let trimmed = match which {
        TrimMatches::Start => text.trim_start_matches(pattern.as_str()),
        TrimMatches::End => text.trim_end_matches(pattern.as_str()),
        // `trim_matches` cannot take `&str` until `DoubleEndedSearcher` is stable.
        TrimMatches::Both => text
            .trim_end_matches(pattern.as_str())
            .trim_start_matches(pattern.as_str()),
    };
    ctx.return_string(trimmed)
}

enum TrimMatches {
    Start,
    End,
    Both,
}

pub fn str_trim_start_matches(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    str_trim_matches(ctx, TrimMatches::Start)
}

pub fn str_trim_end_matches(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    str_trim_matches(ctx, TrimMatches::End)
}

pub fn str_trim_matches_both(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    str_trim_matches(ctx, TrimMatches::Both)
}

/// Replace the first `count` non-overlapping matches. An empty pattern matches
/// at character boundaries, and a zero count returns the original string.
pub fn str_replacen(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(4)?;
    let text = ctx.arg_string(0)?;
    let from = ctx.arg_string(1)?;
    let to = ctx.arg_string(2)?;
    let count = usize::try_from(ctx.arg_i64(3)?)
        .map_err(|_| VmError::InvalidArgument("replace count must not be negative"))?;
    let matches = text.match_indices(from.as_str()).take(count).count();
    let removed = matches
        .checked_mul(from.len())
        .ok_or(VmError::ObjectTooLarge)?;
    let bytes = matches
        .checked_mul(to.len())
        .and_then(|inserted| (text.len() - removed).checked_add(inserted))
        .ok_or(VmError::ObjectTooLarge)?;
    ctx.check_string_size(bytes)?;
    ctx.return_string(&text.replacen(from.as_str(), &to, count))
}

fn split_limit(ctx: &mut BuiltinCtx<'_>, index: u8) -> Result<usize, VmError> {
    usize::try_from(ctx.arg_i64(index)?)
        .map_err(|_| VmError::InvalidArgument("split limit must not be negative"))
}

fn require_separator(separator: &str) -> Result<(), VmError> {
    if separator.is_empty() {
        Err(VmError::InvalidArgument(
            "split separator must not be empty",
        ))
    } else {
        Ok(())
    }
}

pub fn str_splitn(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let text = ctx.arg_string(0)?;
    let separator = ctx.arg_string(1)?;
    require_separator(&separator)?;
    let limit = split_limit(ctx, 2)?;
    let pieces: Vec<&str> = text.splitn(limit, separator.as_str()).collect();
    ctx.return_string_list(&pieces)
}

pub fn str_rsplit(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let separator = ctx.arg_string(1)?;
    require_separator(&separator)?;
    let pieces: Vec<&str> = text.rsplit(separator.as_str()).collect();
    ctx.return_string_list(&pieces)
}

pub fn str_rsplitn(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let text = ctx.arg_string(0)?;
    let separator = ctx.arg_string(1)?;
    require_separator(&separator)?;
    let limit = split_limit(ctx, 2)?;
    let pieces: Vec<&str> = text.rsplitn(limit, separator.as_str()).collect();
    ctx.return_string_list(&pieces)
}

pub fn str_split_inclusive(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let separator = ctx.arg_string(1)?;
    require_separator(&separator)?;
    let pieces: Vec<&str> = text.split_inclusive(separator.as_str()).collect();
    ctx.return_string_list(&pieces)
}

fn terminated_pieces<'a>(text: &'a str, separator: &str, reverse: bool) -> Vec<&'a str> {
    if reverse {
        text.rsplit_terminator(separator).collect()
    } else {
        text.split_terminator(separator).collect()
    }
}

pub fn str_split_terminator(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let separator = ctx.arg_string(1)?;
    require_separator(&separator)?;
    let pieces = terminated_pieces(&text, &separator, false);
    ctx.return_string_list(&pieces)
}

pub fn str_rsplit_terminator(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let separator = ctx.arg_string(1)?;
    require_separator(&separator)?;
    let pieces = terminated_pieces(&text, &separator, true);
    ctx.return_string_list(&pieces)
}

/// Non-overlapping matches. An empty pattern yields one empty piece at every
/// scalar boundary, so the length is `char_len + 1`.
pub fn str_find_all(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let pattern = ctx.arg_string(1)?;
    let pieces: Vec<&str> = text.matches(pattern.as_str()).collect();
    ctx.return_string_list(&pieces)
}

fn match_indexes(text: &str, pattern: &str, reverse: bool, bytes: bool) -> Vec<i64> {
    let indexes: Vec<usize> = if reverse {
        text.rmatch_indices(pattern)
            .map(|(index, _)| index)
            .collect()
    } else {
        text.match_indices(pattern)
            .map(|(index, _)| index)
            .collect()
    };
    if bytes {
        indexes.into_iter().map(|index| index as i64).collect()
    } else {
        indexes
            .into_iter()
            .map(|index| text[..index].chars().count() as i64)
            .collect()
    }
}

fn return_match_indexes(
    ctx: &mut BuiltinCtx<'_>,
    reverse: bool,
    bytes: bool,
) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let pattern = ctx.arg_string(1)?;
    let indexes = match_indexes(&text, &pattern, reverse, bytes);
    ctx.return_i64_list(&indexes)
}

pub fn str_find_indices(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_match_indexes(ctx, false, false)
}

pub fn str_rfind_indices(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_match_indexes(ctx, true, false)
}

pub fn str_byte_find_indices(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_match_indexes(ctx, false, true)
}

pub fn str_byte_rfind_indices(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    return_match_indexes(ctx, true, true)
}

fn trim_affix(ctx: &mut BuiltinCtx<'_>, prefix: bool) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let affix = ctx.arg_string(1)?;
    let trimmed = if prefix {
        text.strip_prefix(affix.as_str()).unwrap_or(text.as_str())
    } else {
        text.strip_suffix(affix.as_str()).unwrap_or(text.as_str())
    };
    ctx.return_string(trimmed)
}

pub fn str_trim_prefix(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    trim_affix(ctx, true)
}

pub fn str_trim_suffix(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    trim_affix(ctx, false)
}

pub fn str_strip_circumfix(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let text = ctx.arg_string(0)?;
    let prefix = ctx.arg_string(1)?;
    let suffix = ctx.arg_string(2)?;
    match text.strip_circumfix(prefix.as_str(), suffix.as_str()) {
        Some(found) => ctx.return_string(found),
        None => {
            ctx.return_null();
            Ok(())
        }
    }
}

fn escape_string(ctx: &mut BuiltinCtx<'_>, escape: fn(&str) -> String) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_string(&escape(&ctx.arg_string(0)?))
}

pub fn str_escape_debug(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    escape_string(ctx, |text| text.escape_debug().collect())
}

pub fn str_escape_unicode(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    escape_string(ctx, |text| text.escape_unicode().collect())
}

pub fn str_escape_default(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    escape_string(ctx, |text| text.escape_default().collect())
}

pub fn str_chars(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let text = ctx.arg_string(0)?;
    ctx.return_char_list(&text)
}

pub fn str_bytes(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let text = ctx.arg_string(0)?;
    let bytes: Vec<i64> = text.bytes().map(i64::from).collect();
    ctx.return_i64_list(&bytes)
}

pub fn str_char_indices(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let text = ctx.arg_string(0)?;
    let offsets: Vec<i64> = text
        .char_indices()
        .map(|(offset, _)| i64::try_from(offset).map_err(|_| VmError::NumericOverflow))
        .collect::<Result<Vec<_>, _>>()?;
    ctx.return_i64_list(&offsets)
}

pub fn str_is_empty(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_bool(ctx.arg_string(0)?.is_empty());
    Ok(())
}

pub fn str_repeat_char(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let count = usize::try_from(ctx.arg_i64(0)?)
        .map_err(|_| VmError::InvalidArgument("repeat count must not be negative"))?;
    let ch = ctx.arg_char(1)?;
    let bytes = ch
        .len_utf8()
        .checked_mul(count)
        .ok_or(VmError::ObjectTooLarge)?;
    ctx.check_string_size(bytes)?;
    ctx.return_string(&ch.to_string().repeat(count))
}

fn take_or_drop_chars(ctx: &mut BuiltinCtx<'_>, keep_prefix: bool) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let count = usize::try_from(ctx.arg_i64(1)?)
        .map_err(|_| VmError::InvalidArgument("character count must not be negative"))?;
    let taken: String = if keep_prefix {
        text.chars().take(count).collect()
    } else {
        text.chars().skip(count).collect()
    };
    ctx.return_string(&taken)
}

pub fn str_take(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    take_or_drop_chars(ctx, true)
}

pub fn str_drop(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    take_or_drop_chars(ctx, false)
}

pub fn str_first(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    match ctx.arg_string(0)?.chars().next() {
        Some(ch) => {
            ctx.return_char(ch);
            Ok(())
        }
        None => {
            ctx.return_null();
            Ok(())
        }
    }
}

pub fn str_last(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    match ctx.arg_string(0)?.chars().next_back() {
        Some(ch) => {
            ctx.return_char(ch);
            Ok(())
        }
        None => {
            ctx.return_null();
            Ok(())
        }
    }
}

pub fn str_starts_with_char(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let ch = ctx.arg_char(1)?;
    ctx.return_bool(text.starts_with(ch));
    Ok(())
}

pub fn str_ends_with_char(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let ch = ctx.arg_char(1)?;
    ctx.return_bool(text.ends_with(ch));
    Ok(())
}

pub fn str_contains_char(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let ch = ctx.arg_char(1)?;
    ctx.return_bool(text.contains(ch));
    Ok(())
}

pub fn str_replace_char(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let text = ctx.arg_string(0)?;
    let from = ctx.arg_char(1)?;
    let to = ctx.arg_char(2)?;
    ctx.return_string(&text.replace(from, &to.to_string()))
}

fn line_pieces(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let bytes = text.as_bytes();
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\r' {
            pieces.push(&text[start..index]);
            index += 1;
            if index < bytes.len() && bytes[index] == b'\n' {
                index += 1;
            }
            start = index;
        } else if bytes[index] == b'\n' {
            pieces.push(&text[start..index]);
            index += 1;
            start = index;
        } else {
            index += 1;
        }
    }
    if start < text.len() {
        pieces.push(&text[start..]);
    }
    pieces
}

pub fn str_lines(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let text = ctx.arg_string(0)?;
    ctx.return_string_list(&line_pieces(&text))
}

pub fn str_split_once(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let separator = ctx.arg_string(1)?;
    require_separator(&separator)?;
    ctx.return_optional_string_pair(text.split_once(separator.as_str()))
}

pub fn str_rsplit_once(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let separator = ctx.arg_string(1)?;
    require_separator(&separator)?;
    ctx.return_optional_string_pair(text.rsplit_once(separator.as_str()))
}

fn strip_affix(ctx: &mut BuiltinCtx<'_>, prefix: bool) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let affix = ctx.arg_string(1)?;
    let stripped = if prefix {
        text.strip_prefix(affix.as_str())
    } else {
        text.strip_suffix(affix.as_str())
    };
    match stripped {
        Some(rest) => ctx.return_string(rest),
        None => {
            ctx.return_null();
            Ok(())
        }
    }
}

pub fn str_strip_prefix(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    strip_affix(ctx, true)
}

pub fn str_strip_suffix(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    strip_affix(ctx, false)
}

fn char_index(text: &str, value: char, last: bool) -> Result<Option<i64>, VmError> {
    let mut found = None;
    for (index, ch) in text.chars().enumerate() {
        if ch == value {
            found = Some(index);
            if !last {
                break;
            }
        }
    }
    found
        .map(|index| i64::try_from(index).map_err(|_| VmError::NumericOverflow))
        .transpose()
}

pub fn str_find_char(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let value = ctx.arg_char(1)?;
    match char_index(&text, value, false)? {
        Some(index) => ctx.return_i64(index)?,
        None => ctx.return_null(),
    }
    Ok(())
}

pub fn str_rfind_char(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let value = ctx.arg_char(1)?;
    match char_index(&text, value, true)? {
        Some(index) => ctx.return_i64(index)?,
        None => ctx.return_null(),
    }
    Ok(())
}

pub fn str_count_char(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let value = ctx.arg_char(1)?;
    let count = text.chars().filter(|ch| *ch == value).count();
    ctx.return_i64(i64::try_from(count).map_err(|_| VmError::NumericOverflow)?)?;
    Ok(())
}

/// Byte offset of scalar `index`, or `None` when `index` is past the end.
/// `index == char_len` is the offset just after the last scalar.
fn scalar_offset(text: &str, index: usize) -> Option<usize> {
    let mut count = 0;
    for (offset, _) in text.char_indices() {
        if count == index {
            return Some(offset);
        }
        count += 1;
    }
    (count == index).then_some(text.len())
}

pub fn str_char_len(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let text = ctx.arg_string(0)?;
    let count = i64::try_from(text.chars().count()).map_err(|_| VmError::NumericOverflow)?;
    ctx.return_i64(count)?;
    Ok(())
}

pub fn str_char_at(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let index = ctx.arg_i64(1)?;
    let Some(index) = usize::try_from(index).ok() else {
        ctx.return_null();
        return Ok(());
    };
    match text.chars().nth(index) {
        Some(value) => ctx.return_char(value),
        None => ctx.return_null(),
    }
    Ok(())
}

fn scalar_count_before(text: &str, byte: usize) -> Result<i64, VmError> {
    i64::try_from(text[..byte].chars().count()).map_err(|_| VmError::NumericOverflow)
}

pub fn str_rfind(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let pattern = ctx.arg_string(1)?;
    if pattern.is_empty() {
        return ctx.return_i64(scalar_count_before(&text, text.len())?);
    }
    let Some(byte) = text.rfind(pattern.as_str()) else {
        ctx.return_null();
        return Ok(());
    };
    ctx.return_i64(scalar_count_before(&text, byte)?)
}

pub fn str_count(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let pattern = ctx.arg_string(1)?;
    let count = if pattern.is_empty() {
        text.chars()
            .count()
            .checked_add(1)
            .ok_or(VmError::NumericOverflow)?
    } else {
        text.matches(pattern.as_str()).count()
    };
    ctx.return_i64(i64::try_from(count).map_err(|_| VmError::NumericOverflow)?)
}

fn byte_index(index: usize) -> Result<i64, VmError> {
    i64::try_from(index).map_err(|_| VmError::NumericOverflow)
}

pub fn str_byte_find(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let pattern = ctx.arg_string(1)?;
    let Some(index) = text.find(pattern.as_str()) else {
        ctx.return_null();
        return Ok(());
    };
    ctx.return_i64(byte_index(index)?)
}

pub fn str_byte_rfind(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let pattern = ctx.arg_string(1)?;
    let Some(index) = text.rfind(pattern.as_str()) else {
        ctx.return_null();
        return Ok(());
    };
    ctx.return_i64(byte_index(index)?)
}

pub fn str_byte_count(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let pattern = ctx.arg_string(1)?;
    let count = if pattern.is_empty() {
        text.len().checked_add(1).ok_or(VmError::NumericOverflow)?
    } else {
        text.matches(pattern.as_str()).count()
    };
    ctx.return_i64(i64::try_from(count).map_err(|_| VmError::NumericOverflow)?)
}

pub fn str_is_ascii(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_bool(ctx.arg_string(0)?.is_ascii());
    Ok(())
}

fn ascii_bytes(ctx: &mut BuiltinCtx<'_>, predicate: fn(u8) -> bool) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let text = ctx.arg_string(0)?;
    ctx.return_bool(text.bytes().all(predicate));
    Ok(())
}

pub fn str_is_ascii_digit(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ascii_bytes(ctx, |byte| byte.is_ascii_digit())
}

pub fn str_is_ascii_alphabetic(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ascii_bytes(ctx, |byte| byte.is_ascii_alphabetic())
}

pub fn str_is_ascii_alphanumeric(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ascii_bytes(ctx, |byte| byte.is_ascii_alphanumeric())
}

pub fn str_is_ascii_lowercase(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ascii_bytes(ctx, |byte| byte.is_ascii_lowercase())
}

pub fn str_is_ascii_uppercase(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ascii_bytes(ctx, |byte| byte.is_ascii_uppercase())
}

pub fn str_is_ascii_whitespace(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ascii_bytes(ctx, |byte| byte.is_ascii_whitespace())
}

pub fn str_is_ascii_hexdigit(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ascii_bytes(ctx, |byte| byte.is_ascii_hexdigit())
}

pub fn str_is_ascii_graphic(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ascii_bytes(ctx, |byte| byte.is_ascii_graphic())
}

pub fn str_is_ascii_punctuation(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ascii_bytes(ctx, |byte| byte.is_ascii_punctuation())
}

pub fn str_is_ascii_control(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ascii_bytes(ctx, |byte| byte.is_ascii_control())
}

fn scalar_all(ctx: &mut BuiltinCtx<'_>, predicate: fn(char) -> bool) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let text = ctx.arg_string(0)?;
    ctx.return_bool(text.chars().all(predicate));
    Ok(())
}

pub fn str_is_numeric(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    scalar_all(ctx, |scalar| scalar.is_numeric())
}

pub fn str_is_alphabetic(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    scalar_all(ctx, |scalar| scalar.is_alphabetic())
}

pub fn str_is_alphanumeric(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    scalar_all(ctx, |scalar| scalar.is_alphanumeric())
}

pub fn str_is_lowercase(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    scalar_all(ctx, |scalar| scalar.is_lowercase())
}

pub fn str_is_uppercase(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    scalar_all(ctx, |scalar| scalar.is_uppercase())
}

pub fn str_is_whitespace(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    scalar_all(ctx, |scalar| scalar.is_whitespace())
}

pub fn str_is_control(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    scalar_all(ctx, |scalar| scalar.is_control())
}

fn map_char_case(ctx: &mut BuiltinCtx<'_>, map: fn(char) -> String) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_string(&map(ctx.arg_char(0)?))
}

/// Full Unicode case mapping. One char may become several scalars, so the
/// result is a String. `'ß'` uppercases to `"SS"`.
pub fn char_to_uppercase(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    map_char_case(ctx, |value| value.to_uppercase().collect())
}

pub fn char_to_lowercase(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    map_char_case(ctx, |value| value.to_lowercase().collect())
}

pub fn char_into_string(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let mut bytes = [0; 4];
    let encoded = ctx.arg_char(0)?.encode_utf8(&mut bytes);
    ctx.check_string_size(encoded.len())?;
    ctx.return_string(encoded)
}

pub fn str_find(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let pattern = ctx.arg_string(1)?;
    let Some(byte) = text.find(pattern.as_str()) else {
        ctx.return_null();
        return Ok(());
    };
    let index =
        i64::try_from(text[..byte].chars().count()).map_err(|_| VmError::NumericOverflow)?;
    ctx.return_i64(index)?;
    Ok(())
}

pub fn str_slice(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let text = ctx.arg_string(0)?;
    let start = ctx.arg_i64(1)?;
    let end = ctx.arg_i64(2)?;
    let out_of_range = || Err(VmError::InvalidArgument("string slice is out of range"));
    let (Some(start), Some(end)) = (usize::try_from(start).ok(), usize::try_from(end).ok()) else {
        return out_of_range();
    };
    if end < start {
        return out_of_range();
    }
    let (Some(start_byte), Some(end_byte)) =
        (scalar_offset(&text, start), scalar_offset(&text, end))
    else {
        return out_of_range();
    };
    ctx.return_string(&text[start_byte..end_byte])
}

fn char_predicate(
    ctx: &mut BuiltinCtx<'_>,
    predicate: impl Fn(char) -> bool,
) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_bool(predicate(ctx.arg_char(0)?));
    Ok(())
}

pub fn char_is_whitespace(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, char::is_whitespace)
}

pub fn char_is_alphabetic(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, char::is_alphabetic)
}

pub fn char_is_numeric(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, char::is_numeric)
}

pub fn char_is_alphanumeric(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, char::is_alphanumeric)
}

pub fn char_is_uppercase(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, char::is_uppercase)
}

pub fn char_is_lowercase(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, char::is_lowercase)
}

pub fn char_is_ascii(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, |value| value.is_ascii())
}

pub fn char_is_ascii_digit(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, |value| value.is_ascii_digit())
}

pub fn char_is_ascii_alphabetic(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, |value| value.is_ascii_alphabetic())
}

/// ASCII `'0'` through `'7'`. The unstable `char::is_ascii_octdigit` is not used.
pub fn char_is_ascii_octdigit(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, |value| value.is_ascii_digit() && value <= '7')
}

pub fn char_is_ascii_hexdigit(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, |value| value.is_ascii_hexdigit())
}

pub fn char_is_ascii_control(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, |value| value.is_ascii_control())
}

pub fn char_is_ascii_graphic(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, |value| value.is_ascii_graphic())
}

pub fn char_is_ascii_punctuation(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, |value| value.is_ascii_punctuation())
}

pub fn char_is_ascii_whitespace(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, |value| value.is_ascii_whitespace())
}

pub fn char_is_ascii_alphanumeric(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, |value| value.is_ascii_alphanumeric())
}

pub fn char_is_ascii_uppercase(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, |value| value.is_ascii_uppercase())
}

pub fn char_is_ascii_lowercase(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, |value| value.is_ascii_lowercase())
}

pub fn char_is_control(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_predicate(ctx, char::is_control)
}

fn char_encoded_len(ctx: &mut BuiltinCtx<'_>, len: fn(char) -> usize) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let len = i64::try_from(len(ctx.arg_char(0)?)).map_err(|_| VmError::NumericOverflow)?;
    ctx.return_i64(len)?;
    Ok(())
}

pub fn char_utf8_len(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_encoded_len(ctx, char::len_utf8)
}

pub fn char_utf16_len(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    char_encoded_len(ctx, char::len_utf16)
}

pub fn char_escape_debug(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    map_char_case(ctx, |value| value.escape_debug().collect())
}

pub fn char_escape_unicode(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    map_char_case(ctx, |value| value.escape_unicode().collect())
}

pub fn char_escape_default(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    map_char_case(ctx, |value| value.escape_default().collect())
}

fn digit_radix(ctx: &mut BuiltinCtx<'_>, index: u8) -> Result<u32, VmError> {
    let radix = u32::try_from(ctx.arg_i64(index)?).unwrap_or(0);
    if (2..=36).contains(&radix) {
        Ok(radix)
    } else {
        Err(VmError::InvalidArgument("digit radix must be from 2 to 36"))
    }
}

pub fn char_is_digit(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let value = ctx.arg_char(0)?;
    let radix = digit_radix(ctx, 1)?;
    ctx.return_bool(value.is_digit(radix));
    Ok(())
}

pub fn char_to_digit(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let value = ctx.arg_char(0)?;
    let radix = digit_radix(ctx, 1)?;
    match value.to_digit(radix) {
        Some(digit) => ctx.return_i64(i64::from(digit)),
        None => {
            ctx.return_null();
            Ok(())
        }
    }
}

/// The character for `digit` in `radix`. A radix outside 2..=36 is an error.
/// A digit that does not fit that radix is null.
pub fn char_from_digit(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let digit = ctx.arg_i64(0)?;
    let radix = digit_radix(ctx, 1)?;
    let Some(digit) = u32::try_from(digit)
        .ok()
        .and_then(|digit| char::from_digit(digit, radix))
    else {
        ctx.return_null();
        return Ok(());
    };
    ctx.return_char(digit);
    Ok(())
}

pub fn char_encode_utf8(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx.arg_char(0)?;
    let mut buffer = [0u8; 4];
    let encoded = value.encode_utf8(&mut buffer);
    let bytes: Vec<i64> = encoded.bytes().map(i64::from).collect();
    ctx.return_i64_list(&bytes)
}

pub fn char_encode_utf16(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx.arg_char(0)?;
    let mut buffer = [0u16; 2];
    let encoded = value.encode_utf16(&mut buffer);
    let units: Vec<i64> = encoded.iter().copied().map(i64::from).collect();
    ctx.return_i64_list(&units)
}

pub fn str_encode_utf8(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let text = ctx.arg_string(0)?;
    let bytes: Vec<i64> = text.bytes().map(i64::from).collect();
    ctx.return_i64_list(&bytes)
}

pub fn str_encode_utf16(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let text = ctx.arg_string(0)?;
    let units: Vec<i64> = text.encode_utf16().map(i64::from).collect();
    ctx.return_i64_list(&units)
}

fn code_units(
    ctx: &mut BuiltinCtx<'_>,
    max: u32,
    message: &'static str,
) -> Result<Vec<u32>, VmError> {
    ctx.require_arity(1)?;
    let list = ctx.arg(0)?;
    let length = ctx.list_len(list)?;
    let mut units = Vec::with_capacity(length);
    for index in 0..length {
        let element = ctx.list_get(list, TaggedValue::from_u64(index as u64))?;
        let number = ctx.number_of(element)?;
        let value = match number {
            Number::I64(value) => i128::from(value),
            Number::U64(value) => i128::from(value),
            Number::I128(value) => value,
            Number::U128(value) => {
                i128::try_from(value).map_err(|_| VmError::InvalidArgument(message))?
            }
            Number::F64(_) => return Err(VmError::TypeError),
        };
        if value < 0 || value > i128::from(max) {
            return Err(VmError::InvalidArgument(message));
        }
        units.push(u32::try_from(value).map_err(|_| VmError::InvalidArgument(message))?);
    }
    Ok(units)
}

pub fn str_from_utf8(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let units = code_units(ctx, u32::from(u8::MAX), "utf-8 byte is out of range")?;
    let bytes: Vec<u8> = units.iter().map(|unit| *unit as u8).collect();
    match String::from_utf8(bytes) {
        Ok(text) => ctx.return_string(&text),
        Err(_) => {
            ctx.return_null();
            Ok(())
        }
    }
}

pub fn str_from_utf8_lossy(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let units = code_units(ctx, u32::from(u8::MAX), "utf-8 byte is out of range")?;
    let bytes: Vec<u8> = units.iter().map(|unit| *unit as u8).collect();
    let text = String::from_utf8_lossy(&bytes);
    ctx.return_string(&text)
}

pub fn str_from_utf16(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let units = code_units(ctx, u32::from(u16::MAX), "utf-16 code unit is out of range")?;
    let units: Vec<u16> = units.iter().map(|unit| *unit as u16).collect();
    match String::from_utf16(&units) {
        Ok(text) => ctx.return_string(&text),
        Err(_) => {
            ctx.return_null();
            Ok(())
        }
    }
}

pub fn str_from_utf16_lossy(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let units = code_units(ctx, u32::from(u16::MAX), "utf-16 code unit is out of range")?;
    let units: Vec<u16> = units.iter().map(|unit| *unit as u16).collect();
    let text = String::from_utf16_lossy(&units);
    ctx.return_string(&text)
}

/// A byte index in `0..=len`. A negative index or one past `len` is an error.
fn string_byte_index(text: &str, index: i64) -> Result<usize, VmError> {
    let Ok(index) = usize::try_from(index) else {
        return Err(VmError::InvalidArgument("string index is out of range"));
    };
    if index > text.len() {
        return Err(VmError::InvalidArgument("string index is out of range"));
    }
    Ok(index)
}

fn boundary_args(ctx: &mut BuiltinCtx<'_>) -> Result<(String, usize), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let index = string_byte_index(&text, ctx.arg_i64(1)?)?;
    Ok((text, index))
}

pub fn str_is_char_boundary(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let (text, index) = boundary_args(ctx)?;
    ctx.return_bool(text.is_char_boundary(index));
    Ok(())
}

pub fn str_floor_char_boundary(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let (text, index) = boundary_args(ctx)?;
    let boundary =
        i64::try_from(text.floor_char_boundary(index)).map_err(|_| VmError::NumericOverflow)?;
    ctx.return_i64(boundary)?;
    Ok(())
}

pub fn str_ceil_char_boundary(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let (text, index) = boundary_args(ctx)?;
    let boundary =
        i64::try_from(text.ceil_char_boundary(index)).map_err(|_| VmError::NumericOverflow)?;
    ctx.return_i64(boundary)?;
    Ok(())
}

pub fn str_char_at_byte(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let (text, index) = boundary_args(ctx)?;
    if index == text.len() || !text.is_char_boundary(index) {
        ctx.return_null();
        return Ok(());
    }
    let Some(ch) = text[index..].chars().next() else {
        return Err(VmError::InvalidArgument("string index is out of range"));
    };
    ctx.return_char(ch);
    Ok(())
}

pub fn str_split_at(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    let (text, index) = boundary_args(ctx)?;
    if !text.is_char_boundary(index) {
        return Err(VmError::InvalidArgument(
            "string index is not a character boundary",
        ));
    }
    let (left, right) = text.split_at(index);
    ctx.return_string_list(&[left, right])
}

pub fn str_byte_at(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let Ok(index) = usize::try_from(ctx.arg_i64(1)?) else {
        return Err(VmError::InvalidArgument("string index is out of range"));
    };
    let Some(byte) = text.as_bytes().get(index) else {
        return Err(VmError::InvalidArgument("string index is out of range"));
    };
    ctx.return_i64(i64::from(*byte))?;
    Ok(())
}

fn optional_byte_index(index: i64) -> Option<usize> {
    usize::try_from(index).ok()
}

pub fn str_get(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(3)?;
    let text = ctx.arg_string(0)?;
    let Some(start) = optional_byte_index(ctx.arg_i64(1)?) else {
        ctx.return_null();
        return Ok(());
    };
    let Some(end) = optional_byte_index(ctx.arg_i64(2)?) else {
        ctx.return_null();
        return Ok(());
    };
    if start > end
        || end > text.len()
        || !text.is_char_boundary(start)
        || !text.is_char_boundary(end)
    {
        ctx.return_null();
        return Ok(());
    }
    ctx.return_string(&text[start..end])
}

pub fn str_split_at_checked(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let text = ctx.arg_string(0)?;
    let Some(index) = optional_byte_index(ctx.arg_i64(1)?) else {
        ctx.return_null();
        return Ok(());
    };
    let Some((left, right)) = text.split_at_checked(index) else {
        ctx.return_null();
        return Ok(());
    };
    ctx.return_string_list(&[left, right])
}

pub fn char_to_ascii_uppercase(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_char(ctx.arg_char(0)?.to_ascii_uppercase());
    Ok(())
}

/// Optional sign, then either a `0b`/`0o`/`0x` magnitude or decimal digits.
/// The whole string must match. Whitespace and trailing junk are malformed.
fn parse_i64_text(text: &str) -> Result<Option<i64>, VmError> {
    let Some((negative, magnitude)) = signed_magnitude(text) else {
        return Ok(None);
    };
    let (radix, digits) = if let Some(digits) = strip_prefix_ignore_ascii_case(magnitude, "0x") {
        (16, digits)
    } else if let Some(digits) = strip_prefix_ignore_ascii_case(magnitude, "0b") {
        (2, digits)
    } else if let Some(digits) = strip_prefix_ignore_ascii_case(magnitude, "0o") {
        (8, digits)
    } else {
        (10, magnitude)
    };
    if digits.is_empty()
        || !digits
            .bytes()
            .all(|digit| is_ascii_radix_digit(digit, radix))
    {
        return Ok(None);
    }
    let magnitude = match i128::from_str_radix(digits, radix) {
        Ok(value) => value,
        Err(_) => return Err(VmError::NumericOverflow),
    };
    let signed = if negative { -magnitude } else { magnitude };
    i64::try_from(signed)
        .map(Some)
        .map_err(|_| VmError::NumericOverflow)
}

fn is_ascii_radix_digit(digit: u8, radix: u32) -> bool {
    match radix {
        2 => matches!(digit, b'0' | b'1'),
        8 => matches!(digit, b'0'..=b'7'),
        10 => digit.is_ascii_digit(),
        16 => digit.is_ascii_hexdigit(),
        _ => false,
    }
}

fn strip_prefix_ignore_ascii_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let (head, tail) = text.split_at_checked(prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then_some(tail)
}

/// Optional sign. A missing magnitude is malformed rather than zero.
fn signed_magnitude(text: &str) -> Option<(bool, &str)> {
    let (negative, magnitude) = match text.as_bytes().first() {
        Some(b'+') => (false, &text[1..]),
        Some(b'-') => (true, &text[1..]),
        Some(_) => (false, text),
        None => return None,
    };
    (!magnitude.is_empty()).then_some((negative, magnitude))
}

/// Decimal form accepted by Nessa float literals, plus a digits-only integer.
/// A dot requires digits on both sides. Exponent overflow becomes non-finite.
fn parse_f64_text(text: &str) -> Result<Option<f64>, VmError> {
    let Some((_, magnitude)) = signed_magnitude(text) else {
        return Ok(None);
    };
    let bytes = magnitude.as_bytes();
    let mut index = 0;
    if !bytes.first().is_some_and(u8::is_ascii_digit) {
        return Ok(None);
    }
    while bytes.get(index).is_some_and(u8::is_ascii_digit) {
        index += 1;
    }
    if bytes.get(index) == Some(&b'.') {
        index += 1;
        if !bytes.get(index).is_some_and(u8::is_ascii_digit) {
            return Ok(None);
        }
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
    }
    if matches!(bytes.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(bytes.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        if !bytes.get(index).is_some_and(u8::is_ascii_digit) {
            return Ok(None);
        }
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
    }
    if index != bytes.len() {
        return Ok(None);
    }
    let value = text.parse::<f64>().map_err(|_| VmError::NumericOverflow)?;
    if !value.is_finite() {
        return Err(VmError::NumericOverflow);
    }
    Ok(Some(value))
}

pub fn str_parse_i64(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    match parse_i64_text(&ctx.arg_string(0)?)? {
        Some(value) => ctx.return_i64(value),
        None => {
            ctx.return_null();
            Ok(())
        }
    }
}

pub fn str_parse_f64(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    match parse_f64_text(&ctx.arg_string(0)?)? {
        Some(value) => ctx.return_f64(value),
        None => {
            ctx.return_null();
            Ok(())
        }
    }
}

fn radix_digits(text: &str) -> Option<(bool, u32, &str)> {
    let (negative, magnitude) = signed_magnitude(text)?;
    let (radix, digits) = if let Some(digits) = strip_prefix_ignore_ascii_case(magnitude, "0x") {
        (16, digits)
    } else if let Some(digits) = strip_prefix_ignore_ascii_case(magnitude, "0b") {
        (2, digits)
    } else if let Some(digits) = strip_prefix_ignore_ascii_case(magnitude, "0o") {
        (8, digits)
    } else {
        (10, magnitude)
    };
    if digits.is_empty()
        || !digits
            .bytes()
            .all(|digit| is_ascii_radix_digit(digit, radix))
    {
        return None;
    }
    Some((negative, radix, digits))
}

fn magnitude_u128(digits: &str, radix: u32) -> Result<u128, VmError> {
    u128::from_str_radix(digits, radix).map_err(|_| VmError::NumericOverflow)
}

pub fn str_parse_u64(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let text = ctx.arg_string(0)?;
    let Some((negative, radix, digits)) = radix_digits(&text) else {
        ctx.return_null();
        return Ok(());
    };
    if negative {
        ctx.return_null();
        return Ok(());
    }
    let value =
        u64::try_from(magnitude_u128(digits, radix)?).map_err(|_| VmError::NumericOverflow)?;
    ctx.return_number(Number::U64(value))
}

pub fn str_parse_i128(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let text = ctx.arg_string(0)?;
    let Some((negative, radix, digits)) = radix_digits(&text) else {
        ctx.return_null();
        return Ok(());
    };
    let magnitude = magnitude_u128(digits, radix)?;
    let value = if negative {
        if magnitude == 1u128 << 127 {
            i128::MIN
        } else {
            let signed = i128::try_from(magnitude).map_err(|_| VmError::NumericOverflow)?;
            -signed
        }
    } else {
        i128::try_from(magnitude).map_err(|_| VmError::NumericOverflow)?
    };
    ctx.return_number(Number::I128(value))
}

pub fn str_parse_u128(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let text = ctx.arg_string(0)?;
    let Some((negative, radix, digits)) = radix_digits(&text) else {
        ctx.return_null();
        return Ok(());
    };
    if negative {
        ctx.return_null();
        return Ok(());
    }
    ctx.return_number(Number::U128(magnitude_u128(digits, radix)?))
}

pub fn char_to_ascii_lowercase(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    ctx.return_char(ctx.arg_char(0)?.to_ascii_lowercase());
    Ok(())
}

pub fn char_replacement(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_char(char::REPLACEMENT_CHARACTER);
    Ok(())
}

pub fn char_max(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_char(char::MAX);
    Ok(())
}

pub fn char_min(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(0)?;
    ctx.return_char('\0');
    Ok(())
}

pub fn char_eq_ignore_ascii_case(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(2)?;
    let left = ctx.arg_char(0)?;
    let right = ctx.arg_char(1)?;
    ctx.return_bool(left.eq_ignore_ascii_case(&right));
    Ok(())
}

pub fn char_to_code(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = u32::from(ctx.arg_char(0)?);
    ctx.return_i64(i64::from(value))?;
    Ok(())
}

/// A Unicode scalar value. Surrogates and codes outside `0..=0x10FFFF` are null.
pub fn char_from_code(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let Some(value) = u32::try_from(ctx.arg_i64(0)?).ok().and_then(char::from_u32) else {
        ctx.return_null();
        return Ok(());
    };
    ctx.return_char(value);
    Ok(())
}

/// Preserve literal quoting for String-typed aggregate components.
pub fn display_quote(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx.arg_string(0)?;
    let quoted = format!("{value:?}");
    ctx.return_string(&quoted)
}

pub fn display_enter_tuple(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx.arg(0)?;
    let entered = ctx.display_tuple_enter(value)?;
    ctx.return_bool(entered);
    Ok(())
}

pub fn display_exit_tuple(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
    ctx.require_arity(1)?;
    let value = ctx.arg(0)?;
    ctx.display_tuple_exit(value)?;
    ctx.set_return(runtime::TaggedValue::UNIT)
}
