//! Exercise collections inside allocation slow paths, not only at VM safepoints.
use runtime::TaggedValue;
use type_pool::{FieldInfo, Intrinsic, TypeId, TypeInfo, TypeKind, VariantInfo};

use crate::{Vm, builtin_ctx::heap_string_to_owned, tests::make_vm};

fn root(vm: &mut Vm, value: TaggedValue) -> usize {
    let mut roots = vm.worker_roots().temporary_values.borrow_mut();
    let index = roots.len();
    roots.push(value);
    index
}

fn load(vm: &Vm, index: usize) -> TaggedValue {
    vm.worker_roots().temporary_values.borrow()[index]
}

#[test]
fn allocation_slow_paths_reload_moved_receivers_and_payloads() {
    const MARKER: &str = "NESSA_ALLOCATION_MOVEMENT_CHILD";
    if std::env::var_os(MARKER).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "moving_allocation_tests::allocation_slow_paths_reload_moved_receivers_and_payloads",
                "--nocapture",
            ])
            .env(MARKER, "1")
            .env("MMTK_STRESS_FACTOR", "1")
            .env("MMTK_PRECISE_STRESS", "true")
            .env("MMTK_IMMIX_ALWAYS_DEFRAG", "true")
            .env("MMTK_IMMIX_DEFRAG_EVERY_BLOCK", "true")
            .output()
            .unwrap();
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
    let (choice, receiver_type) = {
        let mut type_pool = vm.type_pool_write();
        let tuple = type_pool.intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::F64.type_index()],
        });
        let choice = type_pool.register(TypeInfo {
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
        let receiver_type = type_pool.intern_structural(TypeKind::Tuple {
            elements: vec![tuple],
        });
        (choice, receiver_type)
    };
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
    let raw_tuple_type = vm.type_pool_write().intern_structural(TypeKind::Tuple {
        elements: vec![Intrinsic::I64.type_index()],
    });
    let pointer = unsafe { vm.heap_mut().alloc_object(raw_tuple_type, 1) }.unwrap();
    // SAFETY: initialize the fresh one-word allocation before another allocation.
    unsafe {
        pointer
            .as_ptr()
            .cast::<u64>()
            .write(TaggedValue::from_i64(42).raw());
    }
    let input = root(vm, unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) });
    let argument_type = vm.type_pool_write().intern_structural(TypeKind::Tuple {
        elements: vec![Intrinsic::Str.type_index(), raw_tuple_type],
    });
    let pointer = unsafe { vm.heap_mut().alloc_object(argument_type, 2) }.unwrap();
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
    let pointer = unsafe { vm.heap_mut().alloc_object(receiver_type, 1) }.unwrap();
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

#[test]
fn native_value_builder_survives_stress_moving_gc_and_scalar_hash_is_cross_type_coherent() {
    const MARKER: &str = "NESSA_NATIVE_VALUE_MOVEMENT_CHILD";
    if std::env::var_os(MARKER).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "moving_allocation_tests::native_value_builder_survives_stress_moving_gc_and_scalar_hash_is_cross_type_coherent",
                "--nocapture",
            ])
            .env(MARKER, "1")
            .env("MMTK_STRESS_FACTOR", "1")
            .env("MMTK_PRECISE_STRESS", "true")
            .env("MMTK_IMMIX_ALWAYS_DEFRAG", "true")
            .env("MMTK_IMMIX_DEFRAG_EVERY_BLOCK", "true")
            .output()
            .unwrap();
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
    let task = vm.spawn_root(nsbc::FuncId(0));
    let before = vm.completed_collections();

    let complex = crate::NativeValue::Tuple(vec![
        crate::NativeValue::I64(0),
        crate::NativeValue::OptStr(Some("alpha_payload".to_string())),
        crate::NativeValue::OptStr(None),
        crate::NativeValue::OptF64(Some(3.5)),
        crate::NativeValue::List(vec![
            crate::NativeValue::Str("first_item".to_string()),
            crate::NativeValue::Str("second_item".to_string()),
            crate::NativeValue::U64(u64::MAX),
        ]),
        crate::NativeValue::StringMap(vec![
            ("k1".to_string(), crate::NativeValue::Str("v1".to_string())),
            ("k2".to_string(), crate::NativeValue::Str("v2".to_string())),
        ]),
    ]);

    let (built, _) = vm.alloc_native(task, complex).unwrap();
    let built_root = root(vm, built);
    assert!(
        vm.completed_collections() > before + 5,
        "expected multiple moving collections inside nested NativeValue build"
    );

    let elems = vm.tuple_values(load(vm, built_root)).unwrap();
    assert_eq!(elems[0].as_i64(), Some(0));
    assert_eq!(
        heap_string_to_owned(elems[1]).as_deref(),
        Some("alpha_payload")
    );
    assert!(elems[2].is_null());
    assert_eq!(elems[3].as_f64(), Some(3.5));

    let list_root = root(vm, elems[4]);
    let map_root = root(vm, elems[5]);
    assert_eq!(
        heap_string_to_owned(
            vm.list_get(load(vm, list_root), TaggedValue::from_u64(0))
                .unwrap()
        )
        .as_deref(),
        Some("first_item")
    );
    assert_eq!(
        heap_string_to_owned(
            vm.list_get(load(vm, list_root), TaggedValue::from_u64(1))
                .unwrap()
        )
        .as_deref(),
        Some("second_item")
    );
    let k2 = vm.alloc_string("k2").unwrap();
    let k2_root = root(vm, k2);
    assert_eq!(
        heap_string_to_owned(vm.map_get(load(vm, map_root), load(vm, k2_root)).unwrap()).as_deref(),
        Some("v2")
    );

    // Verify SCALAR_HASH cross-numeric coherence across ALL 14 numeric intrinsics!
    let mut hashes_for_one = Vec::new();
    for kind in [
        Intrinsic::I8,
        Intrinsic::I16,
        Intrinsic::I32,
        Intrinsic::I64,
        Intrinsic::I128,
        Intrinsic::Isize,
        Intrinsic::U8,
        Intrinsic::U16,
        Intrinsic::U32,
        Intrinsic::U64,
        Intrinsic::U128,
        Intrinsic::Usize,
        Intrinsic::F32,
        Intrinsic::F64,
    ] {
        let casted = vm
            .cast_value(TaggedValue::from_i64(1), kind.type_index(), 0)
            .unwrap();
        let r = root(vm, casted);
        hashes_for_one.push((kind, vm.scalar_hash(load(vm, r)).unwrap()));
    }
    let expected_one = hashes_for_one[0].1;
    for (kind, h) in &hashes_for_one {
        assert_eq!(
            *h, expected_one,
            "hash(1) mismatch for numeric kind {kind:?}"
        );
    }

    // Zero coherence: -0.0f32 == -0.0f64 == +0.0f64 == 0i8 == 0u128
    let neg_zero_f32 = vm
        .cast_value(TaggedValue::from_f64(-0.0), Intrinsic::F32.type_index(), 0)
        .unwrap();
    let nz32_r = root(vm, neg_zero_f32);
    let zero_u128 = vm
        .cast_value(TaggedValue::from_i64(0), Intrinsic::U128.type_index(), 0)
        .unwrap();
    let z128_r = root(vm, zero_u128);
    let h_zero = vm.scalar_hash(TaggedValue::from_i64(0)).unwrap();
    assert_eq!(vm.scalar_hash(TaggedValue::from_f64(-0.0)).unwrap(), h_zero);
    assert_eq!(vm.scalar_hash(TaggedValue::from_f64(0.0)).unwrap(), h_zero);
    assert_eq!(vm.scalar_hash(load(vm, nz32_r)).unwrap(), h_zero);
    assert_eq!(vm.scalar_hash(load(vm, z128_r)).unwrap(), h_zero);

    // Fractional float coherence: 0.25f32 == 0.25f64
    let q_f32 = vm
        .cast_value(TaggedValue::from_f64(0.25), Intrinsic::F32.type_index(), 0)
        .unwrap();
    let q32_r = root(vm, q_f32);
    assert_eq!(
        vm.scalar_hash(load(vm, q32_r)).unwrap(),
        vm.scalar_hash(TaggedValue::from_f64(0.25)).unwrap()
    );

    // Negative integer / float coherence: -5i32 == -5i128 == -5.0f32 == -5.0f64
    let neg5_i32 = vm
        .cast_value(TaggedValue::from_i64(-5), Intrinsic::I32.type_index(), 0)
        .unwrap();
    let n32_r = root(vm, neg5_i32);
    let neg5_i128 = vm
        .cast_value(TaggedValue::from_i64(-5), Intrinsic::I128.type_index(), 0)
        .unwrap();
    let n128_r = root(vm, neg5_i128);
    let neg5_f32 = vm
        .cast_value(TaggedValue::from_f64(-5.0), Intrinsic::F32.type_index(), 0)
        .unwrap();
    let nf32_r = root(vm, neg5_f32);
    let h_neg5 = vm.scalar_hash(TaggedValue::from_f64(-5.0)).unwrap();
    assert_eq!(vm.scalar_hash(load(vm, n32_r)).unwrap(), h_neg5);
    assert_eq!(vm.scalar_hash(load(vm, n128_r)).unwrap(), h_neg5);
    assert_eq!(vm.scalar_hash(load(vm, nf32_r)).unwrap(), h_neg5);
}
