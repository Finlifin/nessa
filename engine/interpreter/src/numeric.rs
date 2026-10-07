//! Numeric allocation and instruction execution within the VM's root discipline.

use std::cmp::Ordering;

use nsbc::{Instruction, Opcode, Reg};
use runtime::{Number, NumericError, TaggedValue, TaskId};
use type_pool::Intrinsic;

use crate::{DispatchResult, Vm, VmError, r_regs};

impl From<NumericError> for VmError {
    fn from(error: NumericError) -> Self {
        match error {
            NumericError::Overflow => Self::NumericOverflow,
            NumericError::DivisionByZero => Self::DivisionByZero,
            NumericError::TypeError => Self::TypeError,
            NumericError::InvalidShift => Self::InvalidShift,
        }
    }
}

impl Vm {
    /// Return a scalar snapshot, never a managed pointer or borrowed payload.
    pub fn task_result_number(&mut self, task: TaskId) -> Result<Number, VmError> {
        let _operation = self.operation();
        let value = self
            .roots
            .scheduler
            .get_task(task)
            .ok_or(VmError::InvalidTask)?
            .registers
            .get(Reg(0));
        // SAFETY: the task keeps this value rooted and the operation prevents
        // collection during this read. Number owns only copied scalar data.
        unsafe { Number::from_tagged(value) }.ok_or(VmError::TypeError)
    }

    /// Allocate if necessary; callers must publish before any further safepoint.
    pub(crate) fn number_value(&mut self, number: Number) -> Result<TaggedValue, VmError> {
        let (kind, words, bits) = match number {
            Number::I64(value) => {
                if let Some(value) = TaggedValue::try_from_i64(value) {
                    return Ok(value);
                }
                (Intrinsic::I64, 1, value as u64 as u128)
            }
            Number::U64(value) => {
                if let Some(value) = TaggedValue::try_from_u64(value) {
                    return Ok(value);
                }
                (Intrinsic::U64, 1, value as u128)
            }
            Number::I128(value) => (Intrinsic::I128, 2, value as u128),
            Number::U128(value) => (Intrinsic::U128, 2, value),
            Number::F64(value) => {
                if let Some(value) = TaggedValue::try_from_f64(value) {
                    return Ok(value);
                }
                (Intrinsic::F64, 1, value.to_bits() as u128)
            }
        };
        // SAFETY: callers hold execution operations. Inputs are owned scalar
        // snapshots; GC can scan existing roots while allocation is stopped.
        let payload = unsafe { self.state.heap.alloc_object(kind.type_index(), words) }
            .ok_or(VmError::OutOfMemory)?;
        // SAFETY: this fresh allocation has one or two aligned u64 words.
        // No allocation or safepoint occurs between initialization and publication.
        unsafe {
            payload.as_ptr().cast::<u64>().write(bits as u64);
            if words == 2 {
                payload
                    .as_ptr()
                    .cast::<u64>()
                    .add(1)
                    .write((bits >> 64) as u64);
            }
            Ok(TaggedValue::from_heap_ptr(payload.as_ptr()))
        }
    }

    pub(crate) fn dispatch_numeric(&mut self, task: TaskId, instr: Instruction) -> DispatchResult {
        let (destination, left, right) = r_regs(&instr);
        let result = (|| {
            let registers = &self
                .roots
                .scheduler
                .get_task(task)
                .ok_or(VmError::InvalidTask)?
                .registers;
            // SAFETY: execution owns the mutator operation; register values are
            // rooted, and both reads finish before the next managed allocation.
            let a =
                unsafe { Number::from_tagged(registers.get(left)) }.ok_or(VmError::TypeError)?;
            let result = match instr.opcode {
                Opcode::Neg => a.checked_neg()?,
                Opcode::BitNot => a.bit_not()?,
                operation => {
                    let b = unsafe { Number::from_tagged(registers.get(right)) }
                        .ok_or(VmError::TypeError)?;
                    a.binary(b, operation)?
                }
            };
            self.number_value(result)
        })();
        match result {
            Ok(value) => {
                self.roots
                    .scheduler
                    .get_task_mut(task)
                    .unwrap()
                    .registers
                    .set(destination, value);
                DispatchResult::Continue
            }
            Err(error) => DispatchResult::Error(error),
        }
    }

    /// Equality shared by scalar bytecode and the standard-library primitive.
    /// Both operands are live, rooted VM values under the execution operation.
    pub(super) fn scalar_equal(
        &self,
        left: TaggedValue,
        right: TaggedValue,
    ) -> Result<bool, VmError> {
        // SAFETY: callers retain rooted, initialized values and hold an operation.
        if let (Some(left), Some(right)) =
            unsafe { (Number::from_tagged(left), Number::from_tagged(right)) }
        {
            return Ok(left.partial_cmp_numeric(right) == Some(Ordering::Equal));
        }
        if let (Some(left), Some(right)) = (
            crate::builtin_ctx::heap_string_to_owned(left),
            crate::builtin_ctx::heap_string_to_owned(right),
        ) {
            return Ok(left == right);
        }
        if left.is_heap() && right.is_heap() {
            return Err(VmError::UnsupportedEquality);
        }
        Ok(left == right)
    }

    /// Partial ordering for supported live scalar values under an operation.
    pub(super) fn scalar_partial_cmp(
        &self,
        left: TaggedValue,
        right: TaggedValue,
    ) -> Result<Option<Ordering>, VmError> {
        // SAFETY: callers retain rooted, initialized operands under an operation.
        if let (Some(left), Some(right)) =
            unsafe { (Number::from_tagged(left), Number::from_tagged(right)) }
        {
            return Ok(left.partial_cmp_numeric(right));
        }
        if let (Some(left), Some(right)) = (left.as_bool(), right.as_bool()) {
            return Ok(Some(left.cmp(&right)));
        }
        if let (Some(left), Some(right)) = (left.as_char(), right.as_char()) {
            return Ok(Some(left.cmp(&right)));
        }
        if left.is_unit() && right.is_unit() {
            return Ok(Some(Ordering::Equal));
        }
        if let (Some(left), Some(right)) = (
            crate::builtin_ctx::heap_string_to_owned(left),
            crate::builtin_ctx::heap_string_to_owned(right),
        ) {
            return Ok(Some(left.cmp(&right)));
        }
        Err(VmError::TypeError)
    }

    pub(crate) fn dispatch_comparison(
        &mut self,
        task: TaskId,
        instr: Instruction,
    ) -> DispatchResult {
        let (destination, left, right) = r_regs(&instr);
        let registers = &self.roots.scheduler.get_task(task).unwrap().registers;
        let (a, b) = (registers.get(left), registers.get(right));
        if matches!(instr.opcode, Opcode::CmpEq | Opcode::CmpNe) {
            match self.error_equal(a, b, 0) {
                Ok(Some(equal)) => {
                    self.roots
                        .scheduler
                        .get_task_mut(task)
                        .unwrap()
                        .registers
                        .set(
                            destination,
                            TaggedValue::from_bool(equal == (instr.opcode == Opcode::CmpEq)),
                        );
                    return DispatchResult::Continue;
                }
                Err(error) => return DispatchResult::Error(error),
                Ok(None) => {}
            }
            match self.enum_equal(a, b) {
                Ok(Some(equal)) => {
                    self.roots
                        .scheduler
                        .get_task_mut(task)
                        .unwrap()
                        .registers
                        .set(
                            destination,
                            TaggedValue::from_bool(equal == (instr.opcode == Opcode::CmpEq)),
                        );
                    return DispatchResult::Continue;
                }
                Err(error) => return DispatchResult::Error(error),
                Ok(None) => {}
            }
            match self.scalar_equal(a, b) {
                Ok(equal) => {
                    self.roots
                        .scheduler
                        .get_task_mut(task)
                        .unwrap()
                        .registers
                        .set(
                            destination,
                            TaggedValue::from_bool(equal == (instr.opcode == Opcode::CmpEq)),
                        );
                    return DispatchResult::Continue;
                }
                Err(error) => return DispatchResult::Error(error),
            }
        }
        let ordering = match self.scalar_partial_cmp(a, b) {
            Ok(ordering) => ordering,
            Err(error) => return DispatchResult::Error(error),
        };
        let result = match instr.opcode {
            Opcode::CmpLt => ordering == Some(Ordering::Less),
            Opcode::CmpLe => matches!(ordering, Some(Ordering::Less | Ordering::Equal)),
            Opcode::CmpGt => ordering == Some(Ordering::Greater),
            Opcode::CmpGe => matches!(ordering, Some(Ordering::Greater | Ordering::Equal)),
            _ => return DispatchResult::Error(VmError::InvalidInstruction(instr.encode())),
        };
        self.roots
            .scheduler
            .get_task_mut(task)
            .unwrap()
            .registers
            .set(destination, TaggedValue::from_bool(result));
        DispatchResult::Continue
    }
}
