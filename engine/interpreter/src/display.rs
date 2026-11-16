//! Bounded display of rooted aggregate graphs, sharing one traversal and budget.

use std::collections::HashSet;

use gc::ObjectHeader;
use runtime::TaggedValue;
use type_pool::{CollectionRole, Intrinsic, TypeIndex, TypeKind};

use crate::collections::read_u64_atomic;
use crate::{Vm, VmError};

const MAX_DISPLAY_BYTES: usize = 1024 * 1024;
const MAX_DISPLAY_DEPTH: usize = 128;

enum AggregateShape {
    List,
    Map(Vec<TaggedValue>),
    Bytes(usize),
    Tuple,
    Enum(String),
    Error(bool),
}

struct AggregateLayout {
    elements: Vec<TaggedValue>,
    shape: AggregateShape,
    element_types: Option<Vec<TypeIndex>>,
}

impl Vm {
    fn aggregate_layout(&self, value: TaggedValue) -> Result<Option<AggregateLayout>, VmError> {
        if let Some(layout) = self.error_layout(value, 0)? {
            return Ok(Some(AggregateLayout {
                elements: vec![layout.payload],
                shape: AggregateShape::Error(layout.tag != type_pool::TypeId::ZERO),
                element_types: None,
            }));
        }
        if let Some(layout) = self.enum_layout(value)? {
            let name = str_interner::try_get(layout.name).ok_or(VmError::TypeError)?;
            let variant = str_interner::try_get(layout.variant_name).ok_or(VmError::TypeError)?;
            let len = layout.fields.len();
            let mut elements = Vec::with_capacity(len);
            for i in 0..len {
                elements.push(TaggedValue::from_raw(read_u64_atomic(layout.pointer, i)));
            }
            return Ok(Some(AggregateLayout {
                elements,
                shape: AggregateShape::Enum(format!("{name}.{variant}")),
                element_types: Some(layout.fields.iter().map(|field| field.ty).collect()),
            }));
        }
        let Some(pointer) = value.as_heap_ptr() else {
            return Ok(None);
        };
        // SAFETY: caller retains this live value through a rooted aggregate graph.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        let role = self
            .type_pool_read()
            .checked_collection_role(header.type_index)
            .map_err(|_| VmError::TypeError)?;
        match role {
            Some(CollectionRole::List) => {
                let elements = self.snapshot_list_elements(value)?;
                return Ok(Some(AggregateLayout {
                    elements,
                    shape: AggregateShape::List,
                    element_types: None,
                }));
            }
            Some(CollectionRole::Map) => {
                let (keys, elements) = {
                    let _guard = self.stripe_read(value)?;
                    let layout = self.map_layout_unlocked(value)?;
                    let buckets = layout.occupied_buckets()?;
                    let mut keys = Vec::with_capacity(buckets.len());
                    let mut vals = Vec::with_capacity(buckets.len());
                    for bucket in buckets {
                        keys.push(layout.slot(bucket, 2));
                        vals.push(layout.slot(bucket, 3));
                    }
                    (keys, vals)
                };
                return Ok(Some(AggregateLayout {
                    elements,
                    shape: AggregateShape::Map(keys),
                    element_types: None,
                }));
            }
            Some(CollectionRole::Bytes) => {
                let layout = self.bytes_layout(value)?;
                return Ok(Some(AggregateLayout {
                    elements: Vec::new(),
                    shape: AggregateShape::Bytes(layout.length),
                    element_types: None,
                }));
            }
            Some(
                CollectionRole::Buffer | CollectionRole::MapBuffer | CollectionRole::BytesBuffer,
            ) => {
                return Err(VmError::TypeError);
            }
            None => {}
        }
        let tuple_elem_types = {
            let tp = self.type_pool_read();
            let ty = tp
                .canonical_type(header.type_index)
                .ok_or(VmError::TypeError)?;
            match &tp.get(ty).kind {
                TypeKind::Tuple { elements } => Some(elements.clone()),
                _ => None,
            }
        };
        if let Some(elem_types) = tuple_elem_types {
            if header.payload_words() != elem_types.len() {
                return Err(VmError::TypeError);
            }
            let base = pointer as *const u64;
            let elements = (0..elem_types.len())
                .map(|i| TaggedValue::from_raw(read_u64_atomic(base, i)))
                .collect();
            return Ok(Some(AggregateLayout {
                elements,
                shape: AggregateShape::Tuple,
                element_types: Some(elem_types),
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
        let len = layout.elements.len();
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
            AggregateShape::Bytes(len) => format!("Bytes(len={len})"),
            AggregateShape::Tuple => String::from("("),
            AggregateShape::Enum(name) if len == 0 => name.clone(),
            AggregateShape::Enum(name) => format!("{name}("),
        };
        if output.len() > MAX_DISPLAY_BYTES {
            return Err(VmError::DisplaySizeExceeded);
        }
        let tuple = matches!(layout.shape, AggregateShape::Tuple);
        for (index, &element) in layout.elements.iter().enumerate() {
            if index != 0 {
                output.push_str(", ");
            }
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
            let rendered = if let AggregateShape::Map(keys) = &layout.shape {
                let key = self.format_aggregate_element(keys[index], depth + 1, visiting)?;
                format!("{key}: {rendered}")
            } else {
                rendered
            };
            let suffix = if tuple && len == 1 { 2 } else { 1 };
            if output
                .len()
                .checked_add(rendered.len() + suffix)
                .is_none_or(|bytes| bytes > MAX_DISPLAY_BYTES)
            {
                return Err(VmError::DisplaySizeExceeded);
            }
            output.push_str(&rendered);
        }
        if tuple && len == 1 {
            output.push(',');
        }
        match layout.shape {
            AggregateShape::List => output.push(']'),
            AggregateShape::Map(_) => output.push('}'),
            AggregateShape::Tuple => output.push(')'),
            AggregateShape::Enum(_) if len != 0 => output.push(')'),
            AggregateShape::Enum(_) | AggregateShape::Error(_) | AggregateShape::Bytes(_) => {}
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
                .type_pool_read()
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
