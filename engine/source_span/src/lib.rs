//! Coordinates between original source bytes and native SourceMap spans.
//! Lexing uses original text. Only positions change when SourceMap removes a
//! leading BOM or a CR from CRLF. Retained AST origins stay in original bytes.

use std::fmt;
use std::rc::Rc;
use std::sync::Arc;

use rustc_span::{BytePos, SourceFile, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawByteRange {
    pub start: u32,
    pub end: u32,
}

/// A normalized span with explicit provenance, including real zero-width spans.
/// None marks a raw-local context with no registered SourceMap file.
#[derive(Clone, Copy, Debug)]
pub struct LocatedSourceSpan {
    pub span: Span,
    pub file_start: Option<BytePos>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceSpanError {
    InvalidRange,
    InvalidBoundary,
    PositionOverflow,
    InvalidSourceMap,
    SourceMismatch,
}

impl fmt::Display for SourceSpanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidRange => "source range is reversed or outside its file",
            Self::InvalidBoundary => "source range cuts a UTF-8 character",
            Self::PositionOverflow => "source position exceeds the address space",
            Self::InvalidSourceMap => "invalid source normalization metadata",
            Self::SourceMismatch => "original source does not match its SourceMap file",
        })
    }
}

impl std::error::Error for SourceSpanError {}

#[derive(Debug)]
struct Removal {
    raw_start: u32,
    raw_end: u32,
    normalized_after: u32,
    cumulative: u32,
}

#[derive(Debug)]
struct Coordinates {
    base: BytePos,
    raw_length: u32,
    normalized_length: u32,
    registered: bool,
    removals: Vec<Removal>,
}

/// Text-independent metadata retained with AST origins. Cloning shares only
/// coordinate data and does not make an AST depend on SourceMap's Rc text.
#[derive(Clone, Debug)]
pub struct SourceMapping(Arc<Coordinates>);

impl SourceMapping {
    pub fn raw_local(base: BytePos, length: usize) -> Result<Self, SourceSpanError> {
        let length = u32::try_from(length).map_err(|_| SourceSpanError::PositionOverflow)?;
        base.0
            .checked_add(length)
            .ok_or(SourceSpanError::PositionOverflow)?;
        Ok(Self(Arc::new(Coordinates {
            base,
            raw_length: length,
            normalized_length: length,
            registered: false,
            removals: Vec::new(),
        })))
    }

    fn from_file(file: &SourceFile) -> Result<Self, SourceSpanError> {
        let text = file.src.as_ref().ok_or(SourceSpanError::InvalidSourceMap)?;
        let normalized_length = file
            .end_pos
            .0
            .checked_sub(file.start_pos.0)
            .ok_or(SourceSpanError::InvalidSourceMap)?;
        if text.len() != normalized_length as usize {
            return Err(SourceSpanError::InvalidSourceMap);
        }
        let mut removals = Vec::with_capacity(file.normalized_pos.len());
        let mut previous_diff = 0;
        let mut previous_end = 0;
        for position in &file.normalized_pos {
            let after = position
                .pos
                .0
                .checked_sub(file.start_pos.0)
                .ok_or(SourceSpanError::InvalidSourceMap)?;
            let delta = position
                .diff
                .checked_sub(previous_diff)
                .ok_or(SourceSpanError::InvalidSourceMap)?;
            let start = match (after, delta, previous_diff) {
                (0, 3, 0) if removals.is_empty() => 0,
                (_, 1, _)
                    if after > 0 && text.as_bytes().get(after as usize - 1) == Some(&b'\n') =>
                {
                    (after - 1)
                        .checked_add(previous_diff)
                        .ok_or(SourceSpanError::PositionOverflow)?
                }
                _ => return Err(SourceSpanError::InvalidSourceMap),
            };
            let end = start
                .checked_add(delta)
                .ok_or(SourceSpanError::PositionOverflow)?;
            if after > normalized_length
                || start < previous_end
                || removals
                    .last()
                    .is_some_and(|event: &Removal| after <= event.normalized_after)
            {
                return Err(SourceSpanError::InvalidSourceMap);
            }
            removals.push(Removal {
                raw_start: start,
                raw_end: end,
                normalized_after: after,
                cumulative: position.diff,
            });
            previous_diff = position.diff;
            previous_end = end;
        }
        let raw_length = normalized_length
            .checked_add(previous_diff)
            .ok_or(SourceSpanError::PositionOverflow)?;
        file.start_pos
            .0
            .checked_add(raw_length)
            .ok_or(SourceSpanError::PositionOverflow)?;
        if previous_end > raw_length {
            return Err(SourceSpanError::InvalidSourceMap);
        }
        Ok(Self(Arc::new(Coordinates {
            base: file.start_pos,
            raw_length,
            normalized_length,
            registered: true,
            removals,
        })))
    }

    fn normalized_offset(&self, offset: u32) -> Result<u32, SourceSpanError> {
        if offset > self.0.raw_length {
            return Err(SourceSpanError::InvalidRange);
        }
        let count = self
            .0
            .removals
            .partition_point(|event| event.raw_end <= offset);
        let removed = count
            .checked_sub(1)
            .map_or(0, |i| self.0.removals[i].cumulative);
        if let Some(next) = self.0.removals.get(count)
            && offset > next.raw_start
        {
            // Only the multi-byte BOM has an interior integer offset. It is
            // not a UTF-8 boundary and must not become a fabricated location.
            return Err(SourceSpanError::InvalidBoundary);
        }
        offset
            .checked_sub(removed)
            .ok_or(SourceSpanError::InvalidSourceMap)
    }

    /// Check coordinate bounds. Use SourceSpanMapper for UTF-8 text validation.
    pub fn map_range(&self, raw: RawByteRange) -> Result<LocatedSourceSpan, SourceSpanError> {
        if raw.end < raw.start {
            return Err(SourceSpanError::InvalidRange);
        }
        let lo = self.normalized_offset(raw.start)?;
        let hi = self.normalized_offset(raw.end)?;
        if hi > self.0.normalized_length || lo > hi {
            return Err(SourceSpanError::InvalidSourceMap);
        }
        Ok(LocatedSourceSpan {
            span: Span::new(BytePos(self.0.base.0 + lo), BytePos(self.0.base.0 + hi)),
            file_start: self.0.registered.then_some(self.0.base),
        })
    }

    pub fn raw_range(&self, span: Span) -> Result<RawByteRange, SourceSpanError> {
        let start = span
            .lo()
            .0
            .checked_sub(self.0.base.0)
            .ok_or(SourceSpanError::InvalidRange)?;
        let end = span
            .hi()
            .0
            .checked_sub(self.0.base.0)
            .ok_or(SourceSpanError::InvalidRange)?;
        if end < start || end > self.0.raw_length {
            return Err(SourceSpanError::InvalidRange);
        }
        Ok(RawByteRange { start, end })
    }

    /// Inverse normalized endpoints for synthetic nodes. Parsed node origins
    /// must be retained directly: normalization is not one-to-one at newlines.
    pub fn original_range(&self, span: Span) -> Result<RawByteRange, SourceSpanError> {
        let start = span
            .lo()
            .0
            .checked_sub(self.0.base.0)
            .ok_or(SourceSpanError::InvalidRange)?;
        let end = span
            .hi()
            .0
            .checked_sub(self.0.base.0)
            .ok_or(SourceSpanError::InvalidRange)?;
        if end < start || end > self.0.normalized_length {
            return Err(SourceSpanError::InvalidRange);
        }
        let original = |offset| {
            let count = self
                .0
                .removals
                .partition_point(|event| event.normalized_after <= offset);
            let diff = count
                .checked_sub(1)
                .map_or(0, |i| self.0.removals[i].cumulative);
            offset
                .checked_add(diff)
                .ok_or(SourceSpanError::PositionOverflow)
        };
        Ok(RawByteRange {
            start: original(start)?,
            end: original(end)?,
        })
    }
}

/// Checked mapping against the actual normalized source. Its Rc is a shared
/// borrow of native SourceFile text, not another copy of the input.
#[derive(Clone, Debug)]
pub struct SourceSpanMapper {
    mapping: SourceMapping,
    normalized: Rc<String>,
}

impl SourceSpanMapper {
    pub fn from_file(file: &SourceFile) -> Result<Self, SourceSpanError> {
        Ok(Self {
            mapping: SourceMapping::from_file(file)?,
            normalized: file.src.clone().ok_or(SourceSpanError::InvalidSourceMap)?,
        })
    }

    pub fn for_source(file: &SourceFile, raw: &str) -> Result<Self, SourceSpanError> {
        let mapper = Self::from_file(file)?;
        mapper.validate_source(raw)?;
        Ok(mapper)
    }

    /// Validate exact original bytes before attaching retained source provenance.
    pub fn validate_source(&self, raw: &str) -> Result<(), SourceSpanError> {
        if raw.len() != self.mapping.0.raw_length as usize {
            return Err(SourceSpanError::SourceMismatch);
        }
        let mut raw_cursor = 0;
        let mut normalized_cursor = 0;
        for event in &self.mapping.0.removals {
            let start = event.raw_start as usize;
            let end = event.raw_end as usize;
            let removed = raw
                .as_bytes()
                .get(start..end)
                .ok_or(SourceSpanError::SourceMismatch)?;
            if !((start == 0 && removed == b"\xef\xbb\xbf")
                || (removed == b"\r" && raw.as_bytes().get(end) == Some(&b'\n')))
            {
                return Err(SourceSpanError::SourceMismatch);
            }
            let length = start - raw_cursor;
            let next = normalized_cursor + length;
            if raw.get(raw_cursor..start) != self.normalized.get(normalized_cursor..next) {
                return Err(SourceSpanError::SourceMismatch);
            }
            raw_cursor = end;
            normalized_cursor = next;
        }
        if raw.get(raw_cursor..) != self.normalized.get(normalized_cursor..) {
            return Err(SourceSpanError::SourceMismatch);
        }
        Ok(())
    }

    pub fn raw_local(base: BytePos, raw: &str) -> Result<Self, SourceSpanError> {
        Ok(Self {
            mapping: SourceMapping::raw_local(base, raw.len())?,
            normalized: Rc::new(raw.to_owned()),
        })
    }

    pub fn mapping(&self) -> SourceMapping {
        self.mapping.clone()
    }

    pub fn map_range(&self, raw: RawByteRange) -> Result<LocatedSourceSpan, SourceSpanError> {
        let located = self.mapping.map_range(raw)?;
        let start = (located.span.lo().0 - self.mapping.0.base.0) as usize;
        let end = (located.span.hi().0 - self.mapping.0.base.0) as usize;
        if !self.normalized.is_char_boundary(start) || !self.normalized.is_char_boundary(end) {
            return Err(SourceSpanError::InvalidBoundary);
        }
        Ok(located)
    }

    pub fn map_raw_absolute(&self, span: Span) -> Result<LocatedSourceSpan, SourceSpanError> {
        self.map_range(self.mapping.raw_range(span)?)
    }
}
