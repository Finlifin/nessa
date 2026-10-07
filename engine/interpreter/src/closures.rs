//! Checked closure payload access, shared by calls, handlers and delimiters.

use gc::ObjectHeader;
use nsbc::FuncId;
use runtime::{ClosureEnv, TaggedValue, TaskId};
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

    pub(super) fn load_capture_value(
        &self,
        task: TaskId,
        index: u32,
    ) -> Result<TaggedValue, VmError> {
        let context = self
            .roots
            .scheduler
            .get_task(task)
            .expect("dispatched task exists");
        let environment = match context.call_stack.last() {
            Some(frame) => frame.closure_env,
            None => context.stacks.active().entry_closure_env,
        }
        .ok_or(VmError::TypeError)?;
        let layout = Self::closure_layout(environment)?;
        self.check_entry_environment(context.current_func, Some(environment))?;
        if index >= layout.capture_count {
            return Err(VmError::TypeError);
        }
        // SAFETY: the live closure's header and capture index are checked.
        Ok(unsafe { ClosureEnv::get_capture(layout.pointer, index) })
    }
}

#[cfg(test)]
mod tests {
    use nsbc::{Instruction, Reg};
    use runtime::FunctionCode;
    use type_pool::TypeIndex;

    use super::*;
    use crate::VmResult;
    use crate::tests::make_vm;

    fn add(vm: &mut Vm, id: u32, code: &[Instruction], is_closure: bool, params: u8) {
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: None,
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
    fn capture_reads_use_the_current_closure_and_check_the_array_bound() {
        for index in [0, 1] {
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
                false,
                0,
            );
            add(
                &mut vm,
                1,
                &[
                    Instruction::load_capture(Reg(0), index),
                    Instruction::ret(Reg(0)),
                ],
                true,
                1,
            );
            let task = vm.spawn_root(FuncId(0));
            let result = vm.run();
            if index == 0 {
                assert!(matches!(result, VmResult::Finished), "{result:?}");
                assert_eq!(vm.task_result_i64(task).unwrap(), 42);
            } else {
                assert!(
                    matches!(result, VmResult::Error(VmError::TypeError)),
                    "{result:?}"
                );
            }
        }
    }

    #[test]
    fn ordinary_callees_cannot_read_an_outer_closures_environment() {
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
            false,
            0,
        );
        add(
            &mut vm,
            1,
            &[Instruction::call(2, 0), Instruction::ret(Reg(0))],
            true,
            1,
        );
        add(
            &mut vm,
            2,
            &[
                Instruction::load_capture(Reg(0), 0),
                Instruction::ret(Reg(0)),
            ],
            false,
            0,
        );
        vm.spawn_root(FuncId(0));
        assert!(matches!(vm.run(), VmResult::Error(VmError::TypeError)));
    }

    #[test]
    fn delimiter_entry_keeps_its_own_closure_environment() {
        let mut vm = make_vm();
        add(
            &mut vm,
            0,
            &[
                Instruction::load_imm(Reg(0), 42),
                Instruction::new_closure(Reg(1), 1, 1),
                Instruction::reset_closure(Reg(1), 0),
                Instruction::ret(Reg(0)),
            ],
            false,
            0,
        );
        add(
            &mut vm,
            1,
            &[
                Instruction::load_capture(Reg(0), 0),
                Instruction::ret(Reg(0)),
            ],
            true,
            1,
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
