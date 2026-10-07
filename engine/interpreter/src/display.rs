//! Bounded display of rooted aggregate graphs, sharing one traversal and budget.

use std::collections::HashSet;

use gc::ObjectHeader;
use runtime::TaggedValue;
use type_pool::{CollectionRole, Intrinsic, TypeIndex, TypeKind};

use crate::{Vm, VmError};

const MAX_DISPLAY_BYTES: usize = 1024 * 1024;
const MAX_DISPLAY_DEPTH: usize = 128;

enum AggregateShape {
    List,
    Map(Vec<usize>),
    Tuple,
    Enum(String),
    Error(bool),
}

struct AggregateLayout {
    pointer: *const u64,
    length: usize,
    shape: AggregateShape,
    element_types: Option<Vec<TypeIndex>>,
}

impl Vm {
    fn aggregate_layout(&self, value: TaggedValue) -> Result<Option<AggregateLayout>, VmError> {
        if let Some(layout) = self.error_layout(value, 0)? {
            let pointer = value.as_heap_ptr().ok_or(VmError::TypeError)?;
            // SAFETY: checked Error layout contains exactly the payload slot.
            return Ok(Some(AggregateLayout {
                pointer: unsafe { pointer.cast::<u64>().add(2) },
                length: 1,
                shape: AggregateShape::Error(layout.tag != type_pool::TypeId::ZERO),
                element_types: None,
            }));
        }
        if let Some(layout) = self.enum_layout(value)? {
            let name = str_interner::try_get(layout.name).ok_or(VmError::TypeError)?;
            let variant = str_interner::try_get(layout.variant_name).ok_or(VmError::TypeError)?;
            return Ok(Some(AggregateLayout {
                pointer: layout.pointer,
                length: layout.fields.len(),
                shape: AggregateShape::Enum(format!("{name}.{variant}")),
                element_types: Some(layout.fields.iter().map(|field| field.ty).collect()),
            }));
        }
        let Some(pointer) = value.as_heap_ptr() else {
            return Ok(None);
        };
        // SAFETY: caller retains this live value through a rooted aggregate graph.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        match self
            .state
            .type_pool
            .checked_collection_role(header.type_index)
            .map_err(|_| VmError::TypeError)?
        {
            Some(CollectionRole::List) => {
                let layout = self.list_layout(value)?;
                return Ok(Some(AggregateLayout {
                    pointer: layout.buffer,
                    length: layout.length,
                    shape: AggregateShape::List,
                    element_types: None,
                }));
            }
            Some(CollectionRole::Map) => {
                let layout = self.map_layout(value)?;
                let buckets = layout.occupied_buckets()?;
                return Ok(Some(AggregateLayout {
                    pointer: layout.buffer,
                    length: layout.length,
                    shape: AggregateShape::Map(buckets),
                    element_types: None,
                }));
            }
            Some(CollectionRole::Buffer | CollectionRole::MapBuffer) => {
                return Err(VmError::TypeError);
            }
            None => {}
        }
        let ty = self
            .state
            .type_pool
            .canonical_type(header.type_index)
            .ok_or(VmError::TypeError)?;
        if let TypeKind::Tuple { elements } = &self.state.type_pool.get(ty).kind {
            if header.payload_words() != elements.len() {
                return Err(VmError::TypeError);
            }
            return Ok(Some(AggregateLayout {
                pointer: pointer.cast(),
                length: elements.len(),
                shape: AggregateShape::Tuple,
                element_types: Some(elements.clone()),
            }));
        }
        Ok(None)
    }

    /// Managed values remain in the caller's registered graph. Display allocates
    /// host strings only, never managed objects or a collection safepoint.
    pub(super) fn format_aggregate(&self, value: TaggedValue) -> Result<Option<String>, VmError> {
        if self.aggregate_layout(value)?.is_none() {
            return Ok(None);
        }
        self.format_aggregate_inner(value, 0, &mut HashSet::new())
            .map(Some)
    }

    fn format_aggregate_inner(
        &self,
        value: TaggedValue,
        depth: usize,
        visiting: &mut HashSet<u64>,
    ) -> Result<String, VmError> {
        let layout = self.aggregate_layout(value)?.ok_or(VmError::TypeError)?;
        if visiting.contains(&value.raw()) {
            return Ok("<cycle>".into());
        }
        if depth >= MAX_DISPLAY_DEPTH {
            return Err(VmError::DisplayDepthExceeded);
        }
        visiting.insert(value.raw());
        let mut output = match &layout.shape {
            AggregateShape::Error(error) => {
                if *error {
                    "error ".into()
                } else {
                    String::new()
                }
            }
            AggregateShape::List => String::from("["),
            AggregateShape::Map(_) => String::from("{"),
            AggregateShape::Tuple => String::from("("),
            AggregateShape::Enum(name) if layout.length == 0 => name.clone(),
            AggregateShape::Enum(name) => format!("{name}("),
        };
        if output.len() > MAX_DISPLAY_BYTES {
            return Err(VmError::DisplaySizeExceeded);
        }
        let tuple = matches!(layout.shape, AggregateShape::Tuple);
        for index in 0..layout.length {
            if index != 0 {
                output.push_str(", ");
            }
            // SAFETY: layout checked payload bounds. The recursive formatter
            // cannot allocate a managed object or stop the active operation.
            let offset = match &layout.shape {
                AggregateShape::Map(buckets) => buckets[index] * 4 + 3,
                _ => index,
            };
            let element = TaggedValue::from_raw(unsafe { layout.pointer.add(offset).read() });
            if let Some(types) = &layout.element_types
                && !self.value_matches_type(element, types[index], 0)?
            {
                return Err(VmError::TypeError);
            }
            let rendered = if matches!(layout.shape, AggregateShape::Error(_)) && depth == 0 {
                if let Some(text) = crate::builtin_ctx::heap_string_to_owned(element) {
                    text
                } else {
                    self.format_aggregate_element(element, depth + 1, visiting)?
                }
            } else {
                self.format_aggregate_element(element, depth + 1, visiting)?
            };
            let rendered = if matches!(layout.shape, AggregateShape::Map(_)) {
                // SAFETY: the checked bucket key immediately precedes its value.
                let key = TaggedValue::from_raw(unsafe { layout.pointer.add(offset - 1).read() });
                let key = self.format_aggregate_element(key, depth + 1, visiting)?;
                format!("{key}: {rendered}")
            } else {
                rendered
            };
            let suffix = if tuple && layout.length == 1 { 2 } else { 1 };
            if output
                .len()
                .checked_add(rendered.len() + suffix)
                .is_none_or(|bytes| bytes > MAX_DISPLAY_BYTES)
            {
                return Err(VmError::DisplaySizeExceeded);
            }
            output.push_str(&rendered);
        }
        if tuple && layout.length == 1 {
            output.push(',');
        }
        match layout.shape {
            AggregateShape::List => output.push(']'),
            AggregateShape::Map(_) => output.push('}'),
            AggregateShape::Tuple => output.push(')'),
            AggregateShape::Enum(_) if layout.length != 0 => output.push(')'),
            AggregateShape::Enum(_) | AggregateShape::Error(_) => {}
        }
        visiting.remove(&value.raw());
        Ok(output)
    }

    fn format_aggregate_element(
        &self,
        value: TaggedValue,
        depth: usize,
        visiting: &mut HashSet<u64>,
    ) -> Result<String, VmError> {
        if let Some(ty) = value.as_type() {
            return self
                .state
                .type_pool
                .display_name(ty)
                .ok_or(VmError::InvalidType(ty));
        }
        if self.aggregate_layout(value)?.is_some() {
            return self.format_aggregate_inner(value, depth, visiting);
        }
        if let Some(pointer) = value.as_heap_ptr() {
            // SAFETY: the checked aggregate slot is reachable from the registered root.
            let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
            if header.type_index == Intrinsic::Str.type_index() {
                let string =
                    crate::builtin_ctx::heap_string_to_owned(value).ok_or(VmError::TypeError)?;
                return Ok(format!("{string:?}"));
            }
        }
        Ok(crate::builtin_ctx::format_tagged_value(value))
    }
}
