//! Exact original text origins, separate from normalized diagnostic spans.

use rustc_span::Span;
use source_span::{RawByteRange, SourceMapping, SourceSpanError, SourceSpanMapper};

use crate::{Ast, NodeIndex, NodeKind};

pub(crate) enum RetainedText {
    Primary,
    Owned(Option<String>),
}

pub(crate) struct SourceRecord {
    pub(crate) text: RetainedText,
    pub(crate) mapping: SourceMapping,
}

#[derive(Clone, Copy)]
pub(crate) struct OriginalSpan {
    source: usize,
    range: RawByteRange,
}

impl Ast {
    /// Map a freshly parsed raw arena once, retaining every original node range.
    /// The input is validated before changing nodes or replacing source metadata.
    pub fn attach_source(
        &mut self,
        raw: String,
        mapper: &SourceSpanMapper,
    ) -> Result<(), SourceSpanError> {
        if !self.source_records.is_empty() {
            return Err(SourceSpanError::InvalidSourceMap);
        }
        mapper.validate_source(&raw)?;
        let mapping = mapper.mapping();
        let mut origins = Vec::with_capacity(self.nodes.len());
        let mut spans = Vec::with_capacity(self.nodes.len());
        for node in &self.nodes {
            if node.kind == NodeKind::Invalid {
                origins.push(None);
                spans.push(node.span);
                continue;
            }
            let range = mapping.raw_range(node.span)?;
            raw.get(range.start as usize..range.end as usize)
                .ok_or(SourceSpanError::InvalidBoundary)?;
            let located = mapper.map_range(range)?;
            origins.push(Some(OriginalSpan { source: 0, range }));
            spans.push(located.span);
        }
        for (node, span) in self.nodes.iter_mut().zip(spans) {
            node.span = span;
        }
        self.original_spans = origins;
        self.source = Some(raw);
        self.source_records.push(SourceRecord {
            text: RetainedText::Primary,
            mapping,
        });
        Ok(())
    }

    pub(crate) fn replace_local_source(&mut self, source: String) {
        self.source_records.clear();
        self.source = Some(source);
        if let Some(mapping) = self
            .source
            .as_ref()
            .and_then(|text| SourceMapping::raw_local(rustc_span::BytePos(0), text.len()).ok())
        {
            self.source_records.push(SourceRecord {
                text: RetainedText::Primary,
                mapping,
            });
        }
        self.original_spans = self
            .nodes
            .iter()
            .map(|node| self.origin_for_span(node.span))
            .collect();
    }

    fn record_text(&self, index: usize) -> Option<&str> {
        match &self.source_records.get(index)?.text {
            RetainedText::Primary => self.source.as_deref(),
            RetainedText::Owned(text) => text.as_deref(),
        }
    }

    pub(crate) fn origin_for_span(&self, span: Span) -> Option<OriginalSpan> {
        if span.is_dummy() {
            return None;
        }
        self.source_records
            .iter()
            .enumerate()
            .find_map(|(index, record)| {
                let range = record.mapping.original_range(span).ok()?;
                self.record_text(index)?
                    .get(range.start as usize..range.end as usize)?;
                Some(OriginalSpan {
                    source: index,
                    range,
                })
            })
    }

    pub(crate) fn raw_span_text(&self, span: Span) -> Option<&str> {
        if span.is_dummy() {
            return None;
        }
        if self.source_records.is_empty() {
            return self
                .source
                .as_ref()?
                .get(span.lo().0 as usize..span.hi().0 as usize);
        }
        let origin = self.origin_for_span(span)?;
        self.record_text(origin.source)?
            .get(origin.range.start as usize..origin.range.end as usize)
    }

    pub(crate) fn original_node_text(&self, index: NodeIndex) -> Option<&str> {
        let node = self.nodes.get(index.0 as usize)?;
        if let Some(origin) = self.original_spans.get(index.0 as usize).copied().flatten() {
            let record = self.source_records.get(origin.source)?;
            if record.mapping.map_range(origin.range).ok()?.span != node.span {
                // Direct arena mutation must not reuse stale text provenance.
                return None;
            }
            return self
                .record_text(origin.source)?
                .get(origin.range.start as usize..origin.range.end as usize);
        }
        self.raw_span_text(node.span)
    }

    pub(crate) fn append_source_origins(&mut self, other: &mut Ast) {
        let source_offset = self.source_records.len();
        let primary = other.source.take();
        let mut primary = Some(primary);
        for mut record in other.source_records.drain(..) {
            if matches!(record.text, RetainedText::Primary) {
                record.text = RetainedText::Owned(primary.take().flatten());
            }
            self.source_records.push(record);
        }
        // Public arenas may have been extended directly. Missing metadata means
        // unavailable provenance, never a reason to index another node's origin.
        self.original_spans.resize(self.nodes.len(), None);
        for index in 1..other.nodes.len() {
            let origin = other
                .original_spans
                .get(index)
                .copied()
                .flatten()
                .map(|mut origin| {
                    origin.source += source_offset;
                    origin
                });
            self.original_spans.push(origin);
        }
    }
}
