//! Trampoline bridge, profiling hooks, monomorphic IC updaters, and GC-safe slow-path
//! callbacks connecting `Vm` (`interpreter`) with `JitEngine` (`jit`).

use std::ffi::c_void;
use std::sync::atomic::Ordering;

use gc::{GC_FLAGS, ObjectHeader};
use jit::{
    CompilationInput, CompileConstant, FieldStoreFastKind, JitConfig, JitEngine, JitExit,
    JitExitKind, JitHelpers, JitMode, JitRuntimeCtx, JitStats, MonomorphicIc,
};
use nsbc::{FuncId, Instruction, Opcode, Reg};
use runtime::{Number, TaggedValue, TaskId};
use type_pool::{Intrinsic, TypeKind};

use crate::{DispatchResult, Vm, VmError, a_fields, is_truthy};

/// Result returned to `Vm::execute_task` after a native JIT execution slice.
pub(crate) enum JitStepOutcome {
    Finished,
    Return { ret_pc: u32, val: TaggedValue },
    InterpretOne(u32),
    TierUp(u32),
    Yield(u32),
    SuspendRetry(u32),
    Cancelled,
    Error(VmError),
}

struct BridgeSession {
    vm: *mut Vm,
    task_id: TaskId,
    pending_error: Option<VmError>,
}

#[cfg(all(target_arch = "x86_64", unix))]
static JIT_HELPERS: JitHelpers = JitHelpers {
    alloc_slots: helper_alloc_slots,
    safepoint: helper_safepoint,
    numeric_slow: helper_numeric_slow,
    cmp_slow: helper_cmp_slow,
    is_truthy: helper_is_truthy,
    load_const_slow: helper_load_const_slow,
    type_op: helper_type_op,
    load_field_ic: helper_load_field_ic,
    store_field_ic: helper_store_field_ic,
    store_boxed_f64: helper_store_boxed_f64,
    step_inline_op: helper_step_inline_op,
};

#[cfg(not(all(target_arch = "x86_64", unix)))]
static JIT_HELPERS: JitHelpers = JitHelpers {
    alloc_slots: helper_alloc_slots,
    safepoint: helper_safepoint,
    numeric_slow: helper_numeric_slow,
    cmp_slow: helper_cmp_slow,
    is_truthy: helper_is_truthy,
    load_const_slow: helper_load_const_slow,
    type_op: helper_type_op,
    load_field_ic: helper_load_field_ic,
    store_field_ic: helper_store_field_ic,
    store_boxed_f64: helper_store_boxed_f64,
    step_inline_op: helper_step_inline_op,
};

impl Vm {
    /// Configure the per-VM tiered JIT mode and code-cache cap.
    pub fn set_jit_config(&mut self, config: JitConfig) {
        let _operation = self.operation();
        self.state.jit.set_config(config);
    }

    /// Return the active [`JitConfig`] for this VM.
    pub fn jit_config(&self) -> JitConfig {
        self.state.jit.config()
    }

    /// Set the active [`JitMode`] while preserving other [`JitConfig`] parameters.
    pub fn set_jit_mode(&mut self, mode: JitMode) {
        let mut cfg = self.state.jit.config();
        cfg.mode = mode;
        self.set_jit_config(cfg);
    }

    /// Whether JIT compilation and execution are enabled on this VM and host target.
    pub fn jit_enabled(&self) -> bool {
        self.state.jit.is_enabled()
    }

    /// Return current JIT telemetry counters (`compiled_functions`, `code_bytes`, `deopt_count`).
    pub fn jit_stats(&self) -> JitStats {
        self.state.jit.stats()
    }

    #[inline]
    pub(crate) fn new_default_jit() -> JitEngine {
        JitEngine::new(JitConfig::default())
    }

    /// Record function-entry or interpreter backward-jump hotness and trigger compilation if hot.
    #[inline]
    pub(crate) fn maybe_compile_on_hotness(&mut self, func_id: FuncId, delta: u64) {
        if self.state.jit.record_hotness(func_id, delta) {
            self.compile_jit_function(func_id, false);
        }
    }

    /// Compile `func_id` into native code (Tier-1 or Tier-2), degrading gracefully on failure.
    pub(crate) fn compile_jit_function(&mut self, func_id: FuncId, request_tier2: bool) {
        if !self.state.jit.is_enabled() {
            return;
        }
        if self.state.bytecode.try_get_function(func_id).is_none() {
            return;
        }

        let constants = self.classify_constants_for_jit();
        let (hotness_ptr, global_deopt_ptr, func_deopt_ptr) = self.state.jit.counter_ptrs(func_id);

        // Borrow immutable compilation inputs from `self.state` while mutating `self.state.jit`.
        let state = &mut *self.state;
        let instructions = &state.bytecode.get_function(func_id).instructions;
        let type_pool = &state.type_pool;
        let scopes = &state.method_call_scopes;
        let scopes_required = !type_pool.scopes().is_empty();
        let has_scope =
            |pc: usize| -> bool { !scopes_required || scopes.contains_key(&(func_id, pc)) };

        let input = CompilationInput {
            func_id,
            instructions,
            constants: &constants,
            type_pool,
            has_scope: &has_scope,
            helpers: &JIT_HELPERS,
            hotness_ptr,
            global_deopt_ptr,
            func_deopt_ptr,
        };
        state.jit.compile_function(&input, request_tier2);
    }

    fn classify_constants_for_jit(&self) -> Vec<CompileConstant> {
        let values = self.roots.constants.values();
        let mut out = Vec::with_capacity(values.len());
        for &val in values {
            if val.as_enum().is_some() {
                if self.enum_layout(val).is_ok() {
                    out.push(CompileConstant::ImmediateValidated {
                        raw: val.raw(),
                        i64_val: None,
                        f64_bits: None,
                    });
                } else {
                    out.push(CompileConstant::Slow);
                }
            } else if !val.is_heap() {
                let raw = val.raw();
                let i64_val = if (raw & 0x7f) == 0x01 {
                    val.as_i64()
                } else {
                    None
                };
                let f64_bits = if (raw & 0x7f) == 0x11 {
                    val.as_f64().map(f64::to_bits)
                } else {
                    None
                };
                out.push(CompileConstant::ImmediateValidated {
                    raw,
                    i64_val,
                    f64_bits,
                });
            } else {
                let f64_bits = val.as_heap_ptr().and_then(|ptr| {
                    // SAFETY: `val` is rooted in `ConstantPool` under an active VM operation.
                    let header = unsafe { ObjectHeader::from_payload_ptr(ptr) };
                    if header.is_ordinary()
                        && header.type_index == Intrinsic::F64.type_index()
                        && header.payload_words() == 1
                    {
                        // SAFETY: validated 1-word `Intrinsic::F64` box holds raw IEEE-754 bits.
                        Some(unsafe { ptr.cast::<u64>().read() })
                    } else {
                        None
                    }
                });
                out.push(CompileConstant::HeapObject { f64_bits });
            }
        }
        out
    }

    /// Enter compiled native code for `(func_id, pc)` if available.
    pub(crate) fn try_run_jit(
        &mut self,
        task_id: TaskId,
        func_id: FuncId,
        pc: u32,
    ) -> Option<JitStepOutcome> {
        let (entry_fn, entry_offset, allow_tier_up, allow_tier2_loops) =
            self.state.jit.lookup_entry(func_id, pc)?;

        let mut session = BridgeSession {
            vm: std::ptr::from_mut(self),
            task_id,
            pending_error: None,
        };

        let mut rt_ctx = JitRuntimeCtx {
            host_session: std::ptr::from_mut(&mut session).cast::<c_void>(),
            regs_ptr: std::ptr::null_mut(),
            slots_ptr: std::ptr::null_mut(),
            slots_len: 0,
            consts_ptr: std::ptr::null(),
            consts_len: 0,
            gc_requested_ptr: GC_FLAGS.gc_requested.as_ptr().cast::<u8>(),
            preempt_requested_ptr: GC_FLAGS.preempt_requested.as_ptr().cast::<u8>(),
            slice_safepoints: self.state.slice_safepoints,
            has_other_ready: 0,
            allow_tier_up: u8::from(allow_tier_up),
            allow_tier2_loops: u8::from(allow_tier2_loops),
            _pad: 0,
        };
        refresh_runtime_ctx(&mut rt_ctx, self, task_id);
        if rt_ctx.regs_ptr.is_null() {
            return Some(JitStepOutcome::Error(VmError::InvalidTask));
        }

        let mut exit = JitExit::default();
        // SAFETY: `entry_fn` points to a sealed `PROT_READ | PROT_EXEC` buffer owned by
        // `self.state.jit`, `rt_ctx` and `session` outlive the synchronous call on this stack,
        // and all helper callbacks re-borrow `session.vm` exclusively for the callback duration
        // while keeping all managed heap references rooted in `StackContext`.
        unsafe {
            entry_fn(
                std::ptr::from_mut(&mut rt_ctx).cast::<c_void>(),
                rt_ctx.regs_ptr,
                entry_offset,
                std::ptr::from_mut(&mut exit),
            );
        }

        self.state.slice_safepoints = rt_ctx.slice_safepoints;

        let outcome = match exit.kind {
            x if x == JitExitKind::Finished as u32 => JitStepOutcome::Finished,
            x if x == JitExitKind::Return as u32 => JitStepOutcome::Return {
                ret_pc: exit.pc,
                val: TaggedValue::from_raw(exit.value),
            },
            x if x == JitExitKind::InterpretAt as u32 => JitStepOutcome::InterpretOne(exit.pc),
            x if x == JitExitKind::TierUp as u32 => JitStepOutcome::TierUp(exit.pc),
            x if x == JitExitKind::Yield as u32 => JitStepOutcome::Yield(exit.pc),
            x if x == JitExitKind::SuspendRetry as u32 => JitStepOutcome::SuspendRetry(exit.pc),
            x if x == JitExitKind::Cancelled as u32 => JitStepOutcome::Cancelled,
            _ => JitStepOutcome::Error(
                session
                    .pending_error
                    .unwrap_or(VmError::InvalidInstruction(exit.kind)),
            ),
        };
        Some(outcome)
    }
}

fn refresh_runtime_ctx(rt_ctx: &mut JitRuntimeCtx, vm: &mut Vm, task_id: TaskId) {
    let has_other_ready = u8::from(vm.roots.scheduler.has_other_ready_tasks());
    let consts = vm.roots.constants.values();
    rt_ctx.consts_ptr = consts.as_ptr().cast::<u64>();
    rt_ctx.consts_len = consts.len();
    rt_ctx.has_other_ready = has_other_ready;
    if let Some(task) = vm.roots.scheduler.get_task_mut(task_id) {
        rt_ctx.regs_ptr = task.registers.regs.as_mut_ptr().cast::<u64>();
        rt_ctx.slots_len = task.local_slots.len();
        rt_ctx.slots_ptr = if rt_ctx.slots_len == 0 {
            std::ptr::null_mut()
        } else {
            task.local_slots.as_mut_ptr().cast::<u64>()
        };
    } else {
        rt_ctx.regs_ptr = std::ptr::null_mut();
        rt_ctx.slots_ptr = std::ptr::null_mut();
        rt_ctx.slots_len = 0;
    }
}

/// Borrow `(&mut JitRuntimeCtx, &mut BridgeSession, &mut Vm)` inside a synchronous JIT helper.
///
/// # Safety
/// `vm_opaque` must be the valid `*mut JitRuntimeCtx` passed by `Vm::try_run_jit` on the
/// current mutator thread, with no other active Rust reference to `Vm` or `JitRuntimeCtx`.
#[inline]
unsafe fn unpack_ctx<'a>(
    vm_opaque: *mut c_void,
) -> (&'a mut JitRuntimeCtx, &'a mut BridgeSession, &'a mut Vm) {
    // SAFETY: guaranteed by caller contract from `Vm::try_run_jit`.
    let rt_ctx = unsafe { &mut *vm_opaque.cast::<JitRuntimeCtx>() };
    // SAFETY: `host_session` points to the live `BridgeSession` frame in `Vm::try_run_jit`.
    let session = unsafe { &mut *rt_ctx.host_session.cast::<BridgeSession>() };
    // SAFETY: `session.vm` is exclusively owned by `Vm::try_run_jit` and inactive in native code.
    let vm = unsafe { &mut *session.vm };
    (rt_ctx, session, vm)
}

fn finish_dispatch(
    rt_ctx: &mut JitRuntimeCtx,
    session: &mut BridgeSession,
    vm: &mut Vm,
    res: DispatchResult,
) -> u32 {
    refresh_runtime_ctx(rt_ctx, vm, session.task_id);
    match res {
        DispatchResult::Continue => 0,
        DispatchResult::Yield => JitExitKind::Yield as u32,
        DispatchResult::SuspendRetry => JitExitKind::SuspendRetry as u32,
        DispatchResult::Cancelled => JitExitKind::Cancelled as u32,
        DispatchResult::Error(err) => {
            session.pending_error = Some(err);
            JitExitKind::Trap as u32
        }
        DispatchResult::Return(_) => {
            session.pending_error = Some(VmError::InvalidInstruction(0));
            JitExitKind::Trap as u32
        }
    }
}

macro_rules! def_jit_helper {
    (fn $name:ident ( $($arg:ident : $ty:ty),* $(,)? ) -> $ret:ty $body:block) => {
        #[cfg(all(target_arch = "x86_64", unix))]
        unsafe extern "sysv64" fn $name( $($arg : $ty),* ) -> $ret $body

        #[cfg(not(all(target_arch = "x86_64", unix)))]
        unsafe extern "C" fn $name( $($arg : $ty),* ) -> $ret $body
    };
}

def_jit_helper!(
    fn helper_alloc_slots(vm_opaque: *mut c_void, count: u32) -> u32 {
        // SAFETY: invoked synchronously from compiled JIT code with valid `JitRuntimeCtx`.
        let (rt_ctx, session, vm) = unsafe { unpack_ctx(vm_opaque) };
        if count > (1 << 17) {
            session.pending_error = Some(VmError::InvalidInstruction(
                Instruction::allocate_slots(count).encode(),
            ));
            return JitExitKind::Trap as u32;
        }
        let Some(task) = vm.roots.scheduler.get_task_mut(session.task_id) else {
            session.pending_error = Some(VmError::InvalidTask);
            return JitExitKind::Trap as u32;
        };
        let mut slots = Vec::new();
        if slots.try_reserve_exact(count as usize).is_err() {
            session.pending_error = Some(VmError::OutOfMemory);
            return JitExitKind::Trap as u32;
        }
        slots.resize(count as usize, TaggedValue::UNIT);
        task.local_slots = slots;
        refresh_runtime_ctx(rt_ctx, vm, session.task_id);
        0
    }
);

def_jit_helper!(
    fn helper_safepoint(vm_opaque: *mut c_void, resume_pc: u32) -> u32 {
        // SAFETY: invoked synchronously from compiled JIT code with all live state flushed to `StackContext`.
        let (rt_ctx, session, vm) = unsafe { unpack_ctx(vm_opaque) };
        vm.state.slice_safepoints = rt_ctx.slice_safepoints;
        if let Some(task) = vm.roots.scheduler.get_task_mut(session.task_id) {
            task.pc = resume_pc;
        }
        vm.state.heap.safe_point();
        refresh_runtime_ctx(rt_ctx, vm, session.task_id);

        let Some(task) = vm.roots.scheduler.get_task(session.task_id) else {
            session.pending_error = Some(VmError::InvalidTask);
            return JitExitKind::Trap as u32;
        };
        if task.cancelled {
            return JitExitKind::Cancelled as u32;
        }
        if GC_FLAGS.preempt_requested.load(Ordering::Relaxed) {
            return JitExitKind::Yield as u32;
        }
        vm.state.slice_safepoints = vm.state.slice_safepoints.saturating_sub(1);
        if vm.state.slice_safepoints == 0 {
            vm.state.slice_safepoints = 64;
            rt_ctx.slice_safepoints = 64;
            if vm.roots.scheduler.has_other_ready_tasks() {
                return JitExitKind::Yield as u32;
            }
        }
        rt_ctx.slice_safepoints = vm.state.slice_safepoints;
        0
    }
);

def_jit_helper!(
    fn helper_numeric_slow(vm_opaque: *mut c_void, instr_word: u32) -> u32 {
        // SAFETY: invoked synchronously from compiled JIT code with valid `JitRuntimeCtx`.
        let (rt_ctx, session, vm) = unsafe { unpack_ctx(vm_opaque) };
        let Some(instr) = Instruction::decode(instr_word) else {
            session.pending_error = Some(VmError::InvalidInstruction(instr_word));
            return JitExitKind::Trap as u32;
        };
        let res = vm.dispatch_numeric(session.task_id, instr);
        finish_dispatch(rt_ctx, session, vm, res)
    }
);

def_jit_helper!(
    fn helper_cmp_slow(vm_opaque: *mut c_void, instr_word: u32) -> u32 {
        // SAFETY: invoked synchronously from compiled JIT code with valid `JitRuntimeCtx`.
        let (rt_ctx, session, vm) = unsafe { unpack_ctx(vm_opaque) };
        let Some(instr) = Instruction::decode(instr_word) else {
            session.pending_error = Some(VmError::InvalidInstruction(instr_word));
            return JitExitKind::Trap as u32;
        };
        let res = vm.dispatch_comparison(session.task_id, instr);
        finish_dispatch(rt_ctx, session, vm, res)
    }
);

def_jit_helper!(
    fn helper_is_truthy(raw_val: u64) -> u32 {
        u32::from(is_truthy(TaggedValue::from_raw(raw_val)))
    }
);

def_jit_helper!(
    fn helper_load_const_slow(vm_opaque: *mut c_void, dst_reg: u32, const_idx: u32) -> u32 {
        // SAFETY: invoked synchronously from compiled JIT code with valid `JitRuntimeCtx`.
        let (rt_ctx, session, vm) = unsafe { unpack_ctx(vm_opaque) };
        let val = match vm.constant_value(const_idx) {
            Ok(v) => v,
            Err(err) => {
                session.pending_error = Some(err);
                return JitExitKind::Trap as u32;
            }
        };
        if val.as_enum().is_some()
            && let Err(err) = vm.enum_layout(val)
        {
            session.pending_error = Some(err);
            return JitExitKind::Trap as u32;
        }
        let Some(task) = vm.roots.scheduler.get_task_mut(session.task_id) else {
            session.pending_error = Some(VmError::InvalidTask);
            return JitExitKind::Trap as u32;
        };
        task.registers.set(Reg(dst_reg as u8), val);
        refresh_runtime_ctx(rt_ctx, session_vm_reborrow(vm), session.task_id);
        0
    }
);

#[inline]
fn session_vm_reborrow(vm: &mut Vm) -> &mut Vm {
    vm
}

def_jit_helper!(
    fn helper_type_op(vm_opaque: *mut c_void, pc: u32, instr_word: u32) -> u32 {
        // SAFETY: invoked synchronously from compiled JIT code with valid `JitRuntimeCtx`.
        let (rt_ctx, session, vm) = unsafe { unpack_ctx(vm_opaque) };
        let Some(instr) = Instruction::decode(instr_word) else {
            session.pending_error = Some(VmError::InvalidInstruction(instr_word));
            return JitExitKind::Trap as u32;
        };
        if let Some(task) = vm.roots.scheduler.get_task_mut(session.task_id) {
            task.pc = pc + 1;
        }
        let res = vm.dispatch(session.task_id, instr);
        finish_dispatch(rt_ctx, session, vm, res)
    }
);

def_jit_helper!(
    fn helper_load_field_ic(
        vm_opaque: *mut c_void,
        pc: u32,
        instr_word: u32,
        ic_ptr: *mut MonomorphicIc,
    ) -> u32 {
        // SAFETY: invoked synchronously from compiled JIT code with valid `JitRuntimeCtx` and pinned `ic_ptr`.
        let (rt_ctx, session, vm) = unsafe { unpack_ctx(vm_opaque) };
        let Some(instr) = Instruction::decode(instr_word) else {
            session.pending_error = Some(VmError::InvalidInstruction(instr_word));
            return JitExitKind::Trap as u32;
        };
        if let Some(task) = vm.roots.scheduler.get_task_mut(session.task_id) {
            task.pc = pc + 1;
        }
        let res = vm.dispatch(session.task_id, instr);
        if matches!(res, DispatchResult::Continue) && !ic_ptr.is_null() {
            let (_, recv_reg, field_idx) = a_fields(&instr);
            if let Some(recv_val) = vm
                .roots
                .scheduler
                .get_task(session.task_id)
                .map(|t| t.registers.get(recv_reg))
            {
                // SAFETY: `ic_ptr` points into the pinned `Box<[MonomorphicIc]>` owned by `CompiledFunctionArtifact`.
                let ic = unsafe { &mut *ic_ptr };
                try_populate_load_ic(vm, recv_val, field_idx as u32, ic);
            }
        }
        finish_dispatch(rt_ctx, session, vm, res)
    }
);

def_jit_helper!(
    fn helper_store_field_ic(
        vm_opaque: *mut c_void,
        pc: u32,
        instr_word: u32,
        ic_ptr: *mut MonomorphicIc,
    ) -> u32 {
        // SAFETY: invoked synchronously from compiled JIT code with valid `JitRuntimeCtx` and pinned `ic_ptr`.
        let (rt_ctx, session, vm) = unsafe { unpack_ctx(vm_opaque) };
        let Some(instr) = Instruction::decode(instr_word) else {
            session.pending_error = Some(VmError::InvalidInstruction(instr_word));
            return JitExitKind::Trap as u32;
        };
        if let Some(task) = vm.roots.scheduler.get_task_mut(session.task_id) {
            task.pc = pc + 1;
        }
        let res = vm.dispatch(session.task_id, instr);
        if matches!(res, DispatchResult::Continue) && !ic_ptr.is_null() {
            let (_, recv_reg, field_idx) = a_fields(&instr);
            if let Some(recv_val) = vm
                .roots
                .scheduler
                .get_task(session.task_id)
                .map(|t| t.registers.get(recv_reg))
            {
                // SAFETY: `ic_ptr` points into the pinned `Box<[MonomorphicIc]>` owned by `CompiledFunctionArtifact`.
                let ic = unsafe { &mut *ic_ptr };
                try_populate_store_ic(vm, recv_val, field_idx as u32, ic);
            }
        }
        finish_dispatch(rt_ctx, session, vm, res)
    }
);

def_jit_helper!(
    fn helper_store_boxed_f64(
        vm_opaque: *mut c_void,
        is_slot: u32,
        index: u32,
        f64_bits: u64,
    ) -> u32 {
        // SAFETY: invoked synchronously from Tier-2 exit/safepoint/deopt stub with all heap pointers rooted.
        let (rt_ctx, session, vm) = unsafe { unpack_ctx(vm_opaque) };
        let val = match vm.number_value(Number::F64(f64::from_bits(f64_bits))) {
            Ok(v) => v,
            Err(err) => {
                session.pending_error = Some(err);
                return JitExitKind::Trap as u32;
            }
        };
        let Some(task) = vm.roots.scheduler.get_task_mut(session.task_id) else {
            session.pending_error = Some(VmError::InvalidTask);
            return JitExitKind::Trap as u32;
        };
        if is_slot != 0 {
            let idx = index as usize;
            if idx >= task.local_slots.len() {
                session.pending_error = Some(VmError::InvalidSlot(index));
                return JitExitKind::Trap as u32;
            }
            task.local_slots[idx] = val;
        } else {
            task.registers.set(Reg(index as u8), val);
        }
        refresh_runtime_ctx(rt_ctx, vm, session.task_id);
        0
    }
);

def_jit_helper!(
    fn helper_step_inline_op(vm_opaque: *mut c_void, pc: u32, instr_word: u32) -> u32 {
        // SAFETY: invoked synchronously from compiled JIT code with valid `JitRuntimeCtx`.
        let (rt_ctx, session, vm) = unsafe { unpack_ctx(vm_opaque) };
        let Some(instr) = Instruction::decode(instr_word) else {
            session.pending_error = Some(VmError::InvalidInstruction(instr_word));
            return JitExitKind::Trap as u32;
        };
        debug_assert!(!matches!(
            instr.opcode,
            Opcode::Call
                | Opcode::CallFar
                | Opcode::CallIndirect
                | Opcode::CallIndirectProof
                | Opcode::CallMethod
                | Opcode::CallMethodFar
                | Opcode::TraitCall
                | Opcode::CallBuiltin
                | Opcode::Return
                | Opcode::ReturnUnit
        ));
        if let Some(task) = vm.roots.scheduler.get_task_mut(session.task_id) {
            task.pc = pc + 1;
        }
        let res = vm.dispatch(session.task_id, instr);
        finish_dispatch(rt_ctx, session, vm, res)
    }
);

fn inspect_ordinary_field(
    vm: &Vm,
    recv_val: TaggedValue,
    field_idx: u32,
) -> Option<(u32, u32, type_pool::TypeIndex)> {
    let ptr = recv_val.as_heap_ptr()?;
    // SAFETY: `recv_val` is rooted in `task.registers` under an active VM operation.
    let header = unsafe { ObjectHeader::from_payload_ptr(ptr) };
    if !header.is_ordinary() {
        return None;
    }
    let raw_ty = header.type_index.as_u32();
    let canon_ty = vm.state.type_pool.canonical_type(header.type_index)?;
    if canon_ty != header.type_index || vm.state.type_pool.is_reserved_collection_role(canon_ty) {
        return None;
    }
    let words = header.payload_words();
    if words >= (1 << 19) || (field_idx as usize) >= words {
        return None;
    }
    let field_ty = match &vm.state.type_pool.get(canon_ty).kind {
        TypeKind::Struct { fields, .. } if fields.len() == words => {
            fields.get(field_idx as usize)?.ty
        }
        TypeKind::Tuple { elements } if elements.len() == words => {
            *elements.get(field_idx as usize)?
        }
        TypeKind::Newtype { inner, .. } if words == 1 && field_idx == 0 => *inner,
        _ => return None,
    };
    let canon_field_ty = vm.state.type_pool.canonical_type(field_ty)?;
    if vm.state.type_pool.contains_associated_type(canon_field_ty) {
        return None;
    }
    let expected_gc_meta_masked = (words as u32) << 3;
    Some((raw_ty, expected_gc_meta_masked, canon_field_ty))
}

fn try_populate_load_ic(vm: &Vm, recv_val: TaggedValue, field_idx: u32, ic: &mut MonomorphicIc) {
    if ic.receiver_type == MonomorphicIc::MEGAMORPHIC_TYPE {
        return;
    }
    let Some((raw_ty, expected_gc_meta_masked, canon_field_ty)) =
        inspect_ordinary_field(vm, recv_val, field_idx)
    else {
        return;
    };
    if ic.receiver_type != MonomorphicIc::COLD_TYPE && ic.receiver_type != raw_ty {
        ic.receiver_type = MonomorphicIc::MEGAMORPHIC_TYPE;
        ic.fast_kind = 0;
        return;
    }
    // Only enable inline non-UNIT field load when `assert_value_type` is an identity check
    // on already-validated stored values (excluding `ErrorQualified`, `Trait`, `Tuple`, `Newtype`).
    let can_fast_load = match &vm.state.type_pool.get(canon_field_ty).kind {
        TypeKind::Intrinsic(
            Intrinsic::I64
            | Intrinsic::U64
            | Intrinsic::I128
            | Intrinsic::U128
            | Intrinsic::F64
            | Intrinsic::Bool
            | Intrinsic::Char
            | Intrinsic::Str,
        ) => true,
        TypeKind::Struct { .. } => !vm
            .state
            .type_pool
            .is_reserved_collection_role(canon_field_ty),
        TypeKind::Enum { .. } => true,
        _ => false,
    };
    if can_fast_load {
        ic.receiver_type = raw_ty;
        ic.expected_gc_meta_masked = expected_gc_meta_masked;
        ic.fast_kind = 1;
    }
}

fn try_populate_store_ic(vm: &Vm, recv_val: TaggedValue, field_idx: u32, ic: &mut MonomorphicIc) {
    if ic.receiver_type == MonomorphicIc::MEGAMORPHIC_TYPE {
        return;
    }
    let Some((raw_ty, expected_gc_meta_masked, canon_field_ty)) =
        inspect_ordinary_field(vm, recv_val, field_idx)
    else {
        return;
    };
    if ic.receiver_type != MonomorphicIc::COLD_TYPE && ic.receiver_type != raw_ty {
        ic.receiver_type = MonomorphicIc::MEGAMORPHIC_TYPE;
        ic.fast_kind = FieldStoreFastKind::SlowOnly as u32;
        return;
    }
    let fast_kind = match vm.state.type_pool.as_intrinsic(canon_field_ty) {
        Some(Intrinsic::I64) => FieldStoreFastKind::SmallI64,
        Some(Intrinsic::Bool) => FieldStoreFastKind::Bool,
        Some(Intrinsic::F64) => FieldStoreFastKind::CanonicalF64,
        _ => FieldStoreFastKind::SlowOnly,
    };
    if fast_kind != FieldStoreFastKind::SlowOnly {
        ic.receiver_type = raw_ty;
        ic.expected_gc_meta_masked = expected_gc_meta_masked;
        ic.fast_kind = fast_kind as u32;
    }
}
