use gc::{GC_FLAGS, GcConfig, Heap, NessaSlot, ObjectHeader};
use nsbc::{AddrMode, FuncId, Instruction, InstructionData, Opcode, ParameterAbi, Reg};
use runtime::{
    BuiltinFnId, BytecodeStore, CallFrame, ClosureEnv, ConstantPool, FunctionCode, GlobalTable,
    Number, TaggedValue, TaskId,
};
use scheduler::Scheduler;
use stack_pool::StackPool;
use std::cell::{RefCell, UnsafeCell};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use str_interner::StrId;
use type_pool::{Intrinsic, TypeIndex, TypePool};

mod builtin_ctx;
#[cfg(test)]
mod callable_tests;
mod closures;
mod collections;
#[cfg(test)]
mod continuation_gc_tests;
mod continuation_roots;
mod display;
mod display_frames;
mod dynamic_equality;
#[cfg(test)]
mod effect_contract_tests;
mod effects;
mod enums;
#[cfg(test)]
mod error_value_tests;
mod error_values;
mod fields;
mod function_abi;
mod globals;
mod maps;
mod method_access;
#[cfg(test)]
mod moving_allocation_tests;
mod numeric;
mod trait_proofs;
mod tuples;
mod types;
pub use builtin_ctx::{BuiltinCtx, BuiltinFn, BuiltinFnTable, BuiltinOutcome, BuiltinRoot};

// ---------------------------------------------------------------------------
// VM — the top-level virtual machine
// ---------------------------------------------------------------------------

/// The top-level Nessa virtual machine.
pub struct Vm {
    // Field order unregisters roots before freeing their stable backing state.
    _roots: gc::RootRegistration,
    roots: Box<RootCell>,
    state: Box<VmState>,
}

/// Address-stable runtime state, accessed only within mutator operations.
struct VmState {
    bytecode: BytecodeStore,
    type_pool: TypePool,
    heap: Heap,
    builtins: BuiltinFnTable,
    method_call_scopes: std::collections::HashMap<(FuncId, usize), u32>,
    type_query_scope: Option<u32>,
    trait_proofs: std::collections::HashMap<u64, trait_proofs::FrozenTraitProof>,
    trait_proof_cache: std::collections::HashMap<(usize, TypeIndex), TaggedValue>,
    pending_root_entries: std::collections::HashSet<TaskId>,
}

/// The collector accesses only this independently stored root domain, never
/// the allocator or a VmState currently borrowed across allocation/collection.
struct VmRoots {
    globals: GlobalTable,
    constants: ConstantPool,
    scheduler: Scheduler,
    temporary_values: RefCell<Vec<TaggedValue>>,
    native_pins: RefCell<Vec<TaggedValue>>,
    continuations:
        std::collections::HashMap<runtime::ContinuationId, continuation_roots::ManagedContinuation>,
}

struct RootCell(UnsafeCell<VmRoots>);

impl Deref for RootCell {
    type Target = VmRoots;
    fn deref(&self) -> &VmRoots {
        // SAFETY: VM access is guarded by an operation. Collector reads happen
        // only while that operation is stopped, with no outstanding root borrow.
        unsafe { &*self.0.get() }
    }
}

impl DerefMut for RootCell {
    fn deref_mut(&mut self) -> &mut VmRoots {
        self.0.get_mut()
    }
}

impl Vm {
    /// Request a synchronous, exhaustive collection. False means the configured
    /// plan (for example NoGC) did not perform a collection.
    pub fn collect_garbage(&mut self) -> Result<bool, VmError> {
        let _operation = self.execution_operation()?;
        // SAFETY: the operation keeps this mutator live, and public APIs retain
        // managed references only in registered roots or owned host snapshots.
        Ok(unsafe { self.state.heap.collect_garbage() })
    }

    pub fn completed_collections(&self) -> u64 {
        self.state.heap.completed_collections()
    }

    pub fn constant_string(&mut self, index: u32) -> Result<String, VmError> {
        let _operation = self.operation();
        let value = self.constant_value(index)?;
        builtin_ctx::heap_string_to_owned(value).ok_or(VmError::TypeError)
    }

    fn constant_value(&self, index: u32) -> Result<TaggedValue, VmError> {
        self.roots
            .constants
            .values()
            .get(index as usize)
            .copied()
            .ok_or(VmError::InvalidConstant(index))
    }

    fn constant_u32(&self, index: u32) -> Result<u32, VmError> {
        let value = self.constant_value(index)?;
        if let Some(raw) = value.as_u64() {
            u32::try_from(raw).map_err(|_| VmError::TypeError)
        } else if let Some(raw) = value.as_i64() {
            u32::try_from(raw).map_err(|_| VmError::TypeError)
        } else {
            Err(VmError::TypeError)
        }
    }

    fn operation(&self) -> gc::MutatorSession<'static> {
        // SAFETY: all callers keep Vm alive until the guard is dropped, and
        // mutation requires &mut Vm. A nested operation stays on its owner
        // thread. Allocation callers publish results before ending the guard.
        unsafe { self.state.heap.begin_operation() }
    }

    fn execution_operation(&self) -> Result<gc::MutatorSession<'static>, VmError> {
        // SAFETY: same lifetime/publication guarantees as operation; rejects
        // cross-VM reentry that would leave the caller running while blocked.
        unsafe { self.state.heap.begin_execution() }.map_err(|_| VmError::CrossVmOperation)
    }

    pub fn type_pool(&self) -> &TypePool {
        &self.state.type_pool
    }

    pub fn install_type_pool(&mut self, type_pool: TypePool) {
        let _operation = self.operation();
        self.state.trait_proofs.clear();
        self.state.trait_proof_cache.clear();
        self.state.type_pool = type_pool;
        self.state.method_call_scopes.clear();
        self.state.type_query_scope = None;
    }

    /// Allocate, initialize and publish a constant in one operation.
    pub fn push_constant(&mut self, constant: &nsbc::Constant) -> Result<u32, VmError> {
        let _operation = self.execution_operation()?;
        let value = match constant {
            nsbc::Constant::Int(value) => self.number_value(Number::I64(*value))?,
            nsbc::Constant::UInt(value) => self.number_value(Number::U64(*value))?,
            nsbc::Constant::Int128(value) => self.number_value(Number::I128(*value))?,
            nsbc::Constant::UInt128(value) => self.number_value(Number::U128(*value))?,
            nsbc::Constant::Float(value) => self.number_value(Number::F64(*value))?,
            nsbc::Constant::Str(value) => self.alloc_string(value)?,
            nsbc::Constant::Char(value) => TaggedValue::from_char(*value),
            nsbc::Constant::Type(ty) => self.type_value(*ty)?,
            nsbc::Constant::Enum {
                type_index,
                variant,
            } => self.enum_constant(*type_index, *variant)?,
        };
        Ok(self.roots.constants.push(value))
    }

    /// Return an owned scalar snapshot; no borrowed task or heap value escapes.
    pub fn task_result_i64(&mut self, task: TaskId) -> Result<i64, VmError> {
        self.task_result_number(task)?
            .to_i64_checked()
            .ok_or(VmError::TypeError)
    }

    pub fn active_stack_count(&self) -> usize {
        let _operation = self.operation();
        self.roots.scheduler.stack_pool().active_count()
    }

    pub fn new(type_pool: TypePool, gc_config: &GcConfig, stack_pool: Arc<StackPool>) -> Self {
        let state = Box::new(VmState {
            bytecode: BytecodeStore::new(),
            type_pool,
            heap: gc::gc_init(gc_config),
            builtins: BuiltinFnTable::new(),
            method_call_scopes: std::collections::HashMap::new(),
            type_query_scope: None,
            trait_proofs: std::collections::HashMap::new(),
            trait_proof_cache: std::collections::HashMap::new(),
            pending_root_entries: std::collections::HashSet::new(),
        });
        let roots = Box::new(RootCell(UnsafeCell::new(VmRoots {
            globals: GlobalTable::new(),
            constants: ConstantPool::new(),
            scheduler: Scheduler::with_stack_pool(stack_pool),
            temporary_values: RefCell::new(Vec::new()),
            native_pins: RefCell::new(Vec::new()),
            continuations: std::collections::HashMap::new(),
        })));
        // SAFETY: the Box keeps its address when Vm moves. Vm drops its root
        // registration first; collection stops mutators before scanning state.
        let registration = unsafe {
            gc::RootRegistration::new_with_conditional_roots(
                roots.0.get().cast(),
                scan_vm_roots,
                continuation_roots::CALLBACKS,
            )
        };
        Self {
            _roots: registration,
            roots,
            state,
        }
    }

    /// Register a native builtin implementation.
    pub fn register_builtin(&mut self, id: BuiltinFnId, f: BuiltinFn) {
        let _operation = self.operation();
        self.state.builtins.register(id, f);
    }

    /// Add a compiled function to the VM.
    pub fn add_function(&mut self, code: FunctionCode) -> FuncId {
        let _operation = self.operation();
        self.state.bytecode.add_function(code)
    }

    /// Allocate a heap string and return its TaggedValue.
    /// The string bytes are laid out as: [len:u64][...UTF-8 bytes...].
    /// Payload words of a String object: an eight-byte length prefix plus UTF-8 bytes.
    fn string_payload_words(byte_len: usize) -> Result<u16, VmError> {
        let payload_bytes = 8usize
            .checked_add(byte_len)
            .ok_or(VmError::ObjectTooLarge)?;
        u16::try_from(payload_bytes.div_ceil(8)).map_err(|_| VmError::ObjectTooLarge)
    }

    fn alloc_string(&mut self, s: &str) -> Result<TaggedValue, VmError> {
        let bytes = s.as_bytes();
        let payload_words = Self::string_payload_words(bytes.len())?;
        let str_type = self.state.type_pool.intrinsic(Intrinsic::Str);
        // SAFETY: callers hold an operation, and publish the initialized value
        // before ending it. There is no managed allocation during initialization.
        match unsafe { self.state.heap.alloc_object(str_type, payload_words) } {
            Some(ptr) => unsafe {
                // Write length prefix.
                *(ptr.as_ptr() as *mut u64) = bytes.len() as u64;
                // Write UTF-8 bytes after the length.
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.as_ptr().add(8), bytes.len());
                Ok(TaggedValue::from_heap_ptr(ptr.as_ptr()))
            },
            None => Err(VmError::OutOfMemory),
        }
    }

    /// Spawn a root task that starts executing the given function.
    pub fn spawn_root(&mut self, func_id: FuncId) -> TaskId {
        let _operation = self.operation();
        let task = self.roots.scheduler.spawn(func_id, None);
        self.state.pending_root_entries.insert(task);
        task
    }

    /// Run the VM until all tasks have finished.
    pub fn run(&mut self) -> VmResult {
        // SAFETY: &mut Vm owns the operation and keeps its Heap alive. Every
        // instruction publishes results in registered slots before polling.
        let _operation = match self.execution_operation() {
            Ok(operation) => operation,
            Err(error) => return VmResult::Error(error),
        };
        loop {
            let Some(task_id) = self.roots.scheduler.next_ready() else {
                return VmResult::Finished;
            };
            self.roots.scheduler.set_running(task_id);
            if self.state.pending_root_entries.remove(&task_id) {
                let function = self
                    .roots
                    .scheduler
                    .get_task(task_id)
                    .expect("scheduled task exists")
                    .current_func;
                if let Err(error) = self
                    .check_entry_environment(function, None)
                    .and_then(|()| self.check_user_argument_abi(function, 0))
                {
                    self.roots.scheduler.finish(task_id);
                    return VmResult::Error(error);
                }
            }
            let result = self.execute_task(task_id);
            match result {
                ExecOutcome::Finished => {
                    self.roots.scheduler.finish(task_id);
                }
                ExecOutcome::Error(e) => {
                    self.roots.scheduler.finish(task_id);
                    return VmResult::Error(e);
                }
                ExecOutcome::Yield => {
                    self.roots.scheduler.yield_task(task_id);
                }
            }
        }
    }

    /// Execute one task until it finishes, suspends, errors, or yields.
    fn execute_task(&mut self, task_id: TaskId) -> ExecOutcome {
        // Bytecode is immutable while a task runs, so a function's checked
        // entry contract only needs revalidation when control changes function.
        let mut validated_func = None;
        loop {
            // No instruction temporaries or mutable task borrows survive here.
            self.state.heap.safe_point();
            // Get current task's PC and function.
            let (pc, func_id) = {
                let task = match self.roots.scheduler.get_task(task_id) {
                    Some(t) => t,
                    None => return ExecOutcome::Error(VmError::InvalidTask),
                };
                (task.pc, task.current_func)
            };

            let checked = if validated_func == Some(func_id) {
                self.state
                    .bytecode
                    .try_get_function(func_id)
                    .ok_or(VmError::InvalidFunction(func_id))
            } else {
                self.checked_function(func_id)
            };
            let func = match checked {
                Ok(function) => function,
                Err(error) => return ExecOutcome::Error(error),
            };
            validated_func = Some(func_id);
            if pc as usize >= func.instructions.len() {
                return ExecOutcome::Finished;
            }

            let word = func.instructions[pc as usize];
            let instr = match Instruction::decode(word) {
                Some(i) => i,
                None => return ExecOutcome::Error(VmError::InvalidInstruction(word)),
            };

            // Advance PC before executing (jumps will override).
            if let Some(task) = self.roots.scheduler.get_task_mut(task_id) {
                task.pc = pc + 1;
            }

            match self.dispatch(task_id, instr) {
                DispatchResult::Continue => {}
                DispatchResult::Return(val) => {
                    if let Err(error) = self.check_display_return(task_id, val) {
                        return ExecOutcome::Error(error);
                    }
                    // Pop call frame.
                    let should_finish = {
                        let task = self.roots.scheduler.get_task_mut(task_id).unwrap();
                        if let Some(frame) = task.call_stack.pop() {
                            // Restore PC and function.
                            task.pc = frame.return_pc;
                            task.current_func = frame.func_id;
                            task.local_slots = frame.local_slots;
                            task.display_state = frame.display_state;
                            // Restore saved registers.
                            for (reg, saved_val) in &frame.saved_regs {
                                task.registers.set(*reg, *saved_val);
                            }
                            // Return value goes into r0.
                            task.registers.set(Reg(0), val);
                            false
                        } else if task.stacks.return_from_delimiter(val) {
                            false
                        } else {
                            // No frames left — task is finished.
                            task.registers.set(Reg(0), val);
                            true
                        }
                    };
                    if should_finish {
                        return ExecOutcome::Finished;
                    }
                }
                DispatchResult::Yield => return ExecOutcome::Yield,
                DispatchResult::Error(e) => return ExecOutcome::Error(e),
            }
        }
    }

    /// Dispatch a single instruction.
    fn dispatch(&mut self, task_id: TaskId, instr: Instruction) -> DispatchResult {
        // PC belongs to the executing frame, including after continuation resume.
        // Restore the ambient query context even on yield or a language error.
        // Installation admits scope entries only for covered opcodes.
        let scope = if nsbc::instruction_requires_scope(instr.opcode) {
            let found = self.roots.scheduler.get_task(task_id).and_then(|task| {
                task.pc.checked_sub(1).and_then(|pc| {
                    self.state
                        .method_call_scopes
                        .get(&(task.current_func, pc as usize))
                        .copied()
                })
            });
            if found.is_none() && !self.state.type_pool.scopes().is_empty() {
                return DispatchResult::Error(VmError::MissingMethodContext);
            }
            found
        } else {
            None
        };
        let previous = self.state.type_query_scope;
        self.state.type_query_scope = scope;
        let result = self.dispatch_in_scope(task_id, instr);
        self.state.type_query_scope = previous;
        result
    }

    fn dispatch_in_scope(&mut self, task_id: TaskId, instr: Instruction) -> DispatchResult {
        // Helper macro to get task mutably.
        macro_rules! task {
            () => {
                self.roots.scheduler.get_task_mut(task_id).unwrap()
            };
        }

        match instr.opcode {
            // ── Arithmetic (R-type) ────────────────────────────────
            Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::Div
            | Opcode::Mod
            | Opcode::Neg
            | Opcode::BitAnd
            | Opcode::BitOr
            | Opcode::BitXor
            | Opcode::BitNot
            | Opcode::Shl
            | Opcode::Shr
            | Opcode::UShr => self.dispatch_numeric(task_id, instr),
            Opcode::CmpEq
            | Opcode::CmpNe
            | Opcode::CmpLt
            | Opcode::CmpLe
            | Opcode::CmpGt
            | Opcode::CmpGe => self.dispatch_comparison(task_id, instr),

            // ── Register manipulation (R-type) ────────────────────
            Opcode::Mov => {
                let (dst, s1, _) = r_regs(&instr);
                let t = task!();
                let val = t.registers.get(s1);
                t.registers.set(dst, val);
                DispatchResult::Continue
            }
            Opcode::Swap => {
                let (dst, s1, _) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(dst);
                let b = t.registers.get(s1);
                t.registers.set(dst, b);
                t.registers.set(s1, a);
                DispatchResult::Continue
            }

            // ── Constants & types (A-type) ────────────────────────
            Opcode::LoadSlot | Opcode::StoreSlot => {
                let (register, high, low) = a_fields(&instr);
                let index = ((high.0 as usize) << 12) | low as usize;
                let task = task!();
                if index >= task.local_slots.len() {
                    return DispatchResult::Error(VmError::InvalidSlot(index as u32));
                }
                if instr.opcode == Opcode::LoadSlot {
                    let value = task.local_slots[index];
                    task.registers.set(register, value);
                } else {
                    task.local_slots[index] = task.registers.get(register);
                }
                DispatchResult::Continue
            }
            Opcode::AllocateSlots => {
                let count = e_payload(&instr);
                if count > (1 << 17) {
                    return DispatchResult::Error(VmError::InvalidInstruction(instr.encode()));
                }
                let task = task!();
                let mut slots = Vec::new();
                if slots.try_reserve_exact(count as usize).is_err() {
                    return DispatchResult::Error(VmError::OutOfMemory);
                }
                slots.resize(count as usize, TaggedValue::UNIT);
                task.local_slots = slots;
                DispatchResult::Continue
            }
            Opcode::Load => {
                let (dst, _, imm12) = a_fields(&instr);
                let val = match instr.amode {
                    AddrMode::Imm => {
                        // Sign-extend 12-bit immediate to i64.
                        TaggedValue::from_i64(Instruction::sext_imm12(imm12) as i64)
                    }
                    AddrMode::Const => match self.constant_value(imm12 as u32) {
                        Ok(value) => value,
                        Err(error) => return DispatchResult::Error(error),
                    },
                };
                if val.as_enum().is_some()
                    && let Err(error) = self.enum_layout(val)
                {
                    return DispatchResult::Error(error);
                }
                task!().registers.set(dst, val);
                DispatchResult::Continue
            }
            Opcode::LoadConstWide => {
                let (dst, base, imm12) = a_fields(&instr);
                let idx = Instruction::wide_const_index(base, imm12);
                let val = match self.constant_value(idx) {
                    Ok(value) => value,
                    Err(error) => return DispatchResult::Error(error),
                };
                if val.as_enum().is_some()
                    && let Err(error) = self.enum_layout(val)
                {
                    return DispatchResult::Error(error);
                }
                task!().registers.set(dst, val);
                DispatchResult::Continue
            }
            Opcode::LoadUnit => {
                let (dst, _, _) = a_fields(&instr);
                task!().registers.set(dst, TaggedValue::UNIT);
                DispatchResult::Continue
            }
            Opcode::LoadTrue => {
                let (dst, _, _) = a_fields(&instr);
                task!().registers.set(dst, TaggedValue::TRUE);
                DispatchResult::Continue
            }
            Opcode::LoadFalse => {
                let (dst, _, _) = a_fields(&instr);
                task!().registers.set(dst, TaggedValue::FALSE);
                DispatchResult::Continue
            }
            Opcode::LoadNull => {
                let (dst, _, _) = a_fields(&instr);
                task!().registers.set(dst, TaggedValue::NULL);
                DispatchResult::Continue
            }
            Opcode::TypeCheck | Opcode::TypeCast | Opcode::TypeCastSafe | Opcode::TypeAssert => {
                self.dispatch_type_operation(task_id, instr)
            }

            // ── Memory access (A-type) ────────────────────────────
            Opcode::LoadField => {
                let (dst, source, index) = a_fields(&instr);
                let object = task!().registers.get(source);
                match self.load_field_value(object, index as u32) {
                    Ok(value) => task!().registers.set(dst, value),
                    Err(error) => return DispatchResult::Error(error),
                }
                DispatchResult::Continue
            }
            Opcode::StoreField => {
                let (source, destination, index) = a_fields(&instr);
                let object = task!().registers.get(destination);
                let value = task!().registers.get(source);
                match self.store_field_value(object, index as u32, value) {
                    Ok(()) => DispatchResult::Continue,
                    Err(error) => DispatchResult::Error(error),
                }
            }
            Opcode::LoadIndex => {
                let (dst, object, index) = a_fields(&instr);
                let list = task!().registers.get(object);
                if index >= runtime::GP_REGISTER_COUNT as u16 {
                    return DispatchResult::Error(VmError::InvalidIndex);
                }
                let Ok(index) = u8::try_from(index) else {
                    return DispatchResult::Error(VmError::InvalidIndex);
                };
                let index = task!().registers.get(Reg(index));
                match self.collection_get(list, index) {
                    Ok(value) => {
                        task!().registers.set(dst, value);
                        DispatchResult::Continue
                    }
                    Err(error) => DispatchResult::Error(error),
                }
            }
            Opcode::StoreIndex => {
                let (source, object, index) = a_fields(&instr);
                let list = task!().registers.get(object);
                if index >= runtime::GP_REGISTER_COUNT as u16 {
                    return DispatchResult::Error(VmError::InvalidIndex);
                }
                let Ok(index) = u8::try_from(index) else {
                    return DispatchResult::Error(VmError::InvalidIndex);
                };
                let index = task!().registers.get(Reg(index));
                let value = task!().registers.get(source);
                match self.collection_set(list, index, value) {
                    Ok(()) => DispatchResult::Continue,
                    Err(error) => DispatchResult::Error(error),
                }
            }
            Opcode::LoadGlobal | Opcode::LoadGlobalWide => {
                let (dst, base, imm12) = a_fields(&instr);
                if instr.opcode == Opcode::LoadGlobal && base.0 != 0 {
                    return DispatchResult::Error(VmError::InvalidInstruction(instr.encode()));
                }
                let gidx = if instr.opcode == Opcode::LoadGlobalWide {
                    Instruction::wide_const_index(base, imm12)
                } else {
                    imm12 as u32
                };
                let val = match self.roots.globals.get(gidx) {
                    Ok(value) => value,
                    Err(error) => return DispatchResult::Error(error.into()),
                };
                task!().registers.set(dst, val);
                DispatchResult::Continue
            }
            Opcode::StoreGlobal | Opcode::StoreGlobalWide => {
                let (dst, base, imm12) = a_fields(&instr);
                if instr.opcode == Opcode::StoreGlobal && base.0 != 0 {
                    return DispatchResult::Error(VmError::InvalidInstruction(instr.encode()));
                }
                let gidx = if instr.opcode == Opcode::StoreGlobalWide {
                    Instruction::wide_const_index(base, imm12)
                } else {
                    imm12 as u32
                };
                let val = task!().registers.get(dst);
                if let Err(error) = self.store_global(gidx, val) {
                    return DispatchResult::Error(error);
                }
                DispatchResult::Continue
            }

            // ── Object creation (A-type) ──────────────────────────
            Opcode::NewObject => {
                let (dst, _, imm12) = a_fields(&instr);
                let Some(type_idx) = self
                    .state
                    .type_pool
                    .canonical_type(TypeIndex::from_raw(imm12 as u32))
                else {
                    return DispatchResult::Error(VmError::TypeError);
                };
                if self.state.type_pool.is_reserved_collection_role(type_idx) {
                    return DispatchResult::Error(VmError::TypeError);
                }
                let words = match &self.state.type_pool.get(type_idx).kind {
                    type_pool::TypeKind::Struct { fields, .. } => fields.len(),
                    type_pool::TypeKind::Tuple { elements } => elements.len(),
                    _ => return DispatchResult::Error(VmError::TypeError),
                };
                let Ok(words) = u16::try_from(words) else {
                    return DispatchResult::Error(VmError::TypeError);
                };
                // SAFETY: run owns an operation and writes the result to dst
                // before the next instruction boundary can stop the mutator.
                match unsafe { self.state.heap.alloc_object(type_idx, words) } {
                    Some(ptr) => {
                        // SAFETY: the allocation contains exactly `words`
                        // aligned payload slots; initialize them before exposure.
                        for index in 0..words as usize {
                            unsafe {
                                ptr.as_ptr()
                                    .cast::<u64>()
                                    .add(index)
                                    .write(TaggedValue::UNIT.raw());
                            }
                        }
                        let val = unsafe { TaggedValue::from_heap_ptr(ptr.as_ptr()) };
                        task!().registers.set(dst, val);
                    }
                    None => {
                        return DispatchResult::Error(VmError::OutOfMemory);
                    }
                }
                DispatchResult::Continue
            }
            Opcode::ErrorOk | Opcode::ErrorErr | Opcode::ErrorIsOk | Opcode::ErrorPayload => {
                let (destination, source, index) = a_fields(&instr);
                if instr.amode != AddrMode::Imm {
                    return DispatchResult::Error(VmError::TypeError);
                }
                let value = task!().registers.get(source);
                let result = match instr.opcode {
                    Opcode::ErrorOk | Opcode::ErrorErr => {
                        self.constant_value(index as u32).and_then(|constant| {
                            match constant.as_type() {
                                Some(target) => self.construct_error(
                                    value,
                                    target,
                                    instr.opcode == Opcode::ErrorOk,
                                ),
                                None => Err(VmError::TypeError),
                            }
                        })
                    }
                    _ if index != 0 => Err(VmError::TypeError),
                    _ => self.error_layout(value, 0).and_then(|layout| {
                        let layout = layout.ok_or(VmError::TypeError)?;
                        Ok(if instr.opcode == Opcode::ErrorIsOk {
                            TaggedValue::from_bool(layout.tag == type_pool::TypeId::ZERO)
                        } else {
                            layout.payload
                        })
                    }),
                };
                match result {
                    Ok(value) => {
                        task!().registers.set(destination, value);
                        DispatchResult::Continue
                    }
                    Err(error) => DispatchResult::Error(error),
                }
            }
            Opcode::NewEnum | Opcode::EnumIs | Opcode::EnumField => {
                let (destination, source, index) = a_fields(&instr);
                let value = task!().registers.get(source);
                let result = match instr.opcode {
                    Opcode::NewEnum => self
                        .constant_value(index as u32)
                        .and_then(|constant| self.new_enum(constant, value)),
                    Opcode::EnumIs => self
                        .constant_value(index as u32)
                        .and_then(|constant| self.enum_is(value, constant))
                        .map(TaggedValue::from_bool),
                    _ => self.enum_field(value, index as u32),
                };
                match result {
                    Ok(value) => {
                        task!().registers.set(destination, value);
                        DispatchResult::Continue
                    }
                    Err(error) => DispatchResult::Error(error),
                }
            }
            Opcode::MatchFail => {
                if e_payload(&instr) == 0 {
                    DispatchResult::Error(VmError::NoMatchingCase)
                } else {
                    DispatchResult::Error(VmError::InvalidInstruction(instr.encode()))
                }
            }
            Opcode::NewList => {
                let (dst, _, length) = a_fields(&instr);
                match self.new_list(length as usize) {
                    Ok(value) => {
                        task!().registers.set(dst, value);
                        DispatchResult::Continue
                    }
                    Err(error) => DispatchResult::Error(error),
                }
            }
            Opcode::NewMap => {
                let (dst, base, capacity) = a_fields(&instr);
                if base.0 != 0 {
                    return DispatchResult::Error(VmError::InvalidInstruction(instr.encode()));
                }
                match self.new_map(capacity as usize) {
                    Ok(value) => {
                        task!().registers.set(dst, value);
                        DispatchResult::Continue
                    }
                    Err(error) => DispatchResult::Error(error),
                }
            }
            Opcode::NewClosure => {
                let (dst, base, imm12) = a_fields(&instr);
                let func_id = FuncId(imm12 as u32);
                let capture_count = base.0 as u16;
                self.exec_new_closure(task_id, dst, func_id, capture_count)
            }
            Opcode::NewClosureWide => {
                let (dst, base, imm12) = a_fields(&instr);
                let capture_count = base.0 as u16;
                let func_id = match self.constant_u32(imm12 as u32) {
                    Ok(id) => FuncId(id),
                    Err(error) => return DispatchResult::Error(error),
                };
                self.exec_new_closure(task_id, dst, func_id, capture_count)
            }

            // ── Control flow (J-type) ─────────────────────────────
            Opcode::Jmp | Opcode::JmpFar => {
                let (_, offset) = j_fields(&instr);
                let t = task!();
                t.pc = ((t.pc as i64) + (offset as i64) - 1) as u32;
                DispatchResult::Continue
            }
            Opcode::JmpIf => {
                let (cond, offset) = j_fields(&instr);
                let t = task!();
                let val = t.registers.get(cond);
                if is_truthy(val) {
                    t.pc = ((t.pc as i64) + (offset as i64) - 1) as u32;
                }
                DispatchResult::Continue
            }
            Opcode::JmpIfNot => {
                let (cond, offset) = j_fields(&instr);
                let t = task!();
                let val = t.registers.get(cond);
                if !is_truthy(val) {
                    t.pc = ((t.pc as i64) + (offset as i64) - 1) as u32;
                }
                DispatchResult::Continue
            }
            Opcode::JmpIfNull => {
                let (cond, offset) = j_fields(&instr);
                let t = task!();
                let val = t.registers.get(cond);
                if val.is_null() {
                    t.pc = ((t.pc as i64) + (offset as i64) - 1) as u32;
                }
                DispatchResult::Continue
            }
            Opcode::JmpIfNotNull => {
                let (cond, offset) = j_fields(&instr);
                let t = task!();
                let val = t.registers.get(cond);
                if !val.is_null() {
                    t.pc = ((t.pc as i64) + (offset as i64) - 1) as u32;
                }
                DispatchResult::Continue
            }

            // ── Calls (C-type) ────────────────────────────────────
            Opcode::Call => {
                let payload = c_payload(&instr);
                let (arg_count, func_id_raw) = Instruction::c_arg_count_func_id(payload);
                if arg_count as usize > runtime::GP_REGISTER_COUNT {
                    return DispatchResult::Error(VmError::TypeError);
                }
                let arguments = task!().registers.regs[..arg_count as usize].to_vec();
                self.invoke_closure_physical(task_id, FuncId(func_id_raw), None, &arguments)
            }

            Opcode::CallFar => {
                let payload = c_payload(&instr);
                let (arg_count, const_idx) = Instruction::c_arg_count_func_id(payload);
                let func_id_raw = match self.constant_u32(const_idx) {
                    Ok(id) => id,
                    Err(error) => return DispatchResult::Error(error),
                };
                if arg_count as usize > runtime::GP_REGISTER_COUNT {
                    return DispatchResult::Error(VmError::TypeError);
                }
                let arguments = task!().registers.regs[..arg_count as usize].to_vec();
                self.invoke_closure_physical(task_id, FuncId(func_id_raw), None, &arguments)
            }

            Opcode::CallIndirect | Opcode::CallIndirectProof => {
                let payload = c_payload(&instr);
                let (arg_count, closure_reg) = Instruction::c_call_indirect(payload);
                let closure_val = task!().registers.get(closure_reg);

                let payload_ptr = match closure_val.as_heap_ptr() {
                    Some(p) => p,
                    None => {
                        // Callable nominal values can use immediate storage,
                        // notably nullary Enum variants.
                        return if instr.opcode == Opcode::CallIndirectProof {
                            self.exec_call_apply_proof(task_id, closure_reg, arg_count)
                        } else {
                            self.exec_call_method(
                                task_id,
                                closure_reg,
                                str_interner::intern("apply"),
                                arg_count,
                            )
                        };
                    }
                };

                // SAFETY: heap values reference live allocations; verify the
                // closure layout before reading function metadata or captures.
                let header = unsafe { ObjectHeader::from_payload_ptr(payload_ptr) };
                if !header.is_ordinary() {
                    return DispatchResult::Error(VmError::TypeError);
                }
                if header.type_index != Intrinsic::Closure.type_index() {
                    // Dynamic calls include continuations and callable objects.
                    return if instr.opcode == Opcode::CallIndirectProof {
                        self.exec_call_apply_proof(task_id, closure_reg, arg_count)
                    } else {
                        self.exec_call_method(
                            task_id,
                            closure_reg,
                            str_interner::intern("apply"),
                            arg_count,
                        )
                    };
                }
                if header.payload_words() < 2 || arg_count as usize > runtime::GP_REGISTER_COUNT {
                    return DispatchResult::Error(VmError::TypeError);
                }
                // SAFETY: the initialized ClosureEnv layout was checked above.
                let func_id = unsafe { ClosureEnv::func_id(payload_ptr) };
                let explicit_args = task!().registers.regs[..arg_count as usize].to_vec();
                let mut physical = if instr.opcode == Opcode::CallIndirectProof {
                    explicit_args
                } else {
                    match self.expand_logical_arguments(func_id, &explicit_args) {
                        Ok(arguments) => arguments,
                        Err(error) => return DispatchResult::Error(error),
                    }
                };
                if let Err(error) = self.prepare_indirect_arguments(func_id, &mut physical) {
                    return DispatchResult::Error(error);
                }
                // Conversions may collect; read the environment from its actual
                // registered slot again before publishing the callee frame.
                let closure_val = task!().registers.get(closure_reg);
                self.invoke_closure_physical(task_id, func_id, Some(closure_val), &physical)
            }

            Opcode::TraitProof | Opcode::TraitAssert | Opcode::TraitProject => {
                let (dst, base, view) = a_fields(&instr);
                let view = TypeIndex::from_raw(view as u32);
                let source = task!().registers.get(base);
                let result = match instr.opcode {
                    Opcode::TraitProof => self.acquire_trait_proof(source, view),
                    Opcode::TraitProject => self.project_trait_proof(source, view),
                    Opcode::TraitAssert => {
                        let data = task!().registers.get(dst);
                        self.assert_trait_pair(source, data, view).map(|()| data)
                    }
                    _ => unreachable!("matched trait proof opcode"),
                };
                match result {
                    Ok(value) => {
                        task!().registers.set(dst, value);
                        DispatchResult::Continue
                    }
                    Err(error) => DispatchResult::Error(error),
                }
            }
            Opcode::TraitCall => {
                let (count, proof_register, slot) = Instruction::c_call_method(c_payload(&instr));
                if count as usize > runtime::GP_REGISTER_COUNT {
                    return DispatchResult::Error(VmError::TypeError);
                }
                let proof = task!().registers.get(proof_register);
                let args = task!().registers.regs[..count as usize].to_vec();
                self.dispatch_trait_call(task_id, proof, slot as usize, &args)
            }
            Opcode::CallMethod => {
                let payload = c_payload(&instr);
                let (arg_count, recv_reg, method_id) = Instruction::c_call_method(payload);
                self.exec_call_method(task_id, recv_reg, StrId::from_raw(method_id), arg_count)
            }
            Opcode::CallMethodFar => {
                let payload = c_payload(&instr);
                let (arg_count, recv_reg, const_idx) = Instruction::c_call_method(payload);
                let method_id = match self.constant_u32(const_idx) {
                    Ok(id) => id,
                    Err(error) => return DispatchResult::Error(error),
                };
                self.exec_call_method(task_id, recv_reg, StrId::from_raw(method_id), arg_count)
            }
            Opcode::CallBuiltin => {
                let payload = c_payload(&instr);
                let (arg_count, builtin_id) = Instruction::c_arg_count_func_id(payload);
                self.dispatch_builtin(task_id, builtin_id, arg_count)
            }
            Opcode::Return => {
                let src_reg = Instruction::c_return_reg(c_payload(&instr));
                let val = task!().registers.get(src_reg);
                DispatchResult::Return(val)
            }
            Opcode::ReturnUnit => DispatchResult::Return(TaggedValue::UNIT),

            // ── Effects (E-type) ──────────────────────────────────
            Opcode::EffectCallDyn => self.dispatch_effect(task_id, &instr),
            Opcode::PushHandlerClosure | Opcode::PushCapturingHandler => {
                self.dispatch_push_handler_closure(task_id, &instr)
            }
            Opcode::ResetClosure => self.dispatch_reset_closure(task_id, &instr),
            Opcode::ResumeContinuation | Opcode::ResumeContinuationOnce => {
                self.dispatch_language_resume(task_id, &instr)
            }
            Opcode::PopHandler => {
                task!().handler_stack.pop();
                DispatchResult::Continue
            }

            // ── System (E-type) ───────────────────────────────────
            Opcode::Safepoint => {
                self.state.heap.safe_point();
                if GC_FLAGS
                    .preempt_requested
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    return DispatchResult::Yield;
                }
                DispatchResult::Continue
            }
            Opcode::Unreachable => DispatchResult::Error(VmError::UnreachableExecuted),
            Opcode::Nop => DispatchResult::Continue,
        }
    }

    /// Save callee-saved regs and transfer control to `target`.
    fn push_call_frame(
        &mut self,
        task_id: TaskId,
        target: FuncId,
        arguments: &[TaggedValue],
    ) -> DispatchResult {
        let (display_state, cycle) = match self.display_call_state(task_id, target, arguments) {
            Ok(state) => state,
            Err(error) => return DispatchResult::Error(error),
        };
        let t = self.roots.scheduler.get_task_mut(task_id).unwrap();
        let mut saved = Vec::with_capacity(Reg::CALLEE_SAVED.len());
        for &r in Reg::CALLEE_SAVED {
            saved.push((Reg(r), t.registers.get(Reg(r))));
        }
        let frame = CallFrame {
            display_state: t.display_state.take(),
            return_pc: t.pc,
            func_id: t.current_func,
            saved_regs: saved,
            local_slots: std::mem::take(&mut t.local_slots),
        };
        t.call_stack.push(frame);
        t.current_func = target;
        t.pc = 0;
        t.display_state = display_state;
        if cycle {
            t.registers.regs[..arguments.len()].copy_from_slice(arguments);
            return match self.alloc_string("<cycle>") {
                Ok(value) => DispatchResult::Return(value),
                Err(error) => DispatchResult::Error(error),
            };
        }
        DispatchResult::Continue
    }

    fn exec_call_method(
        &mut self,
        task_id: TaskId,
        recv_reg: Reg,
        method_str_id: StrId,
        arg_count: u8,
    ) -> DispatchResult {
        let receiver = self
            .roots
            .scheduler
            .get_task_mut(task_id)
            .unwrap()
            .registers
            .get(recv_reg);
        let recv_type = match self.reflected_type(receiver) {
            Ok(ty) => ty,
            Err(error) => return DispatchResult::Error(error),
        };

        let selected = match self.selected_method(task_id, recv_type, method_str_id) {
            Ok(selected) => selected,
            Err(error) => return DispatchResult::Error(error),
        };
        if let Some(method) = selected {
            let target_func_id = FuncId(method.func_id);
            let function = match self.checked_function(target_func_id) {
                Ok(function) => function,
                Err(error) => return DispatchResult::Error(error),
            };
            let logical_count = function.abi.logical_parameter_count();
            if let Err(error) = self.check_entry_environment(target_func_id, None) {
                return DispatchResult::Error(error);
            }
            // A normal instance method takes self before its explicit arguments.
            // Reject both malformed calls and register overflow before shifting
            // any argument or publishing the receiver to r0.
            if arg_count as usize >= runtime::GP_REGISTER_COUNT {
                return DispatchResult::Error(VmError::ArityError {
                    expected: (runtime::GP_REGISTER_COUNT - 1) as u8,
                    got: arg_count,
                });
            }
            let Some(expected) = logical_count.checked_sub(1) else {
                return DispatchResult::Error(VmError::TypeError);
            };
            if arg_count as usize + 1 != logical_count {
                return DispatchResult::Error(VmError::ArityError {
                    expected: expected as u8,
                    got: arg_count,
                });
            }
            // Intrinsic collection methods keep their checked integer-index
            // ABI after visibility/ambiguity selection. Calling the std wrapper
            // would assert usize before accepting the existing signed indices.
            if let Some(result) =
                self.exec_collection_call(task_id, receiver, recv_type, method_str_id, arg_count)
            {
                return result;
            }
            let t = self.roots.scheduler.get_task(task_id).unwrap();
            let mut arguments = vec![receiver];
            arguments.extend_from_slice(&t.registers.regs[..arg_count as usize]);
            self.invoke_closure(task_id, target_func_id, None, &arguments)
        } else {
            if let Some(result) =
                self.exec_collection_call(task_id, receiver, recv_type, method_str_id, arg_count)
            {
                return result;
            }

            if recv_type == Intrinsic::Continuation.type_index() {
                match str_interner::get(method_str_id).as_str() {
                    "clone" => {
                        if arg_count != 0 {
                            return DispatchResult::Error(VmError::ArityError {
                                expected: 0,
                                got: arg_count,
                            });
                        }
                        return self.clone_language_continuation(task_id, receiver);
                    }
                    "apply" => {
                        if arg_count != 1 {
                            return DispatchResult::Error(VmError::ArityError {
                                expected: 1,
                                got: arg_count,
                            });
                        }
                        let value = self
                            .roots
                            .scheduler
                            .get_task(task_id)
                            .expect("dispatched task exists")
                            .registers
                            .get(Reg(0));
                        return self.resume_language_value(task_id, receiver, value);
                    }
                    _ => {}
                }
            }

            DispatchResult::Error(VmError::MethodNotFound)
        }
    }

    fn exec_call_apply_proof(
        &mut self,
        task_id: TaskId,
        recv_reg: Reg,
        arg_count: u8,
    ) -> DispatchResult {
        let receiver = self
            .roots
            .scheduler
            .get_task(task_id)
            .expect("dispatched task exists")
            .registers
            .get(recv_reg);
        let recv_type = match self.reflected_type(receiver) {
            Ok(ty) => ty,
            Err(error) => return DispatchResult::Error(error),
        };
        let apply_name = str_interner::intern("apply");
        let selected = match self.selected_method(task_id, recv_type, apply_name) {
            Ok(selected) => selected,
            Err(error) => return DispatchResult::Error(error),
        };
        if let Some(method) = selected {
            let target_func_id = FuncId(method.func_id);
            let abi = match self.checked_function(target_func_id) {
                Ok(function) => function.abi.clone(),
                Err(error) => return DispatchResult::Error(error),
            };
            if let Err(error) = self.check_entry_environment(target_func_id, None) {
                return DispatchResult::Error(error);
            }
            let Some(self_param) = abi.parameters.first() else {
                return DispatchResult::Error(VmError::TypeError);
            };
            let expected_explicit: usize = abi.parameters[1..]
                .iter()
                .map(|parameter| match parameter {
                    ParameterAbi::Value => 1,
                    ParameterAbi::Trait { .. } | ParameterAbi::TraitSelf { .. } => 2,
                })
                .sum();
            if arg_count as usize != expected_explicit {
                return DispatchResult::Error(VmError::ArityError {
                    expected: expected_explicit as u8,
                    got: arg_count,
                });
            }
            let mut physical = match self_param {
                ParameterAbi::Value => vec![receiver],
                ParameterAbi::Trait { view } | ParameterAbi::TraitSelf { view } => {
                    let proof = match self.acquire_trait_proof(receiver, *view) {
                        Ok(proof) => proof,
                        Err(error) => return DispatchResult::Error(error),
                    };
                    vec![proof, receiver]
                }
            };
            let t = self.roots.scheduler.get_task(task_id).unwrap();
            physical.extend_from_slice(&t.registers.regs[..arg_count as usize]);
            if let Err(error) = self.prepare_indirect_arguments(target_func_id, &mut physical) {
                return DispatchResult::Error(error);
            }
            self.invoke_closure_physical(task_id, target_func_id, None, &physical)
        } else if recv_type == Intrinsic::Continuation.type_index() {
            let handle = match Self::language_continuation_handle(receiver) {
                Ok(handle) => handle,
                Err(error) => return DispatchResult::Error(error),
            };
            let contract = match self.continuation_input_contract(task_id, handle) {
                Ok(contract) => contract,
                Err(error) => return DispatchResult::Error(error),
            };
            let input_ty = match self.canonical_type(contract.input) {
                Ok(ty) => ty,
                Err(error) => return DispatchResult::Error(error),
            };
            if matches!(
                self.state.type_pool.get(input_ty).kind,
                type_pool::TypeKind::Trait { .. }
            ) {
                if arg_count != 2 {
                    return DispatchResult::Error(VmError::ArityError {
                        expected: 2,
                        got: arg_count,
                    });
                }
                let t = self.roots.scheduler.get_task(task_id).unwrap();
                let proof = t.registers.get(Reg(0));
                let value = t.registers.get(Reg(1));
                if let Err(error) = self.assert_trait_pair(proof, value, input_ty) {
                    return DispatchResult::Error(error);
                }
                return self.resume_language_value(task_id, receiver, value);
            }
            if arg_count != 1 {
                return DispatchResult::Error(VmError::ArityError {
                    expected: 1,
                    got: arg_count,
                });
            }
            let value = self
                .roots
                .scheduler
                .get_task(task_id)
                .expect("dispatched task exists")
                .registers
                .get(Reg(0));
            self.resume_language_value(task_id, receiver, value)
        } else {
            DispatchResult::Error(VmError::MethodNotFound)
        }
    }

    fn exec_new_closure(
        &mut self,
        task_id: TaskId,
        dst: Reg,
        func_id: FuncId,
        capture_count: u16,
    ) -> DispatchResult {
        if capture_count as usize > runtime::GP_REGISTER_COUNT {
            return DispatchResult::Error(VmError::TypeError);
        }
        if let Err(error) = self.check_capture_abi(func_id, capture_count as usize) {
            return DispatchResult::Error(error);
        }
        let captures = self
            .roots
            .scheduler
            .get_task(task_id)
            .expect("dispatched task exists")
            .registers
            .regs[..capture_count as usize]
            .to_vec();
        if let Err(error) = self.check_capture_proofs(func_id, &captures) {
            return DispatchResult::Error(error);
        }
        let payload_words = ClosureEnv::payload_words(capture_count);
        let closure_type = Intrinsic::Closure.type_index();

        // SAFETY: run holds an operation. Captures are read from rooted registers
        // after allocation, and the completed closure is published to dst.
        match unsafe { self.state.heap.alloc_object(closure_type, payload_words) } {
            Some(ptr) => {
                let t = self.roots.scheduler.get_task_mut(task_id).unwrap();
                let mut captures = Vec::with_capacity(capture_count as usize);
                for i in 0..capture_count {
                    captures.push(t.registers.get(Reg(i as u8)));
                }
                unsafe {
                    ClosureEnv::write(ptr.as_ptr(), func_id, &captures);
                }
                let val = unsafe { TaggedValue::from_heap_ptr(ptr.as_ptr()) };
                t.registers.set(dst, val);
                DispatchResult::Continue
            }
            None => DispatchResult::Error(VmError::OutOfMemory),
        }
    }

    /// Dispatch a builtin function call.
    /// Arguments are in r0..rN by calling convention. Result goes into r0.
    fn dispatch_builtin(
        &mut self,
        task_id: TaskId,
        builtin_id: u32,
        arg_count: u8,
    ) -> DispatchResult {
        let Some(f) = self.state.builtins.get(builtin_id) else {
            return DispatchResult::Error(VmError::UnknownBuiltin(builtin_id));
        };
        let mut ctx = BuiltinCtx::new(self, task_id, arg_count);
        if let Err(e) = f(&mut ctx) {
            return DispatchResult::Error(e);
        }
        match ctx.take_outcome() {
            BuiltinOutcome::Continue => DispatchResult::Continue,
            BuiltinOutcome::Exit(code) => {
                std::process::exit(code);
            }
            BuiltinOutcome::Panic(msg) => DispatchResult::Error(VmError::Panic(msg)),
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatch helpers
// ---------------------------------------------------------------------------

fn r_regs(instr: &Instruction) -> (Reg, Reg, Reg) {
    match instr.data {
        InstructionData::R {
            dst, src1, src2, ..
        } => (dst, src1, src2),
        _ => unreachable!("expected R-type"),
    }
}

fn a_fields(instr: &Instruction) -> (Reg, Reg, u16) {
    match instr.data {
        InstructionData::A { dst, base, imm12 } => (dst, base, imm12),
        _ => unreachable!("expected A-type"),
    }
}

fn j_fields(instr: &Instruction) -> (Reg, i32) {
    match instr.data {
        InstructionData::J { cond, offset } => (cond, offset),
        _ => unreachable!("expected J-type"),
    }
}

fn c_payload(instr: &Instruction) -> u32 {
    match instr.data {
        InstructionData::C { payload } => payload,
        _ => unreachable!("expected C-type"),
    }
}

fn e_payload(instr: &Instruction) -> u32 {
    match instr.data {
        InstructionData::E { payload } => payload,
        _ => unreachable!("expected E-type"),
    }
}

fn is_truthy(val: TaggedValue) -> bool {
    if val.is_null() || val.is_unit() {
        return false;
    }
    if let Some(b) = val.as_bool() {
        return b;
    }
    // SAFETY: callers are instruction dispatchers with rooted live values
    // and an active execution operation. Scalar decoding does not allocate.
    if let Some(number) = unsafe { Number::from_tagged(val) } {
        return number.partial_cmp_numeric(Number::I64(0)) != Some(std::cmp::Ordering::Equal);
    }
    // Heap objects and everything else are truthy.
    true
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Outcome of a dispatch of a single instruction.
enum DispatchResult {
    Continue,
    Return(TaggedValue),
    Yield,
    Error(VmError),
}

/// Outcome of executing a task.
enum ExecOutcome {
    Finished,
    Yield,
    Error(VmError),
}

/// Result of running the VM.
#[derive(Debug)]
pub enum VmResult {
    Finished,
    Error(VmError),
}

/// VM runtime errors.
#[derive(Debug)]
pub enum VmError {
    InvalidInstruction(u32),
    InvalidTask,
    InvalidFunction(FuncId),
    InvalidSlot(u32),
    InvalidField(u32),
    InvalidEnumVariant(u32),
    UnsupportedEnumEquality,
    UnsupportedEquality,
    NoMatchingCase,
    UnreachableExecuted,
    InvalidIndex,
    /// A native argument has the right type but violates the operation's domain.
    InvalidArgument(&'static str),
    DisplayDepthExceeded,
    DisplaySizeExceeded,
    InvalidConstant(u32),
    InvalidGlobal(u32),
    UninitializedGlobal(u32),
    ImmutableGlobal(u32),
    GlobalsAlreadyInitialized,
    InvalidType(TypeIndex),
    Stack(runtime::StackError),
    UnhandledEffect(TypeIndex),
    OutOfMemory,
    ObjectTooLarge,
    UnsupportedFunctionAbi,
    InvalidTraitProof,
    TraitProofLimit,
    TraitProofMetadata(type_pool::TraitProofError),
    CrossVmOperation,
    TypeError,
    UnsupportedLegacyErrorLayout(TypeIndex),
    NumericOverflow,
    DivisionByZero,
    InvalidShift,
    ArityError {
        expected: u8,
        got: u8,
    },
    UnknownBuiltin(u32),
    MethodNotFound,
    MethodAccess(type_pool::MethodAccessError),
    MethodAccessDenied,
    AmbiguousMethod,
    MissingMethodContext,
    TraitLookup(type_pool::TraitLookupError),
    MissingTraitContext,
    Panic(String),
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInstruction(word) => write!(f, "invalid instruction {word:#010x}"),
            Self::InvalidTask => f.write_str("invalid task"),
            Self::InvalidFunction(id) => write!(f, "invalid function id {}", id.0),
            Self::InvalidSlot(index) => write!(f, "invalid local slot {index}"),
            Self::InvalidField(index) => write!(f, "invalid field index {index}"),
            Self::InvalidEnumVariant(tag) => write!(f, "invalid enum variant {tag}"),
            Self::UnsupportedEnumEquality => f.write_str("unsupported payload enum equality"),
            Self::UnsupportedEquality => f.write_str("unsupported scalar equality operand"),
            Self::NoMatchingCase => f.write_str("non-exhaustive match failed"),
            Self::UnreachableExecuted => f.write_str("entered unreachable code"),
            Self::InvalidIndex => f.write_str("index out of bounds"),
            Self::InvalidArgument(reason) => write!(f, "invalid argument: {reason}"),
            Self::DisplayDepthExceeded => f.write_str("display formatting depth exceeded"),
            Self::DisplaySizeExceeded => f.write_str("display formatting size exceeded"),
            Self::InvalidConstant(index) => write!(f, "invalid constant index {index}"),
            Self::InvalidGlobal(index) => write!(f, "invalid global index {index}"),
            Self::UninitializedGlobal(index) => write!(f, "uninitialized global {index}"),
            Self::ImmutableGlobal(index) => write!(f, "cannot mutate immutable global {index}"),
            Self::GlobalsAlreadyInitialized => f.write_str("globals already initialized"),
            Self::InvalidType(ty) => write!(f, "invalid type index {}", ty.as_u32()),
            Self::Stack(error) => write!(f, "stack error: {error:?}"),
            Self::UnhandledEffect(ty) => write!(f, "unhandled effect {}", ty.as_u32()),
            Self::OutOfMemory => f.write_str("out of memory"),
            Self::ObjectTooLarge => f.write_str("object exceeds maximum size"),
            Self::UnsupportedFunctionAbi => f.write_str("unsupported function ABI"),
            Self::InvalidTraitProof => f.write_str("invalid trait proof"),
            Self::TraitProofLimit => f.write_str("trait proof registry limit reached"),
            Self::TraitProofMetadata(error) => write!(f, "invalid trait proof metadata: {error:?}"),
            Self::CrossVmOperation => f.write_str("cross-VM reentry is not permitted"),
            Self::TypeError => f.write_str("type error"),
            Self::UnsupportedLegacyErrorLayout(ty) => {
                write!(f, "unsupported error layout for type {}", ty.as_u32())
            }
            Self::NumericOverflow => f.write_str("numeric overflow"),
            Self::DivisionByZero => f.write_str("division by zero"),
            Self::InvalidShift => f.write_str("invalid bit shift"),
            Self::ArityError { expected, got } => {
                write!(f, "arity mismatch: expected {expected}, got {got}")
            }
            Self::UnknownBuiltin(id) => write!(f, "unknown builtin {id}"),
            Self::MethodNotFound => f.write_str("method not found"),
            Self::MethodAccess(error) => write!(f, "invalid method access metadata: {error:?}"),
            Self::MethodAccessDenied => f.write_str("method access denied"),
            Self::AmbiguousMethod => f.write_str("ambiguous method call"),
            Self::MissingMethodContext => f.write_str("missing lexical method call context"),
            Self::TraitLookup(error) => write!(f, "trait lookup error: {error:?}"),
            Self::MissingTraitContext => f.write_str("missing lexical trait context"),
            Self::Panic(message) => write!(f, "panic: {message}"),
        }
    }
}

impl std::error::Error for VmError {}

// ---------------------------------------------------------------------------
// GC root scanning — enumerates all roots in the VM for MMTk
// ---------------------------------------------------------------------------

/// Root scan callback owned by Vm's lifetime-bound registration.
///
/// Walks all root sources in the Vm:
///   1. Global variable table, constant pool, and temporary value roots
///   2. Active and pending-delimiter stack contexts across all scheduled tasks
///   3. Native pinning roots
///
/// Each TaggedValue that might hold a heap pointer is presented to the
/// visitor as a `NessaSlot` pointing at the memory location of the value.
unsafe fn scan_vm_roots(context: *const (), visitor: &mut dyn FnMut(NessaSlot)) {
    // SAFETY: RootCell is stable UnsafeCell storage. STW and the registration
    // lock provide exclusive access, with no outstanding mutator root borrows.
    // Slot addresses remain stable until this collection completes.
    let vm = unsafe { &mut *context.cast::<VmRoots>().cast_mut() };
    let mut visit = |value: &mut TaggedValue| {
        // SAFETY: this exclusive aligned value slot lives until tracing ends.
        visitor(unsafe { NessaSlot::from_raw_ptr((value as *mut TaggedValue).cast::<u64>()) });
    };
    for value in vm.globals.values_mut() {
        visit(value);
    }
    for value in vm.constants.values_mut() {
        visit(value);
    }
    for value in vm.temporary_values.get_mut() {
        visit(value);
    }
    // Copy only scalar task IDs; scheduler storage is neither resized nor moved
    // during stopped-world enumeration or subsequent root processing.
    let tasks: Vec<_> = vm
        .scheduler
        .all_tasks()
        .iter()
        .map(|task| task.id)
        .collect();
    for task_id in tasks {
        let task = vm
            .scheduler
            .get_task_mut(task_id)
            .expect("enumerated task remains alive under STW");
        for stack_context in task.stacks.root_contexts_mut() {
            continuation_roots::visit_stack_slots(stack_context, visitor);
        }
    }
    for value in vm.native_pins.get_mut() {
        // SAFETY: only copied values handed to a live native call enter this
        // buffer. Their addresses and native lifetime remain fixed under STW.
        visitor(unsafe { NessaSlot::from_pinning_root((value as *mut TaggedValue).cast::<u64>()) });
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use nsbc::FunctionAbi;

    use super::*;

    pub(super) struct TestVm {
        // Keep fixture mutations under one operation; release before Vm drops.
        _operation: gc::MutatorSession<'static>,
        vm: Vm,
    }

    impl std::ops::Deref for TestVm {
        type Target = Vm;
        fn deref(&self) -> &Vm {
            &self.vm
        }
    }

    impl std::ops::DerefMut for TestVm {
        fn deref_mut(&mut self) -> &mut Vm {
            &mut self.vm
        }
    }

    pub(super) fn make_vm() -> TestVm {
        let vm = Vm::new(
            TypePool::with_intrinsics(),
            &GcConfig::default(),
            Arc::new(StackPool::new()),
        );
        let operation = vm.operation();
        TestVm {
            _operation: operation,
            vm,
        }
    }

    fn encode_instrs(instrs: &[Instruction]) -> Vec<u32> {
        instrs.iter().map(|i| i.encode()).collect()
    }

    #[test]
    fn simple_add_and_return() {
        let mut vm = make_vm();
        // r0 = 10, r1 = 32, r2 = r0 + r1, return r2
        let code = encode_instrs(&[
            Instruction::load_imm(Reg(0), 10),
            Instruction::load_imm(Reg(1), 32),
            Instruction::add(Reg(2), Reg(0), Reg(1)),
            Instruction::ret(Reg(2)),
        ]);
        let func = FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: code,
            register_count: 3,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        };
        vm.add_function(func);
        let task_id = vm.spawn_root(FuncId(0));
        let result = vm.run();
        assert!(matches!(result, VmResult::Finished));
        let val = vm
            .roots
            .scheduler
            .get_task(task_id)
            .unwrap()
            .registers
            .get(Reg(0));
        assert_eq!(val.as_i64(), Some(42));
    }

    #[test]
    fn wide_local_slot_load_and_store_return_the_saved_value() {
        let mut vm = make_vm();
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: encode_instrs(&[
                Instruction::allocate_slots(70001),
                Instruction::load_imm(Reg(0), 42),
                Instruction::store_slot(70000, Reg(0)),
                Instruction::load_imm(Reg(0), 1),
                Instruction::load_slot(Reg(0), 70000),
                Instruction::ret(Reg(0)),
            ]),
            register_count: 32,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        let task = vm.spawn_root(FuncId(0));
        assert!(matches!(vm.run(), VmResult::Finished));
        let task = vm.roots.scheduler.get_task(task).unwrap();
        assert_eq!(task.registers.get(Reg(0)).as_i64(), Some(42));
        assert_eq!(task.local_slots.capacity(), 0);
    }

    #[test]
    fn invalid_local_slot_access_reports_an_error_and_releases_stacks() {
        for access in [
            Instruction::load_slot(Reg(0), 1),
            Instruction::store_slot(1, Reg(0)),
        ] {
            let mut vm = make_vm();
            vm.add_function(FunctionCode {
                display_owner: None,
                abi: FunctionAbi::value_only(0),
                func_id: FuncId(0),
                instructions: encode_instrs(&[Instruction::allocate_slots(1), access]),
                register_count: 32,
                param_count: 0,
                is_closure: false,
                function_type: type_pool::TypeIndex::INVALID,
            });
            vm.spawn_root(FuncId(0));
            assert!(matches!(vm.run(), VmResult::Error(VmError::InvalidSlot(1))));
            assert_eq!(vm.roots.scheduler.stack_pool().active_count(), 0);
        }
    }

    #[test]
    fn invalid_field_access_cannot_read_or_write_past_the_object() {
        for store in [false, true] {
            let mut vm = make_vm();
            let ty = vm.state.type_pool.register(type_pool::TypeInfo {
                kind: type_pool::TypeKind::Tuple {
                    elements: vec![Intrinsic::I64.type_index()],
                },
                type_id: type_pool::TypeId(99, 99),
                size: 8,
                align: 8,
            });
            let access = if store {
                Instruction::store_field(Reg(1), 1, Reg(0))
            } else {
                Instruction::load_field(Reg(0), Reg(1), 1)
            };
            vm.add_function(FunctionCode {
                display_owner: None,
                abi: FunctionAbi::value_only(0),
                func_id: FuncId(0),
                instructions: encode_instrs(&[Instruction::new_object(Reg(1), ty), access]),
                register_count: 32,
                param_count: 0,
                is_closure: false,
                function_type: type_pool::TypeIndex::INVALID,
            });
            vm.spawn_root(FuncId(0));
            assert!(matches!(
                vm.run(),
                VmResult::Error(VmError::InvalidField(1))
            ));
        }
    }

    #[test]
    fn conditional_jump() {
        let mut vm = make_vm();
        // r0 = true, if r0 jump +2, r1 = 0, return r1, r1 = 42, return r1
        let code = encode_instrs(&[
            Instruction::load_true(Reg(0)),    // 0
            Instruction::jmp_if(Reg(0), 2),    // 1: jump to 3 (1+2=3, but -1 for pre-inc → pc=3)
            Instruction::load_imm(Reg(1), 0),  // 2: skipped
            Instruction::load_imm(Reg(1), 42), // 3: target
            Instruction::ret(Reg(1)),          // 4
        ]);
        let func = FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: code,
            register_count: 2,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        };
        vm.add_function(func);
        let task_id = vm.spawn_root(FuncId(0));
        vm.run();
        // r1 = 42, returned into r0
        let val = vm
            .roots
            .scheduler
            .get_task(task_id)
            .unwrap()
            .registers
            .get(Reg(0));
        assert_eq!(val.as_i64(), Some(42));
    }

    #[test]
    fn function_call_and_return() {
        let mut vm = make_vm();
        let main_code = encode_instrs(&[
            Instruction::load_imm(Reg(0), 5),
            Instruction::call(1, 1),
            Instruction::ret(Reg(0)),
        ]);
        let func1_code = encode_instrs(&[Instruction::load_imm(Reg(1), 10), Instruction::ret(Reg(1))]);

        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: main_code,
            register_count: 2,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(1),
            func_id: FuncId(1),
            instructions: func1_code,
            register_count: 2,
            param_count: 1,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });

        let task_id = vm.spawn_root(FuncId(0));
        vm.run();
        let val = vm
            .roots
            .scheduler
            .get_task(task_id)
            .unwrap()
            .registers
            .get(Reg(0));
        assert_eq!(val.as_i64(), Some(10));
    }

    #[test]
    fn return_unit() {
        let mut vm = make_vm();
        let code = encode_instrs(&[Instruction::return_unit()]);
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: code,
            register_count: 0,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        let task_id = vm.spawn_root(FuncId(0));
        vm.run();
        let val = vm
            .roots
            .scheduler
            .get_task(task_id)
            .unwrap()
            .registers
            .get(Reg(0));
        assert!(val.is_unit());
    }

    #[test]
    fn globals_load_store() {
        let mut vm = make_vm();
        let gidx = vm.roots.globals.alloc(TaggedValue::UNIT);
        let code = encode_instrs(&[
            Instruction::load_imm(Reg(2), 99),
            Instruction::store_global(gidx as u16, Reg(2)),
            Instruction::load_global(Reg(1), gidx as u16),
            Instruction::ret(Reg(1)),
        ]);
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: code,
            register_count: 3,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        let task_id = vm.spawn_root(FuncId(0));
        assert!(matches!(vm.run(), VmResult::Finished));
        let val = vm
            .roots
            .scheduler
            .get_task(task_id)
            .unwrap()
            .registers
            .get(Reg(0));
        assert_eq!(val.as_i64(), Some(99));
    }

    #[test]
    fn comparison_ops() {
        let mut vm = make_vm();
        let code = encode_instrs(&[
            Instruction::load_imm(Reg(0), 5),
            Instruction::load_imm(Reg(1), 10),
            Instruction::r_type(Opcode::CmpLt, Reg(2), Reg(0), Reg(1)),
            Instruction::ret(Reg(2)),
        ]);
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: code,
            register_count: 3,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        let task_id = vm.spawn_root(FuncId(0));
        vm.run();
        let val = vm
            .roots
            .scheduler
            .get_task(task_id)
            .unwrap()
            .registers
            .get(Reg(0));
        assert_eq!(val.as_bool(), Some(true));
    }

    #[test]
    fn call_builtin_to_i64() {
        let mut vm = make_vm();
        vm.register_builtin(runtime::ids::TO_I64, |ctx| {
            let v = ctx.arg(0)?;
            let i = v.as_i64().ok_or(VmError::TypeError)?;
            ctx.return_i64(i)?;
            Ok(())
        });
        let code = encode_instrs(&[
            Instruction::load_imm(Reg(0), 7),
            Instruction::call_builtin(runtime::ids::TO_I64, 1),
            Instruction::ret(Reg(0)),
        ]);
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: code,
            register_count: 1,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        let task_id = vm.spawn_root(FuncId(0));
        let result = vm.run();
        assert!(matches!(result, VmResult::Finished));
        let val = vm
            .roots
            .scheduler
            .get_task(task_id)
            .unwrap()
            .registers
            .get(Reg(0));
        assert_eq!(val.as_i64(), Some(7));
    }

    #[test]
    fn unreachable_opcode_reports_clear_error() {
        let mut vm = make_vm();
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: encode_instrs(&[Instruction::unreachable()]),
            register_count: 1,
            param_count: 0,
            is_closure: false,
            function_type: type_pool::TypeIndex::INVALID,
        });
        vm.spawn_root(FuncId(0));
        let VmResult::Error(error) = vm.run() else {
            panic!("expected unreachable error");
        };
        assert!(matches!(error, VmError::UnreachableExecuted));
        assert_eq!(error.to_string(), "entered unreachable code");
    }

    #[test]
    fn far_calls_reject_invalid_and_non_integer_constants() {
        for call in [
            Instruction::call_far(0, 0),
            Instruction::call_method_far(Reg(0), 0, 0),
        ] {
            let mut missing = make_vm();
            missing.add_function(FunctionCode {
                display_owner: None,
                abi: FunctionAbi::value_only(0),
                func_id: FuncId(0),
                instructions: encode_instrs(&[Instruction::load_true(Reg(0)), call]),
                register_count: 2,
                param_count: 0,
                is_closure: false,
                function_type: type_pool::TypeIndex::INVALID,
            });
            missing.spawn_root(FuncId(0));
            assert!(matches!(
                missing.run(),
                VmResult::Error(VmError::InvalidConstant(0))
            ));
            drop(missing);

            let mut non_int = make_vm();
            non_int.roots.constants.push(TaggedValue::TRUE);
            non_int.add_function(FunctionCode {
                display_owner: None,
                abi: FunctionAbi::value_only(0),
                func_id: FuncId(0),
                instructions: encode_instrs(&[Instruction::load_true(Reg(0)), call]),
                register_count: 2,
                param_count: 0,
                is_closure: false,
                function_type: type_pool::TypeIndex::INVALID,
            });
            non_int.spawn_root(FuncId(0));
            assert!(matches!(non_int.run(), VmResult::Error(VmError::TypeError)));
        }
    }

    #[test]
    fn scalar_cmp_eq_rejects_non_scalar_heap_tuples() {
        let mut vm = make_vm();
        let tuple_ty = vm
            .state
            .type_pool
            .intern_structural(type_pool::TypeKind::Tuple {
                elements: vec![Intrinsic::I64.type_index()],
            });
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::value_only(0),
            func_id: FuncId(0),
            instructions: encode_instrs(&[
                Instruction::new_object(Reg(0), tuple_ty),
                Instruction::new_object(Reg(1), tuple_ty),
                Instruction::r_type(Opcode::CmpEq, Reg(2), Reg(0), Reg(1)),
                Instruction::ret(Reg(2)),
            ]),
            register_count: 3,
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

    #[test]
    fn root_entry_rejects_capturing_closure_function() {
        let mut vm = make_vm();
        vm.add_function(FunctionCode {
            display_owner: None,
            abi: FunctionAbi::closure_values(1, 0),
            func_id: FuncId(0),
            instructions: encode_instrs(&[Instruction::ret(Reg(0))]),
            register_count: 1,
            param_count: 1,
            is_closure: true,
            function_type: type_pool::TypeIndex::INVALID,
        });
        vm.spawn_root(FuncId(0));
        assert!(matches!(vm.run(), VmResult::Error(VmError::TypeError)));
    }
}
