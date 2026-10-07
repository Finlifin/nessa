//! Stack-switching instructions for the delimited continuation ABI.

use nsbc::{FuncId, Instruction, Opcode, Reg};
use runtime::{ContinuationId, PromptId, TaggedValue, TaskId};

use crate::{DispatchResult, Vm, VmError, e_payload};

fn prompt_id(value: TaggedValue) -> Option<PromptId> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|value| u64::try_from(value).ok()))
        .and_then(|value| u32::try_from(value).ok())
        .map(PromptId)
}

impl Vm {
    pub(super) fn dispatch_continuation_lifecycle(
        &mut self,
        task_id: TaskId,
        instr: &Instruction,
    ) -> DispatchResult {
        let payload = e_payload(instr);
        let (first, second) = Instruction::e_control_regs(payload);
        let drop = instr.opcode == Opcode::DropContinuation;
        if payload & 0xFFF != 0 || (drop && second.0 != 0) {
            return DispatchResult::Error(VmError::InvalidInstruction(instr.encode()));
        }
        let task = self
            .roots
            .scheduler
            .get_task_mut(task_id)
            .expect("dispatched task exists");
        let Some(handle) = task
            .registers
            .get(if drop { first } else { second })
            .as_u64()
        else {
            return DispatchResult::Error(VmError::TypeError);
        };
        let id = ContinuationId::from_u64(handle);
        if drop {
            match task.stacks.discard(id) {
                Ok(()) => DispatchResult::Continue,
                Err(error) => DispatchResult::Error(VmError::Stack(error)),
            }
        } else {
            match task.stacks.fork(id) {
                Ok(fork) => {
                    task.registers
                        .set(first, TaggedValue::from_u64(fork.as_u64()));
                    DispatchResult::Continue
                }
                Err(error) => DispatchResult::Error(VmError::Stack(error)),
            }
        }
    }

    pub(super) fn dispatch_reset(
        &mut self,
        task_id: TaskId,
        instr: &Instruction,
    ) -> DispatchResult {
        let payload = e_payload(instr);
        let (prompt_reg, count) = Instruction::e_control_regs(payload);
        let body = FuncId(payload & 0xFFF);
        if count.0 as usize > runtime::GP_REGISTER_COUNT {
            return DispatchResult::Error(VmError::TypeError);
        }
        if let Err(error) = self.check_entry_environment(body, None) {
            return DispatchResult::Error(error);
        }
        if let Err(error) = self.check_user_argument_abi(body, count.0 as usize) {
            return DispatchResult::Error(error);
        }
        let Some(function) = self.state.bytecode.try_get_function(body) else {
            return DispatchResult::Error(VmError::InvalidFunction(body));
        };
        if function.param_count != count.0 {
            return DispatchResult::Error(VmError::ArityError {
                expected: function.param_count,
                got: count.0,
            });
        }
        let task = self
            .roots
            .scheduler
            .get_task_mut(task_id)
            .expect("dispatched task exists");
        let Some(prompt) = prompt_id(task.registers.get(prompt_reg)) else {
            return DispatchResult::Error(VmError::TypeError);
        };
        // Only explicit arguments cross the boundary, never caller registers or frames.
        let mut args = [TaggedValue::UNIT; runtime::GP_REGISTER_COUNT];
        args[..count.0 as usize].copy_from_slice(&task.registers.regs[..count.0 as usize]);
        match task
            .stacks
            .enter_delimiter(prompt, body, &args[..count.0 as usize])
        {
            Ok(()) => DispatchResult::Continue,
            Err(error) => DispatchResult::Error(VmError::Stack(error)),
        }
    }

    pub(super) fn dispatch_shift(
        &mut self,
        task_id: TaskId,
        instr: &Instruction,
    ) -> DispatchResult {
        let payload = e_payload(instr);
        if payload & 0xFFF != 0 {
            return DispatchResult::Error(VmError::InvalidInstruction(instr.encode()));
        }
        let (prompt_reg, destination) = Instruction::e_control_regs(payload);
        let task = self
            .roots
            .scheduler
            .get_task_mut(task_id)
            .expect("dispatched task exists");
        let Some(prompt) = prompt_id(task.registers.get(prompt_reg)) else {
            return DispatchResult::Error(VmError::TypeError);
        };
        match task.stacks.capture(prompt, destination) {
            Ok(handle) => {
                task.registers
                    .set(Reg(0), TaggedValue::from_u64(handle.as_u64()));
                DispatchResult::Continue
            }
            Err(error) => DispatchResult::Error(VmError::Stack(error)),
        }
    }

    pub(super) fn dispatch_resume(
        &mut self,
        task_id: TaskId,
        instr: &Instruction,
    ) -> DispatchResult {
        let payload = e_payload(instr);
        if payload & 0xFFF != 0 {
            return DispatchResult::Error(VmError::InvalidInstruction(instr.encode()));
        }
        let (continuation, value) = Instruction::e_control_regs(payload);
        let task = self
            .roots
            .scheduler
            .get_task_mut(task_id)
            .expect("dispatched task exists");
        let Some(handle) = task.registers.get(continuation).as_u64() else {
            return DispatchResult::Error(VmError::TypeError);
        };
        let value = task.registers.get(value);
        match task.stacks.resume(ContinuationId::from_u64(handle), value) {
            Ok(()) => DispatchResult::Continue,
            Err(error) => DispatchResult::Error(VmError::Stack(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VmResult;
    use runtime::FunctionCode;

    fn add_function(vm: &mut Vm, instructions: &[Instruction], params: u8) {
        let id = FuncId(vm.state.bytecode.function_count() as u32);
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: None,
            func_id: id,
            instructions: instructions
                .iter()
                .map(|instruction| instruction.encode())
                .collect(),
            register_count: 32,
            param_count: params,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
    }

    fn run(functions: &[(&[Instruction], u8)]) -> (VmResult, Option<i64>, usize) {
        let mut vm = crate::tests::make_vm();
        for (instructions, params) in functions {
            add_function(&mut vm, instructions, *params);
        }
        let task = vm.spawn_root(FuncId(0));
        let result = vm.run();
        let value = vm
            .roots
            .scheduler
            .get_task(task)
            .unwrap()
            .registers
            .get(Reg(0))
            .as_i64();
        (
            result,
            value,
            vm.roots.scheduler.stack_pool().active_count(),
        )
    }

    #[test]
    fn delimiter_returns_to_caller_without_overwriting_caller_registers() {
        let main = [
            Instruction::load_imm(Reg(20), 7),
            Instruction::load_imm(Reg(19), 100),
            Instruction::load_imm(Reg(0), 5),
            Instruction::reset(Reg(20), 1, 1),
            Instruction::add(Reg(0), Reg(0), Reg(19)),
            Instruction::ret(Reg(0)),
        ];
        let body = [
            Instruction::load_imm(Reg(19), 999),
            Instruction::load_imm(Reg(1), 2),
            Instruction::r_type(Opcode::Mul, Reg(0), Reg(0), Reg(1)),
            Instruction::ret(Reg(0)),
        ];
        let (result, value, slots) = run(&[(&main, 0), (&body, 1)]);
        assert!(matches!(result, VmResult::Finished));
        assert_eq!(value, Some(110));
        assert_eq!(slots, 0);
    }

    #[test]
    fn capture_resume_restores_ordinary_call_frames() {
        let main = [
            Instruction::load_imm(Reg(20), 7),
            Instruction::reset(Reg(20), 1, 0),
            Instruction::mov(Reg(21), Reg(0)),
            Instruction::load_imm(Reg(1), 40),
            Instruction::resume(Reg(21), Reg(1)),
            Instruction::ret(Reg(0)),
        ];
        let body = [
            Instruction::load_imm(Reg(19), 2),
            Instruction::call(2, 0),
            Instruction::add(Reg(0), Reg(0), Reg(19)),
            Instruction::ret(Reg(0)),
        ];
        let callee = [
            Instruction::load_imm(Reg(20), 7),
            Instruction::load_imm(Reg(19), 999),
            Instruction::shift(Reg(20), Reg(0)),
            Instruction::ret(Reg(0)),
        ];
        let (result, value, slots) = run(&[(&main, 0), (&body, 0), (&callee, 0)]);
        assert!(matches!(result, VmResult::Finished));
        assert_eq!(value, Some(42));
        assert_eq!(slots, 0);
    }

    #[test]
    fn multi_shot_branching_returns_ten_plus_twenty() {
        let main = [
            Instruction::load_imm(Reg(20), 7),
            Instruction::reset(Reg(20), 1, 0),
            Instruction::mov(Reg(21), Reg(0)),
            Instruction::clone_continuation(Reg(22), Reg(21)),
            Instruction::load_imm(Reg(1), 1),
            Instruction::resume(Reg(22), Reg(1)),
            Instruction::mov(Reg(23), Reg(0)),
            Instruction::load_imm(Reg(1), 2),
            Instruction::resume(Reg(21), Reg(1)),
            Instruction::add(Reg(0), Reg(0), Reg(23)),
            Instruction::ret(Reg(0)),
        ];
        let body = [
            Instruction::load_imm(Reg(20), 7),
            Instruction::shift(Reg(20), Reg(0)),
            Instruction::load_imm(Reg(1), 10),
            Instruction::r_type(Opcode::Mul, Reg(0), Reg(0), Reg(1)),
            Instruction::ret(Reg(0)),
        ];
        let (result, value, slots) = run(&[(&main, 0), (&body, 0)]);
        assert!(matches!(result, VmResult::Finished));
        assert_eq!(value, Some(30));
        assert_eq!(slots, 0);
    }

    #[test]
    fn outer_prompt_capture_restores_nested_delimiter_chain() {
        let main = [
            Instruction::load_imm(Reg(20), 1),
            Instruction::reset(Reg(20), 1, 0),
            Instruction::load_imm(Reg(1), 40),
            Instruction::resume(Reg(0), Reg(1)),
            Instruction::ret(Reg(0)),
        ];
        let outer = [
            Instruction::load_imm(Reg(20), 2),
            Instruction::reset(Reg(20), 2, 0),
            Instruction::load_imm(Reg(1), 2),
            Instruction::add(Reg(0), Reg(0), Reg(1)),
            Instruction::ret(Reg(0)),
        ];
        let inner = [
            Instruction::load_imm(Reg(20), 1),
            Instruction::shift(Reg(20), Reg(0)),
            Instruction::ret(Reg(0)),
        ];
        let (result, value, slots) = run(&[(&main, 0), (&outer, 0), (&inner, 0)]);
        assert!(matches!(result, VmResult::Finished));
        assert_eq!(value, Some(42));
        assert_eq!(slots, 0);
    }

    #[test]
    fn missing_prompt_returns_error_and_releases_task_stacks() {
        let main = [
            Instruction::load_imm(Reg(20), 1),
            Instruction::shift(Reg(20), Reg(0)),
        ];
        let (result, _, slots) = run(&[(&main, 0)]);
        assert!(matches!(
            result,
            VmResult::Error(VmError::Stack(runtime::StackError::MissingPrompt(
                PromptId(1)
            )))
        ));
        assert_eq!(slots, 0);
    }

    #[test]
    fn unknown_reset_target_reports_error() {
        let main = [
            Instruction::load_imm(Reg(20), 1),
            Instruction::reset(Reg(20), 999, 0),
        ];
        let (result, _, slots) = run(&[(&main, 0)]);
        assert!(matches!(
            result,
            VmResult::Error(VmError::InvalidFunction(FuncId(999)))
        ));
        assert_eq!(slots, 0);
    }
}
