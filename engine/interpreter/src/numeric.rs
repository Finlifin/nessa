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

    /// Coherent 64-bit hash across scalar values (`Unit`, `null`, `bool`, `char`,
    /// `String`, `Type`, payload-free `Enum`, and all 14 numeric types).
    ///
    /// Equal numbers under `scalar_equal` always produce the exact same `u64`.
    pub(crate) fn scalar_hash(&self, value: TaggedValue) -> Result<u64, VmError> {
        if value.is_unit() {
            return Ok(mix_u64(0x554e_4954_0000_0001));
        }
        if value.is_null() {
            return Ok(mix_u64(0x4e55_4c4c_0000_0002));
        }
        if let Some(flag) = value.as_bool() {
            return Ok(mix_u64(0x424f_4f4c_0000_0000 ^ u64::from(flag)));
        }
        if let Some(ch) = value.as_char() {
            return Ok(mix_u64(0x4348_4152_0000_0000 ^ u64::from(ch as u32)));
        }
        if let Some(ty) = value.as_type() {
            let canonical = self.canonical_type(ty)?;
            return Ok(mix_u64(
                0x5459_5045_0000_0000 ^ u64::from(canonical.as_u32()),
            ));
        }
        if let Some((ty, variant)) = value.as_enum() {
            if self.enum_layout(value)?.is_none() {
                return Err(VmError::TypeError);
            }
            let canonical = self.canonical_type(ty)?;
            let combined = (u64::from(canonical.as_u32()) << 32) | u64::from(variant);
            return Ok(mix_u64(0x454e_554d_0000_0000 ^ combined));
        }
        // SAFETY: callers retain `value` in registered VM roots within an operation.
        if let Some(number) = unsafe { Number::from_tagged(value) } {
            return Ok(hash_canonical_number(number));
        }
        if let Some(s) = crate::builtin_ctx::heap_string_to_owned(value) {
            return Ok(hash_string_bytes(s.as_bytes()));
        }
        if let Ok(layout) = self.bytes_layout(value) {
            return Ok(hash_raw_bytes(layout.as_slice()));
        }
        Err(VmError::TypeError)
    }
}

fn mix_u64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

fn hash_nonneg_u128(v: u128) -> u64 {
    let lo = mix_u64((v as u64) ^ 0x4e55_4d2b_5f30_3031);
    let hi = mix_u64((v >> 64) as u64);
    mix_u64(lo ^ hi.rotate_left(29))
}

fn hash_neg_i128(v: i128) -> u64 {
    debug_assert!(v < 0);
    let bits = v as u128;
    let lo = mix_u64((bits as u64) ^ 0x4e55_4d2d_5f30_3032);
    let hi = mix_u64((bits >> 64) as u64);
    mix_u64(lo ^ hi.rotate_left(29))
}

fn hash_canonical_number(number: Number) -> u64 {
    // 2^128 as IEEE-754 binary64 (`u128::MAX as f64` rounds up to exactly 2^128).
    const TWO_POW_128: f64 = 340282366920938463463374607431768211456.0;
    // -2^127 as IEEE-754 binary64 (`i128::MIN` is exact in f64).
    const MIN_I128_F64: f64 = -170141183460469231731687303715884105728.0;

    match number {
        Number::I64(v) => {
            if v >= 0 {
                hash_nonneg_u128(v as u128)
            } else {
                hash_neg_i128(i128::from(v))
            }
        }
        Number::U64(v) => hash_nonneg_u128(u128::from(v)),
        Number::I128(v) => {
            if v >= 0 {
                hash_nonneg_u128(v as u128)
            } else {
                hash_neg_i128(v)
            }
        }
        Number::U128(v) => hash_nonneg_u128(v),
        Number::F64(x) => {
            if x.is_nan() {
                return mix_u64(0x7ff8_0000_dead_beef);
            }
            if x == 0.0 {
                // Covers both +0.0 and -0.0 so -0.0 == +0.0 == 0 share one hash.
                return hash_nonneg_u128(0);
            }
            if x.is_finite() && x.fract() == 0.0 {
                if (0.0..TWO_POW_128).contains(&x) {
                    return hash_nonneg_u128(x as u128);
                }
                if (MIN_I128_F64..0.0).contains(&x) {
                    return hash_neg_i128(x as i128);
                }
            }
            mix_u64(x.to_bits() ^ 0x464c_5436_345f_6269)
        }
    }
}

fn hash_string_bytes(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ (bytes.len() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    for &b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    mix_u64(h ^ 0x5354_5247_0000_0000)
}

fn hash_raw_bytes(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ (bytes.len() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    for &b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    mix_u64(h ^ 0x4259_5453_0000_0000)
}
