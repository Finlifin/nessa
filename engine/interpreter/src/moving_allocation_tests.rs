//! Exercise collections inside allocation slow paths, not only at VM safepoints.
use runtime::TaggedValue;
use type_pool::{FieldInfo, Intrinsic, TypeId, TypeInfo, TypeKind, VariantInfo};

use crate::{Vm, builtin_ctx::heap_string_to_owned, tests::make_vm};

fn root(vm: &mut Vm, value: TaggedValue) -> usize {
    let mut roots = vm.roots.temporary_values.borrow_mut();
    let index = roots.len();
    roots.push(value);
    index
}

fn load(vm: &Vm, index: usize) -> TaggedValue {
    vm.roots.temporary_values.borrow()[index]
}

#[test]
fn allocation_slow_paths_reload_moved_receivers_and_payloads() {
    const MARKER: &str = "NESSA_ALLOCATION_MOVEMENT_CHILD";
    if std::env::var_os(MARKER).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "moving_allocation_tests::allocation_slow_paths_reload_moved_receivers_and_payloads", "--nocapture"])
            .env(MARKER, "1")
            .env("MMTK_STRESS_FACTOR", "1")
            .env("MMTK_PRECISE_STRESS", "true")
            .env("MMTK_IMMIX_ALWAYS_DEFRAG", "true")
            .env("MMTK_IMMIX_DEFRAG_EVERY_BLOCK", "true")
            .output().unwrap();
        eprintln!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }
    let mut fixture = make_vm();
    let vm: &mut Vm = &mut fixture;
    let field = |name: &str, ty, offset| FieldInfo {
        name: str_interner::intern(name),
        ty,
        has_default: false,
        offset,
    };
    let tuple = vm.state.type_pool.intern_structural(TypeKind::Tuple {
        elements: vec![Intrinsic::F64.type_index()],
    });
    let choice = vm.state.type_pool.register(TypeInfo {
        kind: TypeKind::Enum {
            name: str_interner::intern("MovingChoice"),
            variants: vec![VariantInfo {
                name: str_interner::intern("some"),
                fields: vec![
                    field("text", Intrinsic::Str.type_index(), 0),
                    field("number", tuple, 1),
                ],
                tag: 0,
            }],
        },
        type_id: TypeId(811, 42),
        size: 24,
        align: 8,
    });
    let receiver_type = vm.state.type_pool.intern_structural(TypeKind::Tuple {
        elements: vec![tuple],
    });
    let before = vm.completed_collections();
    let string = vm.alloc_string("kept payload").unwrap();
    let text = root(vm, string);
    let start = vm.completed_collections();
    let old_text = load(vm, text).raw();
    let first = vm.new_list(1).unwrap();
    assert!(
        vm.completed_collections() > start,
        "collection inside nonempty List construction"
    );
    assert_ne!(
        old_text,
        load(vm, text).raw(),
        "heap payload moved during construction"
    );
    let list = root(vm, first);
    vm.list_set(load(vm, list), TaggedValue::from_u64(0), load(vm, text))
        .unwrap();
    let old_list = load(vm, list).raw();
    let start = vm.completed_collections();
    vm.list_push(load(vm, list), load(vm, text)).unwrap();
    assert!(
        vm.completed_collections() > start,
        "collection inside List growth"
    );
    assert_ne!(
        load(vm, list).raw(),
        old_list,
        "List receiver moved during buffer allocation"
    );
    assert_eq!(
        heap_string_to_owned(
            vm.list_get(load(vm, list), TaggedValue::from_u64(1))
                .unwrap()
        )
        .as_deref(),
        Some("kept payload")
    );
    let start = vm.completed_collections();
    let allocated = vm.new_map(4).unwrap();
    assert!(
        vm.completed_collections() > start,
        "collection inside Map construction"
    );
    root(vm, allocated);
    let fresh = vm.new_map(0).unwrap();
    let map = root(vm, fresh);
    let old_map = load(vm, map).raw();
    let start = vm.completed_collections();
    vm.map_set(load(vm, map), load(vm, text), load(vm, list))
        .unwrap();
    assert!(
        vm.completed_collections() > start,
        "collection inside Map growth"
    );
    assert_ne!(
        load(vm, map).raw(),
        old_map,
        "Map receiver moved during buffer allocation"
    );
    assert_eq!(
        vm.map_get(load(vm, map), load(vm, text)).unwrap(),
        load(vm, list)
    );
    let start = vm.completed_collections();
    let keys = vm.map_keys(load(vm, map)).unwrap();
    assert!(
        vm.completed_collections() > start,
        "collection inside keys List allocation"
    );
    assert_eq!(
        heap_string_to_owned(vm.list_get(keys, TaggedValue::from_u64(0)).unwrap()).as_deref(),
        Some("kept payload")
    );
    let raw_tuple_type = vm.state.type_pool.intern_structural(TypeKind::Tuple {
        elements: vec![Intrinsic::I64.type_index()],
    });
    let pointer = unsafe { vm.state.heap.alloc_object(raw_tuple_type, 1) }.unwrap();
    // SAFETY: initialize the fresh one-word allocation before another allocation.
    unsafe {
        pointer
            .as_ptr()
            .cast::<u64>()
            .write(TaggedValue::from_i64(42).raw());
    }
    let input = root(vm, unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) });
    let argument_type = vm.state.type_pool.intern_structural(TypeKind::Tuple {
        elements: vec![Intrinsic::Str.type_index(), raw_tuple_type],
    });
    let pointer = unsafe { vm.state.heap.alloc_object(argument_type, 2) }.unwrap();
    // SAFETY: initialize both fresh fields from roots updated by this allocation.
    unsafe {
        pointer.as_ptr().cast::<u64>().write(load(vm, text).raw());
        pointer
            .as_ptr()
            .cast::<u64>()
            .add(1)
            .write(load(vm, input).raw());
    }
    let arguments = root(vm, unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) });
    let start = vm.completed_collections();
    let value = vm
        .new_enum(
            TaggedValue::from_enum(choice, 0).unwrap(),
            load(vm, arguments),
        )
        .unwrap();
    assert!(
        vm.completed_collections() > start,
        "collection inside enum field conversion/allocation"
    );
    let enumeration = root(vm, value);
    assert_eq!(
        heap_string_to_owned(vm.enum_field(load(vm, enumeration), 0).unwrap()).as_deref(),
        Some("kept payload")
    );
    let number = vm.enum_field(load(vm, enumeration), 1).unwrap();
    assert_eq!(
        unsafe { runtime::Number::from_tagged(vm.load_field_value(number, 0).unwrap()) },
        Some(runtime::Number::F64(42.0))
    );
    let pointer = unsafe { vm.state.heap.alloc_object(receiver_type, 1) }.unwrap();
    unsafe {
        pointer
            .as_ptr()
            .cast::<u64>()
            .write(TaggedValue::UNIT.raw());
    }
    let receiver = root(vm, unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) });
    let old = load(vm, receiver).raw();
    let start = vm.completed_collections();
    vm.store_field_value(load(vm, receiver), 0, load(vm, input))
        .unwrap();
    assert!(
        vm.completed_collections() > start,
        "collection inside StoreField conversion"
    );
    assert_ne!(old, load(vm, receiver).raw());
    let stored = vm.load_field_value(load(vm, receiver), 0).unwrap();
    assert_eq!(
        unsafe { runtime::Number::from_tagged(vm.load_field_value(stored, 0).unwrap()) },
        Some(runtime::Number::F64(42.0))
    );
    eprintln!(
        "allocation-path completed epochs={}",
        vm.completed_collections() - before
    );
}
