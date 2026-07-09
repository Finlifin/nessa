use gc::{GC_FLAGS, GcConfig, Heap, NessaSlot, ObjectHeader};
use nsbc::{AddrMode, FuncId, Instruction, InstructionData, IntrinsicFn, Opcode, Reg};
use runtime::{
    BytecodeStore, CallFrame, ClosureEnv, ConstantPool, EffectHandler, FunctionCode, GlobalTable,
    TaggedValue, TaskId, TaskStatus,
};
use scheduler::Scheduler;
use stack_pool::StackPool;
use std::sync::Arc;
use str_interner::StrId;
use type_pool::{Intrinsic, TypeIndex, TypePool};

// ---------------------------------------------------------------------------
// VM — the top-level virtual machine
// ---------------------------------------------------------------------------

/// The top-level Nessa virtual machine.
pub struct Vm {
    pub bytecode: BytecodeStore,
    pub globals: GlobalTable,
    pub constants: ConstantPool,
    pub type_pool: TypePool,
    pub heap: Heap,
    pub scheduler: Scheduler,
}

impl Vm {
    pub fn new(type_pool: TypePool, gc_config: &GcConfig, stack_pool: Arc<StackPool>) -> Self {
        Self {
            bytecode: BytecodeStore::new(),
            globals: GlobalTable::new(),
            constants: ConstantPool::new(),
            type_pool,
            heap: gc::gc_init(gc_config),
            scheduler: Scheduler::with_stack_pool(stack_pool),
        }
    }

    /// Add a compiled function to the VM.
    pub fn add_function(&mut self, code: FunctionCode) -> FuncId {
        self.bytecode.add_function(code)
    }

    /// Allocate a heap string and return its TaggedValue.
    /// The string bytes are laid out as: [len:u64][...UTF-8 bytes...].
    pub fn alloc_string(&mut self, s: &str) -> TaggedValue {
        let bytes = s.as_bytes();
        let payload_bytes = 8 + bytes.len();
        let payload_words = ((payload_bytes + 7) / 8) as u16;
        let str_type = self.type_pool.intrinsic(Intrinsic::Str);
        match self.heap.alloc_object(str_type, payload_words) {
            Some(ptr) => unsafe {
                // Write length prefix.
                *(ptr.as_ptr() as *mut u64) = bytes.len() as u64;
                // Write UTF-8 bytes after the length.
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.as_ptr().add(8), bytes.len());
                TaggedValue::from_heap_ptr(ptr.as_ptr())
            },
            None => TaggedValue::UNIT,
        }
    }

    /// Spawn a root task that starts executing the given function.
    pub fn spawn_root(&mut self, func_id: FuncId) -> TaskId {
        self.scheduler.spawn(func_id, None)
    }

    /// Run the VM until all tasks have finished.
    pub fn run(&mut self) -> VmResult {
        // Register GC root scanner — pointer is stable for the duration of run().
        unsafe { gc::set_vm_ptr(self as *const Vm as *const ()) };
        gc::register_root_scanner(scan_vm_roots);

        loop {
            let Some(task_id) = self.scheduler.next_ready() else {
                return VmResult::Finished;
            };
            self.scheduler.set_running(task_id);
            let result = self.execute_task(task_id);
            match result {
                ExecOutcome::Finished => {
                    self.scheduler.finish(task_id);
                }
                ExecOutcome::Suspended => {
                    self.scheduler.suspend(task_id);
                }
                ExecOutcome::Error(e) => {
                    self.scheduler.finish(task_id);
                    return VmResult::Error(e);
                }
                ExecOutcome::Yield => {
                    // Re-enqueue as ready.
                    if let Some(task) = self.scheduler.get_task_mut(task_id) {
                        task.status = TaskStatus::Ready;
                    }
                }
            }
        }
    }

    /// Execute one task until it finishes, suspends, errors, or yields.
    fn execute_task(&mut self, task_id: TaskId) -> ExecOutcome {
        loop {
            // Get current task's PC and function.
            let (pc, func_id) = {
                let task = match self.scheduler.get_task(task_id) {
                    Some(t) => t,
                    None => return ExecOutcome::Error(VmError::InvalidTask),
                };
                (task.pc, task.current_func)
            };

            let func = self.bytecode.get_function(func_id);
            if pc as usize >= func.instructions.len() {
                return ExecOutcome::Finished;
            }

            let word = func.instructions[pc as usize];
            let instr = match Instruction::decode(word) {
                Some(i) => i,
                None => return ExecOutcome::Error(VmError::InvalidInstruction(word)),
            };

            // Advance PC before executing (jumps will override).
            if let Some(task) = self.scheduler.get_task_mut(task_id) {
                task.pc = pc + 1;
            }

            match self.dispatch(task_id, instr) {
                DispatchResult::Continue => {}
                DispatchResult::Return(val) => {
                    // Pop call frame.
                    let should_finish = {
                        let task = self.scheduler.get_task_mut(task_id).unwrap();
                        if let Some(frame) = task.call_stack.pop() {
                            // Restore PC and function.
                            task.pc = frame.return_pc;
                            task.current_func = frame.func_id;
                            // Restore saved registers.
                            for (reg, saved_val) in &frame.saved_regs {
                                task.registers.set(*reg, *saved_val);
                            }
                            // Return value goes into r0.
                            task.registers.set(Reg(0), val);
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
                DispatchResult::Suspend => return ExecOutcome::Suspended,
                DispatchResult::Yield => return ExecOutcome::Yield,
                DispatchResult::Error(e) => return ExecOutcome::Error(e),
            }
        }
    }

    /// Dispatch a single instruction.
    fn dispatch(&mut self, task_id: TaskId, instr: Instruction) -> DispatchResult {
        // Helper macro to get task mutably.
        macro_rules! task {
            () => {
                self.scheduler.get_task_mut(task_id).unwrap()
            };
        }

        match instr.opcode {
            // ── Arithmetic (R-type) ────────────────────────────────
            Opcode::Add => {
                let (dst, s1, s2) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1);
                let b = t.registers.get(s2);
                let result = arith_add(a, b);
                t.registers.set(dst, result);
                DispatchResult::Continue
            }
            Opcode::Sub => {
                let (dst, s1, s2) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1);
                let b = t.registers.get(s2);
                let result = arith_sub(a, b);
                t.registers.set(dst, result);
                DispatchResult::Continue
            }
            Opcode::Mul => {
                let (dst, s1, s2) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1);
                let b = t.registers.get(s2);
                let result = arith_mul(a, b);
                t.registers.set(dst, result);
                DispatchResult::Continue
            }
            Opcode::Div => {
                let (dst, s1, s2) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1);
                let b = t.registers.get(s2);
                let result = arith_div(a, b);
                t.registers.set(dst, result);
                DispatchResult::Continue
            }
            Opcode::Mod => {
                let (dst, s1, s2) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1);
                let b = t.registers.get(s2);
                let result = arith_mod(a, b);
                t.registers.set(dst, result);
                DispatchResult::Continue
            }
            Opcode::Neg => {
                let (dst, s1, _) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1);
                let result = if let Some(v) = a.as_i64() {
                    TaggedValue::from_i64(-v)
                } else {
                    TaggedValue::UNIT
                };
                t.registers.set(dst, result);
                DispatchResult::Continue
            }
            Opcode::BitAnd => {
                let (dst, s1, s2) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1).as_i64().unwrap_or(0);
                let b = t.registers.get(s2).as_i64().unwrap_or(0);
                t.registers.set(dst, TaggedValue::from_i64(a & b));
                DispatchResult::Continue
            }
            Opcode::BitOr => {
                let (dst, s1, s2) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1).as_i64().unwrap_or(0);
                let b = t.registers.get(s2).as_i64().unwrap_or(0);
                t.registers.set(dst, TaggedValue::from_i64(a | b));
                DispatchResult::Continue
            }
            Opcode::BitXor => {
                let (dst, s1, s2) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1).as_i64().unwrap_or(0);
                let b = t.registers.get(s2).as_i64().unwrap_or(0);
                t.registers.set(dst, TaggedValue::from_i64(a ^ b));
                DispatchResult::Continue
            }
            Opcode::BitNot => {
                let (dst, s1, _) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1).as_i64().unwrap_or(0);
                t.registers.set(dst, TaggedValue::from_i64(!a));
                DispatchResult::Continue
            }
            Opcode::Shl => {
                let (dst, s1, s2) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1).as_i64().unwrap_or(0);
                let b = t.registers.get(s2).as_i64().unwrap_or(0);
                t.registers
                    .set(dst, TaggedValue::from_i64(a.wrapping_shl(b as u32)));
                DispatchResult::Continue
            }
            Opcode::Shr => {
                let (dst, s1, s2) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1).as_i64().unwrap_or(0);
                let b = t.registers.get(s2).as_i64().unwrap_or(0);
                t.registers
                    .set(dst, TaggedValue::from_i64(a.wrapping_shr(b as u32)));
                DispatchResult::Continue
            }
            Opcode::UShr => {
                let (dst, s1, s2) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1).as_u64().unwrap_or(0);
                let b = t.registers.get(s2).as_i64().unwrap_or(0);
                t.registers
                    .set(dst, TaggedValue::from_u64(a.wrapping_shr(b as u32)));
                DispatchResult::Continue
            }

            // ── Comparison (R-type) ────────────────────────────────
            Opcode::CmpEq => {
                let (dst, s1, s2) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1);
                let b = t.registers.get(s2);
                t.registers
                    .set(dst, TaggedValue::from_bool(a.raw() == b.raw()));
                DispatchResult::Continue
            }
            Opcode::CmpNe => {
                let (dst, s1, s2) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1);
                let b = t.registers.get(s2);
                t.registers
                    .set(dst, TaggedValue::from_bool(a.raw() != b.raw()));
                DispatchResult::Continue
            }
            Opcode::CmpLt => {
                let (dst, s1, s2) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1).as_i64().unwrap_or(0);
                let b = t.registers.get(s2).as_i64().unwrap_or(0);
                t.registers.set(dst, TaggedValue::from_bool(a < b));
                DispatchResult::Continue
            }
            Opcode::CmpLe => {
                let (dst, s1, s2) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1).as_i64().unwrap_or(0);
                let b = t.registers.get(s2).as_i64().unwrap_or(0);
                t.registers.set(dst, TaggedValue::from_bool(a <= b));
                DispatchResult::Continue
            }
            Opcode::CmpGt => {
                let (dst, s1, s2) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1).as_i64().unwrap_or(0);
                let b = t.registers.get(s2).as_i64().unwrap_or(0);
                t.registers.set(dst, TaggedValue::from_bool(a > b));
                DispatchResult::Continue
            }
            Opcode::CmpGe => {
                let (dst, s1, s2) = r_regs(&instr);
                let t = task!();
                let a = t.registers.get(s1).as_i64().unwrap_or(0);
                let b = t.registers.get(s2).as_i64().unwrap_or(0);
                t.registers.set(dst, TaggedValue::from_bool(a >= b));
                DispatchResult::Continue
            }

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
            Opcode::Load => {
                let (dst, base, imm12) = a_fields(&instr);
                let val = match instr.amode {
                    AddrMode::Imm => {
                        // Sign-extend 12-bit immediate to i64.
                        TaggedValue::from_i64(Instruction::sext_imm12(imm12) as i64)
                    }
                    AddrMode::Const => self.constants.get(imm12 as u32),
                    AddrMode::RegOff => {
                        let t = task!();
                        let base_val = t.registers.get(base);
                        let off = Instruction::sext_imm12(imm12) as i64;
                        if let Some(n) = base_val.as_i64() {
                            TaggedValue::from_i64(n.wrapping_add(off))
                        } else if let Some(n) = base_val.as_u64() {
                            TaggedValue::from_u64(n.wrapping_add(off as u64))
                        } else {
                            TaggedValue::UNIT
                        }
                    }
                    AddrMode::Reserved => {
                        return DispatchResult::Error(VmError::InvalidInstruction(instr.encode()));
                    }
                };
                task!().registers.set(dst, val);
                DispatchResult::Continue
            }
            Opcode::LoadConstWide => {
                let (dst, base, imm12) = a_fields(&instr);
                let idx = Instruction::wide_const_index(base, imm12);
                let val = self.constants.get(idx);
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
            Opcode::TypeCheck => {
                // dst = (src is type_idx)
                let (dst, src, imm12) = a_fields(&instr);
                let _type_idx = TypeIndex::from_raw(imm12 as u32);
                // TODO: runtime type check against type pool
                let t = task!();
                let _val = t.registers.get(src);
                t.registers.set(dst, TaggedValue::TRUE); // placeholder
                DispatchResult::Continue
            }
            Opcode::TypeCast | Opcode::TypeCastSafe => {
                let (dst, src, _imm12) = a_fields(&instr);
                let t = task!();
                let val = t.registers.get(src);
                // TODO: actual type cast with check
                t.registers.set(dst, val);
                DispatchResult::Continue
            }

            // ── Memory access (A-type) ────────────────────────────
            Opcode::LoadField => {
                let (dst, obj_reg, imm12) = a_fields(&instr);
                let field_idx = imm12 as usize;
                let t = task!();
                let obj = t.registers.get(obj_reg);
                if let Some(ptr) = obj.as_heap_ptr() {
                    let raw = unsafe { *((ptr as *const u8).add(field_idx * 8) as *const u64) };
                    t.registers.set(dst, TaggedValue::from_raw(raw));
                } else {
                    t.registers.set(dst, TaggedValue::UNIT);
                }
                DispatchResult::Continue
            }
            Opcode::StoreField => {
                // store_field(obj, field_idx, val) encodes as a_type(StoreField, val, obj, field_idx)
                let (val_reg, obj_reg, imm12) = a_fields(&instr);
                let field_idx = imm12 as usize;
                let t = task!();
                let obj = t.registers.get(obj_reg);
                let val = t.registers.get(val_reg);
                if let Some(ptr) = obj.as_heap_ptr() {
                    unsafe {
                        *((ptr as *mut u8).add(field_idx * 8) as *mut u64) = val.raw();
                    }
                }
                DispatchResult::Continue
            }
            Opcode::LoadIndex => {
                let (dst, _obj, _imm12) = a_fields(&instr);
                task!().registers.set(dst, TaggedValue::UNIT);
                DispatchResult::Continue
            }
            Opcode::StoreIndex => DispatchResult::Continue,
            Opcode::LoadGlobal | Opcode::LoadGlobalWide => {
                let (dst, base, imm12) = a_fields(&instr);
                let gidx = if instr.opcode == Opcode::LoadGlobalWide {
                    Instruction::wide_const_index(base, imm12)
                } else {
                    imm12 as u32
                };
                let val = self.globals.get(gidx);
                task!().registers.set(dst, val);
                DispatchResult::Continue
            }
            Opcode::StoreGlobal | Opcode::StoreGlobalWide => {
                let (val_reg, base, imm12) = a_fields(&instr);
                let gidx = if instr.opcode == Opcode::StoreGlobalWide {
                    Instruction::wide_const_index(base, imm12)
                } else {
                    // Narrow: value in base (src), index in imm12; val_reg unused.
                    // Keep compatibility with store_global encoding: a_type(dst=0, base=src, imm=gidx)
                    // Prefer value from base for narrow StoreGlobal.
                    imm12 as u32
                };
                let val = if instr.opcode == Opcode::StoreGlobalWide {
                    task!().registers.get(val_reg)
                } else {
                    task!().registers.get(base)
                };
                self.globals.set(gidx, val);
                DispatchResult::Continue
            }
            Opcode::LoadCapture => {
                let (dst, _, imm12) = a_fields(&instr);
                let capture_idx = imm12 as u32;
                // Find the closure env from the most recent closure call frame
                let t = task!();
                let env = t
                    .call_stack
                    .iter()
                    .rev()
                    .find_map(|f| f.closure_env)
                    .unwrap_or(TaggedValue::UNIT);
                if let Some(payload) = env.as_heap_ptr() {
                    let val = unsafe { ClosureEnv::get_capture(payload, capture_idx) };
                    t.registers.set(dst, val);
                } else {
                    t.registers.set(dst, TaggedValue::UNIT);
                }
                DispatchResult::Continue
            }

            // ── Object creation (A-type) ──────────────────────────
            Opcode::NewObject => {
                let (dst, _, imm12) = a_fields(&instr);
                let type_idx = TypeIndex::from_raw(imm12 as u32);
                // Allocate a heap object with space for fields.
                // For now, allocate 8 fields worth of space (64 bytes).
                match self.heap.alloc_object(type_idx, 8) {
                    Some(ptr) => {
                        let val = unsafe { TaggedValue::from_heap_ptr(ptr.as_ptr()) };
                        task!().registers.set(dst, val);
                    }
                    None => {
                        return DispatchResult::Error(VmError::OutOfMemory);
                    }
                }
                DispatchResult::Continue
            }
            Opcode::NewList => {
                // TODO: allocate list
                let (dst, _, _) = a_fields(&instr);
                task!().registers.set(dst, TaggedValue::UNIT);
                DispatchResult::Continue
            }
            Opcode::NewMap => {
                let (dst, _, _) = a_fields(&instr);
                task!().registers.set(dst, TaggedValue::UNIT);
                DispatchResult::Continue
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
                let const_idx = imm12 as u32;
                let func_id = match self.constants.get(const_idx).as_u64() {
                    Some(id) => FuncId(id as u32),
                    None => {
                        // Also try as i64
                        match self.constants.get(const_idx).as_i64() {
                            Some(id) => FuncId(id as u32),
                            None => return DispatchResult::Error(VmError::TypeError),
                        }
                    }
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
                let _ = arg_count;
                self.push_call_frame(task_id, FuncId(func_id_raw), None)
            }
            Opcode::CallFar => {
                let payload = c_payload(&instr);
                let (arg_count, const_idx) = Instruction::c_arg_count_func_id(payload);
                let _ = arg_count;
                let tv = self.constants.get(const_idx);
                let func_id_raw = tv
                    .as_u64()
                    .or_else(|| tv.as_i64().map(|v| v as u64))
                    .unwrap_or(0);
                self.push_call_frame(task_id, FuncId(func_id_raw as u32), None)
            }
            Opcode::CallIndirect => {
                let payload = c_payload(&instr);
                let (arg_count, closure_reg) = Instruction::c_call_indirect(payload);
                let closure_val = task!().registers.get(closure_reg);

                let payload_ptr = match closure_val.as_heap_ptr() {
                    Some(p) => p,
                    None => return DispatchResult::Error(VmError::TypeError),
                };

                let func_id = unsafe { ClosureEnv::func_id(payload_ptr) };
                let capture_count = unsafe { ClosureEnv::capture_count(payload_ptr) };

                let t = task!();
                let mut explicit_args = Vec::with_capacity(arg_count as usize);
                for i in 0..arg_count {
                    explicit_args.push(t.registers.get(Reg(i)));
                }
                for i in 0..capture_count {
                    let cap = unsafe { ClosureEnv::get_capture(payload_ptr, i) };
                    t.registers.set(Reg(i as u8), cap);
                }
                for (i, arg) in explicit_args.into_iter().enumerate() {
                    t.registers.set(Reg((capture_count as u8) + (i as u8)), arg);
                }

                self.push_call_frame(task_id, func_id, Some(closure_val))
            }
            Opcode::CallMethod => {
                let payload = c_payload(&instr);
                let (arg_count, recv_reg, method_id) = Instruction::c_call_method(payload);
                self.exec_call_method(task_id, recv_reg, StrId::from_raw(method_id), arg_count)
            }
            Opcode::CallMethodFar => {
                let payload = c_payload(&instr);
                let (arg_count, recv_reg, const_idx) = Instruction::c_call_method(payload);
                let tv = self.constants.get(const_idx);
                let method_id = tv
                    .as_u64()
                    .or_else(|| tv.as_i64().map(|v| v as u64))
                    .unwrap_or(0) as u32;
                self.exec_call_method(task_id, recv_reg, StrId::from_raw(method_id), arg_count)
            }
            Opcode::CallWasm => {
                // TODO: WASM FFI call
                DispatchResult::Continue
            }
            Opcode::TailCall => {
                let payload = c_payload(&instr);
                let (_arg_count, func_id_raw) = Instruction::c_arg_count_func_id(payload);
                let t = task!();
                t.current_func = FuncId(func_id_raw);
                t.pc = 0;
                DispatchResult::Continue
            }
            Opcode::CallIntrinsic => {
                let payload = c_payload(&instr);
                let (arg_count, intrinsic_id) = Instruction::c_arg_count_func_id(payload);
                let _ = arg_count;
                match IntrinsicFn::from_u16(intrinsic_id as u16) {
                    Some(ifn) => self.dispatch_intrinsic(task_id, ifn),
                    None => DispatchResult::Error(VmError::InvalidInstruction(instr.encode())),
                }
            }
            Opcode::Return => {
                let src_reg = Instruction::c_return_reg(c_payload(&instr));
                let val = task!().registers.get(src_reg);
                DispatchResult::Return(val)
            }
            Opcode::ReturnUnit => DispatchResult::Return(TaggedValue::UNIT),

            // ── Effects (E-type) ──────────────────────────────────
            Opcode::EffectCall => {
                // TODO: static effect call via evidence
                DispatchResult::Continue
            }
            Opcode::EffectCallDyn => {
                // TODO: dynamic effect dispatch
                DispatchResult::Continue
            }
            Opcode::PushHandler => {
                // payload: effect_type(11) | handler_func(11)
                let payload = e_payload(&instr);
                let effect_type = TypeIndex::from_raw((payload >> 11) & 0x7FF);
                let handler_func = FuncId(payload & 0x7FF);
                let t = task!();
                t.handler_stack.push(EffectHandler {
                    effect_type,
                    handler_func,
                    is_async: false,
                });
                DispatchResult::Continue
            }
            Opcode::PushHandlerWide => {
                let payload = e_payload(&instr);
                let const_idx = payload & 0x3FFFFF;
                let tv = self.constants.get(const_idx);
                let packed = tv.as_u64().unwrap_or(0);
                let effect_type = TypeIndex::from_raw((packed >> 32) as u32);
                let handler_func = FuncId(packed as u32);
                let t = task!();
                t.handler_stack.push(EffectHandler {
                    effect_type,
                    handler_func,
                    is_async: false,
                });
                DispatchResult::Continue
            }
            Opcode::PopHandler => {
                task!().handler_stack.pop();
                DispatchResult::Continue
            }
            Opcode::Shift => {
                // TODO: multi-prompt shift (capture continuation)
                DispatchResult::Suspend
            }
            Opcode::Reset => {
                // TODO: multi-prompt reset (delimit continuation)
                DispatchResult::Continue
            }
            Opcode::Resume => {
                // TODO: resume captured continuation
                DispatchResult::Continue
            }

            // ── System (E-type) ───────────────────────────────────
            Opcode::Safepoint => {
                if GC_FLAGS.should_yield() {
                    return DispatchResult::Yield;
                }
                DispatchResult::Continue
            }
            Opcode::DebugBreak => {
                // No-op in normal execution.
                DispatchResult::Continue
            }
            Opcode::Nop => DispatchResult::Continue,
        }
    }

    /// Save callee-saved regs and transfer control to `target`.
    fn push_call_frame(
        &mut self,
        task_id: TaskId,
        target: FuncId,
        closure_env: Option<TaggedValue>,
    ) -> DispatchResult {
        let t = self.scheduler.get_task_mut(task_id).unwrap();
        let mut saved = Vec::with_capacity(Reg::CALLEE_SAVED.len());
        for &r in Reg::CALLEE_SAVED {
            saved.push((Reg(r), t.registers.get(Reg(r))));
        }
        let frame = CallFrame {
            return_pc: t.pc,
            func_id: t.current_func,
            saved_regs: saved,
            evidence: Vec::new(),
            closure_env,
        };
        t.call_stack.push(frame);
        t.current_func = target;
        t.pc = 0;
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
            .scheduler
            .get_task_mut(task_id)
            .unwrap()
            .registers
            .get(recv_reg);
        let recv_type = value_type_index(receiver);

        if let Some(slot) = self.type_pool.find_method(recv_type, method_str_id) {
            if slot.func_id == type_pool::DERIVE_FUNC_ID {
                return self.dispatch_derived_method(
                    task_id,
                    receiver,
                    recv_type,
                    method_str_id,
                    arg_count,
                );
            }

            let target_func_id = FuncId(slot.func_id);
            let t = self.scheduler.get_task_mut(task_id).unwrap();
            let mut explicit_args = Vec::with_capacity(arg_count as usize);
            for i in 0..arg_count {
                explicit_args.push(t.registers.get(Reg(i)));
            }
            t.registers.set(Reg(0), receiver);
            for (i, arg) in explicit_args.into_iter().enumerate() {
                t.registers.set(Reg((i + 1) as u8), arg);
            }
            self.push_call_frame(task_id, target_func_id, None)
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
        let payload_words = ClosureEnv::payload_words(capture_count);
        let closure_type = Intrinsic::Closure.type_index();

        match self.heap.alloc_object(closure_type, payload_words) {
            Some(ptr) => {
                let t = self.scheduler.get_task_mut(task_id).unwrap();
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

    /// Dispatch a compiler-derived (synthesized) method.
    /// For `derive Eq for Point`, the method `eq` does field-wise comparison.
    fn dispatch_derived_method(
        &mut self,
        task_id: TaskId,
        receiver: TaggedValue,
        recv_type: TypeIndex,
        method_name: StrId,
        arg_count: u8,
    ) -> DispatchResult {
        let t = self.scheduler.get_task_mut(task_id).unwrap();
        let method_str = str_interner::get(method_name);

        match method_str.as_str() {
            "eq" => {
                // Derived eq: compare all fields of a struct.
                // arg r0 = other (first explicit arg)
                let other = if arg_count >= 1 {
                    t.registers.get(Reg(0))
                } else {
                    TaggedValue::UNIT
                };

                let result = self.derived_struct_eq(receiver, other, recv_type);
                let t = self.scheduler.get_task_mut(task_id).unwrap();
                t.registers.set(Reg(0), TaggedValue::from_bool(result));
                DispatchResult::Continue
            }
            "cmp" => {
                // Derived cmp: lexicographic field comparison returning -1/0/1.
                let other = if arg_count >= 1 {
                    t.registers.get(Reg(0))
                } else {
                    TaggedValue::UNIT
                };

                let result = self.derived_struct_cmp(receiver, other, recv_type);
                let t = self.scheduler.get_task_mut(task_id).unwrap();
                t.registers.set(Reg(0), TaggedValue::from_i64(result));
                DispatchResult::Continue
            }
            "hash" => {
                // Derived hash: combine field hashes. Stub: return 0 for now.
                t.registers.set(Reg(0), TaggedValue::from_i64(0));
                DispatchResult::Continue
            }
            "to_string" => {
                // Derived to_string (Display): produce "TypeName { field: val, ... }".
                drop(t);
                let s = self.derived_struct_to_string(receiver, recv_type);
                let result = self.alloc_string(&s);
                let t = self.scheduler.get_task_mut(task_id).unwrap();
                t.registers.set(Reg(0), result);
                DispatchResult::Continue
            }
            _ => DispatchResult::Error(VmError::MethodNotFound),
        }
    }

    /// Field-wise equality comparison for derived Eq.
    fn derived_struct_eq(&self, a: TaggedValue, b: TaggedValue, type_idx: TypeIndex) -> bool {
        use type_pool::TypeKind;

        // For immediate (non-heap) values, compare raw bits.
        if a.tag() != 0 || b.tag() != 0 {
            return a.raw() == b.raw();
        }

        let info = self.type_pool.get(type_idx);
        if let TypeKind::Struct { fields, .. } = &info.kind {
            let a_ptr = match a.as_heap_ptr() {
                Some(p) => p,
                None => return b.as_heap_ptr().is_none(),
            };
            let b_ptr = match b.as_heap_ptr() {
                Some(p) => p,
                None => return false,
            };
            for field in fields {
                let offset = field.offset as usize;
                let a_val = unsafe { *(a_ptr.add(offset) as *const u64) };
                let b_val = unsafe { *(b_ptr.add(offset) as *const u64) };
                if a_val != b_val {
                    return false;
                }
            }
            true
        } else {
            a.raw() == b.raw()
        }
    }

    /// Field-wise comparison for derived Ord. Returns -1, 0, or 1.
    fn derived_struct_cmp(&self, a: TaggedValue, b: TaggedValue, type_idx: TypeIndex) -> i64 {
        use type_pool::TypeKind;

        // For immediate values, compare as i64.
        if a.tag() != 0 || b.tag() != 0 {
            let av = a.raw() as i64;
            let bv = b.raw() as i64;
            return if av < bv {
                -1
            } else if av > bv {
                1
            } else {
                0
            };
        }

        let info = self.type_pool.get(type_idx);
        if let TypeKind::Struct { fields, .. } = &info.kind {
            let a_ptr = match a.as_heap_ptr() {
                Some(p) => p,
                None => return if b.as_heap_ptr().is_some() { -1 } else { 0 },
            };
            let b_ptr = match b.as_heap_ptr() {
                Some(p) => p,
                None => return 1,
            };
            for field in fields {
                let offset = field.offset as usize;
                let a_val = unsafe { *(a_ptr.add(offset) as *const i64) };
                let b_val = unsafe { *(b_ptr.add(offset) as *const i64) };
                if a_val < b_val {
                    return -1;
                }
                if a_val > b_val {
                    return 1;
                }
            }
            0
        } else {
            let av = a.raw() as i64;
            let bv = b.raw() as i64;
            if av < bv {
                -1
            } else if av > bv {
                1
            } else {
                0
            }
        }
    }

    /// Produce a human-readable string for a struct value (derived Display).
    /// Format: "TypeName { field1: val1, field2: val2 }"
    fn derived_struct_to_string(&self, val: TaggedValue, type_idx: TypeIndex) -> String {
        use type_pool::TypeKind;
        let info = self.type_pool.get(type_idx);
        if let TypeKind::Struct { name, fields, .. } = &info.kind {
            let type_name = str_interner::get(*name);
            let ptr = match val.as_heap_ptr() {
                Some(p) => p,
                None => return format!("{} {{ <null> }}", type_name),
            };
            let mut parts: Vec<String> = Vec::with_capacity(fields.len());
            for field in fields {
                let offset = field.offset as usize;
                let fval = TaggedValue::from_raw(unsafe { *(ptr.add(offset) as *const u64) });
                let field_name = str_interner::get(field.name);
                parts.push(format!("{}: {}", field_name, format_tagged_value(fval)));
            }
            format!("{} {{ {} }}", type_name, parts.join(", "))
        } else {
            format_tagged_value(val)
        }
    }

    /// Dispatch an intrinsic function call.
    /// Arguments are in r0..rN by calling convention. Result goes into r0.
    fn dispatch_intrinsic(&mut self, task_id: TaskId, ifn: IntrinsicFn) -> DispatchResult {
        let t = self.scheduler.get_task_mut(task_id).unwrap();
        match ifn {
            IntrinsicFn::Print => {
                let val = t.registers.get(Reg(0));
                let formatted = format_tagged_value(val);
                use std::io::Write;
                let _ = write!(std::io::stdout(), "{}", formatted);
                let _ = std::io::stdout().flush();
                t.registers.set(Reg(0), TaggedValue::UNIT);
                DispatchResult::Continue
            }
            IntrinsicFn::PrintLn => {
                let val = t.registers.get(Reg(0));
                println!("{}", format_tagged_value(val));
                t.registers.set(Reg(0), TaggedValue::UNIT);
                DispatchResult::Continue
            }
            IntrinsicFn::TypeOf => {
                // TODO: return actual type descriptor
                t.registers.set(Reg(0), TaggedValue::UNIT);
                DispatchResult::Continue
            }
            IntrinsicFn::ToI64 => {
                let val = t.registers.get(Reg(0));
                let result = if let Some(v) = val.as_i64() {
                    TaggedValue::from_i64(v)
                } else if let Some(v) = val.as_u64() {
                    TaggedValue::from_i64(v as i64)
                } else if let Some(v) = val.as_f64() {
                    TaggedValue::from_i64(v as i64)
                } else {
                    TaggedValue::from_i64(0)
                };
                t.registers.set(Reg(0), result);
                DispatchResult::Continue
            }
            IntrinsicFn::ToF64 => {
                let val = t.registers.get(Reg(0));
                let result = if let Some(v) = val.as_f64() {
                    TaggedValue::from_f64(v)
                } else if let Some(v) = val.as_i64() {
                    TaggedValue::from_f64(v as f64)
                } else if let Some(v) = val.as_u64() {
                    TaggedValue::from_f64(v as f64)
                } else {
                    TaggedValue::from_f64(0.0)
                };
                t.registers.set(Reg(0), result);
                DispatchResult::Continue
            }
            IntrinsicFn::ToString => {
                let val = t.registers.get(Reg(0));
                drop(t);
                let s = format_tagged_value(val);
                let result = self.alloc_string(&s);
                self.scheduler
                    .get_task_mut(task_id)
                    .unwrap()
                    .registers
                    .set(Reg(0), result);
                DispatchResult::Continue
            }
            IntrinsicFn::Abs => {
                let val = t.registers.get(Reg(0));
                let result = if let Some(v) = val.as_i64() {
                    TaggedValue::from_i64(v.wrapping_abs())
                } else if let Some(v) = val.as_f64() {
                    TaggedValue::from_f64(v.abs())
                } else {
                    TaggedValue::UNIT
                };
                t.registers.set(Reg(0), result);
                DispatchResult::Continue
            }
            IntrinsicFn::Sin => {
                let val = t.registers.get(Reg(0));
                let r = val.as_f64().map(|v| v.sin()).unwrap_or(0.0);
                t.registers.set(Reg(0), TaggedValue::from_f64(r));
                DispatchResult::Continue
            }
            IntrinsicFn::Cos => {
                let val = t.registers.get(Reg(0));
                let r = val.as_f64().map(|v| v.cos()).unwrap_or(0.0);
                t.registers.set(Reg(0), TaggedValue::from_f64(r));
                DispatchResult::Continue
            }
            IntrinsicFn::Sqrt => {
                let val = t.registers.get(Reg(0));
                let r = val.as_f64().map(|v| v.sqrt()).unwrap_or(0.0);
                t.registers.set(Reg(0), TaggedValue::from_f64(r));
                DispatchResult::Continue
            }
            IntrinsicFn::Floor => {
                let val = t.registers.get(Reg(0));
                let r = val.as_f64().map(|v| v.floor()).unwrap_or(0.0);
                t.registers.set(Reg(0), TaggedValue::from_f64(r));
                DispatchResult::Continue
            }
            IntrinsicFn::Ceil => {
                let val = t.registers.get(Reg(0));
                let r = val.as_f64().map(|v| v.ceil()).unwrap_or(0.0);
                t.registers.set(Reg(0), TaggedValue::from_f64(r));
                DispatchResult::Continue
            }
            IntrinsicFn::Round => {
                let val = t.registers.get(Reg(0));
                let r = val.as_f64().map(|v| v.round()).unwrap_or(0.0);
                t.registers.set(Reg(0), TaggedValue::from_f64(r));
                DispatchResult::Continue
            }
            IntrinsicFn::Pow => {
                let base = t.registers.get(Reg(0));
                let exp = t.registers.get(Reg(1));
                let r = match (base.as_f64(), exp.as_f64()) {
                    (Some(b), Some(e)) => TaggedValue::from_f64(b.powf(e)),
                    _ => match (base.as_i64(), exp.as_i64()) {
                        (Some(b), Some(e)) => TaggedValue::from_i64(b.wrapping_pow(e as u32)),
                        _ => TaggedValue::UNIT,
                    },
                };
                t.registers.set(Reg(0), r);
                DispatchResult::Continue
            }
            IntrinsicFn::Log => {
                let val = t.registers.get(Reg(0));
                let r = val.as_f64().map(|v| v.ln()).unwrap_or(0.0);
                t.registers.set(Reg(0), TaggedValue::from_f64(r));
                DispatchResult::Continue
            }
            IntrinsicFn::StrLen => {
                let val = t.registers.get(Reg(0));
                let len: i64 = if let Some(ptr) = val.as_heap_ptr() {
                    (unsafe { *(ptr as *const u64) }) as i64
                } else {
                    0
                };
                t.registers.set(Reg(0), TaggedValue::from_i64(len));
                DispatchResult::Continue
            }
            IntrinsicFn::StrConcat => {
                let a = t.registers.get(Reg(0));
                let b = t.registers.get(Reg(1));
                let mut bytes: Vec<u8> = Vec::new();
                if let Some(ptr) = a.as_heap_ptr() {
                    let len = (unsafe { *(ptr as *const u64) }) as usize;
                    let data =
                        unsafe { std::slice::from_raw_parts((ptr as *const u8).add(8), len) };
                    bytes.extend_from_slice(data);
                }
                if let Some(ptr) = b.as_heap_ptr() {
                    let len = (unsafe { *(ptr as *const u64) }) as usize;
                    let data =
                        unsafe { std::slice::from_raw_parts((ptr as *const u8).add(8), len) };
                    bytes.extend_from_slice(data);
                }
                drop(t);
                let s = String::from_utf8_lossy(&bytes).into_owned();
                let result = self.alloc_string(&s);
                self.scheduler
                    .get_task_mut(task_id)
                    .unwrap()
                    .registers
                    .set(Reg(0), result);
                DispatchResult::Continue
            }
            IntrinsicFn::Exit => {
                let code = t.registers.get(Reg(0)).as_i64().unwrap_or(0);
                std::process::exit(code as i32);
            }
            IntrinsicFn::Panic => {
                let msg = format_tagged_value(t.registers.get(Reg(0)));
                DispatchResult::Error(VmError::Panic(msg))
            }
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
    if let Some(v) = val.as_i64() {
        return v != 0;
    }
    // Heap objects and everything else are truthy.
    true
}

// ---------------------------------------------------------------------------
// Arithmetic helpers
// ---------------------------------------------------------------------------

fn arith_add(a: TaggedValue, b: TaggedValue) -> TaggedValue {
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        return TaggedValue::from_i64(x.wrapping_add(y));
    }
    if let (Some(x), Some(y)) = (a.as_u64(), b.as_u64()) {
        return TaggedValue::from_u64(x.wrapping_add(y));
    }
    TaggedValue::UNIT // type error fallback
}

fn arith_sub(a: TaggedValue, b: TaggedValue) -> TaggedValue {
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        return TaggedValue::from_i64(x.wrapping_sub(y));
    }
    if let (Some(x), Some(y)) = (a.as_u64(), b.as_u64()) {
        return TaggedValue::from_u64(x.wrapping_sub(y));
    }
    TaggedValue::UNIT
}

fn arith_mul(a: TaggedValue, b: TaggedValue) -> TaggedValue {
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        return TaggedValue::from_i64(x.wrapping_mul(y));
    }
    if let (Some(x), Some(y)) = (a.as_u64(), b.as_u64()) {
        return TaggedValue::from_u64(x.wrapping_mul(y));
    }
    TaggedValue::UNIT
}

fn arith_div(a: TaggedValue, b: TaggedValue) -> TaggedValue {
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        if y == 0 {
            return TaggedValue::UNIT; // division by zero
        }
        return TaggedValue::from_i64(x.wrapping_div(y));
    }
    TaggedValue::UNIT
}

fn arith_mod(a: TaggedValue, b: TaggedValue) -> TaggedValue {
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        if y == 0 {
            return TaggedValue::UNIT;
        }
        return TaggedValue::from_i64(x.wrapping_rem(y));
    }
    TaggedValue::UNIT
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

/// Outcome of a dispatch of a single instruction.
enum DispatchResult {
    Continue,
    Return(TaggedValue),
    Suspend,
    Yield,
    Error(VmError),
}

/// Outcome of executing a task.
enum ExecOutcome {
    Finished,
    Suspended,
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
    OutOfMemory,
    TypeError,
    MethodNotFound,
    Panic(String),
}

// ---------------------------------------------------------------------------
// Value formatting for intrinsics
// ---------------------------------------------------------------------------

/// Format a TaggedValue for display (used by print/println intrinsics).
fn format_tagged_value(val: TaggedValue) -> String {
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
        // Try to read as a heap string: layout is [len:u64][...UTF-8 bytes...]
        if let Some(ptr) = val.as_heap_ptr() {
            // Safety: ptr is a valid payload pointer from our allocator.
            let len = unsafe { *(ptr as *const u64) } as usize;
            // Sanity check: len must be small enough to read safely.
            if len <= 4096 {
                let bytes = unsafe { std::slice::from_raw_parts((ptr as *const u8).add(8), len) };
                if let Ok(s) = std::str::from_utf8(bytes) {
                    return s.to_string();
                }
            }
        }
        format!("<object@{:#x}>", val.raw())
    } else {
        format!("<value:{:#018x}>", val.raw())
    }
}

// ---------------------------------------------------------------------------
// GC root scanning — enumerates all roots in the VM for MMTk
// ---------------------------------------------------------------------------

/// Root scan callback installed into `gc::register_root_scanner`.
///
/// Walks all root sources in the Vm:
///   1. Registers (per task)
///   2. Call stack saved_regs (per frame per task)
///   3. Global variable table
///   4. Constant pool
///
/// Each TaggedValue that might hold a heap pointer is presented to the
/// visitor as a `NessaSlot` pointing at the memory location of the value.
fn scan_vm_roots(visitor: &mut dyn FnMut(NessaSlot)) {
    let vm_ptr = gc::get_vm_ptr();
    if vm_ptr.is_null() {
        return;
    }
    let vm: &Vm = unsafe { &*(vm_ptr as *const Vm) };

    // Helper: visit a TaggedValue in-place by address.
    let mut visit = |tv_ptr: *const TaggedValue| {
        // TaggedValue is a transparent u64 wrapper — its address doubles as a
        // pointer to the raw u64 that NessaSlot will load/store.
        let slot = unsafe { NessaSlot::from_raw_ptr(tv_ptr as *const u64) };
        visitor(slot);
    };

    // 1. Global table
    for tv in vm.globals.values() {
        visit(tv as *const TaggedValue);
    }

    // 2. Constant pool
    for tv in vm.constants.values() {
        visit(tv as *const TaggedValue);
    }

    // 3. Per-task: registers + call stack saved_regs
    for task in vm.scheduler.all_tasks() {
        // Current registers
        for reg in &task.registers.regs {
            visit(reg as *const TaggedValue);
        }

        // Saved registers in each call frame
        for frame in &task.call_stack {
            for (_, saved_val) in &frame.saved_regs {
                visit(saved_val as *const TaggedValue);
            }
            // Closure environment reference
            if let Some(ref env) = frame.closure_env {
                visit(env as *const TaggedValue);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Value type introspection
// ---------------------------------------------------------------------------

/// Determine the TypeIndex of a TaggedValue at runtime.
/// For heap objects, reads the ObjectHeader. For immediates, uses the tag.
fn value_type_index(val: TaggedValue) -> TypeIndex {
    if val.is_heap() {
        if let Some(ptr) = val.as_heap_ptr() {
            let header = unsafe { ObjectHeader::from_payload_ptr(ptr) };
            header.type_index
        } else {
            TypeIndex::INVALID
        }
    } else if val.as_i64().is_some() {
        Intrinsic::I64.type_index()
    } else if val.as_bool().is_some() {
        Intrinsic::Bool.type_index()
    } else if val.as_char().is_some() {
        Intrinsic::Char.type_index()
    } else if val.is_unit() {
        Intrinsic::Unit.type_index()
    } else if val.is_null() {
        Intrinsic::Unit.type_index()
    } else {
        TypeIndex::INVALID
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_vm() -> Vm {
        Vm::new(
            TypePool::with_intrinsics(),
            &GcConfig::default(),
            Arc::new(StackPool::new()),
        )
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
            func_id: FuncId(0),
            instructions: code,
            register_count: 3,
            param_count: 0,
            is_closure: false,
        };
        vm.add_function(func);
        let task_id = vm.spawn_root(FuncId(0));
        let result = vm.run();
        assert!(matches!(result, VmResult::Finished));
        let val = vm
            .scheduler
            .get_task(task_id)
            .unwrap()
            .registers
            .get(Reg(0));
        assert_eq!(val.as_i64(), Some(42));
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
            func_id: FuncId(0),
            instructions: code,
            register_count: 2,
            param_count: 0,
            is_closure: false,
        };
        vm.add_function(func);
        let task_id = vm.spawn_root(FuncId(0));
        vm.run();
        // r1 = 42, returned into r0
        let val = vm
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
        // func 0 (main): r0 = 5, call func 1, return r0
        // func 1: r0 = r0 * 2, return r0  (BUT calling convention puts return in r0)
        //
        // Simplified: main loads r0=5, calls func1 which loads r1=10 and returns r1.
        let main_code = encode_instrs(&[
            Instruction::load_imm(Reg(0), 5), // r0 = 5
            Instruction::call(1, 1),          // call func 1 with 1 arg
            Instruction::ret(Reg(0)),         // return r0 (which is return val from func 1)
        ]);
        let func1_code = encode_instrs(&[
            Instruction::load_imm(Reg(1), 10), // r1 = 10
            Instruction::ret(Reg(1)),          // return r1
        ]);

        vm.add_function(FunctionCode {
            func_id: FuncId(0),
            instructions: main_code,
            register_count: 2,
            param_count: 0,
            is_closure: false,
        });
        vm.add_function(FunctionCode {
            func_id: FuncId(1),
            instructions: func1_code,
            register_count: 2,
            param_count: 1,
            is_closure: false,
        });

        let task_id = vm.spawn_root(FuncId(0));
        vm.run();
        let val = vm
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
            func_id: FuncId(0),
            instructions: code,
            register_count: 0,
            param_count: 0,
            is_closure: false,
        });
        let task_id = vm.spawn_root(FuncId(0));
        vm.run();
        let val = vm
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
        let gidx = vm.globals.alloc(TaggedValue::UNIT);
        let code = encode_instrs(&[
            Instruction::load_imm(Reg(0), 99),
            Instruction::a_type(
                Opcode::StoreGlobal,
                AddrMode::Imm,
                Reg(0),
                Reg(0),
                gidx as u16,
            ),
            Instruction::a_type(
                Opcode::LoadGlobal,
                AddrMode::Imm,
                Reg(1),
                Reg(0),
                gidx as u16,
            ),
            Instruction::ret(Reg(1)),
        ]);
        vm.add_function(FunctionCode {
            func_id: FuncId(0),
            instructions: code,
            register_count: 2,
            param_count: 0,
            is_closure: false,
        });
        let task_id = vm.spawn_root(FuncId(0));
        vm.run();
        let val = vm
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
            func_id: FuncId(0),
            instructions: code,
            register_count: 3,
            param_count: 0,
            is_closure: false,
        });
        let task_id = vm.spawn_root(FuncId(0));
        vm.run();
        let val = vm
            .scheduler
            .get_task(task_id)
            .unwrap()
            .registers
            .get(Reg(0));
        assert_eq!(val.as_bool(), Some(true));
    }
}
