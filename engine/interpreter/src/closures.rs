//! Checked closure payload access, shared by calls, handlers and delimiters.

use gc::ObjectHeader;
use nsbc::FuncId;
use runtime::{ClosureEnv, TaggedValue};
use type_pool::Intrinsic;

use crate::{Vm, VmError};

pub(super) struct ClosureLayout {
    pub pointer: *const u8,
    pub function: FuncId,
    pub capture_count: u32,
}

impl Vm {
    /// Caller holds an operation and keeps the value rooted while using layout.
    pub(super) fn closure_layout(value: TaggedValue) -> Result<ClosureLayout, VmError> {
        let pointer = value.as_heap_ptr().ok_or(VmError::TypeError)?;
        // SAFETY: VM values are initialized live allocations during an operation.
        let header = unsafe { ObjectHeader::from_payload_ptr(pointer) };
        if !header.is_ordinary()
            || header.type_index != Intrinsic::Closure.type_index()
            || header.payload_words() < 2
        {
            return Err(VmError::TypeError);
        }
        // SAFETY: the header proves that both closure metadata words exist.
        let capture_count = unsafe { ClosureEnv::capture_count(pointer) };
        if capture_count as usize > header.payload_words() - 2 {
            return Err(VmError::TypeError);
        }
        Ok(ClosureLayout {
            pointer,
            // SAFETY: the initialized metadata prefix was checked above.
            function: unsafe { ClosureEnv::func_id(pointer) },
            capture_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use nsbc::{FunctionAbi, Instruction, Reg};
    use runtime::FunctionCode;
    use type_pool::TypeIndex;

    use super::*;
    use crate::VmResult;
    use crate::tests::make_vm;

    fn add(
        vm: &mut Vm,
        id: u32,
        code: &[Instruction],
        abi: FunctionAbi,
        is_closure: bool,
        params: u8,
    ) {
        vm.add_function(FunctionCode {
            display_owner: None,
            abi,
            func_id: FuncId(id),
            instructions: code
                .iter()
                .map(|instruction| instruction.encode())
                .collect(),
            register_count: 32,
            param_count: params,
            is_closure,
            function_type: TypeIndex::INVALID,
        });
    }

    #[test]
    fn closure_entry_receives_captures_before_explicit_parameters() {
        let mut vm = make_vm();
        add(
            &mut vm,
            0,
            &[
                Instruction::load_imm(Reg(0), 10),
                Instruction::load_imm(Reg(1), 20),
                Instruction::new_closure(Reg(3), 1, 2),
                Instruction::load_imm(Reg(0), 12),
                Instruction::call_indirect(Reg(3), 1),
                Instruction::ret(Reg(0)),
            ],
            FunctionAbi::value_only(0),
            false,
            0,
        );
        add(
            &mut vm,
            1,
            &[
                Instruction::add(Reg(0), Reg(0), Reg(1)),
                Instruction::add(Reg(0), Reg(0), Reg(2)),
                Instruction::ret(Reg(0)),
            ],
            FunctionAbi::closure_values(2, 1),
            true,
            3,
        );
        let task = vm.spawn_root(FuncId(0));
        let result = vm.run();
        assert!(matches!(result, VmResult::Finished), "{result:?}");
        assert_eq!(vm.task_result_i64(task).unwrap(), 42);
    }

    #[test]
    fn ordinary_callees_do_not_inherit_caller_closure_captures() {
        let mut vm = make_vm();
        add(
            &mut vm,
            0,
            &[
                Instruction::load_imm(Reg(0), 42),
                Instruction::new_closure(Reg(1), 1, 1),
                Instruction::call_indirect(Reg(1), 0),
                Instruction::ret(Reg(0)),
            ],
            FunctionAbi::value_only(0),
            false,
            0,
        );
        add(
            &mut vm,
            1,
            &[Instruction::call(2, 0), Instruction::ret(Reg(0))],
            FunctionAbi::closure_values(1, 0),
            true,
            1,
        );
        add(
            &mut vm,
            2,
            &[Instruction::ret(Reg(0))],
            FunctionAbi::closure_values(1, 0),
            true,
            1,
        );
        vm.spawn_root(FuncId(0));
        assert!(matches!(vm.run(), VmResult::Error(VmError::TypeError)));
    }

    #[test]
    fn delimiter_entry_receives_its_own_closure_captures_as_entry_parameters() {
        let mut vm = make_vm();
        add(
            &mut vm,
            0,
            &[
                Instruction::load_imm(Reg(0), 40),
                Instruction::load_imm(Reg(1), 2),
                Instruction::new_closure(Reg(2), 1, 2),
                Instruction::reset_closure(Reg(2), 0),
                Instruction::ret(Reg(0)),
            ],
            FunctionAbi::value_only(0),
            false,
            0,
        );
        add(
            &mut vm,
            1,
            &[
                Instruction::add(Reg(0), Reg(0), Reg(1)),
                Instruction::ret(Reg(0)),
            ],
            FunctionAbi::closure_values(2, 0),
            true,
            2,
        );
        let task = vm.spawn_root(FuncId(0));
        let result = vm.run();
        assert!(matches!(result, VmResult::Finished), "{result:?}");
        assert_eq!(vm.task_result_i64(task).unwrap(), 42);
    }

    #[test]
    fn closure_metadata_cannot_claim_words_outside_the_object() {
        let mut vm = make_vm();
        for words in [0, 1, 2] {
            // SAFETY: the fixture holds an operation and does not allocate or
            // collect while inspecting the newly initialized test payload.
            let pointer = unsafe {
                vm.state
                    .heap
                    .alloc_object(Intrinsic::Closure.type_index(), words)
            }
            .unwrap();
            // SAFETY: write only the words reserved by this test allocation.
            unsafe {
                if words > 0 {
                    pointer.as_ptr().cast::<u64>().write(0);
                }
                if words > 1 {
                    pointer.as_ptr().cast::<u64>().add(1).write(1);
                }
            }
            // SAFETY: the pointer is aligned and belongs to this live allocation.
            let value = unsafe { TaggedValue::from_heap_ptr(pointer.as_ptr()) };
            assert!(matches!(Vm::closure_layout(value), Err(VmError::TypeError)));
        }
    }
}
