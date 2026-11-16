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
            .scheduler()
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
        let payload = unsafe { self.worker.heap.alloc_object(kind.type_index(), words) }
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
        let (a_val, b_val) = {
            let Some(t) = self.task_ref(task) else {
                return DispatchResult::Error(VmError::InvalidTask);
            };
            (t.registers.get(left), t.registers.get(right))
        };

        // Fast path 1: 57-bit signed SMI (`IMM_I_SMALL`)
        if let Some(a) = a_val.as_i64() {
            if instr.opcode == Opcode::Neg {
                if let Some(res) = a.checked_neg().and_then(TaggedValue::try_from_i64) {
                    if let Some(t) = self.task_mut(task) {
                        t.registers.set(destination, res);
                        return DispatchResult::Continue;
                    }
                    return DispatchResult::Error(VmError::InvalidTask);
                }
            } else if instr.opcode == Opcode::BitNot {
                if let Some(res) = TaggedValue::try_from_i64(!a) {
                    if let Some(t) = self.task_mut(task) {
                        t.registers.set(destination, res);
                        return DispatchResult::Continue;
                    }
                    return DispatchResult::Error(VmError::InvalidTask);
                }
            } else if let Some(b) = b_val.as_i64() {
                let fast = match instr.opcode {
                    Opcode::Add => a.checked_add(b).and_then(TaggedValue::try_from_i64),
                    Opcode::Sub => a.checked_sub(b).and_then(TaggedValue::try_from_i64),
                    Opcode::Mul => a.checked_mul(b).and_then(TaggedValue::try_from_i64),
                    Opcode::Div if b != 0 => a.checked_div(b).and_then(TaggedValue::try_from_i64),
                    Opcode::Mod if b != 0 => a.checked_rem(b).and_then(TaggedValue::try_from_i64),
                    Opcode::BitAnd => TaggedValue::try_from_i64(a & b),
                    Opcode::BitOr => TaggedValue::try_from_i64(a | b),
                    Opcode::BitXor => TaggedValue::try_from_i64(a ^ b),
                    _ => None,
                };
                if let Some(res) = fast {
                    if let Some(t) = self.task_mut(task) {
                        t.registers.set(destination, res);
                        return DispatchResult::Continue;
                    }
                    return DispatchResult::Error(VmError::InvalidTask);
                }
            }
        }

        // Fast path 2: inline `f64`
        if let Some(a) = a_val.as_f64() {
            if instr.opcode == Opcode::Neg {
                if let Some(res) = TaggedValue::try_from_f64(-a) {
                    if let Some(t) = self.task_mut(task) {
                        t.registers.set(destination, res);
                        return DispatchResult::Continue;
                    }
                    return DispatchResult::Error(VmError::InvalidTask);
                }
            } else if let Some(b) = b_val.as_f64() {
                let fast = match instr.opcode {
                    Opcode::Add => TaggedValue::try_from_f64(a + b),
                    Opcode::Sub => TaggedValue::try_from_f64(a - b),
                    Opcode::Mul => TaggedValue::try_from_f64(a * b),
                    Opcode::Div if b != 0.0 => TaggedValue::try_from_f64(a / b),
                    Opcode::Mod if b != 0.0 => TaggedValue::try_from_f64(a % b),
                    _ => None,
                };
                if let Some(res) = fast {
                    if let Some(t) = self.task_mut(task) {
                        t.registers.set(destination, res);
                        return DispatchResult::Continue;
                    }
                    return DispatchResult::Error(VmError::InvalidTask);
                }
            }
        }

        let result = (|| {
            // SAFETY: execution owns the mutator operation; register values are
            // rooted, and both reads finish before the next managed allocation.
            let a = unsafe { Number::from_tagged(a_val) }.ok_or(VmError::TypeError)?;
            let result = match instr.opcode {
                Opcode::Neg => a.checked_neg()?,
                Opcode::BitNot => a.bit_not()?,
                operation => {
                    let b = unsafe { Number::from_tagged(b_val) }.ok_or(VmError::TypeError)?;
                    a.binary(b, operation)?
                }
            };
            self.number_value(result)
        })();
        match result {
            Ok(value) => {
                if let Some(t) = self.task_mut(task) {
                    t.registers.set(destination, value);
                    DispatchResult::Continue
                } else {
                    DispatchResult::Error(VmError::InvalidTask)
                }
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
        if let (Some(a), Some(b)) = (left.as_i64(), right.as_i64()) {
            return Ok(a == b);
        }
        if let (Some(a), Some(b)) = (left.as_f64(), right.as_f64()) {
            return Ok(a == b);
        }
        // SAFETY: callers retain rooted, initialized values and hold an operation.
        if let (Some(left), Some(right)) =
            unsafe { (Number::from_tagged(left), Number::from_tagged(right)) }
        {
            return Ok(left.partial_cmp_numeric(right) == Some(Ordering::Equal));
        }
        if left.is_heap() && right.is_heap() {
            if let Some(eq) = crate::builtin_ctx::with_heap_string_str(left, |ls| {
                crate::builtin_ctx::with_heap_string_str(right, |rs| ls == rs)
            })
            .flatten()
            {
                return Ok(eq);
            }
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
        if let (Some(a), Some(b)) = (left.as_i64(), right.as_i64()) {
            return Ok(Some(a.cmp(&b)));
        }
        if let (Some(a), Some(b)) = (left.as_f64(), right.as_f64()) {
            return Ok(a.partial_cmp(&b));
        }
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
        if let Some(ord) = crate::builtin_ctx::with_heap_string_str(left, |ls| {
            crate::builtin_ctx::with_heap_string_str(right, |rs| ls.cmp(rs))
        })
        .flatten()
        {
            return Ok(Some(ord));
        }
        Err(VmError::TypeError)
    }

    pub(crate) fn dispatch_comparison(
        &mut self,
        task: TaskId,
        instr: Instruction,
    ) -> DispatchResult {
        let (destination, left, right) = r_regs(&instr);
        let Some(task_ref) = self.task_ref(task) else {
            return DispatchResult::Error(VmError::InvalidTask);
        };
        let (a, b) = (task_ref.registers.get(left), task_ref.registers.get(right));

        // Fast path 1: 57-bit signed SMI (`IMM_I_SMALL`)
        if let (Some(la), Some(rb)) = (a.as_i64(), b.as_i64()) {
            let res = match instr.opcode {
                Opcode::CmpEq => la == rb,
                Opcode::CmpNe => la != rb,
                Opcode::CmpLt => la < rb,
                Opcode::CmpLe => la <= rb,
                Opcode::CmpGt => la > rb,
                Opcode::CmpGe => la >= rb,
                _ => return DispatchResult::Error(VmError::InvalidInstruction(instr.encode())),
            };
            if let Some(t) = self.task_mut(task) {
                t.registers.set(destination, TaggedValue::from_bool(res));
                return DispatchResult::Continue;
            }
            return DispatchResult::Error(VmError::InvalidTask);
        }

        // Fast path 2: inline `f64`
        if let (Some(la), Some(rb)) = (a.as_f64(), b.as_f64()) {
            let res = match instr.opcode {
                Opcode::CmpEq => la == rb,
                Opcode::CmpNe => la != rb,
                Opcode::CmpLt => la < rb,
                Opcode::CmpLe => la <= rb,
                Opcode::CmpGt => la > rb,
                Opcode::CmpGe => la >= rb,
                _ => return DispatchResult::Error(VmError::InvalidInstruction(instr.encode())),
            };
            if let Some(t) = self.task_mut(task) {
                t.registers.set(destination, TaggedValue::from_bool(res));
                return DispatchResult::Continue;
            }
            return DispatchResult::Error(VmError::InvalidTask);
        }

        if matches!(instr.opcode, Opcode::CmpEq | Opcode::CmpNe) {
            if !a.is_heap() && !b.is_heap() && a.as_enum().is_none() && b.as_enum().is_none() {
                match self.scalar_equal(a, b) {
                    Ok(equal) => {
                        if let Some(t) = self.task_mut(task) {
                            t.registers.set(
                                destination,
                                TaggedValue::from_bool(equal == (instr.opcode == Opcode::CmpEq)),
                            );
                            return DispatchResult::Continue;
                        }
                        return DispatchResult::Error(VmError::InvalidTask);
                    }
                    Err(error) => return DispatchResult::Error(error),
                }
            }
            match self.error_equal(a, b, 0) {
                Ok(Some(equal)) => {
                    if let Some(t) = self.task_mut(task) {
                        t.registers.set(
                            destination,
                            TaggedValue::from_bool(equal == (instr.opcode == Opcode::CmpEq)),
                        );
                        return DispatchResult::Continue;
                    }
                    return DispatchResult::Error(VmError::InvalidTask);
                }
                Err(error) => return DispatchResult::Error(error),
                Ok(None) => {}
            }
            match self.enum_equal(a, b) {
                Ok(Some(equal)) => {
                    if let Some(t) = self.task_mut(task) {
                        t.registers.set(
                            destination,
                            TaggedValue::from_bool(equal == (instr.opcode == Opcode::CmpEq)),
                        );
                        return DispatchResult::Continue;
                    }
                    return DispatchResult::Error(VmError::InvalidTask);
                }
                Err(error) => return DispatchResult::Error(error),
                Ok(None) => {}
            }
            match self.scalar_equal(a, b) {
                Ok(equal) => {
                    if let Some(t) = self.task_mut(task) {
                        t.registers.set(
                            destination,
                            TaggedValue::from_bool(equal == (instr.opcode == Opcode::CmpEq)),
                        );
                        return DispatchResult::Continue;
                    }
                    return DispatchResult::Error(VmError::InvalidTask);
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
        if let Some(t) = self.task_mut(task) {
            t.registers.set(destination, TaggedValue::from_bool(result));
            DispatchResult::Continue
        } else {
            DispatchResult::Error(VmError::InvalidTask)
        }
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
        if let Some(h) =
            crate::builtin_ctx::with_heap_string_str(value, |s| hash_string_bytes(s.as_bytes()))
        {
            return Ok(h);
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
