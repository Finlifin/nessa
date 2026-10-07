//! BuiltinCtx — calling context for native builtin functions.
//!
//! Builtins share the same register ABI as ordinary calls (args in r0..rN,
//! return in r0) but do not push a call frame.  Implementations go through
//! this context instead of touching registers directly.

use std::rc::{Rc, Weak};

use gc::ObjectHeader;
use nsbc::Reg;
use runtime::{BuiltinFnId, Number, TaggedValue, TaskId};
use type_pool::{Intrinsic, TypeIndex, TypePool};

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

/// A movable value retained by one native invocation. Reload after allocation
/// or collection; handles from other or completed invocations are rejected.
#[derive(Clone)]
pub struct BuiltinRoot {
    context: Weak<()>,
    index: usize,
}

/// Context passed to every builtin invocation.
pub struct BuiltinCtx<'vm> {
    pub(crate) vm: &'vm mut Vm,
    pub(crate) task_id: TaskId,
    pub(crate) arg_count: u8,
    pub(crate) outcome: BuiltinOutcome,
    root_base: usize,
    pin_base: usize,
    root_context: Rc<()>,
}

impl Drop for BuiltinCtx<'_> {
    fn drop(&mut self) {
        self.vm
            .roots
            .temporary_values
            .borrow_mut()
            .truncate(self.root_base);
        self.vm
            .roots
            .native_pins
            .borrow_mut()
            .truncate(self.pin_base);
    }
}

impl<'vm> BuiltinCtx<'vm> {
    pub(crate) fn new(vm: &'vm mut Vm, task_id: TaskId, arg_count: u8) -> Self {
        let root_base = vm.roots.temporary_values.borrow().len();
        let pin_base = vm.roots.native_pins.borrow().len();
        Self {
            vm,
            task_id,
            arg_count,
            outcome: BuiltinOutcome::Continue,
            root_base,
            pin_base,
            root_context: Rc::new(()),
        }
    }

    fn require_rooted_value(&self, value: TaggedValue) -> Result<(), VmError> {
        if value.is_heap() && !self.vm.roots.temporary_values.borrow().contains(&value) {
            return Err(VmError::TypeError);
        }
        Ok(())
    }

    fn retain_collection_result(&self, value: TaggedValue) -> TaggedValue {
        if value.is_heap() {
            let mut roots = self.vm.roots.temporary_values.borrow_mut();
            if !roots.contains(&value) {
                roots.push(value);
            }
            let mut pins = self.vm.roots.native_pins.borrow_mut();
            if !pins.contains(&value) {
                pins.push(value);
            }
        }
        value
    }

    pub fn return_empty_list(&mut self) -> Result<(), VmError> {
        let value = self.vm.new_list(0)?;
        self.write_return(value);
        Ok(())
    }

    pub fn list_len(&self, list: TaggedValue) -> Result<usize, VmError> {
        self.require_rooted_value(list)?;
        self.vm.list_len(list)
    }

    pub fn list_get(&self, list: TaggedValue, index: TaggedValue) -> Result<TaggedValue, VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(index)?;
        Ok(self.retain_collection_result(self.vm.list_get(list, index)?))
    }

    pub fn list_set(
        &mut self,
        list: TaggedValue,
        index: TaggedValue,
        value: TaggedValue,
    ) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(index)?;
        self.require_rooted_value(value)?;
        self.vm.list_set(list, index, value)
    }

    pub fn list_push(&mut self, list: TaggedValue, value: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(value)?;
        self.vm.list_push(list, value)
    }

    pub fn list_pop(&mut self, list: TaggedValue) -> Result<TaggedValue, VmError> {
        self.require_rooted_value(list)?;
        let value = self.vm.list_pop(list)?;
        Ok(self.retain_collection_result(value))
    }

    pub fn return_empty_map(&mut self) -> Result<(), VmError> {
        let value = self.vm.new_map(0)?;
        self.write_return(value);
        Ok(())
    }

    pub fn map_len(&self, map: TaggedValue) -> Result<usize, VmError> {
        self.require_rooted_value(map)?;
        self.vm.map_len(map)
    }

    /// Return an independent shallow snapshot of the occupied String keys.
    pub fn return_map_keys(&mut self, map: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(map)?;
        let keys = self.vm.map_keys(map)?;
        self.write_return(keys);
        Ok(())
    }

    pub fn map_get(&self, map: TaggedValue, key: TaggedValue) -> Result<TaggedValue, VmError> {
        self.require_rooted_value(map)?;
        self.require_rooted_value(key)?;
        Ok(self.retain_collection_result(self.vm.map_get(map, key)?))
    }

    pub fn map_set(
        &mut self,
        map: TaggedValue,
        key: TaggedValue,
        value: TaggedValue,
    ) -> Result<(), VmError> {
        self.require_rooted_value(map)?;
        self.require_rooted_value(key)?;
        self.require_rooted_value(value)?;
        self.vm.map_set(map, key, value)
    }

    pub fn map_remove(
        &mut self,
        map: TaggedValue,
        key: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        self.require_rooted_value(map)?;
        self.require_rooted_value(key)?;
        let value = self.vm.map_remove(map, key)?;
        Ok(self.retain_collection_result(value))
    }

    pub fn map_contains(&self, map: TaggedValue, key: TaggedValue) -> Result<bool, VmError> {
        self.require_rooted_value(map)?;
        self.require_rooted_value(key)?;
        self.vm.map_contains(map, key)
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

    fn argument_snapshot(&self, i: u8) -> Result<TaggedValue, VmError> {
        if i >= self.arg_count {
            return Err(VmError::ArityError {
                expected: i.saturating_add(1),
                got: self.arg_count,
            });
        }
        let task = self
            .vm
            .roots
            .scheduler
            .get_task(self.task_id)
            .ok_or(VmError::InvalidTask)?;
        Ok(task.registers.get(Reg(i)))
    }

    /// Retain a copied argument, including after its register is overwritten.
    /// Heap addresses handed out by this legacy API remain pinned until exit.
    pub fn arg(&self, i: u8) -> Result<TaggedValue, VmError> {
        Ok(self.retain_collection_result(self.argument_snapshot(i)?))
    }

    /// Retain an argument in an updateable slot without pinning its object.
    pub fn arg_rooted(&self, i: u8) -> Result<BuiltinRoot, VmError> {
        let value = self.argument_snapshot(i)?;
        let mut roots = self.vm.roots.temporary_values.borrow_mut();
        let index = roots.len();
        roots.push(value);
        Ok(BuiltinRoot {
            context: Rc::downgrade(&self.root_context),
            index,
        })
    }

    /// Snapshot a movable root for immediate inspection. Do not reuse the
    /// returned heap address across allocation or collection; reload the handle.
    pub fn load_rooted(&self, root: &BuiltinRoot) -> Result<TaggedValue, VmError> {
        let owner = root.context.upgrade().ok_or(VmError::TypeError)?;
        if !Rc::ptr_eq(&owner, &self.root_context) || root.index < self.root_base {
            return Err(VmError::TypeError);
        }
        self.vm
            .roots
            .temporary_values
            .borrow()
            .get(root.index)
            .copied()
            .ok_or(VmError::TypeError)
    }

    pub fn arg_i64(&self, i: u8) -> Result<i64, VmError> {
        self.arg_number(i)?
            .to_i64_checked()
            .ok_or(VmError::NumericOverflow)
    }

    pub fn arg_u64(&self, i: u8) -> Result<u64, VmError> {
        self.arg_number(i)?
            .to_u64_checked()
            .ok_or(VmError::NumericOverflow)
    }

    pub fn arg_f64(&self, i: u8) -> Result<f64, VmError> {
        Ok(self.arg_number(i)?.to_f64())
    }

    /// Copy numeric data out of a live argument, retaining no managed pointer.
    pub fn arg_number(&self, i: u8) -> Result<Number, VmError> {
        let value = self.arg(i)?;
        // SAFETY: arg roots the live VM value until this context exits; this
        // read completes without allocation or a collection safepoint.
        unsafe { Number::from_tagged(value) }.ok_or(VmError::TypeError)
    }

    /// Read an immutable Type descriptor, rejecting foreign or malformed indices.
    pub fn arg_type(&self, i: u8) -> Result<TypeIndex, VmError> {
        let ty = self.arg(i)?.as_type().ok_or(VmError::TypeError)?;
        self.vm.type_value(ty)?.as_type().ok_or(VmError::TypeError)
    }

    /// Inspect the actual dynamic type of a rooted argument.
    pub fn arg_value_type(&self, i: u8) -> Result<TypeIndex, VmError> {
        self.vm.reflected_type(self.arg(i)?)
    }

    pub fn return_type(&mut self, ty: TypeIndex) -> Result<(), VmError> {
        let value = self.vm.type_value(ty)?;
        self.write_return(value);
        Ok(())
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

    pub fn set_return(&mut self, val: TaggedValue) -> Result<(), VmError> {
        if val.is_heap() && !self.vm.roots.temporary_values.borrow().contains(&val) {
            return Err(VmError::TypeError);
        }
        self.write_return(val);
        Ok(())
    }

    fn write_return(&mut self, val: TaggedValue) {
        if let Some(t) = self.vm.roots.scheduler.get_task_mut(self.task_id) {
            t.registers.set(Reg(0), val);
        }
    }

    pub fn return_unit(&mut self) {
        self.write_return(TaggedValue::UNIT);
    }

    pub fn return_i64(&mut self, v: i64) -> Result<(), VmError> {
        self.return_number(Number::I64(v))
    }

    pub fn return_f64(&mut self, v: f64) -> Result<(), VmError> {
        self.return_number(Number::F64(v))
    }

    /// Allocate and publish a precise numeric result atomically.
    pub fn return_number(&mut self, number: Number) -> Result<(), VmError> {
        let value = self.vm.number_value(number)?;
        self.write_return(value);
        Ok(())
    }

    pub fn return_bool(&mut self, v: bool) {
        self.write_return(if v {
            TaggedValue::TRUE
        } else {
            TaggedValue::FALSE
        });
    }

    /// Publish the allocation directly to the task's return register.
    pub fn return_string(&mut self, s: &str) -> Result<(), VmError> {
        self.vm.check_display_bytes(self.task_id, s.len())?;
        let value = self.vm.alloc_string(s)?;
        self.write_return(value);
        Ok(())
    }

    /// Check the logical Display result limit before host or managed composition.
    pub fn check_display_size(&self, bytes: usize) -> Result<(), VmError> {
        self.vm.check_display_bytes(self.task_id, bytes)
    }

    /// Internal generated Tuple traversal hook; unauthorized frames are rejected.
    pub fn display_tuple_enter(&mut self, value: TaggedValue) -> Result<bool, VmError> {
        self.require_rooted_value(value)?;
        self.vm.display_inline_tuple(self.task_id, value, true)
    }

    /// Close the exact Tuple path opened in this generated frame.
    pub fn display_tuple_exit(&mut self, value: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(value)?;
        self.vm
            .display_inline_tuple(self.task_id, value, false)
            .map(|_| ())
    }

    /// Compare the scalar values supported by the standard library's intrinsic
    /// Eq/PartialEq bodies. Aggregate and closure identity is never equality.
    pub fn scalar_equal(&self, left: TaggedValue, right: TaggedValue) -> Result<bool, VmError> {
        self.require_rooted_value(left)?;
        self.require_rooted_value(right)?;
        for value in [left, right] {
            if let Some(ty) = value.as_type()
                && self.vm.state.type_pool.canonical_type(ty).is_none()
            {
                return Err(VmError::InvalidType(ty));
            }
            // SAFETY: the facade accepts only values retained in this context's
            // roots; the enclosing native call holds the execution operation.
            let numeric = unsafe { Number::from_tagged(value) }.is_some();
            if !numeric
                && value.as_bool().is_none()
                && value.as_char().is_none()
                && value.as_type().is_none()
                && !value.is_unit()
                && heap_string_to_owned(value).is_none()
            {
                return Err(VmError::UnsupportedEquality);
            }
        }
        self.vm.scalar_equal(left, right)
    }

    /// Compare rooted numeric, Bool, Char, String or Unit operands. NaN is
    /// unordered; unsupported values and mismatched scalar categories are errors.
    pub fn scalar_partial_cmp(
        &self,
        left: TaggedValue,
        right: TaggedValue,
    ) -> Result<Option<std::cmp::Ordering>, VmError> {
        self.require_rooted_value(left)?;
        self.require_rooted_value(right)?;
        self.vm.scalar_partial_cmp(left, right)
    }

    /// Format a compiler-derived Display receiver and publish its String result.
    /// The signed bootstrap contract and tagged-field layout are checked before
    /// reading the rooted object; only the final publication allocates in the GC.
    pub fn return_derived_display(&mut self, value: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(value)?;
        let ty = self.vm.reflected_type(value)?;
        let pool = &self.vm.state.type_pool;
        let key = pool
            .trait_schema(pool.well_known.display)
            .and_then(|schema| {
                schema
                    .slots
                    .iter()
                    .find(|key| str_interner::try_get(key.name).as_deref() == Some("to_string"))
            })
            .ok_or(VmError::UnsupportedDerivedMethod("display contract"))?;
        pool.check_native_derived_method(key, ty)
            .map_err(|_| VmError::UnsupportedDerivedMethod("display contract"))?;
        // A raw Any call to this internal builtin is not a Display wrapper.
        // Authenticate the executing ordinary function against the published
        // global implementation; do not reselect caller-scope trait evidence.
        let current = self
            .vm
            .roots
            .scheduler
            .get_task(self.task_id)
            .ok_or(VmError::InvalidTask)?
            .current_func;
        let implementation = pool
            .find_trait_impl(ty, pool.well_known.display)
            .ok_or(VmError::UnsupportedDerivedMethod("display wrapper"))?;
        if !implementation.methods.iter().any(|method| {
            method.name == key.name && method.func_id == current.0 && method.visible_scope.is_none()
        }) {
            return Err(VmError::UnsupportedDerivedMethod("display wrapper"));
        }
        let function = self.vm.checked_function(current)?;
        pool.check_trait_method_signature(key, ty, function.function_type)
            .map_err(|_| VmError::UnsupportedDerivedMethod("display wrapper"))?;
        if !matches!(&function.abi, Some(abi) if abi.captures.is_empty() && abi.parameters == [nsbc::ParameterAbi::Value])
        {
            return Err(VmError::UnsupportedDerivedMethod("display wrapper"));
        }
        let text = self.vm.derived_struct_to_string(value, ty)?;
        self.return_string(&text)
    }

    /// Synchronously collect with this task's arguments and slots rooted.
    pub fn collect_garbage(&mut self) -> Result<bool, VmError> {
        self.vm.collect_garbage()
    }

    pub fn type_pool(&self) -> &TypePool {
        self.vm.type_pool()
    }

    /// Format any tagged value for display (print/println).
    pub fn format_value(&self, val: TaggedValue) -> Result<String, VmError> {
        if val.is_heap() && !self.vm.roots.temporary_values.borrow().contains(&val) {
            return Err(VmError::TypeError);
        }
        if let Some(ty) = val.as_type() {
            return self
                .vm
                .state
                .type_pool
                .display_name(ty)
                .ok_or(VmError::InvalidType(ty));
        }
        if let Some(list) = self.vm.format_aggregate(val)? {
            return Ok(list);
        }
        Ok(format_tagged_value(val))
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
pub(crate) fn heap_string_to_owned(val: TaggedValue) -> Option<String> {
    let ptr = val.as_heap_ptr()?;
    // SAFETY: callers run within a mutator operation and supply live VM values.
    let header = unsafe { ObjectHeader::from_payload_ptr(ptr) };
    if !header.is_ordinary()
        || header.type_index != Intrinsic::Str.type_index()
        || header.payload_words() == 0
    {
        return None;
    }
    let len = unsafe { *(ptr as *const u64) } as usize;
    if len > header.payload_words() * 8 - 8 {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr.add(8), len) };
    std::str::from_utf8(bytes).ok().map(|s| s.to_owned())
}

/// Format a TaggedValue for display.
pub(crate) fn format_tagged_value(val: TaggedValue) -> String {
    if val.is_null() {
        "null".to_string()
    } else if val.is_unit() {
        "()".to_string()
    // SAFETY: this helper is private; callers supply rooted live VM values
    // within a mutator operation. It never allocates managed objects.
    } else if let Some(number) = unsafe { Number::from_tagged(val) } {
        number.to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_root_handles_reject_wrong_live_context_and_invalid_argument() {
        let mut vm = crate::tests::make_vm();
        let task = vm.spawn_root(nsbc::FuncId(0));
        vm.roots
            .scheduler
            .get_task_mut(task)
            .unwrap()
            .registers
            .set(Reg(0), TaggedValue::from_i64(42));
        let outer = BuiltinCtx::new(&mut vm, task, 1);
        assert!(matches!(
            outer.arg(u8::MAX),
            Err(VmError::ArityError {
                expected: 255,
                got: 1
            })
        ));
        assert!(matches!(
            outer.arg_rooted(u8::MAX),
            Err(VmError::ArityError {
                expected: 255,
                got: 1
            })
        ));
        let handle = outer.arg_rooted(0).unwrap();
        assert_eq!(outer.load_rooted(&handle).unwrap().as_i64(), Some(42));
        {
            let inner = BuiltinCtx::new(outer.vm, task, 1);
            assert!(matches!(
                inner.load_rooted(&handle),
                Err(VmError::TypeError)
            ));
        }
        assert_eq!(outer.load_rooted(&handle).unwrap().as_i64(), Some(42));
        drop(outer);
        let later = BuiltinCtx::new(&mut vm, task, 1);
        assert!(matches!(
            later.load_rooted(&handle),
            Err(VmError::TypeError)
        ));
    }

    #[test]
    fn native_error_and_panic_unwinding_release_temporary_roots_and_pins() {
        let mut vm = crate::tests::make_vm();
        let task = vm.spawn_root(nsbc::FuncId(0));
        let index = vm
            .push_constant(&nsbc::Constant::Str("retained".into()))
            .unwrap();
        let value = vm.roots.constants.get(index);
        vm.roots
            .scheduler
            .get_task_mut(task)
            .unwrap()
            .registers
            .set(Reg(0), value);
        let roots_before = vm.roots.temporary_values.borrow().len();
        let pins_before = vm.roots.native_pins.borrow().len();
        let result = (|| -> Result<(), VmError> {
            let ctx = BuiltinCtx::new(&mut vm, task, 1);
            ctx.arg(0)?;
            ctx.arg_rooted(0)?;
            Err(VmError::TypeError)
        })();
        assert!(matches!(result, Err(VmError::TypeError)));
        assert_eq!(vm.roots.temporary_values.borrow().len(), roots_before);
        assert_eq!(vm.roots.native_pins.borrow().len(), pins_before);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let ctx = BuiltinCtx::new(&mut vm, task, 1);
            ctx.arg(0).unwrap();
            ctx.arg_rooted(0).unwrap();
            panic!("native callback unwind fixture");
        }));
        assert!(result.is_err());
        assert_eq!(vm.roots.temporary_values.borrow().len(), roots_before);
        assert_eq!(vm.roots.native_pins.borrow().len(), pins_before);
    }

    fn concat(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
        ctx.require_arity(2)?;
        let mut value = ctx.arg_string(0)?;
        value.push_str(&ctx.arg_string(1)?);
        ctx.return_string(&value)
    }

    fn concat_length(ctx: &mut BuiltinCtx<'_>) -> Result<(), VmError> {
        ctx.require_arity(1)?;
        let value = ctx.arg_string(0)?;
        assert_eq!(value, "left-right");
        ctx.return_i64(value.len() as i64)
    }

    #[test]
    fn registered_string_methods_use_ordinary_frames_and_exact_string_arguments() {
        use nsbc::{AddrMode, Constant, FuncId, Instruction, Opcode};
        use runtime::FunctionCode;
        for case in [
            "concat",
            "wrong_rhs",
            "unknown",
            "missing",
            "extra",
            "overflow",
            "invalid_function",
        ] {
            let mut vm = crate::tests::make_vm();
            let name = str_interner::intern(if case == "overflow" {
                "apply"
            } else {
                "concat"
            });
            vm.state.type_pool.add_method(
                Intrinsic::Str.type_index(),
                type_pool::MethodSlot {
                    name,
                    func_id: if case == "invalid_function" { 999 } else { 1 },
                    trait_impl: None,
                    visible_scope: None,
                    access: type_pool::MethodAccess::Public,
                },
            );
            vm.register_builtin(0, concat);
            vm.register_builtin(1, concat_length);
            vm.push_constant(&Constant::Str("left-".into())).unwrap();
            vm.push_constant(&Constant::Str("right".into())).unwrap();
            let method = if case == "unknown" {
                str_interner::intern("absent")
            } else {
                name
            };
            let argument_count = match case {
                "missing" => 0,
                "extra" => 2,
                "overflow" => 32,
                _ => 1,
            };
            let rhs = if case == "wrong_rhs" {
                Instruction::load_imm(Reg(0), 42)
            } else {
                Instruction::load_const(Reg(0), 1)
            };
            let call = if case == "overflow" {
                // CallIndirect carries an eight-bit count before delegating a
                // non-closure receiver to apply's ordinary method dispatch.
                Instruction::call_indirect(Reg(10), argument_count)
            } else {
                Instruction::call_method(Reg(10), method.as_u32(), argument_count)
            };
            for (id, instructions, params) in [
                (
                    0,
                    vec![
                        Instruction::load_const(Reg(10), 0),
                        rhs,
                        call,
                        Instruction::call_builtin(1, 1),
                        Instruction::ret(Reg(0)),
                    ],
                    0,
                ),
                (
                    1,
                    vec![
                        Instruction::a_type(
                            Opcode::TypeAssert,
                            AddrMode::Imm,
                            Reg(0),
                            Reg(0),
                            Intrinsic::Str.type_index().as_u32() as u16,
                        ),
                        Instruction::a_type(
                            Opcode::TypeAssert,
                            AddrMode::Imm,
                            Reg(1),
                            Reg(1),
                            Intrinsic::Str.type_index().as_u32() as u16,
                        ),
                        Instruction::call_builtin(0, 2),
                        Instruction::ret(Reg(0)),
                    ],
                    2,
                ),
            ] {
                vm.add_function(FunctionCode {
                    display_owner: None,
                    abi: None,
                    func_id: FuncId(id),
                    instructions: instructions.into_iter().map(Instruction::encode).collect(),
                    register_count: 32,
                    param_count: params,
                    is_closure: false,
                    function_type: TypeIndex::INVALID,
                });
            }
            let task = vm.spawn_root(FuncId(0));
            match case {
                "concat" => {
                    assert!(matches!(vm.run(), crate::VmResult::Finished));
                    assert_eq!(vm.task_result_i64(task).unwrap(), 10);
                }
                "wrong_rhs" => assert!(matches!(
                    vm.run(),
                    crate::VmResult::Error(VmError::TypeError)
                )),
                "missing" | "extra" => assert!(
                    matches!(vm.run(), crate::VmResult::Error(VmError::ArityError { expected: 1, got }) if got == argument_count)
                ),
                "overflow" => assert!(matches!(
                    vm.run(),
                    crate::VmResult::Error(VmError::ArityError {
                        expected: 31,
                        got: 32
                    })
                )),
                "invalid_function" => assert!(matches!(
                    vm.run(),
                    crate::VmResult::Error(VmError::InvalidFunction(FuncId(999)))
                )),
                _ => assert!(matches!(
                    vm.run(),
                    crate::VmResult::Error(VmError::MethodNotFound)
                )),
            }
            assert_eq!(vm.active_stack_count(), 0);
        }
    }

    #[test]
    fn scalar_equality_primitive_rejects_opaque_buffers_and_forged_type_values() {
        let mut vm = crate::tests::make_vm();
        let buffer_type = vm.state.type_pool.list_buffer_type().unwrap();
        let task = vm.spawn_root(nsbc::FuncId(0));
        // SAFETY: TestVm holds an operation. The zero-slot opaque buffer is
        // initialized and immediately placed in the registered task roots.
        let payload = unsafe { vm.state.heap.alloc_object(buffer_type, 0) }.unwrap();
        // SAFETY: this allocation is initialized and remains live under the operation.
        let value = unsafe { TaggedValue::from_heap_ptr(payload.as_ptr()) };
        vm.roots
            .scheduler
            .get_task_mut(task)
            .unwrap()
            .registers
            .set(Reg(0), value);
        let ctx = BuiltinCtx::new(&mut vm, task, 1);
        let value = ctx.arg(0).unwrap();
        assert!(matches!(
            ctx.scalar_equal(value, value),
            Err(VmError::UnsupportedEquality)
        ));
        assert!(matches!(
            ctx.scalar_partial_cmp(value, value),
            Err(VmError::TypeError)
        ));
        assert!(matches!(
            ctx.scalar_equal(TaggedValue::NULL, TaggedValue::NULL),
            Err(VmError::UnsupportedEquality)
        ));
        let ty = TypeIndex::from_raw(u32::MAX - 1);
        let forged = TaggedValue::from_type(ty).unwrap();
        assert!(
            matches!(ctx.scalar_equal(forged, forged), Err(VmError::InvalidType(index)) if index == ty)
        );
        assert!(
            ctx.scalar_equal(TaggedValue::from_i64(42), TaggedValue::from_u64(42))
                .unwrap()
        );
        assert!(
            !ctx.scalar_equal(TaggedValue::from_i64(-1), TaggedValue::from_u64(42))
                .unwrap()
        );
        assert!(
            !ctx.scalar_equal(
                TaggedValue::from_f64(f64::NAN),
                TaggedValue::from_f64(f64::NAN)
            )
            .unwrap()
        );
    }

    #[test]
    fn scalar_partial_ordering_is_exact_and_rejects_non_ordered_operands() {
        use std::cmp::Ordering;

        let mut vm = crate::tests::make_vm();
        let task = vm.spawn_root(nsbc::FuncId(0));
        let ctx = BuiltinCtx::new(&mut vm, task, 0);
        for (left, right, expected) in [
            (
                TaggedValue::from_i64(-1),
                TaggedValue::from_u64(42),
                Some(Ordering::Less),
            ),
            (TaggedValue::FALSE, TaggedValue::TRUE, Some(Ordering::Less)),
            (
                TaggedValue::from_char('é'),
                TaggedValue::from_char('x'),
                Some(Ordering::Greater),
            ),
            (TaggedValue::UNIT, TaggedValue::UNIT, Some(Ordering::Equal)),
            (
                TaggedValue::from_f64(-0.0),
                TaggedValue::from_f64(0.0),
                Some(Ordering::Equal),
            ),
            (
                TaggedValue::from_f64(f64::NAN),
                TaggedValue::from_f64(0.0),
                None,
            ),
        ] {
            assert_eq!(ctx.scalar_partial_cmp(left, right).unwrap(), expected);
        }
        for value in [
            TaggedValue::NULL,
            TaggedValue::from_type(Intrinsic::I64.type_index()).unwrap(),
        ] {
            assert!(matches!(
                ctx.scalar_partial_cmp(value, value),
                Err(VmError::TypeError)
            ));
        }
        assert!(matches!(
            ctx.scalar_partial_cmp(TaggedValue::TRUE, TaggedValue::from_i64(1)),
            Err(VmError::TypeError)
        ));
    }

    #[test]
    fn derived_display_native_checks_contract_payload_and_metadata_names() {
        use type_pool::{
            FieldInfo, TraitDispatchSchema, TraitMethodKey, TraitMethodSignature,
            TraitParameterKind, TraitTypeStep, TypeId, TypeInfo, TypeKind,
        };
        let mut vm = crate::tests::make_vm();
        let owner = vm.state.type_pool.well_known.display;
        let declaration = vm.state.type_pool.intern_structural(TypeKind::Function {
            params: vec![owner],
            ret: Intrinsic::Str.type_index(),
        });
        vm.state
            .type_pool
            .register_trait_schema(TraitDispatchSchema {
                trait_type: owner,
                slots: vec![TraitMethodKey {
                    trait_owner: owner,
                    name: str_interner::intern("to_string"),
                    signature: Some(TraitMethodSignature {
                        associated_paths: Vec::new(),
                        declaration,
                        self_paths: vec![vec![TraitTypeStep::Parameter(0)]],
                        parameter_kinds: vec![TraitParameterKind::Receiver],
                    }),
                }],
            })
            .unwrap();
        let ty = vm.state.type_pool.register(TypeInfo {
            kind: TypeKind::Struct {
                name: str_interner::intern("Native"),
                fields: vec![FieldInfo {
                    name: str_interner::intern("x"),
                    ty: Intrinsic::I64.type_index(),
                    has_default: false,
                    offset: 0,
                }],
            },
            type_id: TypeId(942, 1),
            size: 8,
            align: 8,
        });
        let signature = vm.state.type_pool.intern_structural(TypeKind::Function {
            params: vec![ty],
            ret: Intrinsic::Str.type_index(),
        });
        vm.add_function(runtime::FunctionCode {
            display_owner: None,
            abi: Some(nsbc::FunctionAbi {
                captures: vec![],
                parameters: vec![nsbc::ParameterAbi::Value],
            }),
            func_id: nsbc::FuncId(0),
            instructions: vec![nsbc::Instruction::return_unit().encode()],
            register_count: 1,
            param_count: 1,
            is_closure: false,
            function_type: signature,
        });
        vm.state
            .type_pool
            .add_trait_impl(type_pool::TraitImplRecord {
                implementor: ty,
                trait_type: owner,
                visible_scope: None,
                methods: vec![type_pool::MethodSlot {
                    name: str_interner::intern("to_string"),
                    func_id: 0,
                    trait_impl: Some(owner),
                    visible_scope: None,
                    access: type_pool::MethodAccess::Public,
                }],
            });
        let task = vm.spawn_root(nsbc::FuncId(0));
        for words in [0, 1] {
            // SAFETY: TestVm keeps an operation live. Initialize the complete
            // allocation and publish it to the task before any other allocation.
            let payload = unsafe { vm.state.heap.alloc_object(ty, words) }.unwrap();
            if words == 1 {
                // SAFETY: this allocation owns one aligned tagged payload slot.
                unsafe {
                    payload
                        .as_ptr()
                        .cast::<TaggedValue>()
                        .write(TaggedValue::from_i64(42));
                }
            }
            // SAFETY: the freshly initialized object remains live and rooted.
            let value = unsafe { TaggedValue::from_heap_ptr(payload.as_ptr()) };
            vm.roots
                .scheduler
                .get_task_mut(task)
                .unwrap()
                .registers
                .set(Reg(0), value);
            let mut ctx = BuiltinCtx::new(&mut vm, task, 1);
            let argument = ctx.arg(0).unwrap();
            if words == 0 {
                assert!(matches!(
                    ctx.return_derived_display(argument),
                    Err(VmError::TypeError)
                ));
            } else {
                ctx.return_derived_display(argument).unwrap();
                assert_eq!(ctx.arg_string(0).unwrap(), "Native { x: 42 }");
            }
        }
        vm.roots
            .scheduler
            .get_task_mut(task)
            .unwrap()
            .registers
            .set(Reg(0), TaggedValue::from_i64(42));
        let mut ctx = BuiltinCtx::new(&mut vm, task, 1);
        assert!(matches!(
            ctx.return_derived_display(TaggedValue::from_i64(42)),
            Err(VmError::UnsupportedDerivedMethod("display contract"))
        ));
        drop(ctx);
        if let TypeKind::Struct { fields, .. } = &mut vm.state.type_pool.get_mut(ty).kind {
            fields[0].name = str_interner::StrId::from_raw(u32::MAX);
        }
        // SAFETY: the operation owns this new initialized allocation, and it is
        // placed in the task register before calling the formatting facade.
        let payload = unsafe { vm.state.heap.alloc_object(ty, 1) }.unwrap();
        unsafe {
            payload
                .as_ptr()
                .cast::<TaggedValue>()
                .write(TaggedValue::from_i64(42));
        }
        let value = unsafe { TaggedValue::from_heap_ptr(payload.as_ptr()) };
        vm.roots
            .scheduler
            .get_task_mut(task)
            .unwrap()
            .registers
            .set(Reg(0), value);
        let mut ctx = BuiltinCtx::new(&mut vm, task, 1);
        let argument = ctx.arg(0).unwrap();
        assert!(matches!(
            ctx.return_derived_display(argument),
            Err(VmError::TypeError)
        ));
        drop(ctx);
        if let TypeKind::Struct { fields, .. } = &mut vm.state.type_pool.get_mut(ty).kind {
            fields[0].name = str_interner::intern("x");
        }
        vm.roots.scheduler.get_task_mut(task).unwrap().current_func = nsbc::FuncId(1);
        let mut ctx = BuiltinCtx::new(&mut vm, task, 1);
        let argument = ctx.arg(0).unwrap();
        assert!(matches!(
            ctx.return_derived_display(argument),
            Err(VmError::UnsupportedDerivedMethod("display wrapper"))
        ));
    }

    #[test]
    fn string_arguments_reject_other_heap_layouts_and_invalid_lengths() {
        let mut vm = crate::tests::make_vm();
        let task = vm.spawn_root(nsbc::FuncId(0));
        for kind in [Intrinsic::Closure, Intrinsic::Continuation, Intrinsic::Str] {
            // SAFETY: TestVm holds an operation throughout fixture mutation.
            // The object is published before any subsequent managed allocation.
            let payload = unsafe { vm.state.heap.alloc_object(kind.type_index(), 1) }.unwrap();
            // SAFETY: the allocation owns one aligned u64 payload word. For Str,
            // length 8 is invalid because there is no room after the prefix.
            unsafe {
                payload.as_ptr().cast::<u64>().write(8);
            }
            // SAFETY: payload points to this initialized, live heap allocation.
            let value = unsafe { TaggedValue::from_heap_ptr(payload.as_ptr()) };
            vm.roots
                .scheduler
                .get_task_mut(task)
                .unwrap()
                .registers
                .set(Reg(0), value);
            let ctx = BuiltinCtx::new(&mut vm, task, 1);
            assert!(matches!(ctx.arg_string(0), Err(VmError::TypeError)));
            let value = ctx.arg(0).unwrap();
            assert!(ctx.format_value(value).unwrap().starts_with("<object@"));
        }
        let index = vm
            .push_constant(&nsbc::Constant::Str("abcdefgh".into()))
            .unwrap();
        assert_eq!(vm.constant_string(index).unwrap(), "abcdefgh");
    }
}
