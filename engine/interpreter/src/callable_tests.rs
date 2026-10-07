//! Dynamic apply/update use actual types, exact arity and ordinary VM frames.

use nsbc::{FuncId, Instruction, Reg};
use runtime::{FunctionCode, Number, TaggedValue};
use type_pool::{Intrinsic, MethodSlot, TypeId, TypeIndex, TypeInfo, TypeKind, VariantInfo};

use crate::{VmError, VmResult, tests::make_vm};

#[test]
fn apply_and_update_dispatch_for_heap_and_immediate_receiver_types() {
    for kind in [
        "struct",
        "enum",
        "payload_enum",
        "tuple",
        "string",
        "i64",
        "u64",
        "i128",
        "u128",
        "float",
        "bool",
        "char",
        "unit",
        "null",
        "type",
    ] {
        let mut vm = make_vm();
        let value = match kind {
            "struct" | "tuple" => {
                let ty = if kind == "struct" {
                    vm.state.type_pool.register(TypeInfo {
                        kind: TypeKind::Struct {
                            name: str_interner::intern("Callable"),
                            fields: vec![],
                        },
                        type_id: TypeId(412, 1),
                        size: 0,
                        align: 8,
                    })
                } else {
                    vm.state
                        .type_pool
                        .intern_structural(TypeKind::Tuple { elements: vec![] })
                };
                // SAFETY: TestVm holds an operation. This zero-field allocation
                // has no uninitialized slots and is published before allocating again.
                let pointer = unsafe { vm.state.heap.alloc_object(ty, 0) }.unwrap();
                unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) }
            }
            "enum" | "payload_enum" => {
                let fields = if kind == "enum" {
                    vec![]
                } else {
                    vec![type_pool::FieldInfo {
                        name: str_interner::intern("value"),
                        ty: Intrinsic::I64.type_index(),
                        offset: 0,
                        has_default: false,
                    }]
                };
                let ty = vm.state.type_pool.register(TypeInfo {
                    kind: TypeKind::Enum {
                        name: str_interner::intern("CallableEnum"),
                        variants: vec![VariantInfo {
                            name: str_interner::intern("item"),
                            tag: 0,
                            fields,
                        }],
                    },
                    type_id: TypeId(412, 2),
                    size: if kind == "enum" { 8 } else { 16 },
                    align: 8,
                });
                let tag = vm.enum_constant(ty, 0).unwrap();
                if kind == "enum" {
                    tag
                } else {
                    // SAFETY: initialize both words before publishing the live enum.
                    let pointer = unsafe { vm.state.heap.alloc_object(ty, 2) }.unwrap();
                    unsafe {
                        pointer.as_ptr().cast::<u64>().write(tag.raw());
                        pointer
                            .as_ptr()
                            .cast::<u64>()
                            .add(1)
                            .write(TaggedValue::from_i64(9).raw());
                        TaggedValue::from_heap_ptr(pointer.as_ptr())
                    }
                }
            }
            "string" => vm.alloc_string("callable").unwrap(),
            "i64" => TaggedValue::from_i64(7),
            "u64" => TaggedValue::from_u64(7),
            "i128" => vm.number_value(Number::I128(i128::MAX)).unwrap(),
            "u128" => vm.number_value(Number::U128(u128::MAX)).unwrap(),
            "float" => vm.number_value(Number::F64(7.1)).unwrap(),
            "bool" => TaggedValue::TRUE,
            "char" => TaggedValue::from_char('λ'),
            "unit" => TaggedValue::UNIT,
            "null" => TaggedValue::NULL,
            _ => vm.type_value(Intrinsic::Str.type_index()).unwrap(),
        };
        let ty = vm.reflected_type(value).unwrap();
        let apply = str_interner::intern("apply");
        let update = str_interner::intern("update");
        for (name, function) in [(apply, 1), (update, 2)] {
            vm.state.type_pool.add_method(
                ty,
                MethodSlot {
                    name,
                    func_id: function,
                    trait_impl: None,
                    visible_scope: None,
                    access: type_pool::MethodAccess::Public,
                },
            );
        }
        for (id, instructions, parameters) in [
            (
                0,
                vec![
                    Instruction::load_imm(Reg(0), 42),
                    Instruction::call_indirect(Reg(10), 1),
                    Instruction::mov(Reg(20), Reg(0)),
                    Instruction::load_imm(Reg(0), 2),
                    Instruction::load_imm(Reg(1), 40),
                    Instruction::call_method(Reg(10), update.as_u32(), 2),
                    Instruction::mov(Reg(0), Reg(20)),
                    Instruction::ret(Reg(0)),
                ],
                0,
            ),
            (1, vec![Instruction::ret(Reg(1))], 2),
            (
                2,
                vec![Instruction::load_unit(Reg(0)), Instruction::ret(Reg(0))],
                3,
            ),
        ] {
            vm.add_function(FunctionCode {
                display_owner: None,
                abi: None,
                func_id: FuncId(id),
                instructions: instructions.into_iter().map(Instruction::encode).collect(),
                register_count: 32,
                param_count: parameters,
                is_closure: false,
                function_type: TypeIndex::INVALID,
            });
        }
        let task = vm.spawn_root(FuncId(0));
        vm.roots
            .scheduler
            .get_task_mut(task)
            .unwrap()
            .registers
            .set(Reg(10), value);
        assert!(matches!(vm.run(), VmResult::Finished), "kind={kind}");
        assert_eq!(vm.task_result_i64(task).unwrap(), 42, "kind={kind}");
        assert_eq!(vm.active_stack_count(), 0);
    }
}

#[test]
fn dynamic_callers_reject_missing_methods_and_incomplete_full_arity() {
    for (registered, count) in [(false, 1), (true, 0), (true, 2), (true, 32), (true, 255)] {
        let mut vm = make_vm();
        let apply = str_interner::intern("apply");
        if registered {
            vm.state.type_pool.add_method(
                Intrinsic::Bool.type_index(),
                MethodSlot {
                    name: apply,
                    func_id: 1,
                    trait_impl: None,
                    visible_scope: None,
                    access: type_pool::MethodAccess::Public,
                },
            );
        }
        for (id, instructions, parameters) in [
            (
                0,
                vec![
                    Instruction::call_indirect(Reg(10), count),
                    Instruction::ret(Reg(0)),
                ],
                0,
            ),
            (1, vec![Instruction::ret(Reg(1))], 2),
        ] {
            vm.add_function(FunctionCode {
                display_owner: None,
                abi: None,
                func_id: FuncId(id),
                instructions: instructions.into_iter().map(Instruction::encode).collect(),
                register_count: 32,
                param_count: parameters,
                is_closure: false,
                function_type: TypeIndex::INVALID,
            });
        }
        let task = vm.spawn_root(FuncId(0));
        vm.roots
            .scheduler
            .get_task_mut(task)
            .unwrap()
            .registers
            .set(Reg(10), TaggedValue::TRUE);
        if registered {
            let expected = if count >= 32 { 31 } else { 1 };
            assert!(
                matches!(vm.run(), VmResult::Error(VmError::ArityError { expected: got_expected, got }) if got_expected == expected && got == count)
            );
        } else {
            assert!(matches!(vm.run(), VmResult::Error(VmError::MethodNotFound)));
        }
        assert_eq!(vm.active_stack_count(), 0);
    }
}
