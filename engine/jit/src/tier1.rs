//! Tier-1 Baseline Native Compiler (`x86_64` SysV64).
//!
//! Compiles an entire NSBC function into native `x86_64` code with:
//! - Direct native relative jumps (`jmp` / `jcc`) for all intra-function control flow
//! - Per-PC native offset table enabling instant loop back-edge OSR and post-call re-entry
//! - Inline 57-bit signed `IMM_I_SMALL` fast paths with hardware `JO` overflow checks
//! - Monomorphic inline caches (`MonomorphicIc`) for `LoadField` and `StoreField`
//! - Inline cooperative GC / preemption safepoint polling

use std::pin::Pin;

use nsbc::{AddrMode, Instruction, InstructionData, Opcode, Reg};
use runtime::{GP_REGISTER_COUNT, ids};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::code_cache::JitError;
use crate::layout::{
    GC_META_ORDINARY_1WORD, GC_META_ORDINARY_3WORDS, GC_META_ROLE_AND_WORDS_MASK,
    GC_META_ROLE_MASK, HDR_GC_META_DISP, HDR_TYPE_INDEX_DISP, IMM_BOOL_MASK_NO_BIT7, IMM_F64_TAG,
    IMM_I_SMALL_TAG, IMM_LOW7_MASK, IMM_U_SMALL_TAG, IMM_USIZE_TAG, INTRINSIC_F64_RAW,
    ROT_F64_TAG_MASK, TAG_FALSE, TAG_NULL, TAG_TRUE, TAG_UNIT,
};
use crate::x86_64::{Assembler, Cond, GpReg, Label, XmmReg};
use crate::{
    CompilationInput, CompileConstant, EmittedCode, FieldStoreFastKind, JitExit, JitExitKind,
    JitRuntimeCtx, MonomorphicIc, TIER2_HOTNESS_THRESHOLD,
};

/// Compile `input` with the Tier-1 baseline compiler.
pub fn compile_function_tier1(input: &CompilationInput<'_>) -> Result<EmittedCode, JitError> {
    if input.instructions.is_empty() {
        return Err(JitError::UnsupportedBytecode("empty function"));
    }

    let (ics, ic_map) = allocate_function_ics(input.instructions);
    let mut asm = Assembler::with_capacity(input.instructions.len() * 32 + 128);

    let base_lbl = asm.new_label();
    let epilogue_lbl = asm.new_label();
    let finished_lbl = asm.new_label();

    let pc_labels: Vec<Label> = (0..input.instructions.len())
        .map(|_| asm.new_label())
        .collect();

    emit_prologue(&mut asm, base_lbl, pc_labels[0], 0);

    let mut pc_offsets = vec![u32::MAX; input.instructions.len()];

    for (pc, &word) in input.instructions.iter().enumerate() {
        asm.bind(pc_labels[pc]);
        pc_offsets[pc] = asm.offset() as u32;
        let ic_ptr = ic_map[pc];
        emit_baseline_instruction(
            &mut asm,
            input,
            pc,
            word,
            ic_ptr,
            &pc_labels,
            &pc_labels,
            finished_lbl,
            epilogue_lbl,
            true,
        )?;
    }

    asm.bind(finished_lbl);
    emit_exit(
        &mut asm,
        JitExitKind::Finished,
        input.instructions.len() as u32,
        None,
        epilogue_lbl,
    );

    asm.bind(epilogue_lbl);
    emit_epilogue(&mut asm, 0);

    let bytes = asm.finish()?;
    Ok(EmittedCode {
        bytes,
        pc_offsets: pc_offsets.into_boxed_slice(),
        ics,
        specialized_loops: 0,
    })
}

/// Allocate a pinned array of [`MonomorphicIc`] slots for all `LoadField` / `StoreField` sites.
pub(crate) fn allocate_function_ics(
    instructions: &[u32],
) -> (Pin<Box<[MonomorphicIc]>>, Vec<*mut MonomorphicIc>) {
    let mut count = 0usize;
    for &word in instructions {
        if let Some(instr) = Instruction::decode(word)
            && matches!(instr.opcode, Opcode::LoadField | Opcode::StoreField)
        {
            count += 1;
        }
    }
    let mut boxed: Pin<Box<[MonomorphicIc]>> =
        Box::into_pin(vec![MonomorphicIc::cold(); count].into_boxed_slice());
    let base_ptr = boxed.as_mut().get_mut().as_mut_ptr();
    let mut map = vec![std::ptr::null_mut(); instructions.len()];
    let mut next = 0usize;
    for (pc, &word) in instructions.iter().enumerate() {
        if let Some(instr) = Instruction::decode(word)
            && matches!(instr.opcode, Opcode::LoadField | Opcode::StoreField)
        {
            // SAFETY: `next < count`, and `boxed` is pinned on the heap so element addresses
            // remain valid and stationary for the entire lifetime of the compiled function.
            map[pc] = unsafe { base_ptr.add(next) };
            next += 1;
        }
    }
    (boxed, map)
}

/// Compute aligned stack-frame byte reservation below the 5 pushed callee-saved registers
/// so `rsp % 16 == 0` holds before every `call`.
#[inline]
pub(crate) fn aligned_frame_bytes(spill_slots: usize) -> i32 {
    let raw = spill_slots.saturating_mul(8);
    let aligned16 = (raw + 15) & !15;
    (aligned16 + 8) as i32
}

/// Emit function prologue and OSR entry dispatch (`entry_native_offset` in `rdx`).
pub(crate) fn emit_prologue(
    asm: &mut Assembler,
    base_lbl: Label,
    pc0_lbl: Label,
    spill_slots: usize,
) {
    asm.bind(base_lbl);
    asm.push(GpReg::Rbp);
    asm.mov_rr(GpReg::Rbp, GpReg::Rsp);
    asm.push(GpReg::Rbx);
    asm.push(GpReg::R12);
    asm.push(GpReg::R13);
    asm.push(GpReg::R14);
    asm.push(GpReg::R15);

    let sub_bytes = aligned_frame_bytes(spill_slots);
    asm.sub_ri32(GpReg::Rsp, sub_bytes);

    // Pin ABI arguments into callee-saved registers:
    // rdi = vm_opaque (*mut JitRuntimeCtx), rsi = regs_ptr, rdx = entry_native_offset, rcx = out_exit
    asm.mov_rr(GpReg::R12, GpReg::Rdi);
    asm.mov_rr(GpReg::R13, GpReg::Rsi);
    asm.mov_rm64(GpReg::R14, GpReg::R12, JitRuntimeCtx::OFF_SLOTS_PTR);
    asm.mov_rr(GpReg::R15, GpReg::Rcx);

    // Dispatch OSR when `entry_native_offset != 0`.
    asm.test_rr(GpReg::Rdx, GpReg::Rdx);
    asm.jcc(Cond::E, pc0_lbl);
    asm.lea_rip_label(GpReg::Rax, base_lbl);
    asm.add_rr(GpReg::Rax, GpReg::Rdx);
    asm.jmp_r(GpReg::Rax);
}

/// Emit function epilogue restoring stack alignment and callee-saved registers.
pub(crate) fn emit_epilogue(asm: &mut Assembler, spill_slots: usize) {
    let sub_bytes = aligned_frame_bytes(spill_slots);
    asm.add_ri32(GpReg::Rsp, sub_bytes);
    asm.pop(GpReg::R15);
    asm.pop(GpReg::R14);
    asm.pop(GpReg::R13);
    asm.pop(GpReg::R12);
    asm.pop(GpReg::Rbx);
    asm.pop(GpReg::Rbp);
    asm.ret();
}

/// Write `JitExit` fields to `[r15]` and jump to `epilogue_lbl`.
pub(crate) fn emit_exit(
    asm: &mut Assembler,
    kind: JitExitKind,
    pc: u32,
    value_reg: Option<GpReg>,
    epilogue_lbl: Label,
) {
    asm.mov_mi32(GpReg::R15, JitExit::OFF_KIND, kind as u32);
    asm.mov_mi32(GpReg::R15, JitExit::OFF_PC, pc);
    if let Some(reg) = value_reg {
        asm.mov_mr64(GpReg::R15, JitExit::OFF_VALUE, reg);
    }
    asm.jmp(epilogue_lbl);
}

/// Reload `r13 = regs_ptr` and `r14 = slots_ptr` after any helper call that may allocate or resize.
#[inline]
pub(crate) fn emit_reload_ctx_ptrs(asm: &mut Assembler) {
    asm.mov_rm64(GpReg::R13, GpReg::R12, JitRuntimeCtx::OFF_REGS_PTR);
    asm.mov_rm64(GpReg::R14, GpReg::R12, JitRuntimeCtx::OFF_SLOTS_PTR);
}

/// Reload pointers after a C helper call and exit if `eax != 0` (`JitExitKind`).
pub(crate) fn emit_after_helper_check(asm: &mut Assembler, exit_pc: u32, epilogue_lbl: Label) {
    emit_reload_ctx_ptrs(asm);
    asm.test_rr(GpReg::Rax, GpReg::Rax);
    let ok_lbl = asm.new_label();
    asm.jcc(Cond::E, ok_lbl);
    asm.mov_mr32(GpReg::R15, JitExit::OFF_KIND, GpReg::Rax);
    asm.mov_mi32(GpReg::R15, JitExit::OFF_PC, exit_pc);
    asm.jmp(epilogue_lbl);
    asm.bind(ok_lbl);
}

/// Emit an inline cooperative safepoint check that calls `helpers.safepoint` only when needed.
pub(crate) fn emit_inline_safepoint(
    asm: &mut Assembler,
    input: &CompilationInput<'_>,
    resume_pc: u32,
    epilogue_lbl: Label,
) {
    let slow_lbl = asm.new_label();
    let budget_zero_lbl = asm.new_label();
    let done_lbl = asm.new_label();

    // Check `GC_FLAGS.gc_requested || GC_FLAGS.preempt_requested`.
    asm.mov_rm64(GpReg::Rax, GpReg::R12, JitRuntimeCtx::OFF_GC_REQ_PTR);
    asm.movzx_rm8(GpReg::Rax, GpReg::Rax, 0);
    asm.mov_rm64(GpReg::Rcx, GpReg::R12, JitRuntimeCtx::OFF_PREEMPT_REQ_PTR);
    asm.movzx_rm8(GpReg::Rcx, GpReg::Rcx, 0);
    asm.or_rr(GpReg::Rax, GpReg::Rcx);
    asm.jcc(Cond::Ne, slow_lbl);

    // Decrement `slice_safepoints`; when budget expires, enter `helper_safepoint`
    // so live `is_cancelled(task_id)` and `has_other_ready_tasks()` are always polled.
    asm.mov_rm32(GpReg::Rax, GpReg::R12, JitRuntimeCtx::OFF_SLICE_SAFEPOINTS);
    asm.sub_ri32(GpReg::Rax, 1);
    asm.jcc(Cond::Be, budget_zero_lbl);
    asm.mov_mr32(GpReg::R12, JitRuntimeCtx::OFF_SLICE_SAFEPOINTS, GpReg::Rax);
    asm.jmp(done_lbl);

    asm.bind(budget_zero_lbl);
    asm.mov_mi32(GpReg::R12, JitRuntimeCtx::OFF_SLICE_SAFEPOINTS, 1);

    asm.bind(slow_lbl);
    asm.mov_rr(GpReg::Rdi, GpReg::R12);
    asm.mov_ri64(GpReg::Rsi, resume_pc as u64);
    asm.mov_ri64(GpReg::Rax, input.helpers.safepoint as usize as u64);
    asm.call_r(GpReg::Rax);
    emit_after_helper_check(asm, resume_pc, epilogue_lbl);

    asm.bind(done_lbl);
}

#[inline]
fn reg_disp(reg: Reg) -> i32 {
    (reg.0 as i32) * 8
}

/// Emit baseline native code for a single bytecode instruction at `pc`.
///
/// `forward_targets` is used for forward jumps (`offset > 0`) and `backward_targets` is used
/// for backward jumps (`offset <= 0`) so Tier-2 can route preheader entries to loop guards
/// while keeping baseline fallback back-edges inside the baseline skeleton.
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_baseline_instruction(
    asm: &mut Assembler,
    input: &CompilationInput<'_>,
    pc: usize,
    word: u32,
    ic_ptr: *mut MonomorphicIc,
    forward_targets: &[Label],
    backward_targets: &[Label],
    finished_lbl: Label,
    epilogue_lbl: Label,
    allow_tier_up_check: bool,
) -> Result<(), JitError> {
    let Some(instr) = Instruction::decode(word) else {
        // Delegate malformed instruction words to `step_inline_op` so `InvalidInstruction` is raised.
        emit_step_inline_call(asm, input, pc as u32, word, epilogue_lbl);
        return Ok(());
    };

    match instr.opcode {
        // ── R-type integer fast-path arithmetic ──────────────────────────
        Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::Mod => {
            let InstructionData::R { dst, src1, src2 } = instr.data else {
                unreachable!()
            };
            let not_small_i64_lbl = asm.new_label();
            let slow_lbl = asm.new_label();
            let done_lbl = asm.new_label();

            asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(src1));
            asm.mov_rm64(GpReg::Rcx, GpReg::R13, reg_disp(src2));

            // Guard `(((a ^ 1) | (b ^ 1)) & 0x7f) == 0`
            asm.mov_rr(GpReg::Rdx, GpReg::Rax);
            asm.xor_ri32(GpReg::Rdx, IMM_I_SMALL_TAG as i32);
            asm.mov_rr(GpReg::Rsi, GpReg::Rcx);
            asm.xor_ri32(GpReg::Rsi, IMM_I_SMALL_TAG as i32);
            asm.or_rr(GpReg::Rdx, GpReg::Rsi);
            asm.test_ri32(GpReg::Rdx, IMM_LOW7_MASK as i32);
            asm.jcc(Cond::Ne, not_small_i64_lbl);

            match instr.opcode {
                Opcode::Add => {
                    asm.mov_rr(GpReg::Rdx, GpReg::Rax);
                    asm.sub_ri32(GpReg::Rdx, 1);
                    asm.add_rr(GpReg::Rdx, GpReg::Rcx);
                    asm.jcc(Cond::O, slow_lbl);
                }
                Opcode::Sub => {
                    asm.mov_rr(GpReg::Rdx, GpReg::Rax);
                    asm.sub_rr(GpReg::Rdx, GpReg::Rcx);
                    asm.jcc(Cond::O, slow_lbl);
                    asm.or_ri32(GpReg::Rdx, 1);
                }
                Opcode::Mul => {
                    asm.mov_rr(GpReg::Rdx, GpReg::Rax);
                    asm.sar_ri8(GpReg::Rdx, 7);
                    asm.mov_rr(GpReg::Rsi, GpReg::Rcx);
                    asm.sub_ri32(GpReg::Rsi, 1);
                    asm.imul_rr(GpReg::Rdx, GpReg::Rsi);
                    asm.jcc(Cond::O, slow_lbl);
                    asm.or_ri32(GpReg::Rdx, 1);
                }
                Opcode::Mod => {
                    asm.sar_ri8(GpReg::Rax, 7);
                    asm.sar_ri8(GpReg::Rcx, 7);
                    asm.test_rr(GpReg::Rcx, GpReg::Rcx);
                    asm.jcc(Cond::E, slow_lbl);
                    let not_neg1_lbl = asm.new_label();
                    let mod_done_lbl = asm.new_label();
                    asm.cmp_ri32(GpReg::Rcx, -1);
                    asm.jcc(Cond::Ne, not_neg1_lbl);
                    asm.mov_ri64(GpReg::Rdx, IMM_I_SMALL_TAG);
                    asm.jmp(mod_done_lbl);
                    asm.bind(not_neg1_lbl);
                    asm.cqo();
                    asm.idiv_r(GpReg::Rcx);
                    asm.shl_ri8(GpReg::Rdx, 7);
                    asm.or_ri32(GpReg::Rdx, 1);
                    asm.bind(mod_done_lbl);
                }
                _ => unreachable!(),
            }
            asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rdx);
            asm.jmp(done_lbl);

            asm.bind(not_small_i64_lbl);
            if matches!(instr.opcode, Opcode::Add | Opcode::Sub | Opcode::Mul) {
                emit_try_unbox_inline_f64(asm, GpReg::Rax, GpReg::Rdx, XmmReg::Xmm0, slow_lbl);
                emit_try_unbox_inline_f64(asm, GpReg::Rcx, GpReg::Rdx, XmmReg::Xmm1, slow_lbl);
                match instr.opcode {
                    Opcode::Add => asm.addsd(XmmReg::Xmm0, XmmReg::Xmm1),
                    Opcode::Sub => asm.subsd(XmmReg::Xmm0, XmmReg::Xmm1),
                    Opcode::Mul => asm.mulsd(XmmReg::Xmm0, XmmReg::Xmm1),
                    _ => unreachable!(),
                }
                emit_box_inline_f64_or_slow(asm, XmmReg::Xmm0, GpReg::Rdx, slow_lbl);
                asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rdx);
                asm.jmp(done_lbl);
            }

            asm.bind(slow_lbl);
            emit_numeric_slow_call(asm, input, pc as u32, word, epilogue_lbl);
            asm.bind(done_lbl);
        }

        Opcode::BitAnd | Opcode::BitOr | Opcode::BitXor => {
            let InstructionData::R { dst, src1, src2 } = instr.data else {
                unreachable!()
            };
            let slow_lbl = asm.new_label();
            let done_lbl = asm.new_label();

            asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(src1));
            asm.mov_rm64(GpReg::Rcx, GpReg::R13, reg_disp(src2));

            asm.mov_rr(GpReg::Rdx, GpReg::Rax);
            asm.xor_ri32(GpReg::Rdx, IMM_I_SMALL_TAG as i32);
            asm.mov_rr(GpReg::Rsi, GpReg::Rcx);
            asm.xor_ri32(GpReg::Rsi, IMM_I_SMALL_TAG as i32);
            asm.or_rr(GpReg::Rdx, GpReg::Rsi);
            asm.test_ri32(GpReg::Rdx, IMM_LOW7_MASK as i32);
            asm.jcc(Cond::Ne, slow_lbl);

            match instr.opcode {
                Opcode::BitAnd => asm.and_rr(GpReg::Rax, GpReg::Rcx),
                Opcode::BitOr => asm.or_rr(GpReg::Rax, GpReg::Rcx),
                Opcode::BitXor => {
                    asm.xor_rr(GpReg::Rax, GpReg::Rcx);
                    asm.or_ri32(GpReg::Rax, 1);
                }
                _ => unreachable!(),
            }
            asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
            asm.jmp(done_lbl);

            asm.bind(slow_lbl);
            emit_numeric_slow_call(asm, input, pc as u32, word, epilogue_lbl);
            asm.bind(done_lbl);
        }

        Opcode::Neg | Opcode::BitNot => {
            let InstructionData::R { dst, src1, .. } = instr.data else {
                unreachable!()
            };
            let not_small_i64_lbl = asm.new_label();
            let slow_lbl = asm.new_label();
            let done_lbl = asm.new_label();

            asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(src1));
            asm.mov_rr(GpReg::Rcx, GpReg::Rax);
            asm.and_ri32(GpReg::Rcx, IMM_LOW7_MASK as i32);
            asm.cmp_ri32(GpReg::Rcx, IMM_I_SMALL_TAG as i32);
            asm.jcc(Cond::Ne, not_small_i64_lbl);

            if instr.opcode == Opcode::Neg {
                asm.sub_ri32(GpReg::Rax, 1);
                asm.neg_r(GpReg::Rax);
                asm.jcc(Cond::O, slow_lbl);
                asm.or_ri32(GpReg::Rax, 1);
            } else {
                asm.not_r(GpReg::Rax);
                asm.and_ri32(GpReg::Rax, !(IMM_LOW7_MASK as i32));
                asm.or_ri32(GpReg::Rax, 1);
            }
            asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
            asm.jmp(done_lbl);

            asm.bind(not_small_i64_lbl);
            if instr.opcode == Opcode::Neg {
                emit_try_unbox_inline_f64(asm, GpReg::Rax, GpReg::Rcx, XmmReg::Xmm0, slow_lbl);
                asm.movq_rx(GpReg::Rdx, XmmReg::Xmm0);
                asm.mov_ri64(GpReg::Rcx, 1_u64 << 63);
                asm.xor_rr(GpReg::Rdx, GpReg::Rcx);
                asm.movq_xr(XmmReg::Xmm0, GpReg::Rdx);
                emit_box_inline_f64_or_slow(asm, XmmReg::Xmm0, GpReg::Rdx, slow_lbl);
                asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rdx);
                asm.jmp(done_lbl);
            }

            asm.bind(slow_lbl);
            emit_numeric_slow_call(asm, input, pc as u32, word, epilogue_lbl);
            asm.bind(done_lbl);
        }

        Opcode::Div => {
            let InstructionData::R { dst, src1, src2 } = instr.data else {
                unreachable!()
            };
            let not_small_i64_lbl = asm.new_label();
            let slow_lbl = asm.new_label();
            let done_lbl = asm.new_label();

            asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(src1));
            asm.mov_rm64(GpReg::Rcx, GpReg::R13, reg_disp(src2));

            asm.mov_rr(GpReg::Rdx, GpReg::Rax);
            asm.xor_ri32(GpReg::Rdx, IMM_I_SMALL_TAG as i32);
            asm.mov_rr(GpReg::Rsi, GpReg::Rcx);
            asm.xor_ri32(GpReg::Rsi, IMM_I_SMALL_TAG as i32);
            asm.or_rr(GpReg::Rdx, GpReg::Rsi);
            asm.test_ri32(GpReg::Rdx, IMM_LOW7_MASK as i32);
            asm.jcc(Cond::Ne, not_small_i64_lbl);

            asm.sar_ri8(GpReg::Rax, 7);
            asm.sar_ri8(GpReg::Rcx, 7);
            asm.test_rr(GpReg::Rcx, GpReg::Rcx);
            asm.jcc(Cond::E, slow_lbl);
            asm.cmp_ri32(GpReg::Rcx, -1);
            asm.jcc(Cond::E, slow_lbl);
            asm.cqo();
            asm.idiv_r(GpReg::Rcx);
            asm.shl_ri8(GpReg::Rax, 7);
            asm.or_ri32(GpReg::Rax, 1);
            asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
            asm.jmp(done_lbl);

            asm.bind(not_small_i64_lbl);
            emit_try_unbox_inline_f64(asm, GpReg::Rax, GpReg::Rdx, XmmReg::Xmm0, slow_lbl);
            emit_try_unbox_inline_f64(asm, GpReg::Rcx, GpReg::Rdx, XmmReg::Xmm1, slow_lbl);
            asm.xorpd(XmmReg::Xmm2, XmmReg::Xmm2);
            asm.ucomisd(XmmReg::Xmm1, XmmReg::Xmm2);
            asm.jcc(Cond::E, slow_lbl);
            asm.divsd(XmmReg::Xmm0, XmmReg::Xmm1);
            emit_box_inline_f64_or_slow(asm, XmmReg::Xmm0, GpReg::Rdx, slow_lbl);
            asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rdx);
            asm.jmp(done_lbl);

            asm.bind(slow_lbl);
            emit_numeric_slow_call(asm, input, pc as u32, word, epilogue_lbl);
            asm.bind(done_lbl);
        }

        Opcode::Shl | Opcode::Shr | Opcode::UShr => {
            emit_numeric_slow_call(asm, input, pc as u32, word, epilogue_lbl);
        }

        // ── R-type comparisons ───────────────────────────────────────────
        Opcode::CmpEq
        | Opcode::CmpNe
        | Opcode::CmpLt
        | Opcode::CmpLe
        | Opcode::CmpGt
        | Opcode::CmpGe => {
            let InstructionData::R { dst, src1, src2 } = instr.data else {
                unreachable!()
            };
            let check_usize_lbl = asm.new_label();
            let check_u64_lbl = asm.new_label();
            let check_f64_lbl = asm.new_label();
            let slow_lbl = asm.new_label();
            let done_lbl = asm.new_label();

            asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(src1));
            asm.mov_rm64(GpReg::Rcx, GpReg::R13, reg_disp(src2));

            asm.mov_rr(GpReg::Rdx, GpReg::Rax);
            asm.xor_ri32(GpReg::Rdx, IMM_I_SMALL_TAG as i32);
            asm.mov_rr(GpReg::Rsi, GpReg::Rcx);
            asm.xor_ri32(GpReg::Rsi, IMM_I_SMALL_TAG as i32);
            asm.or_rr(GpReg::Rdx, GpReg::Rsi);
            asm.test_ri32(GpReg::Rdx, IMM_LOW7_MASK as i32);
            asm.jcc(Cond::Ne, check_usize_lbl);

            let signed_cond = match instr.opcode {
                Opcode::CmpEq => Cond::E,
                Opcode::CmpNe => Cond::Ne,
                Opcode::CmpLt => Cond::L,
                Opcode::CmpLe => Cond::Le,
                Opcode::CmpGt => Cond::G,
                Opcode::CmpGe => Cond::Ge,
                _ => unreachable!(),
            };
            asm.cmp_rr(GpReg::Rax, GpReg::Rcx);
            asm.mov_ri64(GpReg::Rdx, TAG_FALSE);
            asm.mov_ri64(GpReg::Rsi, TAG_TRUE);
            asm.cmovcc(signed_cond, GpReg::Rdx, GpReg::Rsi);
            asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rdx);
            asm.jmp(done_lbl);

            // Fast unsigned compare when both are `IMM_USIZE` or both are `IMM_U_SMALL`
            let unsigned_cond = match instr.opcode {
                Opcode::CmpEq => Cond::E,
                Opcode::CmpNe => Cond::Ne,
                Opcode::CmpLt => Cond::B,
                Opcode::CmpLe => Cond::Be,
                Opcode::CmpGt => Cond::A,
                Opcode::CmpGe => Cond::Ae,
                _ => unreachable!(),
            };
            let do_unsigned_cmp_lbl = asm.new_label();
            asm.bind(check_usize_lbl);
            asm.mov_rr(GpReg::Rdx, GpReg::Rax);
            asm.xor_ri32(GpReg::Rdx, IMM_USIZE_TAG as i32);
            asm.mov_rr(GpReg::Rsi, GpReg::Rcx);
            asm.xor_ri32(GpReg::Rsi, IMM_USIZE_TAG as i32);
            asm.or_rr(GpReg::Rdx, GpReg::Rsi);
            asm.test_ri32(GpReg::Rdx, IMM_LOW7_MASK as i32);
            asm.jcc(Cond::E, do_unsigned_cmp_lbl);

            asm.bind(check_u64_lbl);
            asm.mov_rr(GpReg::Rdx, GpReg::Rax);
            asm.xor_ri32(GpReg::Rdx, IMM_U_SMALL_TAG as i32);
            asm.mov_rr(GpReg::Rsi, GpReg::Rcx);
            asm.xor_ri32(GpReg::Rsi, IMM_U_SMALL_TAG as i32);
            asm.or_rr(GpReg::Rdx, GpReg::Rsi);
            asm.test_ri32(GpReg::Rdx, IMM_LOW7_MASK as i32);
            asm.jcc(Cond::Ne, check_f64_lbl);

            asm.bind(do_unsigned_cmp_lbl);
            asm.cmp_rr(GpReg::Rax, GpReg::Rcx);
            asm.mov_ri64(GpReg::Rdx, TAG_FALSE);
            asm.mov_ri64(GpReg::Rsi, TAG_TRUE);
            asm.cmovcc(unsigned_cond, GpReg::Rdx, GpReg::Rsi);
            asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rdx);
            asm.jmp(done_lbl);

            // Fast inline SSE2 `f64` compare (`ucomisd`, delegating unordered NaN `PF=1` to slow path)
            asm.bind(check_f64_lbl);
            emit_try_unbox_inline_f64(asm, GpReg::Rax, GpReg::Rdx, XmmReg::Xmm0, slow_lbl);
            emit_try_unbox_inline_f64(asm, GpReg::Rcx, GpReg::Rdx, XmmReg::Xmm1, slow_lbl);
            asm.ucomisd(XmmReg::Xmm0, XmmReg::Xmm1);
            asm.jcc(Cond::P, slow_lbl);
            asm.mov_ri64(GpReg::Rdx, TAG_FALSE);
            asm.mov_ri64(GpReg::Rsi, TAG_TRUE);
            asm.cmovcc(unsigned_cond, GpReg::Rdx, GpReg::Rsi);
            asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rdx);
            asm.jmp(done_lbl);

            asm.bind(slow_lbl);
            asm.mov_rr(GpReg::Rdi, GpReg::R12);
            asm.mov_ri64(GpReg::Rsi, word as u64);
            asm.mov_ri64(GpReg::Rax, input.helpers.cmp_slow as usize as u64);
            asm.call_r(GpReg::Rax);
            emit_after_helper_check(asm, pc as u32, epilogue_lbl);
            asm.bind(done_lbl);
        }

        // ── Register & local slot moves ──────────────────────────────────
        Opcode::Mov => {
            let InstructionData::R { dst, src1, .. } = instr.data else {
                unreachable!()
            };
            if dst != src1 {
                asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(src1));
                asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
            }
        }

        Opcode::Swap => {
            let InstructionData::R { dst, src1, .. } = instr.data else {
                unreachable!()
            };
            if dst != src1 {
                asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(dst));
                asm.mov_rm64(GpReg::Rcx, GpReg::R13, reg_disp(src1));
                asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rcx);
                asm.mov_mr64(GpReg::R13, reg_disp(src1), GpReg::Rax);
            }
        }

        Opcode::LoadSlot | Opcode::StoreSlot => {
            let InstructionData::A { dst, base, imm12 } = instr.data else {
                unreachable!()
            };
            let index = ((base.0 as usize) << 12) | (imm12 as usize);
            let byte_off = index.checked_mul(8).and_then(|b| i32::try_from(b).ok());
            if let Some(disp) = byte_off {
                if pc > 0 && index < (input.guaranteed_slots as usize) {
                    if instr.opcode == Opcode::LoadSlot {
                        asm.mov_rm64(GpReg::Rax, GpReg::R14, disp);
                        asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
                    } else {
                        asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(dst));
                        asm.mov_mr64(GpReg::R14, disp, GpReg::Rax);
                    }
                } else {
                    let slow_lbl = asm.new_label();
                    let done_lbl = asm.new_label();
                    asm.mov_rm64(GpReg::Rcx, GpReg::R12, JitRuntimeCtx::OFF_SLOTS_LEN);
                    asm.mov_ri64(GpReg::Rax, index as u64);
                    asm.cmp_rr(GpReg::Rax, GpReg::Rcx);
                    asm.jcc(Cond::Ae, slow_lbl);
                    if instr.opcode == Opcode::LoadSlot {
                        asm.mov_rm64(GpReg::Rax, GpReg::R14, disp);
                        asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
                    } else {
                        asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(dst));
                        asm.mov_mr64(GpReg::R14, disp, GpReg::Rax);
                    }
                    asm.jmp(done_lbl);

                    asm.bind(slow_lbl);
                    emit_step_inline_call(asm, input, pc as u32, word, epilogue_lbl);
                    asm.bind(done_lbl);
                }
            } else {
                emit_step_inline_call(asm, input, pc as u32, word, epilogue_lbl);
            }
        }

        Opcode::AllocateSlots => {
            let InstructionData::E { payload } = instr.data else {
                unreachable!()
            };
            if payload <= (1 << 17) {
                let done_lbl = asm.new_label();
                asm.mov_rm64(GpReg::Rcx, GpReg::R12, JitRuntimeCtx::OFF_SLOTS_LEN);
                asm.cmp_ri32(GpReg::Rcx, payload as i32);
                asm.jcc(Cond::E, done_lbl);
                asm.mov_rr(GpReg::Rdi, GpReg::R12);
                asm.mov_ri64(GpReg::Rsi, payload as u64);
                asm.mov_ri64(GpReg::Rax, input.helpers.alloc_slots as usize as u64);
                asm.call_r(GpReg::Rax);
                emit_after_helper_check(asm, pc as u32, epilogue_lbl);
                asm.bind(done_lbl);
            } else {
                asm.mov_rr(GpReg::Rdi, GpReg::R12);
                asm.mov_ri64(GpReg::Rsi, payload as u64);
                asm.mov_ri64(GpReg::Rax, input.helpers.alloc_slots as usize as u64);
                asm.call_r(GpReg::Rax);
                emit_after_helper_check(asm, pc as u32, epilogue_lbl);
            }
        }

        // ── Immediate & Constant Loads ───────────────────────────────────
        Opcode::Load => {
            let InstructionData::A { dst, imm12, .. } = instr.data else {
                unreachable!()
            };
            match instr.amode {
                AddrMode::Imm => {
                    let sext = Instruction::sext_imm12(imm12) as i64;
                    let raw = runtime::TaggedValue::from_i64(sext).raw();
                    asm.mov_ri64(GpReg::Rax, raw);
                    asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
                }
                AddrMode::Const => {
                    emit_const_load(asm, input, pc as u32, dst, imm12 as u32, epilogue_lbl);
                }
            }
        }

        Opcode::LoadConstWide => {
            let InstructionData::A { dst, base, imm12 } = instr.data else {
                unreachable!()
            };
            let idx = Instruction::wide_const_index(base, imm12);
            emit_const_load(asm, input, pc as u32, dst, idx, epilogue_lbl);
        }

        Opcode::LoadUnit => {
            let InstructionData::A { dst, .. } = instr.data else {
                unreachable!()
            };
            asm.mov_ri64(GpReg::Rax, TAG_UNIT);
            asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
        }

        Opcode::LoadTrue => {
            let InstructionData::A { dst, .. } = instr.data else {
                unreachable!()
            };
            asm.mov_ri64(GpReg::Rax, TAG_TRUE);
            asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
        }

        Opcode::LoadFalse => {
            let InstructionData::A { dst, .. } = instr.data else {
                unreachable!()
            };
            asm.mov_ri64(GpReg::Rax, TAG_FALSE);
            asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
        }

        Opcode::LoadNull => {
            let InstructionData::A { dst, .. } = instr.data else {
                unreachable!()
            };
            asm.mov_ri64(GpReg::Rax, TAG_NULL);
            asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
        }

        // ── Type assertions & checks ─────────────────────────────────────
        Opcode::TypeCheck | Opcode::TypeCast | Opcode::TypeCastSafe | Opcode::TypeAssert => {
            let InstructionData::A {
                dst,
                base: src,
                imm12,
            } = instr.data
            else {
                unreachable!()
            };
            let has_scope = (input.has_scope)(pc);
            let canon_ty = if has_scope && instr.amode == AddrMode::Imm {
                input
                    .type_pool
                    .canonical_type(TypeIndex::from_raw(imm12 as u32))
            } else {
                None
            };

            if instr.opcode == Opcode::TypeAssert
                && let Some(ct) = canon_ty
                && let Some(fast_assert) = classify_fast_type_assert(input.type_pool, ct)
            {
                if matches!(fast_assert, FastTypeAssert::Any) {
                    if dst != src {
                        asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(src));
                        asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
                    }
                } else {
                    let slow_lbl = asm.new_label();
                    let ok_lbl = asm.new_label();
                    let done_lbl = asm.new_label();
                    asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(src));
                    match fast_assert {
                        FastTypeAssert::Any => unreachable!(),
                        FastTypeAssert::Unit => {
                            asm.cmp_ri32(GpReg::Rax, TAG_UNIT as i32);
                            asm.jcc(Cond::Ne, slow_lbl);
                        }
                        FastTypeAssert::I64 => {
                            asm.mov_rr(GpReg::Rcx, GpReg::Rax);
                            asm.and_ri32(GpReg::Rcx, IMM_LOW7_MASK as i32);
                            asm.cmp_ri32(GpReg::Rcx, IMM_I_SMALL_TAG as i32);
                            asm.jcc(Cond::Ne, slow_lbl);
                        }
                        FastTypeAssert::Usize => {
                            asm.mov_rr(GpReg::Rcx, GpReg::Rax);
                            asm.and_ri32(GpReg::Rcx, IMM_LOW7_MASK as i32);
                            asm.cmp_ri32(GpReg::Rcx, IMM_USIZE_TAG as i32);
                            asm.jcc(Cond::Ne, slow_lbl);
                        }
                        FastTypeAssert::U64 => {
                            asm.mov_rr(GpReg::Rcx, GpReg::Rax);
                            asm.and_ri32(GpReg::Rcx, IMM_LOW7_MASK as i32);
                            asm.cmp_ri32(GpReg::Rcx, IMM_U_SMALL_TAG as i32);
                            asm.jcc(Cond::Ne, slow_lbl);
                        }
                        FastTypeAssert::Bool => {
                            asm.mov_rr(GpReg::Rcx, GpReg::Rax);
                            asm.and_ri32(GpReg::Rcx, IMM_BOOL_MASK_NO_BIT7);
                            asm.cmp_ri32(GpReg::Rcx, TAG_FALSE as i32);
                            asm.jcc(Cond::Ne, slow_lbl);
                        }
                        FastTypeAssert::F64 => {
                            asm.test_ri32(GpReg::Rax, ROT_F64_TAG_MASK as i32);
                            asm.jcc(Cond::Ne, ok_lbl);
                            asm.mov_rr(GpReg::Rcx, GpReg::Rax);
                            asm.and_ri32(GpReg::Rcx, IMM_LOW7_MASK as i32);
                            asm.cmp_ri32(GpReg::Rcx, IMM_F64_TAG as i32);
                            asm.jcc(Cond::E, ok_lbl);
                            asm.test_rr(GpReg::Rax, GpReg::Rax);
                            asm.jcc(Cond::E, slow_lbl);
                            asm.test_ri32(GpReg::Rax, 0b111);
                            asm.jcc(Cond::Ne, slow_lbl);
                            asm.mov_rm32(GpReg::Rcx, GpReg::Rax, HDR_TYPE_INDEX_DISP);
                            asm.cmp_ri32(GpReg::Rcx, INTRINSIC_F64_RAW as i32);
                            asm.jcc(Cond::Ne, slow_lbl);
                            asm.mov_rm32(GpReg::Rcx, GpReg::Rax, HDR_GC_META_DISP);
                            asm.and_ri32(GpReg::Rcx, GC_META_ROLE_AND_WORDS_MASK);
                            asm.cmp_ri32(GpReg::Rcx, GC_META_ORDINARY_1WORD);
                            asm.jcc(Cond::Ne, slow_lbl);
                        }
                        FastTypeAssert::Struct {
                            raw_ty,
                            expected_gc_meta,
                            nullable,
                        } => {
                            if nullable {
                                asm.cmp_ri32(GpReg::Rax, TAG_NULL as i32);
                                asm.jcc(Cond::E, ok_lbl);
                            }
                            asm.test_rr(GpReg::Rax, GpReg::Rax);
                            asm.jcc(Cond::E, slow_lbl);
                            asm.test_ri32(GpReg::Rax, 0b111);
                            asm.jcc(Cond::Ne, slow_lbl);
                            asm.mov_rm32(GpReg::Rcx, GpReg::Rax, HDR_TYPE_INDEX_DISP);
                            asm.cmp_ri32(GpReg::Rcx, raw_ty as i32);
                            asm.jcc(Cond::Ne, slow_lbl);
                            asm.mov_rm32(GpReg::Rcx, GpReg::Rax, HDR_GC_META_DISP);
                            asm.and_ri32(GpReg::Rcx, GC_META_ROLE_AND_WORDS_MASK);
                            asm.cmp_ri32(GpReg::Rcx, expected_gc_meta);
                            asm.jcc(Cond::Ne, slow_lbl);
                        }
                    }
                    asm.bind(ok_lbl);
                    if dst != src {
                        asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
                    }
                    asm.jmp(done_lbl);

                    asm.bind(slow_lbl);
                    emit_type_op_call(asm, input, pc as u32, word, epilogue_lbl);
                    asm.bind(done_lbl);
                }
            } else if instr.opcode == Opcode::TypeCast
                && let Some(ct) = canon_ty
                && let Some(intr) = input.type_pool.as_intrinsic(ct)
                && matches!(
                    intr,
                    Intrinsic::Any | Intrinsic::Usize | Intrinsic::I64 | Intrinsic::F64
                )
            {
                if intr == Intrinsic::Any {
                    if dst != src {
                        asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(src));
                        asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
                    }
                } else {
                    let slow_lbl = asm.new_label();
                    let ok_lbl = asm.new_label();
                    let done_lbl = asm.new_label();
                    asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(src));
                    match intr {
                        Intrinsic::Usize => {
                            let check_usize_lbl = asm.new_label();
                            asm.mov_rr(GpReg::Rcx, GpReg::Rax);
                            asm.and_ri32(GpReg::Rcx, IMM_LOW7_MASK as i32);
                            asm.cmp_ri32(GpReg::Rcx, IMM_I_SMALL_TAG as i32);
                            asm.jcc(Cond::Ne, check_usize_lbl);
                            asm.test_rr(GpReg::Rax, GpReg::Rax);
                            asm.jcc(Cond::S, slow_lbl);
                            asm.xor_ri32(GpReg::Rax, (IMM_I_SMALL_TAG ^ IMM_USIZE_TAG) as i32);
                            asm.jmp(ok_lbl);
                            asm.bind(check_usize_lbl);
                            asm.cmp_ri32(GpReg::Rcx, IMM_USIZE_TAG as i32);
                            asm.jcc(Cond::Ne, slow_lbl);
                        }
                        Intrinsic::I64 => {
                            let check_usize_lbl = asm.new_label();
                            asm.mov_rr(GpReg::Rcx, GpReg::Rax);
                            asm.and_ri32(GpReg::Rcx, IMM_LOW7_MASK as i32);
                            asm.cmp_ri32(GpReg::Rcx, IMM_I_SMALL_TAG as i32);
                            asm.jcc(Cond::E, ok_lbl);
                            asm.bind(check_usize_lbl);
                            asm.cmp_ri32(GpReg::Rcx, IMM_USIZE_TAG as i32);
                            asm.jcc(Cond::Ne, slow_lbl);
                            asm.test_rr(GpReg::Rax, GpReg::Rax);
                            asm.jcc(Cond::S, slow_lbl);
                            asm.xor_ri32(GpReg::Rax, (IMM_I_SMALL_TAG ^ IMM_USIZE_TAG) as i32);
                        }
                        Intrinsic::F64 => {
                            let check_f64_lbl = asm.new_label();
                            asm.mov_rr(GpReg::Rcx, GpReg::Rax);
                            asm.and_ri32(GpReg::Rcx, IMM_LOW7_MASK as i32);
                            asm.cmp_ri32(GpReg::Rcx, IMM_I_SMALL_TAG as i32);
                            asm.jcc(Cond::Ne, check_f64_lbl);
                            asm.sar_ri8(GpReg::Rax, 7);
                            asm.cvtsi2sd(XmmReg::Xmm0, GpReg::Rax);
                            emit_box_inline_f64_or_slow(asm, XmmReg::Xmm0, GpReg::Rax, slow_lbl);
                            asm.jmp(ok_lbl);
                            asm.bind(check_f64_lbl);
                            asm.test_ri32(GpReg::Rax, ROT_F64_TAG_MASK as i32);
                            asm.jcc(Cond::E, slow_lbl);
                        }
                        _ => unreachable!(),
                    }
                    asm.bind(ok_lbl);
                    asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
                    asm.jmp(done_lbl);

                    asm.bind(slow_lbl);
                    emit_type_op_call(asm, input, pc as u32, word, epilogue_lbl);
                    asm.bind(done_lbl);
                }
            } else {
                emit_type_op_call(asm, input, pc as u32, word, epilogue_lbl);
            }
        }

        // ── Monomorphic Inline Caches for Struct/Tuple/Newtype fields ────
        Opcode::LoadField => {
            let InstructionData::A {
                dst,
                base: recv,
                imm12,
            } = instr.data
            else {
                unreachable!()
            };
            debug_assert!(!ic_ptr.is_null());
            if (input.has_scope)(pc) {
                let slow_lbl = asm.new_label();
                let done_lbl = asm.new_label();

                asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(recv));
                asm.test_rr(GpReg::Rax, GpReg::Rax);
                asm.jcc(Cond::E, slow_lbl);
                asm.test_ri32(GpReg::Rax, 0b111);
                asm.jcc(Cond::Ne, slow_lbl);

                asm.mov_ri64(GpReg::Rdx, ic_ptr as usize as u64);
                asm.mov_rm32(GpReg::Rcx, GpReg::Rax, HDR_TYPE_INDEX_DISP);
                asm.mov_rm32(GpReg::Rsi, GpReg::Rdx, MonomorphicIc::OFF_RECEIVER_TYPE);
                asm.cmp_rr(GpReg::Rcx, GpReg::Rsi);
                asm.jcc(Cond::Ne, slow_lbl);

                asm.mov_rm32(GpReg::Rcx, GpReg::Rax, HDR_GC_META_DISP);
                asm.and_ri32(GpReg::Rcx, GC_META_ROLE_AND_WORDS_MASK);
                asm.mov_rm32(GpReg::Rsi, GpReg::Rdx, MonomorphicIc::OFF_EXPECTED_GC_META);
                asm.cmp_rr(GpReg::Rcx, GpReg::Rsi);
                asm.jcc(Cond::Ne, slow_lbl);

                asm.mov_rm32(GpReg::Rcx, GpReg::Rdx, MonomorphicIc::OFF_FAST_KIND);
                asm.cmp_ri32(GpReg::Rcx, 1);
                asm.jcc(Cond::Ne, slow_lbl);

                let field_disp = (imm12 as i32) * 8;
                asm.mov_rm64(GpReg::Rax, GpReg::Rax, field_disp);
                // Reject uninitialized `UNIT` fields on fast path so `assert_value_type` checks legality.
                asm.cmp_ri32(GpReg::Rax, TAG_UNIT as i32);
                asm.jcc(Cond::E, slow_lbl);

                asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
                asm.jmp(done_lbl);

                asm.bind(slow_lbl);
                asm.mov_rr(GpReg::Rdi, GpReg::R12);
                asm.mov_ri64(GpReg::Rsi, pc as u64);
                asm.mov_ri64(GpReg::Rdx, word as u64);
                asm.mov_ri64(GpReg::Rcx, ic_ptr as usize as u64);
                asm.mov_ri64(GpReg::Rax, input.helpers.load_field_ic as usize as u64);
                asm.call_r(GpReg::Rax);
                emit_after_helper_check(asm, pc as u32, epilogue_lbl);
                asm.bind(done_lbl);
            } else {
                asm.mov_rr(GpReg::Rdi, GpReg::R12);
                asm.mov_ri64(GpReg::Rsi, pc as u64);
                asm.mov_ri64(GpReg::Rdx, word as u64);
                asm.mov_ri64(GpReg::Rcx, ic_ptr as usize as u64);
                asm.mov_ri64(GpReg::Rax, input.helpers.load_field_ic as usize as u64);
                asm.call_r(GpReg::Rax);
                emit_after_helper_check(asm, pc as u32, epilogue_lbl);
            }
        }

        Opcode::StoreField => {
            // Note: in NSBC, `dst` holds the source value register and `base` holds the target object register.
            let InstructionData::A {
                dst: src_val,
                base: recv_obj,
                imm12,
            } = instr.data
            else {
                unreachable!()
            };
            debug_assert!(!ic_ptr.is_null());
            if (input.has_scope)(pc) {
                let slow_lbl = asm.new_label();
                let store_ok_lbl = asm.new_label();
                let check_f64_lbl = asm.new_label();
                let check_bool_lbl = asm.new_label();
                let check_any_lbl = asm.new_label();
                let check_nominal_lbl = asm.new_label();
                let done_lbl = asm.new_label();

                asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(recv_obj));
                asm.test_rr(GpReg::Rax, GpReg::Rax);
                asm.jcc(Cond::E, slow_lbl);
                asm.test_ri32(GpReg::Rax, 0b111);
                asm.jcc(Cond::Ne, slow_lbl);

                asm.mov_ri64(GpReg::Rdx, ic_ptr as usize as u64);
                asm.mov_rm32(GpReg::Rcx, GpReg::Rax, HDR_TYPE_INDEX_DISP);
                asm.mov_rm32(GpReg::Rsi, GpReg::Rdx, MonomorphicIc::OFF_RECEIVER_TYPE);
                asm.cmp_rr(GpReg::Rcx, GpReg::Rsi);
                asm.jcc(Cond::Ne, slow_lbl);

                asm.mov_rm32(GpReg::Rcx, GpReg::Rax, HDR_GC_META_DISP);
                asm.and_ri32(GpReg::Rcx, GC_META_ROLE_AND_WORDS_MASK);
                asm.mov_rm32(GpReg::Rsi, GpReg::Rdx, MonomorphicIc::OFF_EXPECTED_GC_META);
                asm.cmp_rr(GpReg::Rcx, GpReg::Rsi);
                asm.jcc(Cond::Ne, slow_lbl);

                asm.mov_rm64(GpReg::Rsi, GpReg::R13, reg_disp(src_val));
                asm.mov_rm32(GpReg::Rdi, GpReg::Rdx, MonomorphicIc::OFF_FAST_KIND);

                // SmallI64
                asm.cmp_ri32(GpReg::Rdi, FieldStoreFastKind::SmallI64 as i32);
                asm.jcc(Cond::Ne, check_f64_lbl);
                asm.mov_rr(GpReg::Rcx, GpReg::Rsi);
                asm.and_ri32(GpReg::Rcx, IMM_LOW7_MASK as i32);
                asm.cmp_ri32(GpReg::Rcx, IMM_I_SMALL_TAG as i32);
                asm.jcc(Cond::E, store_ok_lbl);
                asm.jmp(slow_lbl);

                // CanonicalF64
                asm.bind(check_f64_lbl);
                asm.cmp_ri32(GpReg::Rdi, FieldStoreFastKind::CanonicalF64 as i32);
                asm.jcc(Cond::Ne, check_bool_lbl);
                asm.test_ri32(GpReg::Rsi, crate::layout::ROT_F64_TAG_MASK as i32);
                asm.jcc(Cond::Ne, store_ok_lbl);
                asm.mov_rr(GpReg::Rcx, GpReg::Rsi);
                asm.and_ri32(GpReg::Rcx, IMM_LOW7_MASK as i32);
                asm.cmp_ri32(GpReg::Rcx, IMM_F64_TAG as i32);
                asm.jcc(Cond::E, store_ok_lbl);
                asm.test_rr(GpReg::Rsi, GpReg::Rsi);
                asm.jcc(Cond::E, slow_lbl);
                asm.test_ri32(GpReg::Rsi, 0b111);
                asm.jcc(Cond::Ne, slow_lbl);
                asm.mov_rm32(GpReg::Rcx, GpReg::Rsi, HDR_TYPE_INDEX_DISP);
                asm.cmp_ri32(GpReg::Rcx, INTRINSIC_F64_RAW as i32);
                asm.jcc(Cond::Ne, slow_lbl);
                asm.mov_rm32(GpReg::Rcx, GpReg::Rsi, HDR_GC_META_DISP);
                asm.and_ri32(GpReg::Rcx, GC_META_ROLE_AND_WORDS_MASK);
                asm.cmp_ri32(GpReg::Rcx, GC_META_ORDINARY_1WORD);
                asm.jcc(Cond::E, store_ok_lbl);
                asm.jmp(slow_lbl);

                // Bool
                asm.bind(check_bool_lbl);
                asm.cmp_ri32(GpReg::Rdi, FieldStoreFastKind::Bool as i32);
                asm.jcc(Cond::Ne, check_any_lbl);
                asm.mov_rr(GpReg::Rcx, GpReg::Rsi);
                asm.and_ri32(GpReg::Rcx, IMM_BOOL_MASK_NO_BIT7);
                asm.cmp_ri32(GpReg::Rcx, TAG_FALSE as i32);
                asm.jcc(Cond::E, store_ok_lbl);
                asm.jmp(slow_lbl);

                // Any
                asm.bind(check_any_lbl);
                asm.cmp_ri32(GpReg::Rdi, FieldStoreFastKind::Any as i32);
                asm.jcc(Cond::E, store_ok_lbl);

                // ExactHeapNominal & OptionalNominal
                let check_opt_nominal_lbl = asm.new_label();
                let check_opt_i64_lbl = asm.new_label();
                let check_usize_lbl = asm.new_label();
                let check_heap_nominal_body_lbl = asm.new_label();

                asm.bind(check_nominal_lbl);
                asm.cmp_ri32(GpReg::Rdi, FieldStoreFastKind::ExactHeapNominal as i32);
                asm.jcc(Cond::E, check_heap_nominal_body_lbl);

                asm.bind(check_opt_nominal_lbl);
                asm.cmp_ri32(GpReg::Rdi, FieldStoreFastKind::OptionalNominal as i32);
                asm.jcc(Cond::Ne, check_opt_i64_lbl);
                asm.cmp_ri32(GpReg::Rsi, TAG_NULL as i32);
                asm.jcc(Cond::E, store_ok_lbl);

                asm.bind(check_heap_nominal_body_lbl);
                asm.test_rr(GpReg::Rsi, GpReg::Rsi);
                asm.jcc(Cond::E, slow_lbl);
                asm.test_ri32(GpReg::Rsi, 0b111);
                asm.jcc(Cond::Ne, slow_lbl);
                asm.mov_rm32(GpReg::Rcx, GpReg::Rsi, HDR_TYPE_INDEX_DISP);
                asm.mov_rm32(GpReg::Rdi, GpReg::Rdx, MonomorphicIc::OFF_EXPECTED_VAL_TYPE);
                asm.cmp_rr(GpReg::Rcx, GpReg::Rdi);
                asm.jcc(Cond::Ne, slow_lbl);
                asm.mov_rm32(GpReg::Rcx, GpReg::Rsi, HDR_GC_META_DISP);
                asm.test_ri32(GpReg::Rcx, GC_META_ROLE_MASK);
                asm.jcc(Cond::E, store_ok_lbl);
                asm.jmp(slow_lbl);

                asm.bind(check_opt_i64_lbl);
                asm.cmp_ri32(GpReg::Rdi, FieldStoreFastKind::OptionalSmallI64 as i32);
                asm.jcc(Cond::Ne, check_usize_lbl);
                asm.cmp_ri32(GpReg::Rsi, TAG_NULL as i32);
                asm.jcc(Cond::E, store_ok_lbl);
                asm.mov_rr(GpReg::Rcx, GpReg::Rsi);
                asm.and_ri32(GpReg::Rcx, IMM_LOW7_MASK as i32);
                asm.cmp_ri32(GpReg::Rcx, IMM_I_SMALL_TAG as i32);
                asm.jcc(Cond::E, store_ok_lbl);
                asm.jmp(slow_lbl);

                asm.bind(check_usize_lbl);
                asm.cmp_ri32(GpReg::Rdi, FieldStoreFastKind::Usize as i32);
                asm.jcc(Cond::Ne, slow_lbl);
                asm.mov_rr(GpReg::Rcx, GpReg::Rsi);
                asm.and_ri32(GpReg::Rcx, IMM_LOW7_MASK as i32);
                asm.cmp_ri32(GpReg::Rcx, IMM_USIZE_TAG as i32);
                asm.jcc(Cond::Ne, slow_lbl);

                asm.bind(store_ok_lbl);
                asm.mov_mr64(GpReg::Rax, (imm12 as i32) * 8, GpReg::Rsi);
                asm.jmp(done_lbl);

                asm.bind(slow_lbl);
                asm.mov_rr(GpReg::Rdi, GpReg::R12);
                asm.mov_ri64(GpReg::Rsi, pc as u64);
                asm.mov_ri64(GpReg::Rdx, word as u64);
                asm.mov_ri64(GpReg::Rcx, ic_ptr as usize as u64);
                asm.mov_ri64(GpReg::Rax, input.helpers.store_field_ic as usize as u64);
                asm.call_r(GpReg::Rax);
                emit_after_helper_check(asm, pc as u32, epilogue_lbl);
                asm.bind(done_lbl);
            } else {
                asm.mov_rr(GpReg::Rdi, GpReg::R12);
                asm.mov_ri64(GpReg::Rsi, pc as u64);
                asm.mov_ri64(GpReg::Rdx, word as u64);
                asm.mov_ri64(GpReg::Rcx, ic_ptr as usize as u64);
                asm.mov_ri64(GpReg::Rax, input.helpers.store_field_ic as usize as u64);
                asm.call_r(GpReg::Rax);
                emit_after_helper_check(asm, pc as u32, epilogue_lbl);
            }
        }

        // ── Control flow (jumps + back-edge hotness/safepoint/tier-up) ───
        Opcode::Jmp | Opcode::JmpFar => {
            let InstructionData::J { offset, .. } = instr.data else {
                unreachable!()
            };
            emit_taken_jump(
                asm,
                input,
                pc,
                offset,
                forward_targets,
                backward_targets,
                finished_lbl,
                epilogue_lbl,
                allow_tier_up_check,
            );
        }

        Opcode::JmpIf | Opcode::JmpIfNot => {
            let InstructionData::J { cond, offset } = instr.data else {
                unreachable!()
            };
            let take_lbl = asm.new_label();
            let fallthrough_lbl = asm.new_label();
            let truthy_lbl = if instr.opcode == Opcode::JmpIf {
                take_lbl
            } else {
                fallthrough_lbl
            };
            let falsy_lbl = if instr.opcode == Opcode::JmpIf {
                fallthrough_lbl
            } else {
                take_lbl
            };

            emit_branch_on_truthy(asm, input, cond, truthy_lbl, falsy_lbl);

            asm.bind(take_lbl);
            emit_taken_jump(
                asm,
                input,
                pc,
                offset,
                forward_targets,
                backward_targets,
                finished_lbl,
                epilogue_lbl,
                allow_tier_up_check,
            );
            asm.bind(fallthrough_lbl);
        }

        Opcode::JmpIfNull | Opcode::JmpIfNotNull => {
            let InstructionData::J { cond, offset } = instr.data else {
                unreachable!()
            };
            let fallthrough_lbl = asm.new_label();
            asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(cond));
            asm.cmp_ri32(GpReg::Rax, TAG_NULL as i32);
            if instr.opcode == Opcode::JmpIfNull {
                asm.jcc(Cond::Ne, fallthrough_lbl);
            } else {
                asm.jcc(Cond::E, fallthrough_lbl);
            }
            emit_taken_jump(
                asm,
                input,
                pc,
                offset,
                forward_targets,
                backward_targets,
                finished_lbl,
                epilogue_lbl,
                allow_tier_up_check,
            );
            asm.bind(fallthrough_lbl);
        }

        // ── Returns ──────────────────────────────────────────────────────
        Opcode::Return => {
            let InstructionData::C { payload } = instr.data else {
                unreachable!()
            };
            let src_reg = Instruction::c_return_reg(payload);
            if (src_reg.0 as usize) >= GP_REGISTER_COUNT {
                emit_step_inline_call(asm, input, pc as u32, word, epilogue_lbl);
            } else {
                asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(src_reg));
                emit_exit(
                    asm,
                    JitExitKind::Return,
                    pc as u32,
                    Some(GpReg::Rax),
                    epilogue_lbl,
                );
            }
        }

        Opcode::ReturnUnit => {
            asm.mov_ri64(GpReg::Rax, TAG_UNIT);
            emit_exit(
                asm,
                JitExitKind::Return,
                pc as u32,
                Some(GpReg::Rax),
                epilogue_lbl,
            );
        }

        // ── Safepoint & Nop ──────────────────────────────────────────────
        Opcode::Safepoint => {
            emit_inline_safepoint(asm, input, (pc + 1) as u32, epilogue_lbl);
        }

        Opcode::Nop => {}

        // ── In-place native function, closure, method & trait calls ──────
        Opcode::Call => {
            let InstructionData::C { payload } = instr.data else {
                unreachable!()
            };
            let (arg_count, func_id_raw) = Instruction::c_arg_count_func_id(payload);
            let callee_id = nsbc::FuncId(func_id_raw);
            if (arg_count as usize) <= GP_REGISTER_COUNT
                && let Some((slots, clobbers_cs)) =
                    (input.fast_direct_callee_slots)(callee_id, arg_count)
                && slots < (1 << 30)
            {
                let packed = (slots << 1) | u32::from(clobbers_cs);
                asm.mov_rr(GpReg::Rdi, GpReg::R12);
                asm.mov_ri64(GpReg::Rsi, (pc + 1) as u64);
                asm.mov_ri64(GpReg::Rdx, func_id_raw as u64);
                asm.mov_ri64(GpReg::Rcx, packed as u64);
                asm.mov_ri64(GpReg::Rax, input.helpers.call_direct_fast as usize as u64);
                asm.call_r(GpReg::Rax);
                emit_after_helper_check(asm, pc as u32, epilogue_lbl);
            } else {
                asm.mov_rr(GpReg::Rdi, GpReg::R12);
                asm.mov_ri64(GpReg::Rsi, pc as u64);
                asm.mov_ri64(GpReg::Rdx, word as u64);
                asm.mov_ri64(GpReg::Rax, input.helpers.call_op as usize as u64);
                asm.call_r(GpReg::Rax);
                emit_after_helper_check(asm, pc as u32, epilogue_lbl);
            }
        }

        Opcode::CallFar
        | Opcode::CallIndirect
        | Opcode::CallIndirectProof
        | Opcode::CallMethod
        | Opcode::CallMethodFar
        | Opcode::TraitCall => {
            asm.mov_rr(GpReg::Rdi, GpReg::R12);
            asm.mov_ri64(GpReg::Rsi, pc as u64);
            asm.mov_ri64(GpReg::Rdx, word as u64);
            asm.mov_ri64(GpReg::Rax, input.helpers.call_op as usize as u64);
            asm.call_r(GpReg::Rax);
            emit_after_helper_check(asm, pc as u32, epilogue_lbl);
        }

        // ── Builtin calls (inline List fast paths + in-place helper) ─────
        Opcode::CallBuiltin => {
            let InstructionData::C { payload } = instr.data else {
                unreachable!()
            };
            let (arg_count, builtin_id) = Instruction::c_arg_count_func_id(payload);
            emit_call_builtin(asm, input, pc as u32, builtin_id, arg_count, epilogue_lbl);
        }

        // ── Effects, continuations & futures (exit cleanly to interpreter) ──
        Opcode::EffectCallDyn
        | Opcode::PushHandlerClosure
        | Opcode::PushCapturingHandler
        | Opcode::PopHandler
        | Opcode::AwaitPopHandler
        | Opcode::ResetClosure
        | Opcode::ResumeContinuation
        | Opcode::ResumeContinuationOnce
        | Opcode::AwaitFuture => {
            emit_exit(asm, JitExitKind::InterpretAt, pc as u32, None, epilogue_lbl);
        }

        // ── Fast pre-resolved struct/tuple/newtype allocation ────────────
        Opcode::NewObject => {
            let InstructionData::A { dst, imm12, .. } = instr.data else {
                unreachable!()
            };
            let fast_obj = input
                .type_pool
                .canonical_type(TypeIndex::from_raw(imm12 as u32))
                .filter(|&ct| !input.type_pool.is_reserved_collection_role(ct))
                .and_then(|ct| {
                    let words = match &input.type_pool.get(ct).kind {
                        TypeKind::Struct { fields, .. } => fields.len(),
                        TypeKind::Tuple { elements } => elements.len(),
                        TypeKind::Newtype { .. } => 1,
                        _ => return None,
                    };
                    u16::try_from(words).ok().map(|w| (ct.as_u32(), w as u32))
                });
            if let Some((canon_raw, words)) = fast_obj {
                asm.mov_rr(GpReg::Rdi, GpReg::R12);
                asm.mov_ri64(GpReg::Rsi, dst.0 as u64);
                asm.mov_ri64(GpReg::Rdx, canon_raw as u64);
                asm.mov_ri64(GpReg::Rcx, words as u64);
                asm.mov_ri64(GpReg::Rax, input.helpers.alloc_object_fast as usize as u64);
                asm.call_r(GpReg::Rax);
                emit_after_helper_check(asm, pc as u32, epilogue_lbl);
            } else {
                emit_step_inline_call(asm, input, pc as u32, word, epilogue_lbl);
            }
        }

        // ── Single-frame heap / collection / global / enum / error ops ───
        Opcode::LoadIndex
        | Opcode::StoreIndex
        | Opcode::LoadGlobal
        | Opcode::StoreGlobal
        | Opcode::LoadGlobalWide
        | Opcode::StoreGlobalWide
        | Opcode::NewList
        | Opcode::NewMap
        | Opcode::NewClosure
        | Opcode::NewClosureWide
        | Opcode::NewEnum
        | Opcode::EnumIs
        | Opcode::EnumField
        | Opcode::TraitProof
        | Opcode::TraitAssert
        | Opcode::TraitProject
        | Opcode::ErrorOk
        | Opcode::ErrorErr
        | Opcode::ErrorIsOk
        | Opcode::ErrorPayload
        | Opcode::MatchFail
        | Opcode::Unreachable => {
            emit_step_inline_call(asm, input, pc as u32, word, epilogue_lbl);
        }
    }

    Ok(())
}

#[derive(Clone, Copy)]
enum FastTypeAssert {
    Any,
    Unit,
    I64,
    Usize,
    U64,
    Bool,
    F64,
    Struct {
        raw_ty: u32,
        expected_gc_meta: i32,
        nullable: bool,
    },
}

fn classify_fast_type_assert(
    type_pool: &type_pool::TypePool,
    canon_ty: TypeIndex,
) -> Option<FastTypeAssert> {
    if type_pool.contains_associated_type(canon_ty) {
        return None;
    }
    if let Some(intr) = type_pool.as_intrinsic(canon_ty) {
        return match intr {
            Intrinsic::Any => Some(FastTypeAssert::Any),
            Intrinsic::Unit => Some(FastTypeAssert::Unit),
            Intrinsic::I64 => Some(FastTypeAssert::I64),
            Intrinsic::Usize => Some(FastTypeAssert::Usize),
            Intrinsic::U64 => Some(FastTypeAssert::U64),
            Intrinsic::Bool => Some(FastTypeAssert::Bool),
            Intrinsic::F64 => Some(FastTypeAssert::F64),
            _ => None,
        };
    }
    match &type_pool.get(canon_ty).kind {
        TypeKind::Struct { fields, .. } => {
            if type_pool.is_reserved_collection_role(canon_ty) {
                if type_pool.list_type() == Some(canon_ty) || type_pool.map_type() == Some(canon_ty)
                {
                    return Some(FastTypeAssert::Struct {
                        raw_ty: canon_ty.as_u32(),
                        expected_gc_meta: GC_META_ORDINARY_3WORDS,
                        nullable: false,
                    });
                }
                return None;
            }
            let words = u16::try_from(fields.len()).ok()?;
            Some(FastTypeAssert::Struct {
                raw_ty: canon_ty.as_u32(),
                expected_gc_meta: (words as i32) << 3,
                nullable: false,
            })
        }
        TypeKind::Optional { inner } => {
            let ci = type_pool.canonical_type(*inner)?;
            if type_pool.contains_associated_type(ci) || type_pool.is_reserved_collection_role(ci) {
                return None;
            }
            if let TypeKind::Struct { fields, .. } = &type_pool.get(ci).kind {
                let words = u16::try_from(fields.len()).ok()?;
                Some(FastTypeAssert::Struct {
                    raw_ty: ci.as_u32(),
                    expected_gc_meta: (words as i32) << 3,
                    nullable: true,
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

fn emit_try_unbox_inline_f64(
    asm: &mut Assembler,
    gp: GpReg,
    scratch: GpReg,
    xmm: XmmReg,
    fail_lbl: Label,
) {
    let try_imm_lbl = asm.new_label();
    let ok_lbl = asm.new_label();
    asm.test_ri32(gp, ROT_F64_TAG_MASK as i32);
    asm.jcc(Cond::E, try_imm_lbl);
    asm.ror_ri8(gp, 4);
    asm.movq_xr(xmm, gp);
    asm.jmp(ok_lbl);

    asm.bind(try_imm_lbl);
    asm.mov_rr(scratch, gp);
    asm.and_ri32(scratch, IMM_LOW7_MASK as i32);
    asm.cmp_ri32(scratch, IMM_F64_TAG as i32);
    asm.jcc(Cond::Ne, fail_lbl);
    asm.and_ri32(gp, !(IMM_LOW7_MASK as i32));
    asm.movq_xr(xmm, gp);
    asm.bind(ok_lbl);
}

fn emit_box_inline_f64_or_slow(asm: &mut Assembler, xmm: XmmReg, out_gp: GpReg, slow_lbl: Label) {
    let ok_lbl = asm.new_label();
    asm.movq_rx(out_gp, xmm);
    asm.rol_ri8(out_gp, 4);
    asm.test_ri32(out_gp, ROT_F64_TAG_MASK as i32);
    asm.jcc(Cond::Ne, ok_lbl);
    asm.ror_ri8(out_gp, 4);
    asm.test_ri32(out_gp, IMM_LOW7_MASK as i32);
    asm.jcc(Cond::Ne, slow_lbl);
    asm.or_ri32(out_gp, IMM_F64_TAG as i32);
    asm.bind(ok_lbl);
}

fn emit_call_builtin(
    asm: &mut Assembler,
    input: &CompilationInput<'_>,
    pc: u32,
    builtin_id: u32,
    arg_count: u8,
    epilogue_lbl: Label,
) {
    let slow_lbl = asm.new_label();
    let done_lbl = asm.new_label();

    if input.fast_list_builtins
        && let (Some(list_ty), Some(buf_ty)) = (input.list_type_raw, input.list_buf_type_raw)
        && matches!(
            (builtin_id, arg_count),
            (ids::LIST_LEN, 1) | (ids::LIST_GET, 2) | (ids::LIST_SET, 3)
        )
    {
        // Guard single-worker lock-free mode (`*aux_started_ptr == 0`).
        asm.mov_rm64(GpReg::Rax, GpReg::R12, JitRuntimeCtx::OFF_AUX_STARTED_PTR);
        asm.movzx_rm8(GpReg::Rax, GpReg::Rax, 0);
        asm.test_rr(GpReg::Rax, GpReg::Rax);
        asm.jcc(Cond::Ne, slow_lbl);

        // Validate `r0` is a 3-word Ordinary `List` wrapper.
        asm.mov_rm64(GpReg::Rax, GpReg::R13, 0);
        asm.test_rr(GpReg::Rax, GpReg::Rax);
        asm.jcc(Cond::E, slow_lbl);
        asm.test_ri32(GpReg::Rax, 0b111);
        asm.jcc(Cond::Ne, slow_lbl);
        asm.mov_rm32(GpReg::Rcx, GpReg::Rax, HDR_TYPE_INDEX_DISP);
        asm.cmp_ri32(GpReg::Rcx, list_ty as i32);
        asm.jcc(Cond::Ne, slow_lbl);
        asm.mov_rm32(GpReg::Rcx, GpReg::Rax, HDR_GC_META_DISP);
        asm.and_ri32(GpReg::Rcx, GC_META_ROLE_AND_WORDS_MASK);
        asm.cmp_ri32(GpReg::Rcx, GC_META_ORDINARY_3WORDS);
        asm.jcc(Cond::Ne, slow_lbl);

        // Read `wrapper[0]` = `length_tagged` (`IMM_U_SMALL_TAG` == `0x09`).
        asm.mov_rm64(GpReg::Rdx, GpReg::Rax, 0);
        asm.mov_rr(GpReg::Rcx, GpReg::Rdx);
        asm.and_ri32(GpReg::Rcx, IMM_LOW7_MASK as i32);
        asm.cmp_ri32(GpReg::Rcx, IMM_U_SMALL_TAG as i32);
        asm.jcc(Cond::Ne, slow_lbl);
        asm.shr_ri8(GpReg::Rdx, 7);

        if builtin_id == ids::LIST_LEN {
            // `length <= 65535 < 2^56`, so `(length << 7) | 0x01` is always valid `IMM_I_SMALL`.
            asm.shl_ri8(GpReg::Rdx, 7);
            asm.or_ri32(GpReg::Rdx, IMM_I_SMALL_TAG as i32);
            asm.mov_mr64(GpReg::R13, 0, GpReg::Rdx);
            asm.jmp(done_lbl);
        } else {
            // Validate `r1` = `index` (`IMM_USIZE` or non-negative `IMM_I_SMALL`) and `index < length`.
            let idx_ok_lbl = asm.new_label();
            asm.mov_rm64(GpReg::Rsi, GpReg::R13, 8);
            asm.test_rr(GpReg::Rsi, GpReg::Rsi);
            asm.jcc(Cond::S, slow_lbl);
            asm.mov_rr(GpReg::Rcx, GpReg::Rsi);
            asm.and_ri32(GpReg::Rcx, IMM_LOW7_MASK as i32);
            asm.cmp_ri32(GpReg::Rcx, IMM_USIZE_TAG as i32);
            asm.jcc(Cond::E, idx_ok_lbl);
            asm.cmp_ri32(GpReg::Rcx, IMM_I_SMALL_TAG as i32);
            asm.jcc(Cond::Ne, slow_lbl);
            asm.bind(idx_ok_lbl);
            asm.shr_ri8(GpReg::Rsi, 7);
            asm.cmp_rr(GpReg::Rsi, GpReg::Rdx);
            asm.jcc(Cond::Ae, slow_lbl);

            // Load `wrapper[2]` = `buffer` and check `type_index == buf_ty` & `index < capacity`.
            asm.mov_rm64(GpReg::Rdi, GpReg::Rax, 16);
            asm.test_rr(GpReg::Rdi, GpReg::Rdi);
            asm.jcc(Cond::E, slow_lbl);
            asm.test_ri32(GpReg::Rdi, 0b111);
            asm.jcc(Cond::Ne, slow_lbl);
            asm.mov_rm32(GpReg::Rcx, GpReg::Rdi, HDR_TYPE_INDEX_DISP);
            asm.cmp_ri32(GpReg::Rcx, buf_ty as i32);
            asm.jcc(Cond::Ne, slow_lbl);
            asm.mov_rm32(GpReg::Rcx, GpReg::Rdi, HDR_GC_META_DISP);
            asm.test_ri32(GpReg::Rcx, GC_META_ROLE_MASK);
            asm.jcc(Cond::Ne, slow_lbl);
            asm.shr_ri8(GpReg::Rcx, 3);
            asm.and_ri32(GpReg::Rcx, 0xFFFF);
            asm.cmp_rr(GpReg::Rsi, GpReg::Rcx);
            asm.jcc(Cond::Ae, slow_lbl);

            asm.shl_ri8(GpReg::Rsi, 3);
            asm.add_rr(GpReg::Rdi, GpReg::Rsi);
            if builtin_id == ids::LIST_GET {
                asm.mov_rm64(GpReg::Rax, GpReg::Rdi, 0);
                asm.mov_mr64(GpReg::R13, 0, GpReg::Rax);
            } else {
                asm.mov_rm64(GpReg::Rax, GpReg::R13, 16);
                asm.mov_mr64(GpReg::Rdi, 0, GpReg::Rax);
                asm.mov_ri64(GpReg::Rax, TAG_UNIT);
                asm.mov_mr64(GpReg::R13, 0, GpReg::Rax);
            }
            asm.jmp(done_lbl);
        }
    }

    asm.bind(slow_lbl);
    asm.mov_rr(GpReg::Rdi, GpReg::R12);
    asm.mov_ri64(GpReg::Rsi, pc as u64);
    asm.mov_ri64(GpReg::Rdx, builtin_id as u64);
    asm.mov_ri64(GpReg::Rcx, arg_count as u64);
    asm.mov_ri64(GpReg::Rax, input.helpers.call_builtin as usize as u64);
    asm.call_r(GpReg::Rax);
    emit_after_helper_check(asm, pc, epilogue_lbl);
    asm.bind(done_lbl);
}

fn emit_numeric_slow_call(
    asm: &mut Assembler,
    input: &CompilationInput<'_>,
    pc: u32,
    word: u32,
    epilogue_lbl: Label,
) {
    asm.mov_rr(GpReg::Rdi, GpReg::R12);
    asm.mov_ri64(GpReg::Rsi, word as u64);
    asm.mov_ri64(GpReg::Rax, input.helpers.numeric_slow as usize as u64);
    asm.call_r(GpReg::Rax);
    emit_after_helper_check(asm, pc, epilogue_lbl);
}

fn emit_type_op_call(
    asm: &mut Assembler,
    input: &CompilationInput<'_>,
    pc: u32,
    word: u32,
    epilogue_lbl: Label,
) {
    asm.mov_rr(GpReg::Rdi, GpReg::R12);
    asm.mov_ri64(GpReg::Rsi, pc as u64);
    asm.mov_ri64(GpReg::Rdx, word as u64);
    asm.mov_ri64(GpReg::Rax, input.helpers.type_op as usize as u64);
    asm.call_r(GpReg::Rax);
    emit_after_helper_check(asm, pc, epilogue_lbl);
}

fn emit_step_inline_call(
    asm: &mut Assembler,
    input: &CompilationInput<'_>,
    pc: u32,
    word: u32,
    epilogue_lbl: Label,
) {
    asm.mov_rr(GpReg::Rdi, GpReg::R12);
    asm.mov_ri64(GpReg::Rsi, pc as u64);
    asm.mov_ri64(GpReg::Rdx, word as u64);
    asm.mov_ri64(GpReg::Rax, input.helpers.step_inline_op as usize as u64);
    asm.call_r(GpReg::Rax);
    emit_after_helper_check(asm, pc, epilogue_lbl);
}

fn emit_const_load(
    asm: &mut Assembler,
    input: &CompilationInput<'_>,
    pc: u32,
    dst: Reg,
    const_idx: u32,
    epilogue_lbl: Label,
) {
    match input.constants.get(const_idx as usize).copied() {
        Some(CompileConstant::ImmediateValidated { raw, .. }) => {
            asm.mov_ri64(GpReg::Rax, raw);
            asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
        }
        Some(CompileConstant::HeapObject { .. })
            if (const_idx as usize)
                .checked_mul(8)
                .and_then(|b| i32::try_from(b).ok())
                .is_some() =>
        {
            let disp = ((const_idx as usize) * 8) as i32;
            asm.mov_rm64(GpReg::Rcx, GpReg::R12, JitRuntimeCtx::OFF_CONSTS_PTR);
            asm.mov_rm64(GpReg::Rax, GpReg::Rcx, disp);
            asm.mov_mr64(GpReg::R13, reg_disp(dst), GpReg::Rax);
        }
        _ => {
            asm.mov_rr(GpReg::Rdi, GpReg::R12);
            asm.mov_ri64(GpReg::Rsi, dst.0 as u64);
            asm.mov_ri64(GpReg::Rdx, const_idx as u64);
            asm.mov_ri64(GpReg::Rax, input.helpers.load_const_slow as usize as u64);
            asm.call_r(GpReg::Rax);
            emit_after_helper_check(asm, pc, epilogue_lbl);
        }
    }
}

fn emit_branch_on_truthy(
    asm: &mut Assembler,
    input: &CompilationInput<'_>,
    cond: Reg,
    truthy_lbl: Label,
    falsy_lbl: Label,
) {
    let call_helper_lbl = asm.new_label();
    asm.mov_rm64(GpReg::Rax, GpReg::R13, reg_disp(cond));
    asm.cmp_ri32(GpReg::Rax, TAG_TRUE as i32);
    asm.jcc(Cond::E, truthy_lbl);
    asm.cmp_ri32(GpReg::Rax, TAG_FALSE as i32);
    asm.jcc(Cond::E, falsy_lbl);
    asm.cmp_ri32(GpReg::Rax, TAG_NULL as i32);
    asm.jcc(Cond::E, falsy_lbl);
    asm.cmp_ri32(GpReg::Rax, TAG_UNIT as i32);
    asm.jcc(Cond::E, falsy_lbl);

    // Check `IMM_I_SMALL` (`(raw & 0x7f) == 0x01`): `0x01` is `0` (falsy), else truthy.
    asm.mov_rr(GpReg::Rcx, GpReg::Rax);
    asm.and_ri32(GpReg::Rcx, IMM_LOW7_MASK as i32);
    asm.cmp_ri32(GpReg::Rcx, IMM_I_SMALL_TAG as i32);
    asm.jcc(Cond::Ne, call_helper_lbl);
    asm.cmp_ri32(GpReg::Rax, IMM_I_SMALL_TAG as i32);
    asm.jcc(Cond::E, falsy_lbl);
    asm.jmp(truthy_lbl);

    asm.bind(call_helper_lbl);
    asm.mov_rr(GpReg::Rdi, GpReg::Rax);
    asm.mov_ri64(GpReg::Rax, input.helpers.is_truthy as usize as u64);
    asm.call_r(GpReg::Rax);
    asm.test_rr(GpReg::Rax, GpReg::Rax);
    asm.jcc(Cond::E, falsy_lbl);
    asm.jmp(truthy_lbl);
}

#[allow(clippy::too_many_arguments)]
fn emit_taken_jump(
    asm: &mut Assembler,
    input: &CompilationInput<'_>,
    pc: usize,
    offset: i32,
    forward_targets: &[Label],
    backward_targets: &[Label],
    finished_lbl: Label,
    epilogue_lbl: Label,
    allow_tier_up_check: bool,
) {
    let target_pc = ((pc as i64) + (offset as i64)) as u32;
    let target_usize = target_pc as usize;
    if target_usize >= input.instructions.len() {
        asm.jmp(finished_lbl);
        return;
    }

    if offset <= 0 {
        // Backward jump: bump hotness, check Tier-2 upgrade threshold, and ensure safepoint poll.
        asm.mov_ri64(GpReg::Rax, input.hotness_ptr as usize as u64);
        asm.add_m64_i32(GpReg::Rax, 0, 1);

        if allow_tier_up_check {
            let skip_tier_up = asm.new_label();
            asm.movzx_rm8(GpReg::Rcx, GpReg::R12, JitRuntimeCtx::OFF_ALLOW_TIER_UP);
            asm.test_rr(GpReg::Rcx, GpReg::Rcx);
            asm.jcc(Cond::E, skip_tier_up);
            asm.mov_rm64(GpReg::Rcx, GpReg::Rax, 0);
            asm.cmp_ri32(GpReg::Rcx, TIER2_HOTNESS_THRESHOLD as i32);
            asm.jcc(Cond::B, skip_tier_up);
            emit_exit(asm, JitExitKind::TierUp, target_pc, None, epilogue_lbl);
            asm.bind(skip_tier_up);
        }

        // If the target instruction is not itself an `Opcode::Safepoint`, emit one on the back-edge
        // so hand-written bytecode loops without block-header `Safepoint` opcodes remain preemptible.
        let target_is_safepoint = Instruction::decode(input.instructions[target_usize])
            .is_some_and(|i| i.opcode == Opcode::Safepoint);
        if !target_is_safepoint {
            emit_inline_safepoint(asm, input, target_pc, epilogue_lbl);
        }

        asm.jmp(backward_targets[target_usize]);
    } else {
        asm.jmp(forward_targets[target_usize]);
    }
}
