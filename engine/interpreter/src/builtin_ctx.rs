//! BuiltinCtx — calling context for native builtin functions.
//!
//! Builtins share the same register ABI as ordinary calls (args in r0..rN,
//! return in r0) but do not push a call frame.  Implementations go through
//! this context instead of touching registers directly.

use nsbc::Reg;
use runtime::{BuiltinFnId, TaggedValue, TaskId};
use type_pool::TypePool;

use crate::{Vm, VmError};

/// Native builtin function pointer.
pub type BuiltinFn = fn(&mut BuiltinCtx<'_>) -> Result<(), VmError>;

/// Table of registered builtin implementations, keyed by [`BuiltinFnId`].
#[derive(Default)]
pub struct BuiltinFnTable {
    entries: Vec<Option<BuiltinFn>>,
}

impl BuiltinFnTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, id: BuiltinFnId, f: BuiltinFn) {
        let idx = id as usize;
        if idx >= self.entries.len() {
            self.entries.resize(idx + 1, None);
        }
        self.entries[idx] = Some(f);
    }

    pub fn get(&self, id: BuiltinFnId) -> Option<BuiltinFn> {
        self.entries.get(id as usize).copied().flatten()
    }
}

/// Outcome requested by a builtin beyond a normal return value.
#[derive(Debug, Clone)]
pub enum BuiltinOutcome {
    /// Continue with return value already written to r0.
    Continue,
    /// Terminate the process with the given exit code.
    Exit(i32),
    /// Abort with a panic message.
    Panic(String),
}

/// Context passed to every builtin invocation.
pub struct BuiltinCtx<'vm> {
    pub(crate) vm: &'vm mut Vm,
    pub(crate) task_id: TaskId,
    pub(crate) arg_count: u8,
    pub(crate) outcome: BuiltinOutcome,
}

impl<'vm> BuiltinCtx<'vm> {
    pub fn new(vm: &'vm mut Vm, task_id: TaskId, arg_count: u8) -> Self {
        Self {
            vm,
            task_id,
            arg_count,
            outcome: BuiltinOutcome::Continue,
        }
    }

    pub fn arg_count(&self) -> u8 {
        self.arg_count
    }

    pub fn require_arity(&self, expected: u8) -> Result<(), VmError> {
        if self.arg_count != expected {
            Err(VmError::ArityError {
                expected,
                got: self.arg_count,
            })
        } else {
            Ok(())
        }
    }

    pub fn arg(&self, i: u8) -> Result<TaggedValue, VmError> {
        if i >= self.arg_count {
            return Err(VmError::ArityError {
                expected: i + 1,
                got: self.arg_count,
            });
        }
        let t = self
            .vm
            .scheduler
            .get_task(self.task_id)
            .ok_or(VmError::InvalidTask)?;
        Ok(t.registers.get(Reg(i)))
    }

    pub fn arg_i64(&self, i: u8) -> Result<i64, VmError> {
        let v = self.arg(i)?;
        v.as_i64().ok_or(VmError::TypeError)
    }

    pub fn arg_u64(&self, i: u8) -> Result<u64, VmError> {
        let v = self.arg(i)?;
        v.as_u64().ok_or(VmError::TypeError)
    }

    pub fn arg_f64(&self, i: u8) -> Result<f64, VmError> {
        let v = self.arg(i)?;
        v.as_f64().ok_or(VmError::TypeError)
    }

    pub fn arg_bool(&self, i: u8) -> Result<bool, VmError> {
        let v = self.arg(i)?;
        v.as_bool().ok_or(VmError::TypeError)
    }

    /// Read a heap string argument as owned UTF-8.
    pub fn arg_string(&self, i: u8) -> Result<String, VmError> {
        let v = self.arg(i)?;
        heap_string_to_owned(v).ok_or(VmError::TypeError)
    }

    pub fn set_return(&mut self, val: TaggedValue) {
        if let Some(t) = self.vm.scheduler.get_task_mut(self.task_id) {
            t.registers.set(Reg(0), val);
        }
    }

    pub fn return_unit(&mut self) {
        self.set_return(TaggedValue::UNIT);
    }

    pub fn return_i64(&mut self, v: i64) {
        self.set_return(TaggedValue::from_i64(v));
    }

    pub fn return_f64(&mut self, v: f64) {
        self.set_return(TaggedValue::from_f64(v));
    }

    pub fn return_bool(&mut self, v: bool) {
        self.set_return(if v {
            TaggedValue::TRUE
        } else {
            TaggedValue::FALSE
        });
    }

    pub fn alloc_string(&mut self, s: &str) -> TaggedValue {
        self.vm.alloc_string(s)
    }

    pub fn type_pool(&self) -> &TypePool {
        &self.vm.type_pool
    }

    /// Format any tagged value for display (print/println).
    pub fn format_value(&self, val: TaggedValue) -> String {
        format_tagged_value(val)
    }

    pub fn exit(&mut self, code: i32) {
        self.outcome = BuiltinOutcome::Exit(code);
    }

    pub fn panic(&mut self, msg: impl Into<String>) {
        self.outcome = BuiltinOutcome::Panic(msg.into());
    }

    pub fn take_outcome(&mut self) -> BuiltinOutcome {
        std::mem::replace(&mut self.outcome, BuiltinOutcome::Continue)
    }
}

/// Extract UTF-8 contents from a heap string TaggedValue.
pub fn heap_string_to_owned(val: TaggedValue) -> Option<String> {
    let ptr = val.as_heap_ptr()?;
    let len = unsafe { *(ptr as *const u64) } as usize;
    if len > 64 * 1024 * 1024 {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts((ptr as *const u8).add(8), len) };
    std::str::from_utf8(bytes).ok().map(|s| s.to_owned())
}

/// Format a TaggedValue for display.
pub fn format_tagged_value(val: TaggedValue) -> String {
    if val.is_null() {
        "null".to_string()
    } else if val.is_unit() {
        "()".to_string()
    } else if let Some(v) = val.as_i64() {
        v.to_string()
    } else if let Some(v) = val.as_u64() {
        v.to_string()
    } else if let Some(v) = val.as_f64() {
        format!("{v}")
    } else if let Some(v) = val.as_bool() {
        v.to_string()
    } else if let Some(v) = val.as_char() {
        v.to_string()
    } else if val.is_heap() {
        if let Some(s) = heap_string_to_owned(val) {
            return s;
        }
        format!("<object@{:#x}>", val.raw())
    } else {
        format!("<value:{:#018x}>", val.raw())
    }
}
