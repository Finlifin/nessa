//! Field operations for compiler-derived methods on the VM's TaggedValue layout.

use std::cmp::Ordering;

use gc::ObjectHeader;
use runtime::{Number, TaggedValue};
use type_pool::{TypeIndex, TypeKind};

use crate::builtin_ctx::{format_tagged_value, heap_string_to_owned};
use crate::{Vm, VmError};

/// Callers hold an execution operation and keep `value` in the task's roots.
/// There are no allocations or safepoints while this pointer is read.
fn struct_payload(
    value: TaggedValue,
    ty: TypeIndex,
    count: usize,
) -> Result<*const TaggedValue, VmError> {
    let pointer = value.as_heap_ptr().ok_or(VmError::TypeError)?;
    // SAFETY: the caller supplies a rooted live VM value. Check its layout
    // before allowing any field read, including a dynamically typed `other`.
    let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
    if !header.is_ordinary() {
        return Err(VmError::TypeError);
    }
    if header.type_index != ty || header.payload_words() != count {
        return Err(VmError::TypeError);
    }
    Ok(pointer.cast())
}

fn compare_values(a: TaggedValue, b: TaggedValue) -> Result<Option<Ordering>, VmError> {
    // SAFETY: callers read initialized fields of rooted live structs while
    // holding the execution operation; scalar decoding does not allocate.
    match unsafe { (Number::from_tagged(a), Number::from_tagged(b)) } {
        (Some(a), Some(b)) => return Ok(a.partial_cmp_numeric(b)),
        (Some(_), None) | (None, Some(_)) => return Err(VmError::TypeError),
        _ => {}
    }
    if let (Some(a), Some(b)) = (a.as_bool(), b.as_bool()) {
        return Ok(Some(a.cmp(&b)));
    }
    if let (Some(a), Some(b)) = (a.as_char(), b.as_char()) {
        return Ok(Some(a.cmp(&b)));
    }
    if let (Some(a), Some(b)) = (heap_string_to_owned(a), heap_string_to_owned(b)) {
        return Ok(Some(a.cmp(&b)));
    }
    if (a.is_unit() && b.is_unit()) || (a.is_null() && b.is_null()) {
        return Ok(Some(Ordering::Equal));
    }
    if let (Some(a), Some(b)) = (a.as_type(), b.as_type()) {
        // Type descriptors support equality; raw pool indices do not define Ord.
        return Ok((a == b).then_some(Ordering::Equal));
    }
    Err(VmError::TypeError)
}

impl Vm {
    pub(crate) fn derived_struct_eq(
        &self,
        a: TaggedValue,
        b: TaggedValue,
        ty: TypeIndex,
    ) -> Result<bool, VmError> {
        let TypeKind::Struct { fields, .. } = &self.state.type_pool.get(ty).kind else {
            return Err(VmError::TypeError);
        };
        let left = struct_payload(a, ty, fields.len())?;
        let right = struct_payload(b, ty, fields.len())?;
        for index in 0..fields.len() {
            // SAFETY: checked headers establish exactly fields.len() aligned,
            // initialized TaggedValue slots. No safepoint occurs in this loop.
            let (a, b) = unsafe { (left.add(index).read(), right.add(index).read()) };
            if compare_values(a, b)? != Some(Ordering::Equal) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) fn derived_struct_cmp(
        &self,
        a: TaggedValue,
        b: TaggedValue,
        ty: TypeIndex,
    ) -> Result<i64, VmError> {
        let TypeKind::Struct { fields, .. } = &self.state.type_pool.get(ty).kind else {
            return Err(VmError::TypeError);
        };
        let left = struct_payload(a, ty, fields.len())?;
        let right = struct_payload(b, ty, fields.len())?;
        for index in 0..fields.len() {
            // SAFETY: both checked payloads contain this initialized slot.
            let (a, b) = unsafe { (left.add(index).read(), right.add(index).read()) };
            match compare_values(a, b)? {
                Some(Ordering::Less) => return Ok(-1),
                Some(Ordering::Greater) => return Ok(1),
                Some(Ordering::Equal) => {}
                None => return Err(VmError::TypeError),
            }
        }
        Ok(0)
    }

    pub(crate) fn derived_struct_to_string(
        &self,
        value: TaggedValue,
        ty: TypeIndex,
    ) -> Result<String, VmError> {
        let TypeKind::Struct { name, fields, .. } = &self.state.type_pool.get(ty).kind else {
            return Err(VmError::TypeError);
        };
        let pointer = struct_payload(value, ty, fields.len())?;
        let mut parts = Vec::with_capacity(fields.len());
        for (index, field) in fields.iter().enumerate() {
            // SAFETY: the checked payload contains this initialized slot. Only
            // owned host strings are allocated while the payload is borrowed.
            let value = unsafe { pointer.add(index).read() };
            parts.push(format!(
                "{}: {}",
                str_interner::try_get(field.name).ok_or(VmError::TypeError)?,
                format_tagged_value(value)
            ));
        }
        Ok(format!(
            "{} {{ {} }}",
            str_interner::try_get(*name).ok_or(VmError::TypeError)?,
            parts.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{VmResult, tests::make_vm};
    use nsbc::{FuncId, Instruction, Opcode, Reg};
    use runtime::FunctionCode;
    use type_pool::{FieldInfo, Intrinsic, TypeId, TypeInfo};

    #[test]
    fn legacy_derived_methods_check_arity_and_never_forge_hash_results() {
        for (method, count) in [
            ("eq", 0),
            ("eq", 2),
            ("cmp", 0),
            ("hash", 0),
            ("to_string", 1),
        ] {
            let mut vm = make_vm();
            let ty = vm.state.type_pool.register(TypeInfo {
                kind: TypeKind::Struct {
                    name: str_interner::intern("Empty"),
                    fields: vec![],
                },
                type_id: TypeId(401, 1),
                size: 0,
                align: 8,
            });
            let name = str_interner::intern(method);
            vm.state.type_pool.add_method(
                ty,
                type_pool::MethodSlot {
                    name,
                    func_id: type_pool::DERIVE_FUNC_ID,
                    trait_impl: None,
                    visible_scope: None,
                    access: type_pool::MethodAccess::Public,
                },
            );
            vm.add_function(FunctionCode {
                display_owner: None,
                abi: None,
                func_id: FuncId(0),
                instructions: [
                    Instruction::new_object(Reg(20), ty),
                    Instruction::call_method(Reg(20), name.as_u32(), count),
                ]
                .into_iter()
                .map(Instruction::encode)
                .collect(),
                register_count: 32,
                param_count: 0,
                is_closure: false,
                function_type: type_pool::TypeIndex::INVALID,
            });
            vm.spawn_root(FuncId(0));
            assert!(matches!(
                (method, vm.run()),
                (
                    "hash",
                    VmResult::Error(VmError::UnsupportedDerivedMethod("hash"))
                ) | (_, VmResult::Error(VmError::ArityError { .. }))
            ));
        }
    }

    #[test]
    fn signed_native_method_rejects_custom_trait_before_execution() {
        let mut vm = make_vm();
        let receiver = vm.state.type_pool.register(TypeInfo {
            kind: TypeKind::Struct {
                name: str_interner::intern("NativeEmpty"),
                fields: vec![],
            },
            type_id: TypeId(932, 1),
            size: 0,
            align: 8,
        });
        let fake = vm.state.type_pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("Eq"),
                parents: vec![],
                assoc_types: vec![],
            },
            type_id: TypeId(932, 2),
            size: 0,
            align: 0,
        });
        let declaration = vm.state.type_pool.intern_structural(TypeKind::Function {
            params: vec![fake, fake],
            ret: Intrinsic::Bool.type_index(),
        });
        let name = str_interner::intern("eq");
        vm.state
            .type_pool
            .register_trait_schema(type_pool::TraitDispatchSchema {
                trait_type: fake,
                slots: vec![type_pool::TraitMethodKey {
                    trait_owner: fake,
                    name,
                    signature: Some(type_pool::TraitMethodSignature {
                        associated_paths: Vec::new(),
                        declaration,
                        self_paths: vec![
                            vec![type_pool::TraitTypeStep::Parameter(0)],
                            vec![type_pool::TraitTypeStep::Parameter(1)],
                        ],
                        parameter_kinds: vec![
                            type_pool::TraitParameterKind::Receiver,
                            type_pool::TraitParameterKind::Required,
                        ],
                    }),
                }],
            })
            .unwrap();
        vm.state.type_pool.add_method(
            receiver,
            type_pool::MethodSlot {
                name,
                func_id: type_pool::DERIVE_FUNC_ID,
                trait_impl: Some(fake),
                visible_scope: None,
                access: type_pool::MethodAccess::Public,
            },
        );
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: None,
            func_id: FuncId(0),
            instructions: [
                Instruction::new_object(Reg(20), receiver),
                Instruction::call_method(Reg(20), name.as_u32(), 1),
            ]
            .into_iter()
            .map(Instruction::encode)
            .collect(),
            register_count: 32,
            param_count: 0,
            is_closure: false,
            function_type: TypeIndex::INVALID,
        });
        vm.spawn_root(FuncId(0));
        assert!(matches!(
            vm.run(),
            VmResult::Error(VmError::UnsupportedDerivedMethod(
                "untrusted native contract"
            ))
        ));
    }

    #[test]
    fn legacy_eq_and_scalar_comparison_reject_heap_identity_without_eq() {
        let mut vm = make_vm();
        let inner = vm.state.type_pool.register(TypeInfo {
            kind: TypeKind::Struct {
                name: str_interner::intern("NoEq"),
                fields: vec![],
            },
            type_id: TypeId(402, 1),
            size: 0,
            align: 8,
        });
        let outer = vm.state.type_pool.register(TypeInfo {
            kind: TypeKind::Struct {
                name: str_interner::intern("Legacy"),
                fields: vec![FieldInfo {
                    name: str_interner::intern("object"),
                    ty: inner,
                    offset: 0,
                    has_default: false,
                }],
            },
            type_id: TypeId(402, 2),
            size: 8,
            align: 8,
        });
        let name = str_interner::intern("eq");
        vm.state.type_pool.add_method(
            outer,
            type_pool::MethodSlot {
                name,
                func_id: type_pool::DERIVE_FUNC_ID,
                trait_impl: None,
                visible_scope: None,
                access: type_pool::MethodAccess::Public,
            },
        );
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: None,
            func_id: FuncId(0),
            instructions: [
                Instruction::new_object(Reg(10), inner),
                Instruction::new_object(Reg(20), outer),
                Instruction::store_field(Reg(20), 0, Reg(10)),
                Instruction::mov(Reg(0), Reg(20)),
                Instruction::call_method(Reg(20), name.as_u32(), 1),
            ]
            .into_iter()
            .map(Instruction::encode)
            .collect(),
            register_count: 32,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        vm.spawn_root(FuncId(0));
        assert!(matches!(vm.run(), VmResult::Error(VmError::TypeError)));
        drop(vm);
        let mut vm = make_vm();
        let ty = vm.state.type_pool.register(TypeInfo {
            kind: TypeKind::Tuple {
                elements: vec![Intrinsic::Any.type_index()],
            },
            type_id: TypeId(403, 1),
            size: 8,
            align: 8,
        });
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: None,
            func_id: FuncId(0),
            instructions: [
                Instruction::new_object(Reg(10), ty),
                Instruction::r_type(Opcode::CmpEq, Reg(0), Reg(10), Reg(10)),
            ]
            .into_iter()
            .map(Instruction::encode)
            .collect(),
            register_count: 32,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        vm.spawn_root(FuncId(0));
        assert!(matches!(
            vm.run(),
            VmResult::Error(VmError::UnsupportedEquality)
        ));
    }
}
