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
    ContinueTaskLoop,
    TierUp(u32),
    Yield(u32),
    YieldAtTaskPc,
    SuspendRetry(u32),
    SuspendAtTaskPc,
    Cancelled,
    Error(VmError),
}

const MAX_NATIVE_JIT_CALL_DEPTH: u32 = 96;

struct BridgeSession {
    vm: *mut Vm,
    task_id: TaskId,
    pending_error: Option<VmError>,
    jit_call_depth: u32,
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
    alloc_object_fast: helper_alloc_object_fast,
    call_builtin: helper_call_builtin,
    call_op: helper_call_op,
    call_direct_fast: helper_call_direct_fast,
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
    alloc_object_fast: helper_alloc_object_fast,
    call_builtin: helper_call_builtin,
    call_op: helper_call_op,
    call_direct_fast: helper_call_direct_fast,
};

impl Vm {
    /// Configure the per-VM tiered JIT mode and code-cache cap across all workers.
    pub fn set_jit_config(&mut self, config: JitConfig) {
        let _operation = self.operation();
        *self.shared.jit_config.write() = config;
        self.shared
            .jit_compiled_functions
            .store(0, Ordering::Release);
        self.shared.jit_code_bytes.store(0, Ordering::Release);
        self.shared.jit_deopt_count.store(0, Ordering::Release);
        let epoch = self.shared.jit_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.worker.jit.set_config(config);
        self.worker.jit_synced_epoch = epoch;
        self.worker.jit_reported_fns = 0;
        self.worker.jit_reported_bytes = 0;
        self.worker.jit_reported_deopts = 0;
    }

    /// Return the active [`JitConfig`] for this VM.
    pub fn jit_config(&self) -> JitConfig {
        *self.shared.jit_config.read()
    }

    /// Set the active [`JitMode`] while preserving other [`JitConfig`] parameters.
    pub fn set_jit_mode(&mut self, mode: JitMode) {
        let mut cfg = self.jit_config();
        cfg.mode = mode;
        self.set_jit_config(cfg);
    }

    /// Whether JIT compilation and execution are enabled on this VM and host target.
    pub fn jit_enabled(&self) -> bool {
        self.jit_config().mode != JitMode::Off && jit::arch::is_target_supported()
    }

    /// Return current JIT telemetry counters aggregated across all workers.
    pub fn jit_stats(&self) -> JitStats {
        JitStats {
            compiled_functions: self.shared.jit_compiled_functions.load(Ordering::Acquire),
            code_bytes: self.shared.jit_code_bytes.load(Ordering::Acquire),
            deopt_count: self.shared.jit_deopt_count.load(Ordering::Acquire),
        }
    }

    /// Invalidate all worker code caches when VM type-pool or method-scope metadata mutates.
    pub(crate) fn invalidate_jit(&mut self) {
        self.shared
            .jit_compiled_functions
            .store(0, Ordering::Release);
        self.shared.jit_code_bytes.store(0, Ordering::Release);
        self.shared.jit_deopt_count.store(0, Ordering::Release);
        let epoch = self.shared.jit_epoch.fetch_add(1, Ordering::AcqRel) + 1;
        self.worker.jit.invalidate_all();
        self.worker.jit_synced_epoch = epoch;
        self.worker.jit_reported_fns = 0;
        self.worker.jit_reported_bytes = 0;
        self.worker.jit_reported_deopts = 0;
    }

    #[inline]
    pub(crate) fn new_default_jit() -> JitEngine {
        JitEngine::new(JitConfig::default())
    }

    #[inline]
    pub(crate) fn sync_worker_jit(&mut self) {
        let epoch = self.shared.jit_epoch.load(Ordering::Acquire);
        if self.worker.jit_synced_epoch != epoch {
            let cfg = *self.shared.jit_config.read();
            self.worker.jit.set_config(cfg);
            self.worker.jit.invalidate_all();
            self.worker.jit_synced_epoch = epoch;
            self.worker.jit_reported_fns = 0;
            self.worker.jit_reported_bytes = 0;
            self.worker.jit_reported_deopts = 0;
        }
    }

    #[inline]
    fn publish_worker_jit_stats(&mut self) {
        let local = self.worker.jit.stats();
        if local.compiled_functions > self.worker.jit_reported_fns {
            self.shared.jit_compiled_functions.fetch_add(
                local.compiled_functions - self.worker.jit_reported_fns,
                Ordering::Release,
            );
            self.worker.jit_reported_fns = local.compiled_functions;
        }
        if local.code_bytes > self.worker.jit_reported_bytes {
            self.shared.jit_code_bytes.fetch_add(
                local.code_bytes - self.worker.jit_reported_bytes,
                Ordering::Release,
            );
            self.worker.jit_reported_bytes = local.code_bytes;
        }
        if local.deopt_count > self.worker.jit_reported_deopts {
            self.shared.jit_deopt_count.fetch_add(
                local.deopt_count - self.worker.jit_reported_deopts,
                Ordering::Release,
            );
            self.worker.jit_reported_deopts = local.deopt_count;
        }
    }

    /// Record function-entry or interpreter backward-jump hotness and trigger compilation if hot.
    #[inline]
    pub(crate) fn maybe_compile_on_hotness(&mut self, func_id: FuncId, delta: u64) {
        self.sync_worker_jit();
        if self.worker.jit.record_hotness(func_id, delta) {
            self.compile_jit_function(func_id, false);
        }
    }

    /// Compile `func_id` into native code (Tier-1 or Tier-2), degrading gracefully on failure.
    pub(crate) fn compile_jit_function(&mut self, func_id: FuncId, request_tier2: bool) {
        self.sync_worker_jit();
        if !self.worker.jit.is_enabled() {
            return;
        }
        if self.shared.bytecode.try_get_function(func_id).is_none() {
            return;
        }

        let constants = self.classify_constants_for_jit();
        let (hotness_ptr, global_deopt_ptr, func_deopt_ptr) = self.worker.jit.counter_ptrs(func_id);

        // Pre-validate direct `Opcode::Call` targets before borrowing `type_pool` so
        // `fast_direct_callee_slots` can classify value-only non-closure callees lock-free.
        let mut validated_direct_callees = std::collections::HashSet::new();
        {
            let instrs = &self.shared.bytecode.get_function(func_id).instructions;
            for &w in instrs {
                if let Some(ins) = Instruction::decode(w)
                    && ins.opcode == Opcode::Call
                    && let nsbc::InstructionData::C { payload } = ins.data
                {
                    let (_, cid_raw) = Instruction::c_arg_count_func_id(payload);
                    let cid = FuncId(cid_raw);
                    if self.checked_function(cid).is_ok() {
                        validated_direct_callees.insert(cid);
                    }
                }
            }
        }

        {
            let shared = &*self.shared;
            let instructions = &shared.bytecode.get_function(func_id).instructions;
            let tp_guard = shared.type_pool.read(self.worker.worker_id);
            let scopes = &shared.method_call_scopes;
            let scopes_required = !tp_guard.scopes().is_empty();
            let has_scope =
                |pc: usize| -> bool { !scopes_required || scopes.contains_key(&(func_id, pc)) };

            let compute_guaranteed_slots = |code_words: &[u32]| -> u32 {
                match code_words.first().copied().and_then(Instruction::decode) {
                    Some(first)
                        if first.opcode == Opcode::AllocateSlots
                            && matches!(first.data, nsbc::InstructionData::E { payload } if payload <= (1 << 17))
                            && code_words[1..].iter().all(|&w| {
                                Instruction::decode(w)
                                    .is_none_or(|i| i.opcode != Opcode::AllocateSlots)
                            }) =>
                    {
                        let nsbc::InstructionData::E { payload } = first.data else {
                            unreachable!()
                        };
                        payload
                    }
                    _ => 0,
                }
            };
            let guaranteed_slots = compute_guaranteed_slots(instructions);
            let list_type_raw = tp_guard.list_type().map(|t| t.as_u32());
            let list_buf_type_raw = tp_guard.list_buffer_type().map(|t| t.as_u32());
            let fast_list_builtins = shared.builtins.can_fast_path(runtime::ids::LIST_LEN)
                && shared.builtins.can_fast_path(runtime::ids::LIST_GET)
                && shared.builtins.can_fast_path(runtime::ids::LIST_SET);

            let fast_direct_callee_slots = |cid: FuncId, arg_count: u8| -> Option<(u32, bool)> {
                if !validated_direct_callees.contains(&cid) {
                    return None;
                }
                let code = shared.bytecode.try_get_function(cid)?;
                if code.is_closure
                    || code.display_owner.is_some()
                    || code.instructions.is_empty()
                    || code.param_count != arg_count
                    || !code.abi.captures.is_empty()
                    || code.abi.parameters.len() != arg_count as usize
                    || code
                        .abi
                        .parameters
                        .iter()
                        .any(|p| *p != nsbc::ParameterAbi::Value)
                {
                    return None;
                }
                let slots = compute_guaranteed_slots(&code.instructions);
                let clobbers_cs = shared.bytecode.get_meta(cid).clobbers_callee_saved;
                Some((slots, clobbers_cs))
            };

            let input = CompilationInput {
                func_id,
                instructions,
                constants: &constants,
                type_pool: &tp_guard,
                has_scope: &has_scope,
                helpers: &JIT_HELPERS,
                hotness_ptr,
                global_deopt_ptr,
                func_deopt_ptr,
                guaranteed_slots,
                list_type_raw,
                list_buf_type_raw,
                fast_list_builtins,
                fast_direct_callee_slots: &fast_direct_callee_slots,
            };
            self.worker.jit.compile_function(&input, request_tier2);
        }
        self.publish_worker_jit_stats();
    }

    fn classify_constants_for_jit(&self) -> Vec<CompileConstant> {
        let consts_guard = self.constants_read();
        let values = consts_guard.values();
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
                let f64_bits = val.as_f64().map(f64::to_bits);
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
        self.sync_worker_jit();
        let (entry_fn, entry_offset, allow_tier_up, allow_tier2_loops) =
            self.worker.jit.lookup_entry(func_id, pc)?;

        let mut session = BridgeSession {
            vm: std::ptr::from_mut(self),
            task_id,
            pending_error: None,
            jit_call_depth: 0,
        };

        let (consts_ptr, consts_len) = {
            let consts_guard = self.constants_read();
            let consts = consts_guard.values();
            (consts.as_ptr().cast::<u64>(), consts.len())
        };

        let mut rt_ctx = JitRuntimeCtx {
            host_session: std::ptr::from_mut(&mut session).cast::<c_void>(),
            regs_ptr: std::ptr::null_mut(),
            slots_ptr: std::ptr::null_mut(),
            slots_len: 0,
            consts_ptr,
            consts_len,
            gc_requested_ptr: GC_FLAGS.gc_requested.as_ptr().cast::<u8>(),
            preempt_requested_ptr: GC_FLAGS.preempt_requested.as_ptr().cast::<u8>(),
            aux_started_ptr: self.shared.aux_started.as_ptr().cast::<u8>(),
            slice_safepoints: self.worker.slice_safepoints,
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
        // `self.worker.jit`, `rt_ctx` and `session` outlive the synchronous call on this stack,
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

        self.worker.slice_safepoints = rt_ctx.slice_safepoints;
        self.publish_worker_jit_stats();

        let outcome = match exit.kind {
            x if x == JitExitKind::Finished as u32 => JitStepOutcome::Finished,
            x if x == JitExitKind::Return as u32 => JitStepOutcome::Return {
                ret_pc: exit.pc,
                val: TaggedValue::from_raw(exit.value),
            },
            x if x == JitExitKind::InterpretAt as u32 => JitStepOutcome::InterpretOne(exit.pc),
            x if x == JitExitKind::ContinueTaskLoop as u32 => JitStepOutcome::ContinueTaskLoop,
            x if x == JitExitKind::TierUp as u32 => JitStepOutcome::TierUp(exit.pc),
            x if x == JitExitKind::Yield as u32 => JitStepOutcome::Yield(exit.pc),
            x if x == JitExitKind::YieldAtTaskPc as u32 => JitStepOutcome::YieldAtTaskPc,
            x if x == JitExitKind::SuspendRetry as u32 => JitStepOutcome::SuspendRetry(exit.pc),
            x if x == JitExitKind::SuspendAtTaskPc as u32 => JitStepOutcome::SuspendAtTaskPc,
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

#[inline]
fn refresh_runtime_ctx(rt_ctx: &mut JitRuntimeCtx, vm: &mut Vm, task_id: TaskId) {
    if let Some(task) = vm.task_mut(task_id) {
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
        DispatchResult::StartCancelUnwind | DispatchResult::ResumeCancelUnwind => {
            JitExitKind::Cancelled as u32
        }
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
        if let Err(err) = vm.alloc_task_frame_slots(session.task_id, count as usize) {
            session.pending_error = Some(err);
            return JitExitKind::Trap as u32;
        }
        refresh_runtime_ctx(rt_ctx, vm, session.task_id);
        0
    }
);

def_jit_helper!(
    fn helper_safepoint(vm_opaque: *mut c_void, resume_pc: u32) -> u32 {
        // SAFETY: invoked synchronously from compiled JIT code with all live state flushed to `StackContext`.
        let (rt_ctx, session, vm) = unsafe { unpack_ctx(vm_opaque) };
        vm.worker.slice_safepoints = rt_ctx.slice_safepoints;
        if let Some(task) = vm.task_mut(session.task_id) {
            task.pc = resume_pc;
        }
        vm.worker.heap.safe_point();
        refresh_runtime_ctx(rt_ctx, vm, session.task_id);

        let async_cancelled = vm.scheduler().is_cancelled(session.task_id);
        let Some(task) = vm.task_mut(session.task_id) else {
            session.pending_error = Some(VmError::InvalidTask);
            return JitExitKind::Trap as u32;
        };
        if async_cancelled {
            task.cancelled = true;
        }
        if task.cancelled {
            return JitExitKind::Cancelled as u32;
        }
        if GC_FLAGS.preempt_requested.load(Ordering::Relaxed) {
            return JitExitKind::Yield as u32;
        }
        vm.worker.slice_safepoints = vm.worker.slice_safepoints.saturating_sub(1);
        if vm.worker.slice_safepoints == 0 {
            vm.worker.slice_safepoints = 32_768;
            rt_ctx.slice_safepoints = 32_768;
            if vm.scheduler().has_other_ready_tasks() {
                return JitExitKind::Yield as u32;
            }
        }
        rt_ctx.slice_safepoints = vm.worker.slice_safepoints;
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
        let Some(task) = vm.task_mut(session.task_id) else {
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
        if let Some(task) = vm.task_mut(session.task_id) {
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
        if let Some(task) = vm.task_mut(session.task_id) {
            task.pc = pc + 1;
        }
        let res = vm.dispatch(session.task_id, instr);
        if matches!(res, DispatchResult::Continue) && !ic_ptr.is_null() {
            let (_, recv_reg, field_idx) = a_fields(&instr);
            if let Some(recv_val) = vm
                .task_ref(session.task_id)
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
        if let Some(task) = vm.task_mut(session.task_id) {
            task.pc = pc + 1;
        }
        let res = vm.dispatch(session.task_id, instr);
        if matches!(res, DispatchResult::Continue) && !ic_ptr.is_null() {
            let (_, recv_reg, field_idx) = a_fields(&instr);
            if let Some(recv_val) = vm
                .task_ref(session.task_id)
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
        let Some(task) = vm.task_mut(session.task_id) else {
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
                | Opcode::CallWasm
                | Opcode::CallIndirect
                | Opcode::CallIndirectProof
                | Opcode::CallMethod
                | Opcode::CallMethodFar
                | Opcode::TraitCall
                | Opcode::CallBuiltin
                | Opcode::Return
                | Opcode::ReturnUnit
        ));
        if let Some(task) = vm.task_mut(session.task_id) {
            task.pc = pc + 1;
        }
        let res = vm.dispatch(session.task_id, instr);
        finish_dispatch(rt_ctx, session, vm, res)
    }
);

def_jit_helper!(
    fn helper_alloc_object_fast(
        vm_opaque: *mut c_void,
        dst_reg: u32,
        canon_ty_raw: u32,
        words: u32,
    ) -> u32 {
        // SAFETY: invoked synchronously from compiled JIT code with all live state rooted in `StackContext`.
        let (rt_ctx, session, vm) = unsafe { unpack_ctx(vm_opaque) };
        let type_idx = type_pool::TypeIndex::from_raw(canon_ty_raw);
        let words_u16 = words as u16;
        // SAFETY: mutator owns an active execution operation; all payload words are initialized
        // to `TaggedValue::UNIT` before the object reference is published to `task.registers`.
        let Some(ptr) = (unsafe { vm.worker.heap.alloc_object(type_idx, words_u16) }) else {
            session.pending_error = Some(VmError::OutOfMemory);
            return JitExitKind::Trap as u32;
        };
        let base = ptr.as_ptr().cast::<u64>();
        for i in 0..(words as usize) {
            // SAFETY: `i < words_u16` is within the freshly allocated object payload.
            unsafe {
                base.add(i).write(TaggedValue::UNIT.raw());
            }
        }
        // SAFETY: `ptr` is a live, initialized managed object allocation.
        let val = unsafe { TaggedValue::from_heap_ptr(ptr.as_ptr()) };
        let Some(task) = vm.task_mut(session.task_id) else {
            session.pending_error = Some(VmError::InvalidTask);
            return JitExitKind::Trap as u32;
        };
        task.registers.set(Reg(dst_reg as u8), val);
        refresh_runtime_ctx(rt_ctx, vm, session.task_id);
        0
    }
);

def_jit_helper!(
    fn helper_call_builtin(
        vm_opaque: *mut c_void,
        pc: u32,
        builtin_id: u32,
        arg_count: u32,
    ) -> u32 {
        // SAFETY: invoked synchronously from compiled JIT code with arguments in `task.registers`.
        let (rt_ctx, session, vm) = unsafe { unpack_ctx(vm_opaque) };
        if let Some(task) = vm.task_mut(session.task_id) {
            task.pc = pc + 1;
        }
        let res = vm.dispatch_builtin(session.task_id, builtin_id, arg_count as u8);
        refresh_runtime_ctx(rt_ctx, vm, session.task_id);
        match res {
            DispatchResult::Continue => 0,
            DispatchResult::Yield => JitExitKind::YieldAtTaskPc as u32,
            DispatchResult::SuspendRetry => {
                if let Some(task) = vm.task_mut(session.task_id) {
                    task.pc = pc;
                }
                JitExitKind::SuspendAtTaskPc as u32
            }
            DispatchResult::StartCancelUnwind | DispatchResult::ResumeCancelUnwind => {
                JitExitKind::Cancelled as u32
            }
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
);

fn complete_return_in_jit(
    rt_ctx: &mut JitRuntimeCtx,
    session: &mut BridgeSession,
    vm: &mut Vm,
    ret_pc: u32,
    val: TaggedValue,
) -> u32 {
    let mut validated_func = None;
    let outcome = vm.complete_return(session.task_id, ret_pc, val, &mut validated_func);
    refresh_runtime_ctx(rt_ctx, vm, session.task_id);
    match outcome {
        None => 0,
        Some(crate::ExecOutcome::Finished) => JitExitKind::Finished as u32,
        Some(crate::ExecOutcome::Yield) => JitExitKind::YieldAtTaskPc as u32,
        Some(crate::ExecOutcome::Suspend) => JitExitKind::SuspendAtTaskPc as u32,
        Some(crate::ExecOutcome::Cancelled) => JitExitKind::Cancelled as u32,
        Some(crate::ExecOutcome::Error(err)) => {
            session.pending_error = Some(err);
            JitExitKind::Trap as u32
        }
    }
}

def_jit_helper!(
    fn helper_call_op(vm_opaque: *mut c_void, pc: u32, instr_word: u32) -> u32 {
        // SAFETY: invoked synchronously from compiled JIT code with valid `JitRuntimeCtx`.
        let (rt_ctx, session, vm) = unsafe { unpack_ctx(vm_opaque) };
        let Some(instr) = Instruction::decode(instr_word) else {
            session.pending_error = Some(VmError::InvalidInstruction(instr_word));
            return JitExitKind::Trap as u32;
        };
        let Some((caller_func, depth_before, ctx_addr_before)) =
            vm.task_ref(session.task_id).map(|t| {
                (
                    t.current_func,
                    t.call_stack.len(),
                    std::ptr::from_ref::<runtime::StackContext>(t) as usize,
                )
            })
        else {
            session.pending_error = Some(VmError::InvalidTask);
            return JitExitKind::Trap as u32;
        };

        if let Some(task) = vm.task_mut(session.task_id) {
            task.pc = pc + 1;
        }

        let res = if instr.opcode == Opcode::Call {
            let nsbc::InstructionData::C { payload } = instr.data else {
                unreachable!()
            };
            let (arg_count, func_id_raw) = Instruction::c_arg_count_func_id(payload);
            let arg_len = arg_count as usize;
            if arg_len > runtime::GP_REGISTER_COUNT {
                DispatchResult::Error(VmError::TypeError)
            } else {
                let mut arg_buf = [TaggedValue::UNIT; runtime::GP_REGISTER_COUNT];
                arg_buf[..arg_len].copy_from_slice(
                    &vm.task_ref(session.task_id).unwrap().registers.regs[..arg_len],
                );
                vm.invoke_closure_physical(
                    session.task_id,
                    FuncId(func_id_raw),
                    None,
                    &arg_buf[..arg_len],
                )
            }
        } else {
            vm.dispatch(session.task_id, instr)
        };

        match res {
            DispatchResult::Error(err) => {
                refresh_runtime_ctx(rt_ctx, vm, session.task_id);
                session.pending_error = Some(err);
                return JitExitKind::Trap as u32;
            }
            DispatchResult::Yield => {
                refresh_runtime_ctx(rt_ctx, vm, session.task_id);
                return JitExitKind::YieldAtTaskPc as u32;
            }
            DispatchResult::SuspendRetry => {
                if let Some(task) = vm.task_mut(session.task_id) {
                    task.pc = pc;
                }
                refresh_runtime_ctx(rt_ctx, vm, session.task_id);
                return JitExitKind::SuspendAtTaskPc as u32;
            }
            DispatchResult::StartCancelUnwind | DispatchResult::ResumeCancelUnwind => {
                refresh_runtime_ctx(rt_ctx, vm, session.task_id);
                return JitExitKind::Cancelled as u32;
            }
            DispatchResult::Return(val) => {
                return complete_return_in_jit(rt_ctx, session, vm, pc, val);
            }
            DispatchResult::Continue => {}
        }

        let Some((cur_func, cur_pc, cur_depth, cur_ctx_addr, unwinding_or_cancelled)) =
            vm.task_ref(session.task_id).map(|t| {
                (
                    t.current_func,
                    t.pc,
                    t.call_stack.len(),
                    std::ptr::from_ref::<runtime::StackContext>(t) as usize,
                    t.unwinding_cancel || t.cancelled,
                )
            })
        else {
            session.pending_error = Some(VmError::InvalidTask);
            return JitExitKind::Trap as u32;
        };

        // In-place method completion (e.g. `List.apply`/`Map.apply`/`Continuation.clone`).
        if cur_ctx_addr == ctx_addr_before
            && cur_depth == depth_before
            && cur_func == caller_func
            && cur_pc == pc + 1
            && !unwinding_or_cancelled
        {
            refresh_runtime_ctx(rt_ctx, vm, session.task_id);
            return 0;
        }

        // Standard frame push: execute callee directly in native JIT if compiled and within recursion budget!
        if cur_ctx_addr == ctx_addr_before
            && cur_depth == depth_before + 1
            && cur_pc == 0
            && !unwinding_or_cancelled
        {
            let callee_len = match vm.checked_function(cur_func) {
                Ok(code) => code.instructions.len(),
                Err(err) => {
                    session.pending_error = Some(err);
                    return JitExitKind::Trap as u32;
                }
            };
            if callee_len == 0 {
                return JitExitKind::Finished as u32;
            }
            vm.maybe_compile_on_hotness(cur_func, jit::ENTRY_HOTNESS_INCREMENT);

            let mut callee_pc = 0u32;
            while session.jit_call_depth < MAX_NATIVE_JIT_CALL_DEPTH {
                let Some((callee_fn, callee_off, allow_tu, allow_t2)) =
                    vm.worker.jit.lookup_entry(cur_func, callee_pc)
                else {
                    break;
                };

                let saved_allow_tu = rt_ctx.allow_tier_up;
                let saved_allow_t2 = rt_ctx.allow_tier2_loops;
                rt_ctx.allow_tier_up = u8::from(allow_tu);
                rt_ctx.allow_tier2_loops = u8::from(allow_t2);
                refresh_runtime_ctx(rt_ctx, vm, session.task_id);
                if rt_ctx.regs_ptr.is_null() {
                    rt_ctx.allow_tier_up = saved_allow_tu;
                    rt_ctx.allow_tier2_loops = saved_allow_t2;
                    session.pending_error = Some(VmError::InvalidTask);
                    return JitExitKind::Trap as u32;
                }

                let mut callee_exit = JitExit::default();
                session.jit_call_depth += 1;
                // SAFETY: `callee_fn` points to a sealed W^X `ExecutableBuffer` in `vm.worker.jit`
                // that is never unmapped during task execution; `rt_ctx` and `session` remain live.
                unsafe {
                    callee_fn(
                        std::ptr::from_mut(rt_ctx).cast::<c_void>(),
                        rt_ctx.regs_ptr,
                        callee_off,
                        std::ptr::from_mut(&mut callee_exit),
                    );
                }
                session.jit_call_depth -= 1;
                rt_ctx.allow_tier_up = saved_allow_tu;
                rt_ctx.allow_tier2_loops = saved_allow_t2;

                match callee_exit.kind {
                    x if x == JitExitKind::Return as u32 => {
                        let ret_val = TaggedValue::from_raw(callee_exit.value);
                        return complete_return_in_jit(
                            rt_ctx,
                            session,
                            vm,
                            callee_exit.pc,
                            ret_val,
                        );
                    }
                    x if x == JitExitKind::TierUp as u32 => {
                        vm.compile_jit_function(cur_func, true);
                        callee_pc = callee_exit.pc;
                        if let Some(t) = vm.task_mut(session.task_id) {
                            t.pc = callee_pc;
                        }
                        continue;
                    }
                    x if x == JitExitKind::InterpretAt as u32 => {
                        if let Some(t) = vm.task_mut(session.task_id) {
                            t.pc = callee_exit.pc;
                        }
                        return JitExitKind::ContinueTaskLoop as u32;
                    }
                    x if x == JitExitKind::ContinueTaskLoop as u32 => {
                        return JitExitKind::ContinueTaskLoop as u32;
                    }
                    x if x == JitExitKind::Yield as u32 => {
                        if let Some(t) = vm.task_mut(session.task_id) {
                            t.pc = callee_exit.pc;
                        }
                        return JitExitKind::YieldAtTaskPc as u32;
                    }
                    x if x == JitExitKind::YieldAtTaskPc as u32 => {
                        return JitExitKind::YieldAtTaskPc as u32;
                    }
                    x if x == JitExitKind::SuspendRetry as u32 => {
                        if let Some(t) = vm.task_mut(session.task_id) {
                            t.pc = callee_exit.pc;
                        }
                        return JitExitKind::SuspendAtTaskPc as u32;
                    }
                    x if x == JitExitKind::SuspendAtTaskPc as u32 => {
                        return JitExitKind::SuspendAtTaskPc as u32;
                    }
                    x if x == JitExitKind::Finished as u32 => {
                        return JitExitKind::Finished as u32;
                    }
                    x if x == JitExitKind::Cancelled as u32 => {
                        return JitExitKind::Cancelled as u32;
                    }
                    _ => {
                        if session.pending_error.is_none() {
                            session.pending_error =
                                Some(VmError::InvalidInstruction(callee_exit.kind));
                        }
                        return JitExitKind::Trap as u32;
                    }
                }
            }
        }

        refresh_runtime_ctx(rt_ctx, vm, session.task_id);
        JitExitKind::ContinueTaskLoop as u32
    }
);

def_jit_helper!(
    fn helper_call_direct_fast(
        vm_opaque: *mut c_void,
        return_pc: u32,
        callee_raw: u32,
        packed_slots_and_cs: u32,
    ) -> u32 {
        // SAFETY: invoked synchronously from compiled JIT code for a pre-validated, non-closure,
        // non-display, value-only direct `Opcode::Call` with arguments already in `r0..r{k-1}`.
        let (rt_ctx, session, vm) = unsafe { unpack_ctx(vm_opaque) };
        let callee_id = FuncId(callee_raw);
        let callee_slots = (packed_slots_and_cs >> 1) as usize;
        let clobbers_cs = (packed_slots_and_cs & 1) != 0;

        // Ensure callee is compiled once hot.
        if vm.worker.jit.lookup_entry(callee_id, 0).is_none() {
            vm.maybe_compile_on_hotness(callee_id, jit::ENTRY_HOTNESS_INCREMENT);
        }

        // Pop a pre-sized slot vector from `slot_vec_pool` before borrowing `task_mut`.
        let pre_slots = if callee_slots > 0 {
            let mut v = vm
                .worker
                .slot_vec_pool
                .pop()
                .unwrap_or_else(|| Vec::with_capacity(callee_slots));
            v.resize(callee_slots, TaggedValue::UNIT);
            v
        } else {
            Vec::new()
        };

        // Single `task_mut` borrow to push `CallFrame` and install `pre_slots` + `current_func`!
        let (regs_ptr, slots_ptr, slots_len) = {
            let Some(t) = vm.task_mut(session.task_id) else {
                if pre_slots.capacity() > 0 {
                    vm.recycle_frame_slots(pre_slots);
                }
                session.pending_error = Some(VmError::InvalidTask);
                return JitExitKind::Trap as u32;
            };
            if t.unwinding_cancel || t.cancelled {
                if pre_slots.capacity() > 0 {
                    vm.recycle_frame_slots(pre_slots);
                }
                return JitExitKind::Cancelled as u32;
            }
            let task: &mut runtime::StackContext = t;
            let mut saved = runtime::SavedRegs::new();
            if clobbers_cs {
                for &r in Reg::CALLEE_SAVED {
                    saved.push(Reg(r), task.registers.get(Reg(r)));
                }
            }
            let prev_slots = std::mem::replace(&mut task.local_slots, pre_slots);
            let frame = runtime::CallFrame {
                display_state: task.display_state.take(),
                return_pc,
                func_id: task.current_func,
                saved_regs: saved,
                local_slots: prev_slots,
                unwind_stack: std::mem::take(&mut task.unwind_stack),
                is_unwind_target: std::mem::replace(&mut task.is_unwind_target, false),
            };
            task.call_stack.push(frame);
            task.current_func = callee_id;
            task.pc = 0;
            let s_len = task.local_slots.len();
            let s_ptr = if s_len == 0 {
                std::ptr::null_mut()
            } else {
                task.local_slots.as_mut_ptr().cast::<u64>()
            };
            (task.registers.regs.as_mut_ptr().cast::<u64>(), s_ptr, s_len)
        };

        rt_ctx.regs_ptr = regs_ptr;
        rt_ctx.slots_ptr = slots_ptr;
        rt_ctx.slots_len = slots_len;

        let mut callee_pc = 0u32;
        while session.jit_call_depth < MAX_NATIVE_JIT_CALL_DEPTH {
            let Some((callee_fn, callee_off, allow_tu, allow_t2)) =
                vm.worker.jit.lookup_entry(callee_id, callee_pc)
            else {
                break;
            };

            let saved_allow_tu = rt_ctx.allow_tier_up;
            let saved_allow_t2 = rt_ctx.allow_tier2_loops;
            rt_ctx.allow_tier_up = u8::from(allow_tu);
            rt_ctx.allow_tier2_loops = u8::from(allow_t2);

            let mut callee_exit = JitExit::default();
            session.jit_call_depth += 1;
            // SAFETY: `callee_fn` points to a sealed W^X `ExecutableBuffer` in `vm.worker.jit`;
            // `rt_ctx` and `session` remain live across the synchronous call.
            unsafe {
                callee_fn(
                    std::ptr::from_mut(rt_ctx).cast::<c_void>(),
                    rt_ctx.regs_ptr,
                    callee_off,
                    std::ptr::from_mut(&mut callee_exit),
                );
            }
            session.jit_call_depth -= 1;
            rt_ctx.allow_tier_up = saved_allow_tu;
            rt_ctx.allow_tier2_loops = saved_allow_t2;

            if callee_exit.kind == JitExitKind::Return as u32 {
                let ret_val = TaggedValue::from_raw(callee_exit.value);
                // Ultra-fast single-borrow frame pop when no unwind/display state is active!
                let mut fast_popped_slots = None;
                let fast_ok = if let Some(t) = vm.task_mut(session.task_id) {
                    if !t.unwinding_cancel && t.display_state.is_none() {
                        if let Some(frame) = t.call_stack.pop() {
                            let task: &mut runtime::StackContext = t;
                            task.pc = frame.return_pc;
                            task.current_func = frame.func_id;
                            fast_popped_slots =
                                Some(std::mem::replace(&mut task.local_slots, frame.local_slots));
                            task.display_state = frame.display_state;
                            task.unwind_stack = frame.unwind_stack;
                            task.is_unwind_target = frame.is_unwind_target;
                            if !frame.saved_regs.is_empty() {
                                for (reg, saved_val) in &frame.saved_regs {
                                    task.registers.set(*reg, *saved_val);
                                }
                            }
                            task.registers.set(Reg(0), ret_val);
                            rt_ctx.regs_ptr = task.registers.regs.as_mut_ptr().cast::<u64>();
                            rt_ctx.slots_len = task.local_slots.len();
                            rt_ctx.slots_ptr = if rt_ctx.slots_len == 0 {
                                std::ptr::null_mut()
                            } else {
                                task.local_slots.as_mut_ptr().cast::<u64>()
                            };
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };
                if let Some(recycled) = fast_popped_slots {
                    vm.recycle_frame_slots(recycled);
                }
                if fast_ok {
                    return 0;
                }
                return complete_return_in_jit(rt_ctx, session, vm, callee_exit.pc, ret_val);
            }

            match callee_exit.kind {
                x if x == JitExitKind::TierUp as u32 => {
                    vm.compile_jit_function(callee_id, true);
                    callee_pc = callee_exit.pc;
                    if let Some(t) = vm.task_mut(session.task_id) {
                        t.pc = callee_pc;
                    }
                    refresh_runtime_ctx(rt_ctx, vm, session.task_id);
                    continue;
                }
                x if x == JitExitKind::InterpretAt as u32 => {
                    if let Some(t) = vm.task_mut(session.task_id) {
                        t.pc = callee_exit.pc;
                    }
                    return JitExitKind::ContinueTaskLoop as u32;
                }
                x if x == JitExitKind::ContinueTaskLoop as u32 => {
                    return JitExitKind::ContinueTaskLoop as u32;
                }
                x if x == JitExitKind::Yield as u32 => {
                    if let Some(t) = vm.task_mut(session.task_id) {
                        t.pc = callee_exit.pc;
                    }
                    return JitExitKind::YieldAtTaskPc as u32;
                }
                x if x == JitExitKind::YieldAtTaskPc as u32 => {
                    return JitExitKind::YieldAtTaskPc as u32;
                }
                x if x == JitExitKind::SuspendRetry as u32 => {
                    if let Some(t) = vm.task_mut(session.task_id) {
                        t.pc = callee_exit.pc;
                    }
                    return JitExitKind::SuspendAtTaskPc as u32;
                }
                x if x == JitExitKind::SuspendAtTaskPc as u32 => {
                    return JitExitKind::SuspendAtTaskPc as u32;
                }
                x if x == JitExitKind::Finished as u32 => {
                    return JitExitKind::Finished as u32;
                }
                x if x == JitExitKind::Cancelled as u32 => {
                    return JitExitKind::Cancelled as u32;
                }
                _ => {
                    if session.pending_error.is_none() {
                        session.pending_error = Some(VmError::InvalidInstruction(callee_exit.kind));
                    }
                    return JitExitKind::Trap as u32;
                }
            }
        }

        refresh_runtime_ctx(rt_ctx, vm, session.task_id);
        JitExitKind::ContinueTaskLoop as u32
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
    let tp = vm.type_pool_read();
    let canon_ty = tp.canonical_type(header.type_index)?;
    if canon_ty != header.type_index || tp.is_reserved_collection_role(canon_ty) {
        return None;
    }
    let words = header.payload_words();
    if words >= (1 << 19) || (field_idx as usize) >= words {
        return None;
    }
    let field_ty = match &tp.get(canon_ty).kind {
        TypeKind::Struct { fields, .. } if fields.len() == words => {
            fields.get(field_idx as usize)?.ty
        }
        TypeKind::Tuple { elements } if elements.len() == words => {
            *elements.get(field_idx as usize)?
        }
        TypeKind::Newtype { inner, .. } if words == 1 && field_idx == 0 => *inner,
        _ => return None,
    };
    let canon_field_ty = tp.canonical_type(field_ty)?;
    if tp.contains_associated_type(canon_field_ty) {
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
    let can_fast_load = {
        let tp = vm.type_pool_read();
        let is_identity_leaf = |t: type_pool::TypeIndex| -> bool {
            match &tp.get(t).kind {
                TypeKind::Intrinsic(
                    Intrinsic::Any
                    | Intrinsic::I64
                    | Intrinsic::U64
                    | Intrinsic::Isize
                    | Intrinsic::Usize
                    | Intrinsic::I128
                    | Intrinsic::U128
                    | Intrinsic::F64
                    | Intrinsic::Bool
                    | Intrinsic::Char
                    | Intrinsic::Str,
                ) => true,
                TypeKind::Struct { .. } => !tp.is_reserved_collection_role(t),
                TypeKind::Enum { .. } => true,
                _ => false,
            }
        };
        if is_identity_leaf(canon_field_ty) {
            true
        } else if let TypeKind::Optional { inner } = &tp.get(canon_field_ty).kind {
            tp.canonical_type(*inner).is_some_and(is_identity_leaf)
        } else {
            false
        }
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
    let (fast_kind, expected_val_type) = {
        let tp = vm.type_pool_read();
        match &tp.get(canon_field_ty).kind {
            TypeKind::Intrinsic(Intrinsic::I64) => (FieldStoreFastKind::SmallI64, u32::MAX),
            TypeKind::Intrinsic(Intrinsic::Bool) => (FieldStoreFastKind::Bool, u32::MAX),
            TypeKind::Intrinsic(Intrinsic::F64) => (FieldStoreFastKind::CanonicalF64, u32::MAX),
            TypeKind::Intrinsic(Intrinsic::Usize) => (FieldStoreFastKind::Usize, u32::MAX),
            TypeKind::Struct { .. } if !tp.is_reserved_collection_role(canon_field_ty) => (
                FieldStoreFastKind::ExactHeapNominal,
                canon_field_ty.as_u32(),
            ),
            TypeKind::Optional { inner } => match tp.canonical_type(*inner) {
                Some(ci) if tp.as_intrinsic(ci) == Some(Intrinsic::I64) => {
                    (FieldStoreFastKind::OptionalSmallI64, u32::MAX)
                }
                Some(ci)
                    if matches!(tp.get(ci).kind, TypeKind::Struct { .. })
                        && !tp.is_reserved_collection_role(ci) =>
                {
                    (FieldStoreFastKind::OptionalNominal, ci.as_u32())
                }
                _ => (FieldStoreFastKind::SlowOnly, u32::MAX),
            },
            _ => (FieldStoreFastKind::SlowOnly, u32::MAX),
        }
    };
    if fast_kind != FieldStoreFastKind::SlowOnly {
        ic.receiver_type = raw_ty;
        ic.expected_gc_meta_masked = expected_gc_meta_masked;
        ic.fast_kind = fast_kind as u32;
        ic.expected_val_type = expected_val_type;
    }
}
