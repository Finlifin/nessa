//! Native facts used by the standard library's equality of Any values.
//!
//! The VM cannot call source `eq` implementations from a native operation, so
//! it only decides what its kernels can decide and exposes tuple elements;
//! same-type values are left to the Eq/PartialEq implementation visible at
//! the comparison site.

use runtime::{Number, TaggedValue};
use type_pool::TypeKind;

use crate::{Vm, VmError};

impl Vm {
    /// Values compared by the scalar kernel; payload-free enums and error
    /// envelopes have their own native comparisons.
    fn is_scalar_value(&self, value: TaggedValue) -> bool {
        // SAFETY: callers hold the execution operation and keep `value` rooted.
        let numeric = unsafe { Number::from_tagged(value) }.is_some();
        numeric
            || value.as_bool().is_some()
            || value.as_char().is_some()
            || value.as_type().is_some()
            || value.is_unit()
            || value.is_null()
            || crate::builtin_ctx::heap_string_to_owned(value).is_some()
    }

    /// `Some` when native kernels decide equality: scalars of any kinds
    /// (numbers compare across kinds; other kind mismatches are unequal),
    /// enums without payload, error envelopes, and a scalar against any heap
    /// value. `None` leaves two non-scalar values to source-level equality.
    pub(super) fn native_equal(
        &self,
        left: TaggedValue,
        right: TaggedValue,
    ) -> Result<Option<bool>, VmError> {
        if let Some(equal) = self.error_equal(left, right, 0)? {
            return Ok(Some(equal));
        }
        match self.enum_equal(left, right) {
            Ok(Some(equal)) => return Ok(Some(equal)),
            Err(VmError::UnsupportedEnumEquality) => return Ok(None),
            Err(error) => return Err(error),
            Ok(None) => {}
        }
        match (self.is_scalar_value(left), self.is_scalar_value(right)) {
            (true, true) => self.scalar_equal(left, right).map(Some),
            (true, false) | (false, true) => Ok(Some(false)),
            (false, false) => Ok(None),
        }
    }

    /// Element count of a tuple object, or `None` for any other value.
    pub(super) fn tuple_length(&self, value: TaggedValue) -> Result<Option<usize>, VmError> {
        let Some(ty) = self.value_type(value)? else {
            return Ok(None);
        };
        let ty = self.canonical_type(ty)?;
        if !matches!(self.state.type_pool.get(ty).kind, TypeKind::Tuple { .. }) {
            return Ok(None);
        }
        self.tuple_values(value).map(|values| Some(values.len()))
    }

    pub(super) fn tuple_element(
        &self,
        value: TaggedValue,
        index: i64,
    ) -> Result<TaggedValue, VmError> {
        let values = self.tuple_values(value)?;
        usize::try_from(index)
            .ok()
            .and_then(|index| values.get(index).copied())
            .ok_or(VmError::InvalidIndex)
    }
}

#[cfg(test)]
mod tests {
    use type_pool::{Intrinsic, TypeKind};

    use crate::tests::make_vm;
    use crate::{Vm, VmError};
    use runtime::TaggedValue;

    #[test]
    fn native_equality_decides_scalars_and_defers_heap_pairs() {
        let mut fixture = make_vm();
        let vm: &mut Vm = &mut fixture;
        let one = TaggedValue::from_i64(1);
        let text = vm.alloc_string("1").unwrap();
        let other_text = vm.alloc_string("1").unwrap();
        let tuple_type = vm.state.type_pool.intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::I64.type_index()],
        });
        // SAFETY: the test VM holds an operation; the one-word payload is
        // initialized before any other allocation can move or collect it.
        let tuple = unsafe {
            let payload = vm.state.heap.alloc_object(tuple_type, 1).unwrap();
            payload.as_ptr().cast::<u64>().write(one.raw());
            TaggedValue::from_heap_ptr(payload.as_ptr())
        };

        assert_eq!(vm.native_equal(one, one).unwrap(), Some(true));
        assert_eq!(vm.native_equal(text, other_text).unwrap(), Some(true));
        assert_eq!(vm.native_equal(one, text).unwrap(), Some(false));
        assert_eq!(
            vm.native_equal(TaggedValue::TRUE, TaggedValue::UNIT)
                .unwrap(),
            Some(false)
        );
        assert_eq!(
            vm.native_equal(TaggedValue::NULL, tuple).unwrap(),
            Some(false)
        );
        assert_eq!(vm.native_equal(tuple, tuple).unwrap(), None);

        assert_eq!(vm.tuple_length(tuple).unwrap(), Some(1));
        assert_eq!(vm.tuple_length(text).unwrap(), None);
        assert_eq!(vm.tuple_length(one).unwrap(), None);
        assert_eq!(vm.tuple_element(tuple, 0).unwrap(), one);
        for index in [1, -1] {
            assert!(matches!(
                vm.tuple_element(tuple, index),
                Err(VmError::InvalidIndex)
            ));
        }
        assert!(matches!(vm.tuple_element(text, 0), Err(VmError::TypeError)));
    }
}
