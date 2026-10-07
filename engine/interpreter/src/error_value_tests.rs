use gc::ObjectHeader;
use runtime::TaggedValue;
use type_pool::{Intrinsic, TypeId, TypeIdentityInput, TypeIndex};

use crate::{
    VmError,
    tests::{TestVm, make_vm},
};

fn fixture() -> (TestVm, TypeIndex, TypeIndex, TypeIndex) {
    let mut vm = make_vm();
    let same = vm
        .state
        .type_pool
        .normalize_error_type(
            vec![Intrinsic::I64.type_index()],
            Intrinsic::I64.type_index(),
        )
        .unwrap();
    let heap = vm
        .state
        .type_pool
        .normalize_error_type(
            vec![Intrinsic::Str.type_index()],
            Intrinsic::Str.type_index(),
        )
        .unwrap();
    let open = vm
        .state
        .type_pool
        .normalize_error_type(
            vec![Intrinsic::Any.type_index()],
            Intrinsic::Any.type_index(),
        )
        .unwrap();
    vm.state
        .type_pool
        .finalize_type_identities(TypeIdentityInput {
            schema: 1,
            packages: vec![],
            declarations: vec![],
        })
        .unwrap();
    (vm, same, heap, open)
}

#[test]
fn overlap_constructors_lifting_and_casts_preserve_branch_and_exact_tag() {
    let (mut vm, same, _, open) = fixture();
    let value = TaggedValue::from_i64(42);
    let ok = vm.construct_error(value, same, true).unwrap();
    vm.roots.temporary_values.borrow_mut().push(ok);
    let err = vm.construct_error(value, same, false).unwrap();
    vm.roots.temporary_values.borrow_mut().push(err);
    assert_eq!(vm.error_layout(ok, 0).unwrap().unwrap().tag, TypeId::ZERO);
    let tag = vm
        .state
        .type_pool
        .stable_type_id(Intrinsic::I64.type_index())
        .unwrap();
    assert_eq!(vm.error_layout(err, 0).unwrap().unwrap().tag, tag);
    assert_eq!(vm.error_equal(ok, err, 0).unwrap(), Some(false));
    assert_eq!(vm.error_equal(err, err, 0).unwrap(), Some(true));
    assert!(!vm.value_matches_type(value, same, 0).unwrap());
    let lifted = vm.assert_value_type(value, same).unwrap();
    vm.roots.temporary_values.borrow_mut().push(lifted);
    assert_eq!(
        vm.error_layout(lifted, 0).unwrap().unwrap().tag,
        TypeId::ZERO
    );
    let widened = vm.cast_value(err, open, 0).unwrap();
    vm.roots.temporary_values.borrow_mut().push(widened);
    assert_eq!(vm.error_layout(widened, 0).unwrap().unwrap().tag, tag);
    assert_eq!(
        vm.cast_value(ok, Intrinsic::I64.type_index(), 0)
            .unwrap()
            .as_i64(),
        Some(42)
    );
    assert!(matches!(
        vm.cast_value(err, Intrinsic::I64.type_index(), 0),
        Err(VmError::TypeError)
    ));
}

#[test]
fn malformed_role_tag_payload_and_cyclic_envelopes_are_rejected_even_by_any() {
    let (mut vm, same, _, _) = fixture();
    let value = vm
        .construct_error(TaggedValue::from_i64(42), same, false)
        .unwrap();
    vm.roots.temporary_values.borrow_mut().push(value);
    let pointer = value.as_heap_ptr().unwrap().cast::<u64>().cast_mut();
    let original = vm.error_layout(value, 0).unwrap().unwrap().tag;
    // SAFETY: rooted test-only allocation, active operation, no collection during corruption.
    unsafe {
        pointer.write(original.hi() ^ 1);
    }
    assert!(
        vm.value_matches_type(value, Intrinsic::Any.type_index(), 0)
            .is_err()
    );
    unsafe {
        pointer.write(original.hi());
        pointer.add(2).write(TaggedValue::TRUE.raw());
    }
    assert!(vm.error_layout(value, 0).is_err());
    unsafe {
        pointer.write(0);
        pointer.add(1).write(0);
        pointer.add(2).write(value.raw());
    }
    assert!(vm.error_layout(value, 0).is_err());
    unsafe {
        pointer.add(2).write(TaggedValue::from_i64(42).raw());
    }
    let header = unsafe { ObjectHeader::from_payload_ptr_mut(pointer.cast()) };
    header.gc_meta &= !(7 << 19);
    assert!(vm.error_layout(value, 0).is_err());
}

#[test]
fn error_payloads_remain_rooted_through_actual_completed_collections() {
    let (mut vm, _, heap, open) = fixture();
    let text = vm.alloc_string("retained payload").unwrap();
    vm.roots.temporary_values.borrow_mut().push(text);
    let wrapped = vm.construct_error(text, heap, false).unwrap();
    vm.roots.temporary_values.borrow_mut().clear();
    vm.roots.temporary_values.borrow_mut().push(wrapped);
    for _ in 0..3 {
        let before = vm.completed_collections();
        assert!(vm.collect_garbage().unwrap());
        assert!(vm.completed_collections() > before);
        let wrapped = vm.roots.temporary_values.borrow()[0];
        let layout = vm.error_layout(wrapped, 0).unwrap().unwrap();
        assert_eq!(
            crate::builtin_ctx::heap_string_to_owned(layout.payload).as_deref(),
            Some("retained payload")
        );
    }
    for payload in [TaggedValue::NULL, TaggedValue::UNIT] {
        let wrapped = vm.construct_error(payload, open, false).unwrap();
        vm.roots.temporary_values.borrow_mut().push(wrapped);
        let layout = vm.error_layout(wrapped, 0).unwrap().unwrap();
        assert_ne!(layout.tag, TypeId::ZERO);
        assert_eq!(layout.payload, payload);
    }
}

#[test]
fn closed_error_membership_is_exact_even_for_numeric_subtypes() {
    let mut vm = crate::tests::make_vm();
    let source = vm
        .state
        .type_pool
        .normalize_error_type(
            vec![Intrinsic::I8.type_index()],
            Intrinsic::Any.type_index(),
        )
        .unwrap();
    let target = vm
        .state
        .type_pool
        .normalize_error_type(
            vec![Intrinsic::I64.type_index()],
            Intrinsic::Any.type_index(),
        )
        .unwrap();
    vm.state
        .type_pool
        .finalize_type_identities(type_pool::TypeIdentityInput {
            schema: 1,
            packages: vec![],
            declarations: vec![],
        })
        .unwrap();
    let value = vm
        .cast_value(TaggedValue::from_i64(42), Intrinsic::I8.type_index(), 0)
        .unwrap();
    vm.roots.temporary_values.borrow_mut().push(value);
    let wrapped = vm.construct_error(value, source, false).unwrap();
    vm.roots.temporary_values.borrow_mut().push(wrapped);
    let value = vm.roots.temporary_values.borrow()[0];
    let wrapped = vm.roots.temporary_values.borrow()[1];
    let tag = vm.error_layout(wrapped, 0).unwrap().unwrap().tag;
    assert_eq!(
        tag,
        vm.state
            .type_pool
            .stable_type_id(Intrinsic::I8.type_index())
            .unwrap()
    );
    assert!(matches!(
        vm.construct_error(value, target, false),
        Err(crate::VmError::TypeError)
    ));
    assert!(matches!(
        vm.cast_value(wrapped, target, 0),
        Err(crate::VmError::TypeError)
    ));
    assert_eq!(vm.error_layout(wrapped, 0).unwrap().unwrap().tag, tag);
}
