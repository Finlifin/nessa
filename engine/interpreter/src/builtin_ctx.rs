//! BuiltinCtx — calling context for native builtin functions.
//!
//! Builtins share the same register ABI as ordinary calls (args in r0..rN,
//! return in r0) but do not push a call frame.  Implementations go through
//! this context instead of touching registers directly.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use nsbc::Reg;
use runtime::{BuiltinFnId, Number, TaggedValue, TaskId};
use scheduler::{NetOutcome, ProcessSpec, TaskOpPoll, TaskOpResult};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::{SharedVm, Vm, VmError, VmTypePoolRef};

/// Native builtin function pointer.
pub type BuiltinFn = fn(&mut BuiltinCtx<'_>) -> Result<(), VmError>;

/// Table of registered builtin implementations, keyed by [`BuiltinFnId`].
#[derive(Default)]
pub struct BuiltinFnTable {
    entries: Vec<Option<BuiltinFn>>,
    overridden: Vec<bool>,
    fast_paths_enabled: bool,
}

impl BuiltinFnTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, id: BuiltinFnId, f: BuiltinFn) {
        let idx = id as usize;
        if idx >= self.entries.len() {
            self.entries.resize(idx + 1, None);
            self.overridden.resize(idx + 1, false);
        }
        if self.fast_paths_enabled {
            self.overridden[idx] = true;
        }
        self.entries[idx] = Some(f);
    }

    pub fn enable_fast_paths(&mut self) {
        self.fast_paths_enabled = true;
    }

    #[inline]
    pub fn can_fast_path(&self, id: BuiltinFnId) -> bool {
        self.fast_paths_enabled && !self.overridden.get(id as usize).copied().unwrap_or(false)
    }

    #[inline]
    pub fn get(&self, id: BuiltinFnId) -> Option<BuiltinFn> {
        self.entries.get(id as usize).copied().flatten()
    }
}

/// Outcome requested by a builtin beyond a normal return value.
#[derive(Debug, Clone)]
pub enum BuiltinOutcome {
    /// Continue with return value already written to r0.
    Continue,
    /// Cooperatively yield the current task's execution slice.
    Yield,
    /// Suspend the current task and rewind PC by 1 to retry when awakened.
    SuspendRetry,
    /// Immediately begin cooperative cancellation unwinding (`defer` blocks).
    StartCancelUnwind,
    /// Advance cooperative cancellation unwinding after a `defer` block completes.
    ResumeCancelUnwind,
    /// Terminate the process with the given exit code.
    Exit(i32),
    /// Abort with a panic message.
    Panic(String),
}

/// A movable value retained by one native invocation. Reload after allocation
/// or collection; handles from other or completed invocations are rejected.
#[derive(Clone)]
pub struct BuiltinRoot {
    context_id: u64,
    index: usize,
}

static NEXT_BUILTIN_CTX_ID: AtomicU64 = AtomicU64::new(1);

/// Host-owned value specification converted into GC-managed values with every
/// intermediate heap reference rooted across moving Immix allocations.
#[derive(Debug, Clone)]
pub enum NativeValue {
    Null,
    Unit,
    Bool(bool),
    Char(char),
    I64(i64),
    U64(u64),
    F64(f64),
    Str(String),
    Bytes(Vec<u8>),
    OptI64(Option<i64>),
    OptF64(Option<f64>),
    OptStr(Option<String>),
    List(Vec<NativeValue>),
    StringMap(Vec<(String, NativeValue)>),
    Tuple(Vec<NativeValue>),
}

/// Thread-safe handle to host-owned VM resources (`VmFileTable`, `VmRng`) passed to
/// offloaded `uv_queue_work` closures without exposing GC heap or mutator state.
#[derive(Clone)]
pub struct VmIoContext {
    shared: Arc<SharedVm>,
}

impl VmIoContext {
    #[inline]
    pub fn files(&self) -> &crate::resources::VmFileTable {
        &self.shared.files
    }

    #[inline]
    pub fn rng_next_u64(&self) -> u64 {
        self.shared.rng.lock().next_u64()
    }

    #[inline]
    pub fn create_temp_file(&self) -> (i64, i64, String) {
        self.shared.files.create_temp(&self.shared.rng)
    }
}

/// Control-flow and value outcome returned by an embedder [`HostHandler`].
#[derive(Debug, Clone)]
pub enum HostCallOutcome {
    /// Return `(true, value)` to the Nessa caller.
    Return(NativeValue),
    /// Return `(false, error_message)` to the Nessa caller.
    Error(String),
    /// Suspend `task_id` and retry the `__host_call` instruction when awakened via [`Vm::wake_task`].
    SuspendRetry,
    /// Cooperatively yield the active task's slice.
    Yield,
}

/// Host-provided callback interface invoked by the privileged `__host_call` builtin.
pub trait HostHandler: Send + Sync {
    fn call(&mut self, op: &str, args: &[NativeValue], task_id: TaskId) -> HostCallOutcome;
}

/// Context passed to every builtin invocation.
pub struct BuiltinCtx<'vm> {
    pub(crate) vm: &'vm mut Vm,
    pub(crate) task_id: TaskId,
    pub(crate) arg_count: u8,
    pub(crate) outcome: BuiltinOutcome,
    root_base: usize,
    pin_base: usize,
    context_id: u64,
    dirty_roots: Cell<bool>,
}

impl Drop for BuiltinCtx<'_> {
    #[inline]
    fn drop(&mut self) {
        if self.dirty_roots.get() {
            let wr = self.vm.worker_roots();
            wr.temporary_values.borrow_mut().truncate(self.root_base);
            wr.native_pins.borrow_mut().truncate(self.pin_base);
        }
    }
}

impl<'vm> BuiltinCtx<'vm> {
    #[inline]
    pub(crate) fn new(vm: &'vm mut Vm, task_id: TaskId, arg_count: u8) -> Self {
        let (root_base, pin_base) = {
            let wr = vm.worker_roots();
            (
                wr.temporary_values.borrow().len(),
                wr.native_pins.borrow().len(),
            )
        };
        Self {
            vm,
            task_id,
            arg_count,
            outcome: BuiltinOutcome::Continue,
            root_base,
            pin_base,
            context_id: NEXT_BUILTIN_CTX_ID.fetch_add(1, AtomicOrdering::Relaxed),
            dirty_roots: Cell::new(false),
        }
    }

    fn require_rooted_value(&self, value: TaggedValue) -> Result<(), VmError> {
        if value.is_heap()
            && !self
                .vm
                .worker_roots()
                .temporary_values
                .borrow()
                .contains(&value)
        {
            return Err(VmError::TypeError);
        }
        Ok(())
    }

    fn retain_collection_result(&self, value: TaggedValue) -> TaggedValue {
        if value.is_heap() {
            self.dirty_roots.set(true);
            let wr = self.vm.worker_roots();
            let mut roots = wr.temporary_values.borrow_mut();
            if !roots.contains(&value) {
                roots.push(value);
            }
            let mut pins = wr.native_pins.borrow_mut();
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

    fn return_fresh_list(&mut self, value: TaggedValue) -> Result<(), VmError> {
        self.write_return(value);
        Ok(())
    }

    pub fn list_windows(&mut self, list: TaggedValue, size: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(size)?;
        let value = self.vm.list_windows(list, size)?;
        self.return_fresh_list(value)
    }

    pub fn list_chunks(&mut self, list: TaggedValue, size: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(size)?;
        let value = self.vm.list_chunks(list, size)?;
        self.return_fresh_list(value)
    }

    pub fn list_chunks_exact(
        &mut self,
        list: TaggedValue,
        size: TaggedValue,
    ) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(size)?;
        let value = self.vm.list_chunks_exact(list, size)?;
        self.return_fresh_list(value)
    }

    pub fn list_rchunks(&mut self, list: TaggedValue, size: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(size)?;
        let value = self.vm.list_rchunks(list, size)?;
        self.return_fresh_list(value)
    }

    pub fn list_rchunks_exact(
        &mut self,
        list: TaggedValue,
        size: TaggedValue,
    ) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(size)?;
        let value = self.vm.list_rchunks_exact(list, size)?;
        self.return_fresh_list(value)
    }

    pub fn list_chunks_remainder(
        &mut self,
        list: TaggedValue,
        size: TaggedValue,
    ) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(size)?;
        let value = self.vm.list_chunks_remainder(list, size)?;
        self.return_fresh_list(value)
    }

    pub fn list_rchunks_remainder(
        &mut self,
        list: TaggedValue,
        size: TaggedValue,
    ) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(size)?;
        let value = self.vm.list_rchunks_remainder(list, size)?;
        self.return_fresh_list(value)
    }

    pub fn list_split_at(&mut self, list: TaggedValue, index: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(index)?;
        let value = self.vm.list_split_at(list, index)?;
        self.return_fresh_list(value)
    }

    pub fn list_rotate_left(
        &mut self,
        list: TaggedValue,
        distance: TaggedValue,
    ) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(distance)?;
        let value = self.vm.list_rotate_left(list, distance)?;
        self.return_fresh_list(value)
    }

    pub fn list_rotate_right(
        &mut self,
        list: TaggedValue,
        distance: TaggedValue,
    ) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(distance)?;
        let value = self.vm.list_rotate_right(list, distance)?;
        self.return_fresh_list(value)
    }

    pub fn list_repeat(&mut self, list: TaggedValue, count: usize) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        let value = self.vm.list_repeat(list, count)?;
        self.return_fresh_list(value)
    }

    pub fn list_truncate(&mut self, list: TaggedValue, len: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(len)?;
        self.vm.list_truncate(list, len)
    }

    pub fn list_fill(&mut self, list: TaggedValue, value: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(value)?;
        self.vm.list_fill(list, value)
    }

    pub fn list_swap_remove(
        &mut self,
        list: TaggedValue,
        index: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(index)?;
        let value = self.vm.list_swap_remove(list, index)?;
        Ok(self.retain_collection_result(value))
    }

    pub fn list_split_off(&mut self, list: TaggedValue, index: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(index)?;
        let value = self.vm.list_split_off(list, index)?;
        self.return_fresh_list(value)
    }

    pub fn list_resize(
        &mut self,
        list: TaggedValue,
        len: TaggedValue,
        value: TaggedValue,
    ) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(len)?;
        self.require_rooted_value(value)?;
        self.vm.list_resize(list, len, value)
    }

    pub fn list_copy(&mut self, list: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        let value = self.vm.list_copy(list)?;
        self.return_fresh_list(value)
    }

    pub fn list_reverse(&mut self, list: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        let value = self.vm.list_reverse(list)?;
        self.return_fresh_list(value)
    }

    pub fn list_concat(&mut self, left: TaggedValue, right: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(left)?;
        self.require_rooted_value(right)?;
        let value = self.vm.list_concat(left, right)?;
        self.return_fresh_list(value)
    }

    pub fn list_swap(
        &mut self,
        list: TaggedValue,
        left: TaggedValue,
        right: TaggedValue,
    ) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(left)?;
        self.require_rooted_value(right)?;
        self.vm.list_swap(list, left, right)
    }

    pub fn list_clear(&mut self, list: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.vm.list_clear(list)
    }

    pub fn list_insert(
        &mut self,
        list: TaggedValue,
        index: TaggedValue,
        value: TaggedValue,
    ) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(index)?;
        self.require_rooted_value(value)?;
        self.vm.list_insert(list, index, value)
    }

    pub fn list_remove(
        &mut self,
        list: TaggedValue,
        index: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(index)?;
        let value = self.vm.list_remove(list, index)?;
        Ok(self.retain_collection_result(value))
    }

    pub fn list_take(&mut self, list: TaggedValue, count: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(count)?;
        let value = self.vm.list_take(list, count)?;
        self.return_fresh_list(value)
    }

    pub fn list_drop(&mut self, list: TaggedValue, count: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(count)?;
        let value = self.vm.list_drop(list, count)?;
        self.return_fresh_list(value)
    }

    pub fn list_first(&mut self, list: TaggedValue) -> Result<TaggedValue, VmError> {
        self.require_rooted_value(list)?;
        let value = self.vm.list_first(list)?;
        Ok(self.retain_collection_result(value))
    }

    pub fn list_last(&mut self, list: TaggedValue) -> Result<TaggedValue, VmError> {
        self.require_rooted_value(list)?;
        let value = self.vm.list_last(list)?;
        Ok(self.retain_collection_result(value))
    }

    pub fn list_append(&mut self, list: TaggedValue, other: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(other)?;
        self.vm.list_append(list, other)
    }

    pub fn list_enumerate(&mut self, list: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        let value = self.vm.list_enumerate(list)?;
        self.return_fresh_list(value)
    }

    pub fn list_flatten(&mut self, list: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        let value = self.vm.list_flatten(list)?;
        self.return_fresh_list(value)
    }

    pub fn list_zip(&mut self, left: TaggedValue, right: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(left)?;
        self.require_rooted_value(right)?;
        let value = self.vm.list_zip(left, right)?;
        self.return_fresh_list(value)
    }

    pub fn list_join(&mut self, list: TaggedValue, separator: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        self.require_rooted_value(separator)?;
        let joined = self.vm.list_join(list, separator)?;
        self.return_string(&joined)
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

    /// Return values in the same order as `return_map_keys`.
    pub fn return_map_values(&mut self, map: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(map)?;
        let values = self.vm.map_values(map)?;
        self.write_return(values);
        Ok(())
    }

    pub fn return_map_entries(&mut self, map: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(map)?;
        let entries = self.vm.map_entries(map)?;
        self.write_return(entries);
        Ok(())
    }

    pub fn map_copy(&mut self, map: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(map)?;
        let value = self.vm.map_copy(map)?;
        self.write_return(value);
        Ok(())
    }

    pub fn map_clear(&mut self, map: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(map)?;
        self.vm.map_clear(map)
    }

    pub fn map_merge(&mut self, map: TaggedValue, other: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(map)?;
        self.require_rooted_value(other)?;
        self.vm.map_merge(map, other)
    }

    pub fn map_get_or_insert(
        &mut self,
        map: TaggedValue,
        key: TaggedValue,
        value: TaggedValue,
    ) -> Result<TaggedValue, VmError> {
        self.require_rooted_value(map)?;
        self.require_rooted_value(key)?;
        self.require_rooted_value(value)?;
        let stored = self.vm.map_get_or_insert(map, key, value)?;
        Ok(self.retain_collection_result(stored))
    }

    pub fn map_insert_absent(
        &mut self,
        map: TaggedValue,
        key: TaggedValue,
        value: TaggedValue,
    ) -> Result<bool, VmError> {
        self.require_rooted_value(map)?;
        self.require_rooted_value(key)?;
        self.require_rooted_value(value)?;
        self.vm.map_insert_absent(map, key, value)
    }

    /// Return a fresh List containing a new String for each piece.
    pub fn return_string_list(&mut self, pieces: &[&str]) -> Result<(), VmError> {
        let list = self.vm.new_string_list(pieces)?;
        self.write_return(list);
        Ok(())
    }

    /// Return a fresh List of the Unicode scalar values of `text`.
    pub fn return_char_list(&mut self, text: &str) -> Result<(), VmError> {
        let list = self.vm.new_char_list(text)?;
        self.write_return(list);
        Ok(())
    }

    /// Return a fresh List of immediate i64 values.
    pub fn return_i64_list(&mut self, values: &[i64]) -> Result<(), VmError> {
        let list = self.vm.new_i64_list(values)?;
        self.write_return(list);
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

    pub(crate) fn argument_snapshot(&self, i: u8) -> Result<TaggedValue, VmError> {
        if i >= self.arg_count {
            return Err(VmError::ArityError {
                expected: i.saturating_add(1),
                got: self.arg_count,
            });
        }
        let task = self.vm.task_ref(self.task_id).ok_or(VmError::InvalidTask)?;
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
        self.push_movable_root(value)
    }

    /// Retain a live VM value in an updateable movable root slot.
    pub fn root_value(&self, value: TaggedValue) -> Result<BuiltinRoot, VmError> {
        self.require_rooted_value(value)?;
        self.push_movable_root(value)
    }

    fn push_movable_root(&self, value: TaggedValue) -> Result<BuiltinRoot, VmError> {
        self.dirty_roots.set(true);
        let mut roots = self.vm.worker_roots().temporary_values.borrow_mut();
        let index = roots.len();
        roots.push(value);
        Ok(BuiltinRoot {
            context_id: self.context_id,
            index,
        })
    }

    /// Snapshot a movable root for immediate inspection. Do not reuse the
    /// returned heap address across allocation or collection; reload the handle.
    pub fn load_rooted(&self, root: &BuiltinRoot) -> Result<TaggedValue, VmError> {
        if root.context_id != self.context_id || root.index < self.root_base {
            return Err(VmError::TypeError);
        }
        self.vm
            .worker_roots()
            .temporary_values
            .borrow()
            .get(root.index)
            .copied()
            .ok_or(VmError::TypeError)
    }

    /// Alias for [`Self::load_rooted`] emphasizing reload after heap allocation.
    pub fn reload_root(&self, root: &BuiltinRoot) -> Result<TaggedValue, VmError> {
        self.load_rooted(root)
    }

    /// Read element `index` of a rooted `List` into a movable `BuiltinRoot` without pinning.
    pub fn list_get_rooted(
        &self,
        list: &BuiltinRoot,
        index: usize,
    ) -> Result<BuiltinRoot, VmError> {
        let list_val = self.reload_root(list)?;
        let idx_val = TaggedValue::try_from_u64(index as u64).ok_or(VmError::InvalidIndex)?;
        let elem = self.vm.list_get(list_val, idx_val)?;
        self.push_movable_root(elem)
    }

    #[inline]
    pub fn arg_i64(&self, i: u8) -> Result<i64, VmError> {
        let v = self.argument_snapshot(i)?;
        if let Some(n) = v.as_i64() {
            return Ok(n);
        }
        self.number_of(v)?
            .to_i64_checked()
            .ok_or(VmError::NumericOverflow)
    }

    #[inline]
    pub fn arg_u64(&self, i: u8) -> Result<u64, VmError> {
        let v = self.argument_snapshot(i)?;
        if let Some(n) = v.as_u64() {
            return Ok(n);
        }
        if let Some(n) = v.as_usize_imm() {
            return Ok(n);
        }
        self.number_of(v)?
            .to_u64_checked()
            .ok_or(VmError::NumericOverflow)
    }

    #[inline]
    pub fn arg_f64(&self, i: u8) -> Result<f64, VmError> {
        let v = self.argument_snapshot(i)?;
        if let Some(f) = v.as_f64() {
            return Ok(f);
        }
        Ok(self.number_of(v)?.to_f64())
    }

    /// Copy numeric data out of a live argument, retaining no managed pointer.
    #[inline]
    pub fn arg_number(&self, i: u8) -> Result<Number, VmError> {
        let value = self.argument_snapshot(i)?;
        self.number_of(value)
    }

    /// Read an immediate number. Heap values that are not numbers are `TypeError`.
    #[inline]
    pub fn number_of(&self, value: TaggedValue) -> Result<Number, VmError> {
        // SAFETY: `value` was copied from a rooted VM slot. Number payloads are
        // immediates or 1/2-word scalar boxes read synchronously without allocation.
        unsafe { Number::from_tagged(value) }.ok_or(VmError::TypeError)
    }

    /// Read an immutable Type descriptor, rejecting foreign or malformed indices.
    pub fn arg_type(&self, i: u8) -> Result<TypeIndex, VmError> {
        let ty = self
            .argument_snapshot(i)?
            .as_type()
            .ok_or(VmError::TypeError)?;
        self.vm.type_value(ty)?.as_type().ok_or(VmError::TypeError)
    }

    /// Inspect the actual dynamic type of a rooted argument.
    pub fn arg_value_type(&self, i: u8) -> Result<TypeIndex, VmError> {
        self.vm.reflected_type(self.argument_snapshot(i)?)
    }

    pub fn return_type(&mut self, ty: TypeIndex) -> Result<(), VmError> {
        let value = self.vm.type_value(ty)?;
        self.write_return(value);
        Ok(())
    }

    #[inline]
    pub fn arg_char(&self, i: u8) -> Result<char, VmError> {
        self.argument_snapshot(i)?
            .as_char()
            .ok_or(VmError::TypeError)
    }

    #[inline]
    pub fn arg_bool(&self, i: u8) -> Result<bool, VmError> {
        let v = self.argument_snapshot(i)?;
        v.as_bool().ok_or(VmError::TypeError)
    }

    /// Zero-copy borrow of a heap string argument as `&str` for synchronous inspection.
    #[inline]
    pub fn with_arg_str<R>(&self, i: u8, f: impl FnOnce(&str) -> R) -> Result<R, VmError> {
        let v = self.argument_snapshot(i)?;
        with_heap_string_str(v, f).ok_or(VmError::TypeError)
    }

    /// Zero-copy borrow of two heap string arguments `(i, j)` as `(&str, &str)`.
    #[inline]
    pub fn with_arg_str_pair<R>(
        &self,
        i: u8,
        j: u8,
        f: impl FnOnce(&str, &str) -> R,
    ) -> Result<R, VmError> {
        let va = self.argument_snapshot(i)?;
        let vb = self.argument_snapshot(j)?;
        with_heap_string_str(va, |sa| with_heap_string_str(vb, |sb| f(sa, sb)))
            .flatten()
            .ok_or(VmError::TypeError)
    }

    /// Single-allocation concatenation of string arguments `(i, j)`.
    pub fn return_concat_arg_strings(&mut self, i: u8, j: u8) -> Result<(), VmError> {
        let va = self.argument_snapshot(i)?;
        let vb = self.argument_snapshot(j)?;
        let val = self.vm.alloc_string_concat(self.task_id, va, vb)?;
        self.write_return(val);
        Ok(())
    }

    /// Single-allocation byte-subrange slice `[start_byte..end_byte]` of string argument `i`.
    pub fn return_subrange_arg_string(
        &mut self,
        i: u8,
        start_byte: usize,
        end_byte: usize,
    ) -> Result<(), VmError> {
        let va = self.argument_snapshot(i)?;
        let val = self
            .vm
            .alloc_string_subrange(self.task_id, va, start_byte, end_byte)?;
        self.write_return(val);
        Ok(())
    }

    /// Read a heap string argument as owned UTF-8.
    pub fn arg_string(&self, i: u8) -> Result<String, VmError> {
        let v = self.argument_snapshot(i)?;
        heap_string_to_owned(v).ok_or(VmError::TypeError)
    }

    /// Read an optional heap string argument (`?String`) as owned UTF-8.
    pub fn arg_optional_string(&self, i: u8) -> Result<Option<String>, VmError> {
        let v = self.argument_snapshot(i)?;
        if v.is_null() {
            return Ok(None);
        }
        heap_string_to_owned(v).map(Some).ok_or(VmError::TypeError)
    }

    pub fn set_return(&mut self, val: TaggedValue) -> Result<(), VmError> {
        if val.is_heap()
            && !self
                .vm
                .worker_roots()
                .temporary_values
                .borrow()
                .contains(&val)
        {
            return Err(VmError::TypeError);
        }
        self.write_return(val);
        Ok(())
    }

    pub(crate) fn write_return(&mut self, val: TaggedValue) {
        if let Some(t) = self.vm.task_mut(self.task_id) {
            t.registers.set(Reg(0), val);
        }
    }

    pub fn return_unit(&mut self) {
        self.write_return(TaggedValue::UNIT);
    }

    pub fn return_null(&mut self) {
        self.write_return(TaggedValue::NULL);
    }

    pub fn return_i64(&mut self, v: i64) -> Result<(), VmError> {
        self.return_number(Number::I64(v))
    }

    pub fn return_u64(&mut self, v: u64) -> Result<(), VmError> {
        self.return_number(Number::U64(v))
    }

    pub fn return_u8(&mut self, v: u8) -> Result<(), VmError> {
        let val = self
            .vm
            .typed_number_value(Number::U64(u64::from(v)), Intrinsic::U8)?;
        self.write_return(val);
        Ok(())
    }

    pub fn return_usize(&mut self, v: usize) -> Result<(), VmError> {
        let val = self
            .vm
            .typed_number_value(Number::U64(v as u64), Intrinsic::Usize)?;
        self.write_return(val);
        Ok(())
    }

    pub fn arg_usize(&self, i: u8) -> Result<usize, VmError> {
        crate::bytes_obj::checked_usize_arg(self.arg(i)?)
    }

    pub fn arg_byte(&self, i: u8) -> Result<u8, VmError> {
        crate::bytes_obj::checked_byte_arg(self.arg(i)?)
    }

    /// Borrow a validated UTF-8 `String` argument's bytes without copying.
    /// The closure receives no mutable VM handle, preventing GC safepoints during the borrow.
    pub fn with_str_bytes<R>(
        &self,
        val: TaggedValue,
        f: impl FnOnce(&[u8]) -> Result<R, VmError>,
    ) -> Result<R, VmError> {
        self.require_rooted_value(val)?;
        let slice = crate::bytes_obj::heap_str_slice(val).ok_or(VmError::TypeError)?;
        f(slice)
    }

    /// Borrow a `Bytes` object's active `[offset .. offset + len]` byte slice without copying.
    pub fn with_bytes_slice<R>(
        &self,
        val: TaggedValue,
        f: impl FnOnce(&[u8]) -> Result<R, VmError>,
    ) -> Result<R, VmError> {
        self.require_rooted_value(val)?;
        let layout = self.vm.bytes_layout(val)?;
        f(layout.as_slice())
    }

    /// Detach any shared COW view on `root`, reserve at least `min_cap` bytes if requested,
    /// and pass `(writable_slice, &mut logical_len)` to `f` with no GC safepoints possible.
    pub fn with_bytes_mut<R>(
        &mut self,
        root: &BuiltinRoot,
        min_cap: Option<usize>,
        f: impl FnOnce(&mut [u8], &mut usize) -> Result<R, VmError>,
    ) -> Result<R, VmError> {
        let val = self.load_rooted(root)?;
        let cur_len = self.vm.bytes_len(val)?;
        let req = min_cap.unwrap_or(cur_len).max(cur_len);
        let unique = self.vm.ensure_bytes_unique(val, req)?;
        let layout = self.vm.bytes_layout(unique)?;
        let avail = layout.capacity.saturating_sub(layout.offset);
        let mut logical_len = layout.length;
        // SAFETY: `ensure_bytes_unique` guaranteed `share_count == 1` and `offset + avail <= capacity`.
        // `f` cannot access `self` or allocate on the managed heap while `buf` is alive.
        let buf = if avail == 0 {
            &mut [][..]
        } else {
            unsafe { std::slice::from_raw_parts_mut(layout.data_ptr.add(layout.offset), avail) }
        };
        let out = f(buf, &mut logical_len)?;
        if logical_len > avail {
            return Err(VmError::InvalidIndex);
        }
        crate::collections::write_u64_atomic(
            layout.wrapper,
            0,
            TaggedValue::from_u64(logical_len as u64).raw(),
        );
        Ok(out)
    }

    pub fn alloc_bytes(&mut self, data: &[u8]) -> Result<TaggedValue, VmError> {
        let value = self.vm.alloc_bytes_from_slice(data)?;
        Ok(self.retain_collection_result(value))
    }

    pub fn return_new_bytes(&mut self, capacity: usize, initial_len: usize) -> Result<(), VmError> {
        let value = self.vm.new_bytes_with_len(capacity, initial_len)?;
        self.write_return(value);
        Ok(())
    }

    pub fn bytes_len(&self, bytes: TaggedValue) -> Result<usize, VmError> {
        self.require_rooted_value(bytes)?;
        self.vm.bytes_len(bytes)
    }

    pub fn bytes_cap(&self, bytes: TaggedValue) -> Result<usize, VmError> {
        self.require_rooted_value(bytes)?;
        self.vm.bytes_cap(bytes)
    }

    pub fn bytes_get(&self, bytes: TaggedValue, index: usize) -> Result<u8, VmError> {
        self.require_rooted_value(bytes)?;
        self.vm.bytes_get(bytes, index)
    }

    pub fn bytes_set(&mut self, bytes: TaggedValue, index: usize, byte: u8) -> Result<(), VmError> {
        self.require_rooted_value(bytes)?;
        self.vm.bytes_set(bytes, index, byte)
    }

    pub fn bytes_push(&mut self, bytes: TaggedValue, byte: u8) -> Result<(), VmError> {
        self.require_rooted_value(bytes)?;
        self.vm.bytes_push(bytes, byte)
    }

    pub fn bytes_clear(&mut self, bytes: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(bytes)?;
        self.vm.bytes_clear(bytes)
    }

    pub fn return_bytes_slice(
        &mut self,
        bytes: TaggedValue,
        start: usize,
        end: usize,
    ) -> Result<(), VmError> {
        self.require_rooted_value(bytes)?;
        let value = self.vm.bytes_slice(bytes, start, end)?;
        self.write_return(value);
        Ok(())
    }

    pub fn return_bytes_copy(&mut self, bytes: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(bytes)?;
        let value = self.vm.bytes_copy(bytes)?;
        self.write_return(value);
        Ok(())
    }

    pub fn bytes_extend(&mut self, dst: TaggedValue, src: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(dst)?;
        self.require_rooted_value(src)?;
        self.vm.bytes_extend(dst, src)
    }

    pub fn return_bytes_from_str(&mut self, s: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(s)?;
        let value = self.vm.bytes_from_str(s)?;
        self.write_return(value);
        Ok(())
    }

    pub fn return_bytes_to_str(&mut self, bytes: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(bytes)?;
        let value = self.vm.bytes_to_str(bytes)?;
        self.write_return(value);
        Ok(())
    }

    pub fn return_bytes_from_list(&mut self, list: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(list)?;
        let value = self.vm.bytes_from_list(list)?;
        self.write_return(value);
        Ok(())
    }

    pub fn return_bytes_to_list(&mut self, bytes: TaggedValue) -> Result<(), VmError> {
        self.require_rooted_value(bytes)?;
        let value = self.vm.bytes_to_list(bytes)?;
        self.write_return(value);
        Ok(())
    }

    pub fn bytes_find(
        &self,
        haystack: TaggedValue,
        needle: TaggedValue,
        start: usize,
    ) -> Result<Option<usize>, VmError> {
        self.require_rooted_value(haystack)?;
        self.require_rooted_value(needle)?;
        self.vm.bytes_find(haystack, needle, start)
    }

    pub fn bytes_eq(&self, left: TaggedValue, right: TaggedValue) -> Result<bool, VmError> {
        self.require_rooted_value(left)?;
        self.require_rooted_value(right)?;
        self.vm.bytes_eq(left, right)
    }

    pub fn bytes_read_u64_le(&self, bytes: TaggedValue, offset: usize) -> Result<u64, VmError> {
        self.require_rooted_value(bytes)?;
        self.vm.bytes_read_u64_le(bytes, offset)
    }

    pub fn resources(&self) -> parking_lot::MutexGuard<'_, crate::resources::VmResourceTable> {
        self.vm.resources()
    }

    pub fn resources_mut(
        &mut self,
    ) -> parking_lot::MutexGuard<'_, crate::resources::VmResourceTable> {
        self.vm.resources_mut()
    }

    pub fn is_ffi_library_allowed(&self, name: &str) -> bool {
        self.vm.is_ffi_library_allowed(name)
    }

    /// Attach a weak-reference cleanup callback that executes when `target` is swept by GC
    /// or when the `Vm` drops.
    pub fn attach_weak_finalizer(
        &mut self,
        target: TaggedValue,
        cleanup: impl FnOnce() + Send + 'static,
    ) -> Result<(), VmError> {
        self.require_rooted_value(target)?;
        self.vm.register_weak_finalizer(target, cleanup)
    }

    /// Detach any shared COW backing storage on a rooted `Bytes` value prior to a native FFI call.
    pub fn prepare_ffi_bytes_mut(&mut self, root: &BuiltinRoot) -> Result<(), VmError> {
        let val = self.load_rooted(root)?;
        let len = self.vm.bytes_len(val)?;
        let _ = self.vm.ensure_bytes_unique(val, len)?;
        Ok(())
    }

    /// Execute a non-allocating native FFI call under the VM's `native_frame_depth` guard.
    /// Borrowing `&Vm` immutably inside `f` statically prevents managed heap allocations
    /// and GC safepoints while raw pointers are live on the C stack.
    pub fn with_native_ffi_frame<R>(
        &mut self,
        f: impl FnOnce(&Vm) -> Result<R, VmError>,
    ) -> Result<R, VmError> {
        self.vm.with_native_frame(|vm| f(&*vm))
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

    pub fn return_optional_string_pair(
        &mut self,
        pair: Option<(&str, &str)>,
    ) -> Result<(), VmError> {
        let Some((left, right)) = pair else {
            self.return_null();
            return Ok(());
        };
        self.check_string_size(left.len())?;
        self.check_string_size(right.len())?;
        let left_val = self.vm.alloc_string(left)?;
        let left_root = self.push_movable_root(left_val)?;
        let right_val = self.vm.alloc_string(right)?;
        let left_reloaded = self.reload_root(&left_root)?;
        let str_ty = Intrinsic::Str.type_index();
        let value = self
            .vm
            .alloc_tuple(&[str_ty, str_ty], &[left_reloaded, right_val])?;
        self.write_return(value);
        Ok(())
    }

    /// Build a complex return value (`Tuple`, `List`, `Map`, `?String`, numbers)
    /// while rooting every intermediate allocation across moving GC cycles.
    pub fn return_native(&mut self, spec: NativeValue) -> Result<(), VmError> {
        let (value, _) = self.vm.alloc_native(self.task_id, spec)?;
        self.write_return(value);
        Ok(())
    }

    pub fn return_char(&mut self, value: char) {
        self.write_return(TaggedValue::from_char(value));
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

    /// Check that a String of `bytes` UTF-8 bytes can be returned, before the
    /// host builds it: the Display limit applies, then the managed object limit.
    pub fn check_string_size(&self, bytes: usize) -> Result<(), VmError> {
        self.check_display_size(bytes)?;
        Vm::check_string_bytes(bytes)
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
                && self.vm.type_pool_read().canonical_type(ty).is_none()
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

    /// Native equality of two rooted values when VM kernels can decide it;
    /// `None` defers two non-scalar values to source-level `PartialEq`.
    pub fn native_equal(
        &self,
        left: TaggedValue,
        right: TaggedValue,
    ) -> Result<Option<bool>, VmError> {
        self.require_rooted_value(left)?;
        self.require_rooted_value(right)?;
        self.vm.native_equal(left, right)
    }

    /// Element count when `value` is a tuple object.
    pub fn tuple_length(&self, value: TaggedValue) -> Result<Option<usize>, VmError> {
        self.require_rooted_value(value)?;
        self.vm.tuple_length(value)
    }

    /// Tuple element `index`, retained for the caller as a native result.
    pub fn tuple_element(&self, value: TaggedValue, index: i64) -> Result<TaggedValue, VmError> {
        self.require_rooted_value(value)?;
        Ok(self.retain_collection_result(self.vm.tuple_element(value, index)?))
    }

    /// Whether `value` is the `done` result of any concrete `IterationStep(T)`.
    /// Nominal enums, templates and non-enum values are type errors.
    pub fn iteration_step_done(&self, value: TaggedValue) -> Result<bool, VmError> {
        self.vm.iteration_step_done(value)
    }

    /// Synchronously collect with this task's arguments and slots rooted.
    pub fn collect_garbage(&mut self) -> Result<bool, VmError> {
        self.vm.collect_garbage()
    }

    pub fn completed_collections(&self) -> u64 {
        self.vm.completed_collections()
    }

    pub fn jit_enabled(&self) -> bool {
        self.vm.jit_enabled()
    }

    pub fn jit_stats(&self) -> crate::JitStats {
        self.vm.jit_stats()
    }

    pub fn scalar_hash(&self, value: TaggedValue) -> Result<u64, VmError> {
        self.require_rooted_value(value)?;
        self.vm.scalar_hash(value)
    }

    pub fn script_args(&self) -> &[String] {
        self.vm.script_args()
    }

    pub fn rng_next_u64(&mut self) -> u64 {
        self.vm.shared.rng.lock().next_u64()
    }

    pub fn rng_seed(&mut self, mode: i64, s1: i64, s2: i64) -> Result<(i64, i64), VmError> {
        self.vm.shared.rng.lock().seed(mode, s1, s2)
    }

    pub fn format_backtrace(
        &self,
        msg: Option<&str>,
        skip_frames: usize,
    ) -> Result<String, VmError> {
        self.vm.format_backtrace(self.task_id, msg, skip_frames)
    }

    pub fn file_open(&mut self, path: &str, mode: &str) -> (i64, i64, String) {
        self.vm.shared.files.open(path, mode)
    }

    pub fn file_temp(&mut self) -> (i64, i64, String) {
        self.vm.shared.files.create_temp(&self.vm.shared.rng)
    }

    pub fn file_close(&mut self, id: i64) -> (i64, String) {
        self.vm.shared.files.close(id)
    }

    pub fn file_is_open(&self, id: i64) -> bool {
        self.vm.shared.files.is_open(id)
    }

    pub fn file_flush(&mut self, id: i64) -> (i64, String) {
        self.vm.shared.files.flush(id)
    }

    pub fn file_read_all(&mut self, id: i64) -> Result<(i64, String), VmError> {
        self.vm.shared.files.read_all(id)
    }

    pub fn file_read_line(
        &mut self,
        id: i64,
        keep_newline: bool,
    ) -> Result<(i64, Option<String>, String), VmError> {
        self.vm.shared.files.read_line(id, keep_newline)
    }

    pub fn file_read_chars(
        &mut self,
        id: i64,
        count: i64,
    ) -> Result<(i64, Option<String>, String), VmError> {
        self.vm.shared.files.read_chars(id, count)
    }

    pub fn file_read_bytes(
        &mut self,
        id: i64,
        max_bytes: i64,
    ) -> Result<(i64, Vec<u8>, String), VmError> {
        self.vm.shared.files.read_bytes(id, max_bytes)
    }

    pub fn file_read_number(&mut self, id: i64) -> (i64, Option<f64>, String) {
        self.vm.shared.files.read_number(id)
    }

    pub fn file_write_bytes(&mut self, id: i64, bytes: &[u8]) -> (i64, String) {
        self.vm.shared.files.write_bytes(id, bytes)
    }

    pub fn file_seek(&mut self, id: i64, whence: i64, offset: i64) -> (i64, i64, String) {
        self.vm.shared.files.seek(id, whence, offset)
    }

    /// True if `self.task_id` has been cancelled and is not yet unwinding `defer` blocks.
    #[inline]
    pub fn should_abort_for_cancel(&self) -> bool {
        let already_unwinding = self
            .vm
            .task_ref(self.task_id)
            .is_some_and(|t| t.unwinding_cancel);
        !already_unwinding && self.vm.scheduler().is_cancelled(self.task_id)
    }

    /// Non-blocking `libuv` timer sleep (`uv_timer_t` on `nessa-io-reactor`).
    pub fn sleep_async_ms(&mut self, duration_ms: u64) -> Result<(), VmError> {
        match self.vm.scheduler().poll_task_op(self.task_id) {
            TaskOpPoll::Ready(TaskOpResult::Unit) => {
                self.return_unit();
                Ok(())
            }
            TaskOpPoll::Ready(_) => unreachable!("unexpected TaskOpResult variant for sleep"),
            TaskOpPoll::Pending => {
                self.outcome = BuiltinOutcome::SuspendRetry;
                Ok(())
            }
            TaskOpPoll::Vacant => {
                if self.should_abort_for_cancel() {
                    self.outcome = BuiltinOutcome::StartCancelUnwind;
                    return Ok(());
                }
                if duration_ms == 0 {
                    self.return_unit();
                    if self.vm.scheduler().has_other_ready_tasks() {
                        self.outcome = BuiltinOutcome::Yield;
                    }
                    return Ok(());
                }
                self.vm.scheduler().submit_sleep(self.task_id, duration_ms);
                self.outcome = BuiltinOutcome::SuspendRetry;
                Ok(())
            }
        }
    }

    /// Non-blocking `libuv` child process execution (`uv_spawn` + pipes on `nessa-io-reactor`).
    ///
    /// `prepare` runs only on initial submission (`TaskOpPoll::Vacant`) and may return `Ok(None)`
    /// after writing a synchronous return value (such as empty shell command fast-path).
    pub fn spawn_process_async<P>(&mut self, prepare: P) -> Result<(), VmError>
    where
        P: FnOnce(&mut Self) -> Result<Option<ProcessSpec>, VmError>,
    {
        match self.vm.scheduler().poll_task_op(self.task_id) {
            TaskOpPoll::Ready(TaskOpResult::Process(out)) => {
                self.check_string_size(out.stdout.len())?;
                self.check_string_size(out.stderr.len())?;
                self.return_native(NativeValue::Tuple(vec![
                    NativeValue::I64(out.code),
                    NativeValue::I64(out.exit_code),
                    NativeValue::Str(out.stdout),
                    NativeValue::Str(out.stderr),
                ]))
            }
            TaskOpPoll::Ready(_) => unreachable!("unexpected TaskOpResult variant for process"),
            TaskOpPoll::Pending => {
                self.outcome = BuiltinOutcome::SuspendRetry;
                Ok(())
            }
            TaskOpPoll::Vacant => {
                if self.should_abort_for_cancel() {
                    self.outcome = BuiltinOutcome::StartCancelUnwind;
                    return Ok(());
                }
                let Some(spec) = prepare(self)? else {
                    return Ok(());
                };
                self.vm.scheduler().submit_process(self.task_id, spec);
                self.outcome = BuiltinOutcome::SuspendRetry;
                Ok(())
            }
        }
    }

    /// Offload blocking file/filesystem work onto `libuv`'s `uv_queue_work` threadpool without
    /// blocking any compute worker thread.
    ///
    /// `prepare` runs only on initial submission (`TaskOpPoll::Vacant`) to validate and extract
    /// arguments from GC-managed registers; returning `Ok(None)` completes synchronously.
    pub fn run_blocking_native<P, W>(&mut self, prepare: P) -> Result<(), VmError>
    where
        P: FnOnce(&mut Self) -> Result<Option<W>, VmError>,
        W: FnOnce(&VmIoContext) -> Result<NativeValue, VmError> + Send + 'static,
    {
        match self.vm.scheduler().poll_task_op(self.task_id) {
            TaskOpPoll::Ready(TaskOpResult::Work(boxed)) => {
                let res = *boxed
                    .downcast::<Result<NativeValue, VmError>>()
                    .expect("blocking IO task result type matches");
                let native = res?;
                self.return_native(native)
            }
            TaskOpPoll::Ready(_) => {
                unreachable!("unexpected TaskOpResult variant for blocking IO")
            }
            TaskOpPoll::Pending => {
                self.outcome = BuiltinOutcome::SuspendRetry;
                Ok(())
            }
            TaskOpPoll::Vacant => {
                if self.should_abort_for_cancel() {
                    self.outcome = BuiltinOutcome::StartCancelUnwind;
                    return Ok(());
                }
                let Some(work_fn) = prepare(self)? else {
                    return Ok(());
                };
                let io_ctx = VmIoContext {
                    shared: Arc::clone(&self.vm.shared),
                };
                self.vm
                    .scheduler()
                    .submit_work(self.task_id, move || -> Result<NativeValue, VmError> {
                        work_fn(&io_ctx)
                    });
                self.outcome = BuiltinOutcome::SuspendRetry;
                Ok(())
            }
        }
    }

    pub fn type_pool(&self) -> VmTypePoolRef<'_> {
        self.vm.type_pool()
    }

    /// Format any tagged value for display (print/println).
    pub fn format_value(&self, val: TaggedValue) -> Result<String, VmError> {
        if val.is_heap()
            && !self
                .vm
                .worker_roots()
                .temporary_values
                .borrow()
                .contains(&val)
        {
            return Err(VmError::TypeError);
        }
        if let Some(ty) = val.as_type() {
            return self
                .vm
                .type_pool_read()
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

    pub fn task_yield(&mut self) {
        self.write_return(TaggedValue::UNIT);
        self.outcome = BuiltinOutcome::Yield;
    }

    pub fn suspend_retry(&mut self) {
        self.outcome = BuiltinOutcome::SuspendRetry;
    }

    pub fn defer_push(&mut self) {
        if let Some(t) = self.vm.task_mut(self.task_id) {
            let resume_pc = t.pc;
            t.unwind_stack
                .push(runtime::UnwindEntry::DeferPc(resume_pc));
            t.registers.set(Reg(0), TaggedValue::FALSE);
        }
    }

    pub fn defer_pop(&mut self) {
        if let Some(t) = self.vm.task_mut(self.task_id) {
            if let Some(pos) = t
                .unwind_stack
                .iter()
                .rposition(|e| matches!(e, runtime::UnwindEntry::DeferPc(_)))
            {
                t.unwind_stack.remove(pos);
            }
            t.registers.set(Reg(0), TaggedValue::UNIT);
        }
    }

    pub fn defer_resume_unwind(&mut self) {
        self.write_return(TaggedValue::UNIT);
        self.outcome = BuiltinOutcome::ResumeCancelUnwind;
    }

    pub fn task_is_ready(&self, future_val: TaggedValue) -> Result<bool, VmError> {
        self.require_rooted_value(future_val)?;
        let fid = self.vm.decode_future_id(future_val)?;
        let promises = self.vm.roots().promises.shard(fid).lock();
        let entry = promises.get(&fid).ok_or(VmError::TypeError)?;
        Ok(!matches!(
            entry.state,
            crate::futures::PromiseState::Pending
        ))
    }

    pub fn task_await_all(&mut self, list_handle: &BuiltinRoot) -> Result<(), VmError> {
        if self.vm.scheduler().is_cancelled(self.task_id) {
            if let Some(st) = self.vm.shared.await_all_ops.remove(self.task_id) {
                st.early_exit
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            self.outcome = BuiltinOutcome::StartCancelUnwind;
            return Ok(());
        }

        let existing = self.vm.shared.await_all_ops.get(self.task_id);
        if let Some(st) = existing {
            if !st.early_exit.load(std::sync::atomic::Ordering::Acquire)
                && st.remaining.load(std::sync::atomic::Ordering::Acquire) > 0
            {
                self.outcome = BuiltinOutcome::SuspendRetry;
                return Ok(());
            }
            let _ = self.vm.shared.await_all_ops.remove(self.task_id);
        }

        let list_val = self.load_rooted(list_handle)?;
        let elems = self.vm.snapshot_list_elements(list_val)?;
        let len = elems.len();
        let mut fids = Vec::with_capacity(len);
        for elem in elems {
            fids.push(self.vm.decode_future_id(elem)?);
        }

        let completed_vals = if fids.is_empty() {
            Vec::new()
        } else {
            let mut promises = self.vm.roots().promises.lock_fids(&fids);
            let mut failed_fid = None;
            let mut failed_err = None;
            let mut cancelled = false;
            let mut pending_unique = Vec::new();
            let mut seen_pending = std::collections::HashSet::new();
            for &fid in &fids {
                let entry = promises.get(fid).ok_or(VmError::TypeError)?;
                match &entry.state {
                    crate::futures::PromiseState::Failed(err) => {
                        failed_fid = Some(fid);
                        failed_err = Some(err.clone());
                        break;
                    }
                    crate::futures::PromiseState::Cancelled => {
                        cancelled = true;
                        break;
                    }
                    crate::futures::PromiseState::Pending => {
                        if seen_pending.insert(fid) {
                            pending_unique.push(fid);
                        }
                    }
                    crate::futures::PromiseState::Completed(_) => {}
                }
            }
            if let (Some(fid), Some(err)) = (failed_fid, failed_err) {
                drop(promises);
                self.vm.mark_promise_failure_observed(fid);
                return Err(err);
            }
            if cancelled {
                return Err(VmError::TaskCancelled);
            }
            if !pending_unique.is_empty() {
                let state = std::sync::Arc::new(crate::futures::AwaitAllState {
                    task_id: self.task_id,
                    remaining: std::sync::atomic::AtomicUsize::new(pending_unique.len()),
                    early_exit: std::sync::atomic::AtomicBool::new(false),
                });
                for pfid in pending_unique {
                    if let Some(entry) = promises.get_mut(pfid) {
                        entry.all_waiters.push(std::sync::Arc::clone(&state));
                    }
                }
                drop(promises);
                self.vm.shared.await_all_ops.insert(self.task_id, state);
                self.outcome = BuiltinOutcome::SuspendRetry;
                return Ok(());
            }
            let mut vals = Vec::with_capacity(len);
            for &fid in &fids {
                let entry = promises
                    .get(fid)
                    .expect("all promises verified Completed under lock");
                match &entry.state {
                    crate::futures::PromiseState::Completed(v) => vals.push(*v),
                    _ => unreachable!("all promises verified Completed under lock"),
                }
            }
            vals
        };

        let base = self.vm.worker_roots().temporary_values.borrow().len();
        let out_list = self.vm.with_temporary_roots(&completed_vals, |vm| {
            let list = vm.new_list(len)?;
            let list_slot = vm.worker_roots().temporary_values.borrow().len();
            vm.worker_roots().temporary_values.borrow_mut().push(list);
            for i in 0..len {
                let (cur_list, val) = {
                    let roots = vm.worker_roots().temporary_values.borrow();
                    (roots[list_slot], roots[base + i])
                };
                vm.list_set(cur_list, TaggedValue::from_i64(i as i64), val)?;
            }
            let final_list = vm.worker_roots().temporary_values.borrow()[list_slot];
            vm.worker_roots()
                .temporary_values
                .borrow_mut()
                .truncate(list_slot);
            Ok(final_list)
        })?;
        let out_list = self.retain_collection_result(out_list);
        self.write_return(out_list);
        Ok(())
    }

    fn resolve_await_any(
        &mut self,
        list_handle: &BuiltinRoot,
    ) -> Result<Option<(usize, TaggedValue)>, VmError> {
        let list_val = self.load_rooted(list_handle)?;
        let elems = self.vm.snapshot_list_elements(list_val)?;
        let len = elems.len();
        if len == 0 {
            return Err(VmError::InvalidArgument(
                "await_any requires a non-empty list of Futures",
            ));
        }
        let mut fids = Vec::with_capacity(len);
        for elem in elems {
            fids.push(self.vm.decode_future_id(elem)?);
        }

        let (winner, any_pending) = {
            let mut promises = self.vm.roots().promises.lock_fids(&fids);
            let mut winner: Option<(usize, Result<TaggedValue, VmError>)> = None;
            let mut failed_fid_to_mark = None;
            for (idx, &fid) in fids.iter().enumerate() {
                let entry = promises.get(fid).ok_or(VmError::TypeError)?;
                match &entry.state {
                    crate::futures::PromiseState::Completed(val) => {
                        winner = Some((idx, Ok(*val)));
                        break;
                    }
                    crate::futures::PromiseState::Failed(err) => {
                        failed_fid_to_mark = Some(fid);
                        winner = Some((idx, Err(err.clone())));
                        break;
                    }
                    _ => {}
                }
            }
            if let Some(fid) = failed_fid_to_mark {
                drop(promises);
                self.vm.mark_promise_failure_observed(fid);
                (winner, false)
            } else if winner.is_some() {
                (winner, false)
            } else {
                let mut any_pending = false;
                for &fid in &fids {
                    if let Some(entry) = promises.get_mut(fid)
                        && matches!(entry.state, crate::futures::PromiseState::Pending)
                    {
                        any_pending = true;
                        if !entry.waiters.contains(&self.task_id) {
                            entry.waiters.push(self.task_id);
                        }
                    }
                }
                (None, any_pending)
            }
        };

        if let Some((winner_idx, outcome)) = winner {
            let winner_fid = fids[winner_idx];
            for (idx, &other_fid) in fids.iter().enumerate() {
                if idx != winner_idx && other_fid != winner_fid {
                    self.vm.cancel_future(other_fid);
                }
            }
            return outcome.map(|val| Some((winner_idx, val)));
        }

        if any_pending {
            self.outcome = BuiltinOutcome::SuspendRetry;
            Ok(None)
        } else {
            Err(VmError::TaskCancelled)
        }
    }

    pub fn task_await_any(&mut self, list_handle: &BuiltinRoot) -> Result<(), VmError> {
        if let Some((_, val)) = self.resolve_await_any(list_handle)? {
            self.write_return(val);
        }
        Ok(())
    }

    pub fn task_await_indexed(&mut self, list_handle: &BuiltinRoot) -> Result<(), VmError> {
        let Some((winner_idx, val)) = self.resolve_await_any(list_handle)? else {
            return Ok(());
        };
        let base = self.vm.worker_roots().temporary_values.borrow().len();
        let tuple = self
            .vm
            .with_temporary_roots(&[val, TaggedValue::UNIT], |vm| {
                let idx_val = vm.number_value(Number::I64(winner_idx as i64))?;
                vm.worker_roots().temporary_values.borrow_mut()[base + 1] = idx_val;
                let (cur_val, cur_idx) = {
                    let roots = vm.worker_roots().temporary_values.borrow();
                    (roots[base], roots[base + 1])
                };
                vm.alloc_tuple(
                    &[Intrinsic::I64.type_index(), Intrinsic::Any.type_index()],
                    &[cur_idx, cur_val],
                )
            })?;
        self.write_return(tuple);
        Ok(())
    }

    pub fn take_outcome(&mut self) -> BuiltinOutcome {
        std::mem::replace(&mut self.outcome, BuiltinOutcome::Continue)
    }

    // -----------------------------------------------------------------------
    // Task introspection (`803..=804`)
    // -----------------------------------------------------------------------

    pub fn task_current_id(&mut self) -> Result<(), VmError> {
        self.return_i64(self.task_id.0 as i64)
    }

    pub fn task_tree_snapshot(&mut self) -> Result<(), VmError> {
        let snapshot = self.vm.scheduler().format_tree_snapshot();
        self.return_string(&snapshot)
    }

    // -----------------------------------------------------------------------
    // Tuple & byte-list helpers for `std.sync` and `std.net`
    // -----------------------------------------------------------------------

    fn return_i64_any_tuple(&mut self, code: i64, val: TaggedValue) -> Result<(), VmError> {
        let base = self.vm.worker_roots().temporary_values.borrow().len();
        let tuple = self
            .vm
            .with_temporary_roots(&[val, TaggedValue::UNIT], |vm| {
                let code_val = vm.number_value(Number::I64(code))?;
                vm.worker_roots().temporary_values.borrow_mut()[base + 1] = code_val;
                let (cur_val, cur_code) = {
                    let roots = vm.worker_roots().temporary_values.borrow();
                    (roots[base], roots[base + 1])
                };
                vm.alloc_tuple(
                    &[Intrinsic::I64.type_index(), Intrinsic::Any.type_index()],
                    &[cur_code, cur_val],
                )
            })?;
        self.write_return(tuple);
        Ok(())
    }

    fn return_bool_any_tuple(&mut self, flag: bool, val: TaggedValue) -> Result<(), VmError> {
        let base = self.vm.worker_roots().temporary_values.borrow().len();
        let tuple = self.vm.with_temporary_roots(&[val], |vm| {
            let cur_val = vm.worker_roots().temporary_values.borrow()[base];
            vm.alloc_tuple(
                &[Intrinsic::Bool.type_index(), Intrinsic::Any.type_index()],
                &[TaggedValue::from_bool(flag), cur_val],
            )
        })?;
        self.write_return(tuple);
        Ok(())
    }

    pub fn extract_byte_list(
        &self,
        list_root: &BuiltinRoot,
    ) -> Result<Result<Vec<u8>, String>, VmError> {
        let list_val = self.reload_root(list_root)?;
        let elems = self.vm.snapshot_list_elements(list_val)?;
        let mut out = Vec::with_capacity(elems.len());
        for elem in elems {
            let num = self.number_of(elem)?;
            let Some(v) = num.to_i64_checked() else {
                return Ok(Err("byte value out of valid range 0..=255".to_string()));
            };
            if !(0..=255).contains(&v) {
                return Ok(Err(format!("byte value {v} out of valid range 0..=255")));
            }
            out.push(v as u8);
        }
        Ok(Ok(out))
    }

    // -----------------------------------------------------------------------
    // `std.sync` primitives (`805..=846`)
    // -----------------------------------------------------------------------

    pub fn sync_chan_new(&mut self, capacity: i64) -> Result<(), VmError> {
        let id = self.vm.roots().sync.chan_new(capacity)?;
        self.return_i64(id)
    }

    pub fn sync_chan_send(&mut self, id: i64, val_root: &BuiltinRoot) -> Result<(), VmError> {
        if self.should_abort_for_cancel() {
            if self
                .vm
                .roots()
                .sync
                .chan_take_accepted_on_cancel(id, self.task_id)
            {
                return self.return_i64(0);
            }
            let sched = self.vm.scheduler().clone();
            self.vm.roots().sync.cancel_task(&sched, self.task_id);
            self.outcome = BuiltinOutcome::StartCancelUnwind;
            return Ok(());
        }
        let val = self.load_rooted(val_root)?;
        let sched = self.vm.scheduler().clone();
        match self
            .vm
            .roots()
            .sync
            .chan_send(&sched, id, self.task_id, val)?
        {
            crate::sync_primitives::SyncPoll::Ready(code) => self.return_i64(code),
            crate::sync_primitives::SyncPoll::SuspendRetry => {
                self.outcome = BuiltinOutcome::SuspendRetry;
                Ok(())
            }
        }
    }

    pub fn sync_chan_try_send(&mut self, id: i64, val_root: &BuiltinRoot) -> Result<(), VmError> {
        let val = self.load_rooted(val_root)?;
        let sched = self.vm.scheduler().clone();
        let code = self.vm.roots().sync.chan_try_send(&sched, id, val)?;
        self.return_i64(code)
    }

    pub fn sync_chan_recv(&mut self, id: i64) -> Result<(), VmError> {
        if self.should_abort_for_cancel() {
            let sched = self.vm.scheduler().clone();
            self.vm.roots().sync.cancel_task(&sched, self.task_id);
            self.outcome = BuiltinOutcome::StartCancelUnwind;
            return Ok(());
        }
        let sched = self.vm.scheduler().clone();
        match self.vm.roots().sync.chan_recv(&sched, id, self.task_id)? {
            crate::sync_primitives::SyncPoll::Ready((code, val)) => {
                self.return_i64_any_tuple(code, val)
            }
            crate::sync_primitives::SyncPoll::SuspendRetry => {
                self.outcome = BuiltinOutcome::SuspendRetry;
                Ok(())
            }
        }
    }

    pub fn sync_chan_try_recv(&mut self, id: i64) -> Result<(), VmError> {
        let sched = self.vm.scheduler().clone();
        let (code, val) = self.vm.roots().sync.chan_try_recv(&sched, id)?;
        self.return_i64_any_tuple(code, val)
    }

    pub fn sync_chan_close(&mut self, id: i64) -> Result<(), VmError> {
        let sched = self.vm.scheduler().clone();
        self.vm.roots().sync.chan_close(&sched, id)?;
        self.return_unit();
        Ok(())
    }

    pub fn sync_chan_is_closed(&mut self, id: i64) -> Result<(), VmError> {
        let closed = self.vm.roots().sync.chan_is_closed(id)?;
        self.return_bool(closed);
        Ok(())
    }

    pub fn sync_chan_len(&mut self, id: i64) -> Result<(), VmError> {
        let len = self.vm.roots().sync.chan_len(id)?;
        self.return_i64(len)
    }

    pub fn sync_mutex_new(&mut self, val_root: &BuiltinRoot) -> Result<(), VmError> {
        let val = self.load_rooted(val_root)?;
        let id = self.vm.roots().sync.mutex_new(val);
        self.return_i64(id)
    }

    pub fn sync_mutex_lock(&mut self, id: i64) -> Result<(), VmError> {
        if self.should_abort_for_cancel() {
            let sched = self.vm.scheduler().clone();
            self.vm.roots().sync.cancel_task(&sched, self.task_id);
            self.outcome = BuiltinOutcome::StartCancelUnwind;
            return Ok(());
        }
        match self.vm.roots().sync.mutex_lock(id, self.task_id)? {
            crate::sync_primitives::SyncPoll::Ready(val) => {
                self.write_return(val);
                Ok(())
            }
            crate::sync_primitives::SyncPoll::SuspendRetry => {
                self.outcome = BuiltinOutcome::SuspendRetry;
                Ok(())
            }
        }
    }

    pub fn sync_mutex_try_lock(&mut self, id: i64) -> Result<(), VmError> {
        let ok = self.vm.roots().sync.mutex_try_lock(id, self.task_id)?;
        self.return_bool(ok);
        Ok(())
    }

    pub fn sync_mutex_unlock(&mut self, id: i64) -> Result<(), VmError> {
        let sched = self.vm.scheduler().clone();
        self.vm
            .roots()
            .sync
            .mutex_unlock(&sched, id, self.task_id)?;
        self.return_unit();
        Ok(())
    }

    pub fn sync_mutex_get(&mut self, id: i64) -> Result<(), VmError> {
        let val = self.vm.roots().sync.mutex_get(id, self.task_id)?;
        self.write_return(val);
        Ok(())
    }

    pub fn sync_mutex_set(&mut self, id: i64, val_root: &BuiltinRoot) -> Result<(), VmError> {
        let val = self.load_rooted(val_root)?;
        self.vm.roots().sync.mutex_set(id, self.task_id, val)?;
        self.return_unit();
        Ok(())
    }

    pub fn sync_mutex_is_locked(&mut self, id: i64) -> Result<(), VmError> {
        let locked = self.vm.roots().sync.mutex_is_locked(id)?;
        self.return_bool(locked);
        Ok(())
    }

    pub fn sync_rwlock_new(&mut self, val_root: &BuiltinRoot) -> Result<(), VmError> {
        let val = self.load_rooted(val_root)?;
        let id = self.vm.roots().sync.rwlock_new(val);
        self.return_i64(id)
    }

    pub fn sync_rwlock_read_lock(&mut self, id: i64) -> Result<(), VmError> {
        if self.should_abort_for_cancel() {
            let sched = self.vm.scheduler().clone();
            self.vm.roots().sync.cancel_task(&sched, self.task_id);
            self.outcome = BuiltinOutcome::StartCancelUnwind;
            return Ok(());
        }
        match self.vm.roots().sync.rwlock_read_lock(id, self.task_id)? {
            crate::sync_primitives::SyncPoll::Ready(val) => {
                self.write_return(val);
                Ok(())
            }
            crate::sync_primitives::SyncPoll::SuspendRetry => {
                self.outcome = BuiltinOutcome::SuspendRetry;
                Ok(())
            }
        }
    }

    pub fn sync_rwlock_try_read_lock(&mut self, id: i64) -> Result<(), VmError> {
        let ok = self
            .vm
            .roots()
            .sync
            .rwlock_try_read_lock(id, self.task_id)?;
        self.return_bool(ok);
        Ok(())
    }

    pub fn sync_rwlock_read_unlock(&mut self, id: i64) -> Result<(), VmError> {
        let sched = self.vm.scheduler().clone();
        self.vm
            .roots()
            .sync
            .rwlock_read_unlock(&sched, id, self.task_id)?;
        self.return_unit();
        Ok(())
    }

    pub fn sync_rwlock_write_lock(&mut self, id: i64) -> Result<(), VmError> {
        if self.should_abort_for_cancel() {
            let sched = self.vm.scheduler().clone();
            self.vm.roots().sync.cancel_task(&sched, self.task_id);
            self.outcome = BuiltinOutcome::StartCancelUnwind;
            return Ok(());
        }
        match self.vm.roots().sync.rwlock_write_lock(id, self.task_id)? {
            crate::sync_primitives::SyncPoll::Ready(val) => {
                self.write_return(val);
                Ok(())
            }
            crate::sync_primitives::SyncPoll::SuspendRetry => {
                self.outcome = BuiltinOutcome::SuspendRetry;
                Ok(())
            }
        }
    }

    pub fn sync_rwlock_try_write_lock(&mut self, id: i64) -> Result<(), VmError> {
        let ok = self
            .vm
            .roots()
            .sync
            .rwlock_try_write_lock(id, self.task_id)?;
        self.return_bool(ok);
        Ok(())
    }

    pub fn sync_rwlock_write_unlock(&mut self, id: i64) -> Result<(), VmError> {
        let sched = self.vm.scheduler().clone();
        self.vm
            .roots()
            .sync
            .rwlock_write_unlock(&sched, id, self.task_id)?;
        self.return_unit();
        Ok(())
    }

    pub fn sync_rwlock_get(&mut self, id: i64) -> Result<(), VmError> {
        let val = self.vm.roots().sync.rwlock_get(id, self.task_id)?;
        self.write_return(val);
        Ok(())
    }

    pub fn sync_rwlock_set(&mut self, id: i64, val_root: &BuiltinRoot) -> Result<(), VmError> {
        let val = self.load_rooted(val_root)?;
        self.vm.roots().sync.rwlock_set(id, self.task_id, val)?;
        self.return_unit();
        Ok(())
    }

    pub fn sync_sem_new(&mut self, permits: i64) -> Result<(), VmError> {
        let id = self.vm.roots().sync.sem_new(permits)?;
        self.return_i64(id)
    }

    pub fn sync_sem_acquire(&mut self, id: i64) -> Result<(), VmError> {
        if self.should_abort_for_cancel() {
            let sched = self.vm.scheduler().clone();
            self.vm.roots().sync.cancel_task(&sched, self.task_id);
            self.outcome = BuiltinOutcome::StartCancelUnwind;
            return Ok(());
        }
        match self.vm.roots().sync.sem_acquire(id, self.task_id)? {
            crate::sync_primitives::SyncPoll::Ready(()) => {
                self.return_unit();
                Ok(())
            }
            crate::sync_primitives::SyncPoll::SuspendRetry => {
                self.outcome = BuiltinOutcome::SuspendRetry;
                Ok(())
            }
        }
    }

    pub fn sync_sem_try_acquire(&mut self, id: i64) -> Result<(), VmError> {
        let ok = self.vm.roots().sync.sem_try_acquire(id)?;
        self.return_bool(ok);
        Ok(())
    }

    pub fn sync_sem_release(&mut self, id: i64, count: i64) -> Result<(), VmError> {
        let sched = self.vm.scheduler().clone();
        self.vm.roots().sync.sem_release(&sched, id, count)?;
        self.return_unit();
        Ok(())
    }

    pub fn sync_sem_available(&mut self, id: i64) -> Result<(), VmError> {
        let avail = self.vm.roots().sync.sem_available(id)?;
        self.return_i64(avail)
    }

    pub fn sync_once_new(&mut self) -> Result<(), VmError> {
        let id = self.vm.roots().sync.once_new();
        self.return_i64(id)
    }

    pub fn sync_once_begin(&mut self, id: i64) -> Result<(), VmError> {
        if self.should_abort_for_cancel() {
            let sched = self.vm.scheduler().clone();
            self.vm.roots().sync.cancel_task(&sched, self.task_id);
            self.outcome = BuiltinOutcome::StartCancelUnwind;
            return Ok(());
        }
        match self.vm.roots().sync.once_begin(id, self.task_id)? {
            crate::sync_primitives::SyncPoll::Ready((code, val)) => {
                self.return_i64_any_tuple(code, val)
            }
            crate::sync_primitives::SyncPoll::SuspendRetry => {
                self.outcome = BuiltinOutcome::SuspendRetry;
                Ok(())
            }
        }
    }

    pub fn sync_once_complete(&mut self, id: i64, val_root: &BuiltinRoot) -> Result<(), VmError> {
        let val = self.load_rooted(val_root)?;
        let is_err = self
            .vm
            .error_layout(val, 0)?
            .is_some_and(|layout| layout.tag != type_pool::TypeId::ZERO);
        let sched = self.vm.scheduler().clone();
        if is_err {
            self.vm.roots().sync.once_abort(&sched, id, self.task_id)?;
        } else {
            self.vm
                .roots()
                .sync
                .once_complete(&sched, id, self.task_id, val)?;
        }
        self.return_unit();
        Ok(())
    }

    pub fn sync_once_abort(&mut self, id: i64) -> Result<(), VmError> {
        let sched = self.vm.scheduler().clone();
        self.vm.roots().sync.once_abort(&sched, id, self.task_id)?;
        self.return_unit();
        Ok(())
    }

    pub fn sync_once_get(&mut self, id: i64) -> Result<(), VmError> {
        match self.vm.roots().sync.once_get(id)? {
            Some(val) => self.return_bool_any_tuple(true, val),
            None => self.return_bool_any_tuple(false, TaggedValue::UNIT),
        }
    }

    pub fn sync_barrier_new(&mut self, parties: i64) -> Result<(), VmError> {
        let id = self.vm.roots().sync.barrier_new(parties)?;
        self.return_i64(id)
    }

    pub fn sync_barrier_wait(&mut self, id: i64) -> Result<(), VmError> {
        if self.should_abort_for_cancel() {
            if let Some(is_leader) = self
                .vm
                .roots()
                .sync
                .barrier_take_completed_on_cancel(id, self.task_id)
            {
                self.return_bool(is_leader);
                return Ok(());
            }
            let sched = self.vm.scheduler().clone();
            self.vm.roots().sync.cancel_task(&sched, self.task_id);
            self.outcome = BuiltinOutcome::StartCancelUnwind;
            return Ok(());
        }
        let sched = self.vm.scheduler().clone();
        match self
            .vm
            .roots()
            .sync
            .barrier_wait(&sched, id, self.task_id)?
        {
            crate::sync_primitives::SyncPoll::Ready(is_leader) => {
                self.return_bool(is_leader);
                Ok(())
            }
            crate::sync_primitives::SyncPoll::SuspendRetry => {
                self.outcome = BuiltinOutcome::SuspendRetry;
                Ok(())
            }
        }
    }

    pub fn sync_atomic_new(&mut self, initial: i64) -> Result<(), VmError> {
        let id = self.vm.roots().sync.atomic_new(initial)?;
        self.return_i64(id)
    }

    pub fn sync_atomic_load(&mut self, id: i64) -> Result<(), VmError> {
        let val = self.vm.roots().sync.atomic_load(id)?;
        self.return_i64(val)
    }

    pub fn sync_atomic_store(&mut self, id: i64, val: i64) -> Result<(), VmError> {
        self.vm.roots().sync.atomic_store(id, val)?;
        self.return_unit();
        Ok(())
    }

    pub fn sync_atomic_swap(&mut self, id: i64, val: i64) -> Result<(), VmError> {
        let prev = self.vm.roots().sync.atomic_swap(id, val)?;
        self.return_i64(prev)
    }

    pub fn sync_atomic_fetch_add(&mut self, id: i64, delta: i64) -> Result<(), VmError> {
        let prev = self.vm.roots().sync.atomic_fetch_add(id, delta)?;
        self.return_i64(prev)
    }

    pub fn sync_atomic_cas(&mut self, id: i64, expected: i64, desired: i64) -> Result<(), VmError> {
        let (ok, actual) = self.vm.roots().sync.atomic_cas(id, expected, desired)?;
        self.return_native(NativeValue::Tuple(vec![
            NativeValue::Bool(ok),
            NativeValue::I64(actual),
        ]))
    }

    // -----------------------------------------------------------------------
    // `std.net` non-blocking `libuv` operations (`847..=864`)
    // -----------------------------------------------------------------------

    fn complete_net_outcome(&mut self, out: NetOutcome) -> Result<(), VmError> {
        match out {
            NetOutcome::HandleAddr {
                code,
                msg,
                handle_id,
                host,
                port,
            } => {
                self.check_string_size(msg.len())?;
                self.check_string_size(host.len())?;
                self.return_native(NativeValue::Tuple(vec![
                    NativeValue::I64(code),
                    NativeValue::Str(msg),
                    NativeValue::I64(handle_id),
                    NativeValue::Str(host),
                    NativeValue::I64(port),
                ]))
            }
            NetOutcome::Bytes { code, msg, data } => {
                self.check_string_size(msg.len())?;
                let items = data
                    .into_iter()
                    .map(|b| NativeValue::I64(i64::from(b)))
                    .collect();
                self.return_native(NativeValue::Tuple(vec![
                    NativeValue::I64(code),
                    NativeValue::Str(msg),
                    NativeValue::List(items),
                ]))
            }
            NetOutcome::Line { code, msg, line } => {
                self.check_string_size(msg.len())?;
                if let Some(ref s) = line {
                    self.check_string_size(s.len())?;
                }
                self.return_native(NativeValue::Tuple(vec![
                    NativeValue::I64(code),
                    NativeValue::Str(msg),
                    NativeValue::OptStr(line),
                ]))
            }
            NetOutcome::Count { code, msg, count } => {
                self.check_string_size(msg.len())?;
                self.return_native(NativeValue::Tuple(vec![
                    NativeValue::I64(code),
                    NativeValue::Str(msg),
                    NativeValue::I64(count),
                ]))
            }
            NetOutcome::Datagram {
                code,
                msg,
                data,
                host,
                port,
            } => {
                self.check_string_size(msg.len())?;
                self.check_string_size(host.len())?;
                let items = data
                    .into_iter()
                    .map(|b| NativeValue::I64(i64::from(b)))
                    .collect();
                self.return_native(NativeValue::Tuple(vec![
                    NativeValue::I64(code),
                    NativeValue::Str(msg),
                    NativeValue::List(items),
                    NativeValue::Str(host),
                    NativeValue::I64(port),
                ]))
            }
            NetOutcome::Hosts { code, msg, addrs } => {
                self.check_string_size(msg.len())?;
                let mut items = Vec::with_capacity(addrs.len());
                for ip in addrs {
                    self.check_string_size(ip.len())?;
                    items.push(NativeValue::Str(ip));
                }
                self.return_native(NativeValue::Tuple(vec![
                    NativeValue::I64(code),
                    NativeValue::Str(msg),
                    NativeValue::List(items),
                ]))
            }
        }
    }

    pub fn run_net_op<S>(&mut self, submit: S) -> Result<(), VmError>
    where
        S: FnOnce(&mut Self) -> Result<bool, VmError>,
    {
        match self.vm.scheduler().poll_task_op(self.task_id) {
            TaskOpPoll::Ready(TaskOpResult::Net(out)) => self.complete_net_outcome(out),
            TaskOpPoll::Ready(_) => unreachable!("unexpected TaskOpResult variant for net op"),
            TaskOpPoll::Pending => {
                self.outcome = BuiltinOutcome::SuspendRetry;
                Ok(())
            }
            TaskOpPoll::Vacant => {
                if self.should_abort_for_cancel() {
                    self.outcome = BuiltinOutcome::StartCancelUnwind;
                    return Ok(());
                }
                if submit(self)? {
                    self.outcome = BuiltinOutcome::SuspendRetry;
                }
                Ok(())
            }
        }
    }

    pub fn net_tcp_local_addr(&mut self, id: i64) -> Result<(), VmError> {
        match self.vm.scheduler().tcp_local_addr(id) {
            Some((ip, port)) => self.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(0),
                NativeValue::Str(String::new()),
                NativeValue::Str(ip),
                NativeValue::I64(port),
            ])),
            None => self.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(6),
                NativeValue::Str("TCP socket is closed".to_string()),
                NativeValue::Str(String::new()),
                NativeValue::I64(0),
            ])),
        }
    }

    pub fn net_tcp_peer_addr(&mut self, id: i64) -> Result<(), VmError> {
        match self.vm.scheduler().tcp_peer_addr(id) {
            Some((ip, port)) => self.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(0),
                NativeValue::Str(String::new()),
                NativeValue::Str(ip),
                NativeValue::I64(port),
            ])),
            None => self.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(6),
                NativeValue::Str("TCP stream is closed".to_string()),
                NativeValue::Str(String::new()),
                NativeValue::I64(0),
            ])),
        }
    }

    pub fn net_tcp_close(&mut self, id: i64) -> Result<(), VmError> {
        if !self.vm.scheduler().tcp_close(id) {
            return Err(VmError::InvalidArgument("invalid TCP handle"));
        }
        self.return_unit();
        Ok(())
    }

    pub fn net_udp_local_addr(&mut self, id: i64) -> Result<(), VmError> {
        match self.vm.scheduler().udp_local_addr(id) {
            Some((ip, port)) => self.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(0),
                NativeValue::Str(String::new()),
                NativeValue::Str(ip),
                NativeValue::I64(port),
            ])),
            None => self.return_native(NativeValue::Tuple(vec![
                NativeValue::I64(6),
                NativeValue::Str("UDP socket is closed".to_string()),
                NativeValue::Str(String::new()),
                NativeValue::I64(0),
            ])),
        }
    }

    pub fn net_udp_close(&mut self, id: i64) -> Result<(), VmError> {
        if !self.vm.scheduler().udp_close(id) {
            return Err(VmError::InvalidArgument("invalid UDP handle"));
        }
        self.return_unit();
        Ok(())
    }

    pub fn submit_tcp_listen(&self, host: String, port: i64, backlog: i64) {
        self.vm
            .scheduler()
            .submit_tcp_listen(self.task_id, host, port, backlog);
    }

    pub fn submit_tcp_accept(&self, listener_id: i64) {
        self.vm
            .scheduler()
            .submit_tcp_accept(self.task_id, listener_id);
    }

    pub fn submit_tcp_connect(&self, host: String, port: i64) {
        self.vm
            .scheduler()
            .submit_tcp_connect(self.task_id, host, port);
    }

    pub fn submit_tcp_read_bytes(&self, stream_id: i64, max_bytes: usize) {
        self.vm
            .scheduler()
            .submit_tcp_read_bytes(self.task_id, stream_id, max_bytes);
    }

    pub fn submit_tcp_read_line(&self, stream_id: i64) {
        self.vm
            .scheduler()
            .submit_tcp_read_line(self.task_id, stream_id);
    }

    pub fn submit_tcp_write(&self, stream_id: i64, data: Vec<u8>) {
        self.vm
            .scheduler()
            .submit_tcp_write(self.task_id, stream_id, data);
    }

    pub fn submit_tcp_set_nodelay(&self, stream_id: i64, enable: bool) {
        self.vm
            .scheduler()
            .submit_tcp_set_nodelay(self.task_id, stream_id, enable);
    }

    pub fn submit_udp_bind(&self, host: String, port: i64) {
        self.vm
            .scheduler()
            .submit_udp_bind(self.task_id, host, port);
    }

    pub fn submit_udp_send(&self, sock_id: i64, host: String, port: i64, data: Vec<u8>) {
        self.vm
            .scheduler()
            .submit_udp_send(self.task_id, sock_id, host, port, data);
    }

    pub fn submit_udp_recv(&self, sock_id: i64, max_bytes: usize) {
        self.vm
            .scheduler()
            .submit_udp_recv(self.task_id, sock_id, max_bytes);
    }

    pub fn submit_resolve_host(&self, host: String) {
        self.vm.scheduler().submit_resolve_host(self.task_id, host);
    }
}

impl Vm {
    fn intern_optional_type(&self, inner: TypeIndex) -> TypeIndex {
        {
            let tp = self.type_pool_read();
            if let Some(found) = (0..tp.len())
                .map(|index| TypeIndex::from_raw(index as u32))
                .find(|&index| {
                    matches!(
                        &tp.get(index).kind,
                        TypeKind::Optional { inner: found } if *found == inner
                    )
                })
            {
                return found;
            }
        }
        self.type_pool_write()
            .intern_structural(TypeKind::Optional { inner })
    }

    pub(crate) fn alloc_native(
        &mut self,
        task_id: TaskId,
        spec: NativeValue,
    ) -> Result<(TaggedValue, TypeIndex), VmError> {
        match spec {
            NativeValue::Null => Ok((
                TaggedValue::NULL,
                self.intern_optional_type(Intrinsic::Any.type_index()),
            )),
            NativeValue::Unit => Ok((TaggedValue::UNIT, Intrinsic::Unit.type_index())),
            NativeValue::Bool(v) => Ok((TaggedValue::from_bool(v), Intrinsic::Bool.type_index())),
            NativeValue::Char(c) => Ok((TaggedValue::from_char(c), Intrinsic::Char.type_index())),
            NativeValue::I64(v) => Ok((
                self.number_value(Number::I64(v))?,
                Intrinsic::I64.type_index(),
            )),
            NativeValue::U64(v) => Ok((
                self.number_value(Number::U64(v))?,
                Intrinsic::U64.type_index(),
            )),
            NativeValue::F64(v) => Ok((
                self.number_value(Number::F64(v))?,
                Intrinsic::F64.type_index(),
            )),
            NativeValue::Str(s) => {
                self.check_display_bytes(task_id, s.len())?;
                Self::check_string_bytes(s.len())?;
                let v = self.alloc_string(&s)?;
                Ok((v, Intrinsic::Str.type_index()))
            }
            NativeValue::Bytes(data) => {
                let bytes_ty = self
                    .type_pool_read()
                    .bytes_type()
                    .ok_or(VmError::TypeError)?;
                let v = self.alloc_bytes_from_slice(&data)?;
                Ok((v, bytes_ty))
            }
            NativeValue::OptI64(opt) => {
                let ty = self.intern_optional_type(Intrinsic::I64.type_index());
                let v = match opt {
                    Some(n) => self.number_value(Number::I64(n))?,
                    None => TaggedValue::NULL,
                };
                Ok((v, ty))
            }
            NativeValue::OptF64(opt) => {
                let ty = self.intern_optional_type(Intrinsic::F64.type_index());
                let v = match opt {
                    Some(n) => self.number_value(Number::F64(n))?,
                    None => TaggedValue::NULL,
                };
                Ok((v, ty))
            }
            NativeValue::OptStr(opt) => {
                let ty = self.intern_optional_type(Intrinsic::Str.type_index());
                let v = match opt {
                    Some(s) => {
                        self.check_display_bytes(task_id, s.len())?;
                        Self::check_string_bytes(s.len())?;
                        self.alloc_string(&s)?
                    }
                    None => TaggedValue::NULL,
                };
                Ok((v, ty))
            }
            NativeValue::List(items) => {
                let list_ty = self
                    .type_pool_read()
                    .list_type()
                    .ok_or(VmError::TypeError)?;
                let list = self.new_list(items.len())?;
                let base = self.worker_roots().temporary_values.borrow().len();
                self.with_temporary_roots(&[list, TaggedValue::UNIT], |vm| {
                    for (idx, item) in items.into_iter().enumerate() {
                        let (elem, _) = vm.alloc_native(task_id, item)?;
                        vm.worker_roots().temporary_values.borrow_mut()[base + 1] = elem;
                        let (cur_list, cur_elem) = {
                            let roots = vm.worker_roots().temporary_values.borrow();
                            (roots[base], roots[base + 1])
                        };
                        vm.list_set(cur_list, TaggedValue::from_u64(idx as u64), cur_elem)?;
                        vm.worker_roots().temporary_values.borrow_mut()[base + 1] =
                            TaggedValue::UNIT;
                    }
                    let final_list = vm.worker_roots().temporary_values.borrow()[base];
                    Ok((final_list, list_ty))
                })
            }
            NativeValue::StringMap(entries) => {
                let map_ty = self.type_pool_read().map_type().ok_or(VmError::TypeError)?;
                let cap = if entries.is_empty() {
                    0
                } else {
                    entries
                        .len()
                        .saturating_mul(2)
                        .min(crate::maps::MAX_MAP_CAPACITY)
                };
                let map = self.new_map(cap)?;
                let base = self.worker_roots().temporary_values.borrow().len();
                self.with_temporary_roots(&[map, TaggedValue::UNIT, TaggedValue::UNIT], |vm| {
                    for (key_str, val_spec) in entries {
                        vm.check_display_bytes(task_id, key_str.len())?;
                        Self::check_string_bytes(key_str.len())?;
                        let k = vm.alloc_string(&key_str)?;
                        vm.worker_roots().temporary_values.borrow_mut()[base + 1] = k;
                        let (v, _) = vm.alloc_native(task_id, val_spec)?;
                        vm.worker_roots().temporary_values.borrow_mut()[base + 2] = v;
                        let (cur_map, cur_key, cur_val) = {
                            let roots = vm.worker_roots().temporary_values.borrow();
                            (roots[base], roots[base + 1], roots[base + 2])
                        };
                        vm.map_set(cur_map, cur_key, cur_val)?;
                        let mut roots = vm.worker_roots().temporary_values.borrow_mut();
                        roots[base + 1] = TaggedValue::UNIT;
                        roots[base + 2] = TaggedValue::UNIT;
                    }
                    let final_map = vm.worker_roots().temporary_values.borrow()[base];
                    Ok((final_map, map_ty))
                })
            }
            NativeValue::Tuple(items) => {
                let len = items.len();
                let init_slots = vec![TaggedValue::UNIT; len];
                let base = self.worker_roots().temporary_values.borrow().len();
                self.with_temporary_roots(&init_slots, |vm| {
                    let mut elem_types = Vec::with_capacity(len);
                    for (idx, item) in items.into_iter().enumerate() {
                        let (val, ty) = vm.alloc_native(task_id, item)?;
                        vm.worker_roots().temporary_values.borrow_mut()[base + idx] = val;
                        elem_types.push(ty);
                    }
                    let values: Vec<TaggedValue> =
                        vm.worker_roots().temporary_values.borrow()[base..base + len].to_vec();
                    let tuple = vm.alloc_tuple(&elem_types, &values)?;
                    let tuple_ty = vm.reflected_type(tuple)?;
                    Ok((tuple, tuple_ty))
                })
            }
        }
    }

    /// Snapshot a live rooted VM value into a host-owned [`NativeValue`] without allocating on the GC heap.
    pub fn value_to_native(&self, val: TaggedValue, depth: usize) -> Result<NativeValue, VmError> {
        if depth > 64 {
            return Err(VmError::TypeError);
        }
        if val.is_null() {
            return Ok(NativeValue::Null);
        }
        if val.is_unit() {
            return Ok(NativeValue::Unit);
        }
        if let Some(b) = val.as_bool() {
            return Ok(NativeValue::Bool(b));
        }
        if let Some(c) = val.as_char() {
            return Ok(NativeValue::Char(c));
        }
        // SAFETY: `val` is a live rooted VM value in the active mutator operation;
        // `value_to_native` performs only host allocations with no GC safepoints.
        if let Some(num) = unsafe { Number::from_tagged(val) } {
            return match num {
                Number::I64(n) => Ok(NativeValue::I64(n)),
                Number::U64(n) => Ok(NativeValue::U64(n)),
                Number::F64(n) => Ok(NativeValue::F64(n)),
                Number::I128(n) => i64::try_from(n)
                    .map(NativeValue::I64)
                    .map_err(|_| VmError::NumericOverflow),
                Number::U128(n) => u64::try_from(n)
                    .map(NativeValue::U64)
                    .map_err(|_| VmError::NumericOverflow),
            };
        }
        if let Some(s) = heap_string_to_owned(val) {
            return Ok(NativeValue::Str(s));
        }
        if let Ok(layout) = self.bytes_layout(val) {
            return Ok(NativeValue::Bytes(layout.as_slice().to_vec()));
        }
        if let Ok(elems) = self.snapshot_list_elements(val) {
            let mut items = Vec::with_capacity(elems.len());
            for elem in elems {
                items.push(self.value_to_native(elem, depth + 1)?);
            }
            return Ok(NativeValue::List(items));
        }
        let maybe_map_entries = (|| -> Result<Vec<(String, TaggedValue)>, VmError> {
            let _guard = self.stripe_read(val)?;
            let layout = self.map_layout_unlocked(val)?;
            layout.snapshot_entries()
        })();
        if let Ok(raw_entries) = maybe_map_entries {
            let mut entries = Vec::with_capacity(raw_entries.len());
            for (k, v) in raw_entries {
                entries.push((k, self.value_to_native(v, depth + 1)?));
            }
            return Ok(NativeValue::StringMap(entries));
        }
        if let Ok(elems) = self.tuple_values(val) {
            let mut items = Vec::with_capacity(elems.len());
            for elem in elems {
                items.push(self.value_to_native(elem, depth + 1)?);
            }
            return Ok(NativeValue::Tuple(items));
        }
        Err(VmError::TypeError)
    }
}

impl BuiltinCtx<'_> {
    fn return_host_call_pair(&mut self, ok: bool, payload: NativeValue) -> Result<(), VmError> {
        let flag = if ok {
            TaggedValue::TRUE
        } else {
            TaggedValue::FALSE
        };
        let (payload_val, _) = self.vm.alloc_native(self.task_id, payload)?;
        let elem_types = [Intrinsic::Bool.type_index(), Intrinsic::Any.type_index()];
        let tuple = self.vm.alloc_tuple(&elem_types, &[flag, payload_val])?;
        self.write_return(tuple);
        Ok(())
    }

    /// Dispatch `(op, args)` to the embedder's [`HostHandler`] and encode `(ok: bool, payload: Any)`.
    pub fn invoke_host_call(&mut self, op: &str, args_root: &BuiltinRoot) -> Result<(), VmError> {
        let list_val = self.load_rooted(args_root)?;
        let elems = self.vm.snapshot_list_elements(list_val)?;
        let mut native_args = Vec::with_capacity(elems.len());
        for elem in elems {
            native_args.push(self.vm.value_to_native(elem, 0)?);
        }
        let outcome = {
            let mut guard = self.vm.shared.host_handler.lock();
            let Some(handler) = guard.as_mut() else {
                drop(guard);
                return self.return_host_call_pair(
                    false,
                    NativeValue::Str("no host handler installed".to_string()),
                );
            };
            handler.call(op, &native_args, self.task_id)
        };
        match outcome {
            HostCallOutcome::Return(val) => self.return_host_call_pair(true, val),
            HostCallOutcome::Error(msg) => self.return_host_call_pair(false, NativeValue::Str(msg)),
            HostCallOutcome::SuspendRetry => {
                self.vm.scheduler().mark_external_waiter(self.task_id);
                self.outcome = BuiltinOutcome::SuspendRetry;
                Ok(())
            }
            HostCallOutcome::Yield => {
                self.outcome = BuiltinOutcome::Yield;
                self.return_host_call_pair(true, NativeValue::Unit)
            }
        }
    }
}

/// Extract UTF-8 contents from a heap string TaggedValue.
pub(crate) fn with_heap_string_str<R>(val: TaggedValue, f: impl FnOnce(&str) -> R) -> Option<R> {
    let ptr = val.as_heap_ptr()?;
    // SAFETY: callers run within a mutator operation and supply live VM values;
    // `f` does not allocate managed objects, so `ptr` cannot move during `f`.
    let bytes = unsafe { gc::heap_string_bytes(ptr) }?;
    let s = if bytes.is_ascii() {
        // SAFETY: `bytes.is_ascii()` verifies every byte is `< 0x80`, which is valid UTF-8.
        unsafe { std::str::from_utf8_unchecked(bytes) }
    } else {
        std::str::from_utf8(bytes).ok()?
    };
    Some(f(s))
}

pub(crate) fn heap_string_to_owned(val: TaggedValue) -> Option<String> {
    with_heap_string_str(val, str::to_owned)
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
        vm.task_mut(task)
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
        let value = vm.roots().constants.read(0).get(index);
        vm.task_mut(task).unwrap().registers.set(Reg(0), value);
        let roots_before = vm.worker_roots().temporary_values.borrow().len();
        let pins_before = vm.worker_roots().native_pins.borrow().len();
        let result = (|| -> Result<(), VmError> {
            let ctx = BuiltinCtx::new(&mut vm, task, 1);
            ctx.arg(0)?;
            ctx.arg_rooted(0)?;
            Err(VmError::TypeError)
        })();
        assert!(matches!(result, Err(VmError::TypeError)));
        assert_eq!(
            vm.worker_roots().temporary_values.borrow().len(),
            roots_before
        );
        assert_eq!(vm.worker_roots().native_pins.borrow().len(), pins_before);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let ctx = BuiltinCtx::new(&mut vm, task, 1);
            ctx.arg(0).unwrap();
            ctx.arg_rooted(0).unwrap();
            panic!("native callback unwind fixture");
        }));
        assert!(result.is_err());
        assert_eq!(
            vm.worker_roots().temporary_values.borrow().len(),
            roots_before
        );
        assert_eq!(vm.worker_roots().native_pins.borrow().len(), pins_before);
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
            vm.type_pool_write().add_method(
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
                    abi: nsbc::FunctionAbi::value_only(params as usize),
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
        let buffer_type = vm.type_pool_read().list_buffer_type().unwrap();
        let task = vm.spawn_root(nsbc::FuncId(0));
        // SAFETY: TestVm holds an operation. The zero-slot opaque buffer is
        // initialized and immediately placed in the registered task roots.
        let payload = unsafe { vm.heap_mut().alloc_object(buffer_type, 0) }.unwrap();
        // SAFETY: this allocation is initialized and remains live under the operation.
        let value = unsafe { TaggedValue::from_heap_ptr(payload.as_ptr()) };
        vm.task_mut(task).unwrap().registers.set(Reg(0), value);
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
    fn string_arguments_reject_other_heap_layouts_and_invalid_lengths() {
        let mut vm = crate::tests::make_vm();
        let task = vm.spawn_root(nsbc::FuncId(0));
        for kind in [Intrinsic::Closure, Intrinsic::Continuation, Intrinsic::Str] {
            // SAFETY: TestVm holds an operation throughout fixture mutation.
            // The object is published before any subsequent managed allocation.
            let payload = unsafe { vm.heap_mut().alloc_object(kind.type_index(), 1) }.unwrap();
            // SAFETY: the allocation owns one aligned u64 payload word. For Str,
            // length 8 is invalid because there is no room after the prefix.
            unsafe {
                payload.as_ptr().cast::<u64>().write(8);
            }
            // SAFETY: payload points to this initialized, live heap allocation.
            let value = unsafe { TaggedValue::from_heap_ptr(payload.as_ptr()) };
            vm.task_mut(task).unwrap().registers.set(Reg(0), value);
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
