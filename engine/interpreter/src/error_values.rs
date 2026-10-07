//! Authenticated Error envelopes. Scalar tag words are never GC reference slots.

use gc::{ObjectHeader, PayloadRole};
use runtime::TaggedValue;
use type_pool::{ErrorDomain, Intrinsic, TypeId, TypeIndex};

use crate::{Vm, VmError};

pub(super) const MAX_VALUE_DEPTH: usize = 128;

#[derive(Clone, Copy)]
pub(super) struct ErrorLayout {
    pub ty: TypeIndex,
    pub tag: TypeId,
    pub payload: TaggedValue,
}

impl Vm {
    pub(super) fn error_shape(
        &self,
        ty: TypeIndex,
    ) -> Result<(TypeIndex, Vec<TypeIndex>), VmError> {
        let ty = self.canonical_type(ty)?;
        let info = self.state.type_pool.get(ty);
        let shape = self
            .state
            .type_pool
            .error_shape(ty)
            .map_err(|_| VmError::InvalidType(ty))?
            .ok_or(VmError::InvalidType(ty))?;
        let errors = match shape.domain {
            ErrorDomain::Closed(errors) => errors,
            ErrorDomain::Open => vec![Intrinsic::Any.type_index()],
        };
        if info.size != 24 || info.align != 8 {
            return Err(VmError::UnsupportedLegacyErrorLayout(ty));
        }
        if errors.is_empty()
            || self.state.type_pool.identity_input().is_none()
            || self.state.type_pool.stable_type_id(ty).is_err()
        {
            return Err(VmError::InvalidType(ty));
        }
        for payload in std::iter::once(shape.inner).chain(
            errors
                .iter()
                .copied()
                .filter(|&ty| ty != Intrinsic::Any.type_index()),
        ) {
            let mut pending = vec![payload];
            let mut seen = std::collections::HashSet::new();
            while let Some(ty) = pending.pop() {
                let ty = self.canonical_type(ty)?;
                if !seen.insert(ty) {
                    continue;
                }
                match &self.state.type_pool.get(ty).kind {
                    type_pool::TypeKind::Trait { .. } => {
                        return Err(VmError::UnsupportedFunctionAbi);
                    }
                    type_pool::TypeKind::Optional { inner }
                    | type_pool::TypeKind::EffectQualified { inner, .. } => pending.push(*inner),
                    type_pool::TypeKind::Tuple { elements } => pending.extend(elements),
                    _ => {}
                }
            }
        }
        Ok((shape.inner, errors))
    }

    pub(super) fn error_member(
        &self,
        concrete: TypeIndex,
        members: &[TypeIndex],
    ) -> Result<bool, VmError> {
        if members == [Intrinsic::Any.type_index()] {
            return Ok(true);
        }
        let concrete = self.canonical_type(concrete)?;
        for &member in members {
            // Tags identify exact concrete families. Success payload conversion
            // can widen numbers; error membership never widens or retags them.
            if concrete == self.canonical_type(member)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn error_layout(
        &self,
        value: TaggedValue,
        depth: usize,
    ) -> Result<Option<ErrorLayout>, VmError> {
        if depth >= MAX_VALUE_DEPTH {
            return Err(VmError::TypeError);
        }
        let Some(pointer) = value.as_heap_ptr() else {
            return Ok(None);
        };
        // SAFETY: callers hold a mutator operation and keep this live value rooted.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        match header.payload_role() {
            Some(PayloadRole::Ordinary) => {
                let ty = self.canonical_type(header.type_index)?;
                if self
                    .state
                    .type_pool
                    .error_shape(ty)
                    .map_err(|_| VmError::InvalidType(ty))?
                    .is_some()
                {
                    return Err(VmError::TypeError);
                }
                return Ok(None);
            }
            Some(PayloadRole::ErrorEnvelope) if header.payload_words() == 3 => {}
            _ => return Err(VmError::TypeError),
        }
        let ty = self.canonical_type(header.type_index)?;
        let (inner, members) = self.error_shape(ty)?;
        // SAFETY: the authenticated role contains exactly three initialized words.
        let (tag, payload) = unsafe {
            (
                TypeId(
                    pointer.cast::<u64>().read(),
                    pointer.cast::<u64>().add(1).read(),
                ),
                TaggedValue::from_raw(pointer.cast::<u64>().add(2).read()),
            )
        };
        if tag == TypeId::ZERO {
            if !self.value_matches_type(payload, inner, depth + 1)? {
                return Err(VmError::TypeError);
            }
        } else {
            let concrete = self
                .state
                .type_pool
                .lookup_by_id(tag)
                .ok_or(VmError::TypeError)?;
            let actual = self
                .value_type_depth(payload, depth + 1)?
                .unwrap_or_else(|| self.state.type_pool.null_type());
            if concrete == Intrinsic::Any.type_index()
                || concrete == Intrinsic::NoReturn.type_index()
                || self.canonical_type(actual)? != concrete
                || self
                    .state
                    .type_pool
                    .stable_type_id(concrete)
                    .map_err(|_| VmError::TypeError)?
                    != tag
                || !self.error_member(concrete, &members)?
                || !self.value_matches_type(payload, concrete, depth + 1)?
            {
                return Err(VmError::TypeError);
            }
        }
        Ok(Some(ErrorLayout { ty, tag, payload }))
    }

    fn publish_error(
        &mut self,
        target: TypeIndex,
        tag: TypeId,
        payload: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        let root = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&[payload], |vm| {
            // SAFETY: converted payload is rooted; no safepoint occurs between
            // allocation, complete initialization and returned publication.
            let pointer = unsafe { vm.state.heap.alloc_error_envelope(target) }
                .ok_or(VmError::OutOfMemory)?;
            let payload = vm.roots.temporary_values.borrow()[root];
            // SAFETY: exactly three aligned words, with role set by the allocator.
            unsafe {
                pointer.as_ptr().cast::<u64>().write(tag.hi());
                pointer.as_ptr().cast::<u64>().add(1).write(tag.lo());
                pointer.as_ptr().cast::<u64>().add(2).write(payload.raw());
                Ok(TaggedValue::from_heap_ptr(pointer.as_ptr()))
            }
        })
    }

    pub(super) fn construct_error(
        &mut self,
        value: TaggedValue,
        target: TypeIndex,
        ok: bool,
    ) -> Result<TaggedValue, VmError> {
        let target = self.canonical_type(target)?;
        let (inner, members) = self.error_shape(target)?;
        let base = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&[value], |vm| {
            let value = vm.roots.temporary_values.borrow()[base];
            if ok {
                let payload = vm.cast_value(value, inner, 1)?;
                vm.publish_error(target, TypeId::ZERO, payload)
            } else {
                // Construction deliberately selects Err; payload type only
                // supplies its concrete identity, never the branch decision.
                let concrete = vm
                    .value_type_depth(value, 1)?
                    .unwrap_or_else(|| vm.state.type_pool.null_type());
                let concrete = vm.canonical_type(concrete)?;
                let tag = vm
                    .state
                    .type_pool
                    .stable_type_id(concrete)
                    .map_err(|_| VmError::TypeError)?;
                if tag == TypeId::ZERO
                    || concrete == Intrinsic::Any.type_index()
                    || concrete == Intrinsic::NoReturn.type_index()
                    || !vm.value_matches_type(value, concrete, 1)?
                    || !vm.error_member(concrete, &members)?
                {
                    return Err(VmError::TypeError);
                }
                vm.publish_error(target, tag, value)
            }
        })
    }

    pub(super) fn assert_error(
        &mut self,
        value: TaggedValue,
        target: TypeIndex,
        depth: usize,
    ) -> Result<TaggedValue, VmError> {
        let (inner, _) = self.error_shape(target)?;
        let payload = match self.error_layout(value, depth)? {
            Some(layout) if layout.tag != TypeId::ZERO => {
                return self.convert_error(value, target, depth);
            }
            Some(layout) => layout.payload,
            None => value,
        };
        if !self.value_matches_type(payload, inner, depth + 1)? {
            return Err(VmError::TypeError);
        }
        self.convert_error(value, target, depth)
    }

    pub(super) fn convert_error(
        &mut self,
        value: TaggedValue,
        target: TypeIndex,
        depth: usize,
    ) -> Result<TaggedValue, VmError> {
        let (inner, members) = self.error_shape(target)?;
        let base = self.roots.temporary_values.borrow().len();
        self.with_temporary_roots(&[value], |vm| {
            let value = vm.roots.temporary_values.borrow()[base];
            if let Some(layout) = vm.error_layout(value, depth + 1)? {
                if layout.tag != TypeId::ZERO {
                    let concrete = vm
                        .state
                        .type_pool
                        .lookup_by_id(layout.tag)
                        .ok_or(VmError::TypeError)?;
                    if !vm.error_member(concrete, &members)? {
                        return Err(VmError::TypeError);
                    }
                    if layout.ty == target {
                        return Ok(value);
                    }
                    return vm.publish_error(target, layout.tag, layout.payload);
                }
                let payload = vm.cast_value(layout.payload, inner, depth + 1)?;
                vm.publish_error(target, TypeId::ZERO, payload)
            } else {
                let payload = vm.cast_value(value, inner, depth + 1)?;
                vm.publish_error(target, TypeId::ZERO, payload)
            }
        })
    }

    pub(super) fn error_equal(
        &self,
        left: TaggedValue,
        right: TaggedValue,
        depth: usize,
    ) -> Result<Option<bool>, VmError> {
        match (
            self.error_layout(left, depth)?,
            self.error_layout(right, depth)?,
        ) {
            (None, None) => Ok(None),
            (Some(a), Some(b)) if a.tag == b.tag => {
                if let Some(result) = self.error_equal(a.payload, b.payload, depth + 1)? {
                    return Ok(Some(result));
                }
                if let Some(result) = self.enum_equal(a.payload, b.payload)? {
                    return Ok(Some(result));
                }
                self.scalar_equal(a.payload, b.payload).map(Some)
            }
            _ => Ok(Some(false)),
        }
    }
}
