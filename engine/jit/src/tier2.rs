//! Tier-2 Natural-Loop Specializing JIT Compiler (`x86_64` SysV64 + SSE2).
//!
//! Identifies innermost natural numeric loops via exact CFG reachability, performs
//! live-in / upward-exposed use analysis and scalar type inference (`i64`, `f64`, `bool`),
//! emits Preheader & On-Stack Replacement (OSR) runtime tag guards and unboxing into
//! dedicated `x86_64` GP and SSE2 `XMM` registers, executes the hot loop body with
//! **zero heap allocations and zero tag shifts**, and boxes modified state back to
//! `StackContext` only on loop exits, cooperative safepoints, or 57-bit overflow deopts.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use nsbc::{AddrMode, Instruction, InstructionData, Opcode, Reg};
use runtime::GP_REGISTER_COUNT;
use type_pool::{Intrinsic, TypeIndex};

use crate::code_cache::JitError;
use crate::layout::{
    GC_META_ORDINARY_1WORD, GC_META_ROLE_AND_WORDS_MASK, HDR_GC_META_DISP, HDR_TYPE_INDEX_DISP,
    IMM_BOOL_MASK_NO_BIT7, IMM_F64_TAG, IMM_I_SMALL_TAG, IMM_LOW7_MASK, INTRINSIC_F64_RAW,
    TAG_FALSE, TAG_TRUE,
};
use crate::tier1::{
    allocate_function_ics, emit_after_helper_check, emit_baseline_instruction, emit_epilogue,
    emit_exit, emit_prologue,
};
use crate::x86_64::{Assembler, Cond, GpReg, Label, XmmReg};
use crate::{CompilationInput, CompileConstant, EmittedCode, JitExitKind, JitRuntimeCtx};

const MAX_TRACKED_SLOTS: usize = 64;

/// Dedicated GP registers reserved exclusively for unboxed `I64` / `Bool` loop values.
/// Scratch (`Rax`, `Rcx`, `Rdx`) and pinned context (`Rbp`, `Rsp`, `R12..R15`) never collide.
const LOOP_GP_POOL: [GpReg; 7] = [
    GpReg::Rbx,
    GpReg::R8,
    GpReg::R9,
    GpReg::R10,
    GpReg::R11,
    GpReg::Rsi,
    GpReg::Rdi,
];

/// Dedicated SSE2 registers reserved exclusively for unboxed `F64` loop values.
/// `Xmm6` and `Xmm7` remain free as instruction-level SSE2 scratch registers.
const LOOP_XMM_POOL: [XmmReg; 6] = [
    XmmReg::Xmm0,
    XmmReg::Xmm1,
    XmmReg::Xmm2,
    XmmReg::Xmm3,
    XmmReg::Xmm4,
    XmmReg::Xmm5,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum ScalarType {
    /// 57-bit signed `IMM_I_SMALL` at loop boundary; raw signed `i64` inside loop.
    I64,
    /// Hybrid `IMM_F64` or 1-word `Intrinsic::F64` box at boundary; IEEE-754 `f64` inside loop.
    F64,
    /// `TaggedValue::FALSE`/`TRUE` at boundary; `0` or `1` (`u64`) inside loop.
    Bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PhysLoc {
    Gp(GpReg),
    Xmm(XmmReg),
    Spill(i32),
}

#[derive(Debug, Clone)]
struct SlotVar {
    slot_idx: u32,
    ty: ScalarType,
    live_in: bool,
    modified: bool,
    loc: PhysLoc,
    /// Dedicated stack save slot (`[rbp + disp]`) used when boxing `F64` across C calls.
    f64_save_disp: Option<i32>,
}

#[derive(Debug, Clone, Copy)]
struct RegTypedLoc {
    loc: PhysLoc,
    f64_save_disp: Option<i32>,
}

struct LoopPlan {
    header_pc: usize,
    body_pcs: Vec<usize>,
    pc_to_idx: BTreeMap<usize, usize>,
    slots: Vec<SlotVar>,
    slot_map: BTreeMap<u32, usize>,
    /// Physical storage for `(reg_index, ScalarType)`.
    reg_locs: BTreeMap<(u8, ScalarType), RegTypedLoc>,
    /// Statically inferred type of each virtual register on entry to `body_pcs[idx]`.
    reg_in: Vec<[Option<ScalarType>; GP_REGISTER_COUNT]>,
    /// Statically inferred type of each virtual register at each external exit target `exit_pc`.
    exit_reg_types: BTreeMap<usize, [Option<ScalarType>; GP_REGISTER_COUNT]>,
    reg_live_in: [Option<ScalarType>; GP_REGISTER_COUNT],
    reg_modified: [bool; GP_REGISTER_COUNT],
    /// Stack displacement (`[rbp + disp]`) holding 64-bit initialized/dirty mask for `slots`.
    slot_dirty_disp: i32,
    /// Stack displacement (`[rbp + disp]`) holding 64-bit initialized/dirty mask for `regs`.
    reg_dirty_disp: i32,
}

/// Compile `input` at Tier-2, specializing all innermost natural numeric loops while emitting
/// a baseline native skeleton for surrounding code and post-deopt fallback.
pub fn compile_function_tier2(input: &CompilationInput<'_>) -> Result<EmittedCode, JitError> {
    if input.instructions.is_empty() {
        return Err(JitError::UnsupportedBytecode("empty function"));
    }

    let decoded = decode_all(input.instructions)?;
    let mut spill_counter = 0usize;
    let loops = discover_and_plan_loops(input, &decoded, &mut spill_counter);
    if loops.is_empty() {
        return Err(JitError::UnsupportedBytecode(
            "no eligible natural numeric loop",
        ));
    }

    let (ics, ic_map) = allocate_function_ics(input.instructions);
    let mut asm = Assembler::with_capacity(input.instructions.len() * 48 + 512);

    let base_lbl = asm.new_label();
    let epilogue_lbl = asm.new_label();
    let finished_lbl = asm.new_label();

    let baseline_labels: Vec<Label> = (0..input.instructions.len())
        .map(|_| asm.new_label())
        .collect();

    // Map loop `header_pc` -> `loop_entry_lbl` (used for preheader entry, forward jumps, and OSR).
    let mut header_entry_labels: BTreeMap<usize, Label> = BTreeMap::new();
    let mut backedge_reentry_labels = baseline_labels.clone();
    for plan in &loops {
        let entry_lbl = asm.new_label();
        header_entry_labels.insert(plan.header_pc, entry_lbl);

        // Baseline back-edge checks `*func_deopt_ptr == 0` so resumed coroutines re-enter Tier-2
        // while loops that promoted past 57-bit SmallInt remain in baseline without guard overhead.
        let gate_lbl = asm.new_label();
        backedge_reentry_labels[plan.header_pc] = gate_lbl;
    }

    let mut forward_targets = baseline_labels.clone();
    for (&hdr, &entry_lbl) in &header_entry_labels {
        forward_targets[hdr] = entry_lbl;
    }

    let pc0_entry_lbl = header_entry_labels
        .get(&0)
        .copied()
        .unwrap_or(baseline_labels[0]);
    emit_prologue(&mut asm, base_lbl, pc0_entry_lbl, spill_counter);

    let mut pc_offsets = vec![u32::MAX; input.instructions.len()];

    // 1. Emit the complete baseline skeleton (handles prologue/epilogue code outside loops,
    //    fallback after mid-loop deopt, and coroutine resume mid-block).
    for (pc, &word) in input.instructions.iter().enumerate() {
        // If `pc` is a specialized loop header and `pc - 1` could fall through into `pc`,
        // route that natural preheader fallthrough into `header_entry_labels[pc]`.
        if let Some(&entry_lbl) = header_entry_labels.get(&pc)
            && pc > 0
            && can_fall_through(&decoded[pc - 1])
        {
            asm.jmp(entry_lbl);
        }

        asm.bind(baseline_labels[pc]);
        if !header_entry_labels.contains_key(&pc) {
            pc_offsets[pc] = asm.offset() as u32;
        }
        emit_baseline_instruction(
            &mut asm,
            input,
            pc,
            word,
            ic_map[pc],
            &forward_targets,
            &backedge_reentry_labels,
            finished_lbl,
            epilogue_lbl,
            false,
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

    // Emit baseline back-edge gates (`*func_deopt_ptr == 0 -> entry_lbl`, else `baseline_labels[hdr]`).
    for plan in &loops {
        let gate_lbl = backedge_reentry_labels[plan.header_pc];
        let entry_lbl = header_entry_labels[&plan.header_pc];
        asm.bind(gate_lbl);
        asm.mov_ri64(GpReg::Rax, input.func_deopt_ptr as usize as u64);
        asm.mov_rm32(GpReg::Rax, GpReg::Rax, 0);
        asm.test_rr(GpReg::Rax, GpReg::Rax);
        asm.jcc(Cond::E, entry_lbl);
        asm.jmp(baseline_labels[plan.header_pc]);
    }

    // 2. Emit each specialized Tier-2 unboxed loop region.
    for plan in &loops {
        let entry_lbl = header_entry_labels[&plan.header_pc];
        asm.bind(entry_lbl);
        pc_offsets[plan.header_pc] = asm.offset() as u32;
        emit_specialized_loop(
            &mut asm,
            input,
            &decoded,
            plan,
            &baseline_labels,
            &forward_targets,
            finished_lbl,
            epilogue_lbl,
        )?;
    }

    asm.bind(epilogue_lbl);
    emit_epilogue(&mut asm, spill_counter);

    let bytes = asm.finish()?;
    Ok(EmittedCode {
        bytes,
        pc_offsets: pc_offsets.into_boxed_slice(),
        ics,
        specialized_loops: loops.len(),
    })
}

fn decode_all(words: &[u32]) -> Result<Vec<Instruction>, JitError> {
    let mut out = Vec::with_capacity(words.len());
    for &w in words {
        let instr = Instruction::decode(w)
            .ok_or(JitError::UnsupportedBytecode("invalid instruction word"))?;
        out.push(instr);
    }
    Ok(out)
}

fn can_fall_through(instr: &Instruction) -> bool {
    !matches!(
        instr.opcode,
        Opcode::Jmp
            | Opcode::JmpFar
            | Opcode::Return
            | Opcode::ReturnUnit
            | Opcode::Unreachable
            | Opcode::MatchFail
    )
}

#[inline]
fn next_spill_disp(spill_counter: &mut usize) -> i32 {
    let idx = *spill_counter;
    *spill_counter += 1;
    // Callee-saved pushes (`rbx`, `r12`, `r13`, `r14`, `r15`) occupy `[rbp - 40 .. rbp]`.
    -40 - ((idx as i32 + 1) * 8)
}

// ---------------------------------------------------------------------------
// Natural Loop Discovery, Scalar Type Inference & Liveness
// ---------------------------------------------------------------------------

fn discover_and_plan_loops(
    input: &CompilationInput<'_>,
    decoded: &[Instruction],
    spill_counter: &mut usize,
) -> Vec<LoopPlan> {
    let len = decoded.len();
    if len == 0 {
        return Vec::new();
    }

    // 1. Build instruction-level CFG successors (`0..=len`, where `len` denotes function end).
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); len];
    let mut backedges_by_header: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (pc, instr) in decoded.iter().enumerate() {
        if matches!(
            instr.opcode,
            Opcode::Return | Opcode::ReturnUnit | Opcode::Unreachable | Opcode::MatchFail
        ) {
            continue;
        }
        if let InstructionData::J { offset, .. } = instr.data {
            let target = (pc as i64) + (offset as i64);
            if target < 0 || (target as usize) > len {
                return Vec::new();
            }
            let tgt = target as usize;
            succs[pc].push(tgt);
            if offset <= 0 && tgt <= pc && tgt < len {
                backedges_by_header.entry(tgt).or_default().push(pc);
            }
        }
        if can_fall_through(instr) {
            let next = pc + 1;
            if !succs[pc].contains(&next) {
                succs[pc].push(next);
            }
        }
    }

    // 2. Compute forward reachability from function entry (`pc == 0`) so dead blocks cannot
    //    fabricate spurious predecessors or unreachable loop paths.
    let mut reachable = vec![false; len];
    let mut reach_q = VecDeque::new();
    reachable[0] = true;
    reach_q.push_back(0usize);
    while let Some(pc) = reach_q.pop_front() {
        for &s in &succs[pc] {
            if s < len && !reachable[s] {
                reachable[s] = true;
                reach_q.push_back(s);
            }
        }
    }

    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); len];
    for (pc, s_list) in succs.iter().enumerate() {
        if !reachable[pc] {
            continue;
        }
        for &s in s_list {
            if s < len {
                preds[s].push(pc);
            }
        }
    }

    // 3. Construct exact natural loop `body_set` for each candidate `header_pc` via backward
    //    reachability from reachable back-edge latches stopping at `header_pc`.
    let mut candidates: Vec<(usize, Vec<usize>)> = Vec::new();
    for (header_pc, latches) in backedges_by_header {
        if !reachable[header_pc] {
            continue;
        }
        let mut body_set: BTreeSet<usize> = BTreeSet::new();
        body_set.insert(header_pc);
        let mut work = VecDeque::new();
        let mut any_reachable_latch = false;
        for latch_pc in latches {
            if reachable[latch_pc] {
                any_reachable_latch = true;
                if body_set.insert(latch_pc) {
                    work.push_back(latch_pc);
                }
            }
        }
        if !any_reachable_latch {
            continue;
        }
        while let Some(u) = work.pop_front() {
            if u == header_pc {
                continue;
            }
            for &p in &preds[u] {
                if body_set.insert(p) {
                    work.push_back(p);
                }
            }
        }

        // Verify strict single-entry dominance (`header_pc` is the sole entry and minimum PC)
        // and absence of nested inner back-edges inside `body_set`.
        let valid_single_entry = body_set.iter().all(|&u| {
            u == header_pc || (u > header_pc && preds[u].iter().all(|p| body_set.contains(p)))
        });
        if !valid_single_entry {
            continue;
        }
        let nested_backedge = body_set.iter().any(|&u| {
            succs[u]
                .iter()
                .any(|&s| body_set.contains(&s) && s <= u && s != header_pc)
        });
        if nested_backedge {
            continue;
        }

        candidates.push((header_pc, body_set.into_iter().collect()));
    }

    // Sort smallest natural loops first so innermost loops win.
    candidates.sort_by_key(|(h, body)| (body.len(), *h));

    let pre_slot_hints = collect_pre_loop_slot_hints(input, decoded);
    let mut accepted_pcs: BTreeSet<usize> = BTreeSet::new();
    let mut plans = Vec::new();

    for (header_pc, body_pcs) in candidates {
        if body_pcs.iter().any(|pc| accepted_pcs.contains(pc)) {
            continue;
        }
        if let Some(plan) = analyze_loop_candidate(
            input,
            decoded,
            &preds,
            header_pc,
            body_pcs,
            &pre_slot_hints,
            spill_counter,
        ) {
            accepted_pcs.extend(plan.body_pcs.iter().copied());
            plans.push(plan);
        }
    }

    plans.sort_by_key(|p| p.header_pc);
    plans
}

/// Inspect instructions across the function to seed initial type hints for local slots.
fn collect_pre_loop_slot_hints(
    input: &CompilationInput<'_>,
    decoded: &[Instruction],
) -> BTreeMap<u32, ScalarType> {
    let mut reg_hints = [None::<ScalarType>; GP_REGISTER_COUNT];
    let mut slot_hints: BTreeMap<u32, ScalarType> = BTreeMap::new();
    for (pc, instr) in decoded.iter().enumerate() {
        match instr.opcode {
            Opcode::Load => {
                if let InstructionData::A { dst, imm12, .. } = instr.data {
                    reg_hints[dst.0 as usize] = match instr.amode {
                        AddrMode::Imm => Some(ScalarType::I64),
                        AddrMode::Const => const_scalar_type(input, imm12 as u32),
                    };
                }
            }
            Opcode::LoadConstWide => {
                if let InstructionData::A { dst, base, imm12 } = instr.data {
                    let idx = Instruction::wide_const_index(base, imm12);
                    reg_hints[dst.0 as usize] = const_scalar_type(input, idx);
                }
            }
            Opcode::LoadTrue | Opcode::LoadFalse => {
                if let InstructionData::A { dst, .. } = instr.data {
                    reg_hints[dst.0 as usize] = Some(ScalarType::Bool);
                }
            }
            Opcode::TypeAssert | Opcode::TypeCast if (input.has_scope)(pc) => {
                if let InstructionData::A { dst, imm12, .. } = instr.data {
                    let ty = TypeIndex::from_raw(imm12 as u32);
                    reg_hints[dst.0 as usize] = match input
                        .type_pool
                        .canonical_type(ty)
                        .and_then(|c| input.type_pool.as_intrinsic(c))
                    {
                        Some(Intrinsic::I64) => Some(ScalarType::I64),
                        Some(Intrinsic::F64) => Some(ScalarType::F64),
                        Some(Intrinsic::Bool) => Some(ScalarType::Bool),
                        _ => None,
                    };
                }
            }
            Opcode::StoreSlot => {
                if let InstructionData::A { dst, base, imm12 } = instr.data {
                    let slot = ((base.0 as u32) << 12) | (imm12 as u32);
                    if let Some(ty) = reg_hints[dst.0 as usize] {
                        slot_hints.entry(slot).or_insert(ty);
                    }
                }
            }
            Opcode::LoadSlot => {
                if let InstructionData::A { dst, base, imm12 } = instr.data {
                    let slot = ((base.0 as u32) << 12) | (imm12 as u32);
                    reg_hints[dst.0 as usize] = slot_hints.get(&slot).copied();
                }
            }
            _ => {}
        }
    }
    slot_hints
}

fn const_scalar_type(input: &CompilationInput<'_>, idx: u32) -> Option<ScalarType> {
    match input.constants.get(idx as usize).copied()? {
        CompileConstant::ImmediateValidated {
            raw,
            i64_val,
            f64_bits,
        } => {
            if i64_val.is_some() {
                Some(ScalarType::I64)
            } else if f64_bits.is_some() {
                Some(ScalarType::F64)
            } else if raw == TAG_TRUE || raw == TAG_FALSE {
                Some(ScalarType::Bool)
            } else {
                None
            }
        }
        CompileConstant::HeapObject { f64_bits: Some(_) } => Some(ScalarType::F64),
        _ => None,
    }
}

fn const_f64_bits(input: &CompilationInput<'_>, idx: u32) -> Option<u64> {
    match input.constants.get(idx as usize).copied()? {
        CompileConstant::ImmediateValidated {
            f64_bits: Some(bits),
            ..
        }
        | CompileConstant::HeapObject {
            f64_bits: Some(bits),
        } => Some(bits),
        _ => None,
    }
}

fn const_i64_val(input: &CompilationInput<'_>, idx: u32) -> Option<i64> {
    match input.constants.get(idx as usize).copied()? {
        CompileConstant::ImmediateValidated {
            i64_val: Some(v), ..
        } => Some(v),
        _ => None,
    }
}

#[allow(clippy::too_many_lines)]
fn analyze_loop_candidate(
    input: &CompilationInput<'_>,
    decoded: &[Instruction],
    preds: &[Vec<usize>],
    header_pc: usize,
    body_pcs: Vec<usize>,
    pre_slot_hints: &BTreeMap<u32, ScalarType>,
    spill_counter: &mut usize,
) -> Option<LoopPlan> {
    let loop_len = body_pcs.len();
    if loop_len == 0 || body_pcs[0] != header_pc {
        return None;
    }
    let pc_to_idx: BTreeMap<usize, usize> = body_pcs
        .iter()
        .copied()
        .enumerate()
        .map(|(idx, pc)| (pc, idx))
        .collect();

    // 1. Check opcode eligibility and gather referenced slots & exit PCs.
    let mut slot_set: BTreeSet<u32> = BTreeSet::new();
    let mut slot_modified_set: BTreeSet<u32> = BTreeSet::new();
    let mut reg_modified = [false; GP_REGISTER_COUNT];
    let mut exit_pcs: BTreeSet<usize> = BTreeSet::new();

    for &pc in &body_pcs {
        let instr = &decoded[pc];
        match instr.opcode {
            Opcode::Safepoint | Opcode::Nop => {}
            Opcode::Mov => {
                let InstructionData::R { dst, .. } = instr.data else {
                    return None;
                };
                reg_modified[dst.0 as usize] = true;
            }
            Opcode::Swap => {
                let InstructionData::R { dst, src1, .. } = instr.data else {
                    return None;
                };
                reg_modified[dst.0 as usize] = true;
                reg_modified[src1.0 as usize] = true;
            }
            Opcode::LoadSlot => {
                let InstructionData::A { dst, base, imm12 } = instr.data else {
                    return None;
                };
                let slot = ((base.0 as u32) << 12) | (imm12 as u32);
                slot_set.insert(slot);
                reg_modified[dst.0 as usize] = true;
            }
            Opcode::StoreSlot => {
                let InstructionData::A { base, imm12, .. } = instr.data else {
                    return None;
                };
                let slot = ((base.0 as u32) << 12) | (imm12 as u32);
                slot_set.insert(slot);
                slot_modified_set.insert(slot);
            }
            Opcode::Load => {
                let InstructionData::A { dst, imm12, .. } = instr.data else {
                    return None;
                };
                if instr.amode == AddrMode::Const
                    && const_scalar_type(input, imm12 as u32).is_none()
                {
                    return None;
                }
                reg_modified[dst.0 as usize] = true;
            }
            Opcode::LoadConstWide => {
                let InstructionData::A { dst, base, imm12 } = instr.data else {
                    return None;
                };
                let idx = Instruction::wide_const_index(base, imm12);
                const_scalar_type(input, idx)?;
                reg_modified[dst.0 as usize] = true;
            }
            Opcode::LoadTrue | Opcode::LoadFalse => {
                let InstructionData::A { dst, .. } = instr.data else {
                    return None;
                };
                reg_modified[dst.0 as usize] = true;
            }
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
            | Opcode::CmpEq
            | Opcode::CmpNe
            | Opcode::CmpLt
            | Opcode::CmpLe
            | Opcode::CmpGt
            | Opcode::CmpGe => {
                let InstructionData::R { dst, .. } = instr.data else {
                    return None;
                };
                reg_modified[dst.0 as usize] = true;
            }
            Opcode::TypeAssert | Opcode::TypeCast => {
                if !(input.has_scope)(pc) || instr.amode != AddrMode::Imm {
                    return None;
                }
                let InstructionData::A { dst, imm12, .. } = instr.data else {
                    return None;
                };
                let ty = TypeIndex::from_raw(imm12 as u32);
                let ok = input
                    .type_pool
                    .canonical_type(ty)
                    .and_then(|c| input.type_pool.as_intrinsic(c))
                    .is_some_and(|k| {
                        matches!(k, Intrinsic::I64 | Intrinsic::F64 | Intrinsic::Bool)
                    });
                if !ok {
                    return None;
                }
                reg_modified[dst.0 as usize] = true;
            }
            Opcode::Jmp | Opcode::JmpFar | Opcode::JmpIf | Opcode::JmpIfNot => {
                let InstructionData::J { offset, .. } = instr.data else {
                    return None;
                };
                let target = ((pc as i64) + (offset as i64)) as usize;
                if !pc_to_idx.contains_key(&target) {
                    exit_pcs.insert(target);
                }
                if can_fall_through(instr) && !pc_to_idx.contains_key(&(pc + 1)) {
                    exit_pcs.insert(pc + 1);
                }
            }
            Opcode::Return | Opcode::ReturnUnit => {
                // Allow direct `return` from inside loop by treating `pc` as a side exit to baseline.
                exit_pcs.insert(pc);
            }
            _ => return None,
        }
        if can_fall_through(instr) && !pc_to_idx.contains_key(&(pc + 1)) {
            exit_pcs.insert(pc + 1);
        }
    }

    if slot_set.len() > MAX_TRACKED_SLOTS {
        return None;
    }

    // 2. Compute upward-exposed (`live_in`) slots & virtual registers at `header_pc`.
    let (live_in_slots, live_in_regs_mask) =
        compute_loop_upward_exposed(decoded, header_pc, &body_pcs, &pc_to_idx);

    // 3. Infer invariant `ScalarType` for every slot in `slot_set` and every `live_in` register,
    //    plus per-PC `reg_in[idx]` via fixed-point abstract interpretation!
    let mut slot_types: BTreeMap<u32, ScalarType> = BTreeMap::new();
    for &slot in &slot_set {
        if let Some(&hint) = pre_slot_hints.get(&slot) {
            slot_types.insert(slot, hint);
        }
    }

    let mut reg_live_in = [None::<ScalarType>; GP_REGISTER_COUNT];
    for (r, slot) in reg_live_in.iter_mut().enumerate() {
        if (live_in_regs_mask & (1u32 << r)) != 0 {
            // Default live-in loop registers without explicit float constants to I64 unless unified to F64/Bool.
            *slot = Some(ScalarType::I64);
        }
    }

    // Iteratively refine `slot_types` and `reg_live_in` from loop instructions.
    for _pass in 0..8 {
        let mut changed = false;
        let mut cur_regs = reg_live_in;
        for &pc in &body_pcs {
            let instr = &decoded[pc];
            match instr.opcode {
                Opcode::Load => {
                    if let InstructionData::A { dst, imm12, .. } = instr.data {
                        cur_regs[dst.0 as usize] = match instr.amode {
                            AddrMode::Imm => Some(ScalarType::I64),
                            AddrMode::Const => const_scalar_type(input, imm12 as u32),
                        };
                    }
                }
                Opcode::LoadConstWide => {
                    if let InstructionData::A { dst, base, imm12 } = instr.data {
                        cur_regs[dst.0 as usize] =
                            const_scalar_type(input, Instruction::wide_const_index(base, imm12));
                    }
                }
                Opcode::LoadTrue | Opcode::LoadFalse => {
                    if let InstructionData::A { dst, .. } = instr.data {
                        cur_regs[dst.0 as usize] = Some(ScalarType::Bool);
                    }
                }
                Opcode::LoadSlot => {
                    if let InstructionData::A { dst, base, imm12 } = instr.data {
                        let slot = ((base.0 as u32) << 12) | (imm12 as u32);
                        cur_regs[dst.0 as usize] = slot_types.get(&slot).copied();
                    }
                }
                Opcode::StoreSlot => {
                    if let InstructionData::A { dst, base, imm12 } = instr.data {
                        let slot = ((base.0 as u32) << 12) | (imm12 as u32);
                        if let Some(ty) = cur_regs[dst.0 as usize]
                            && slot_types.get(&slot) != Some(&ty)
                        {
                            slot_types.insert(slot, ty);
                            changed = true;
                        }
                    }
                }
                Opcode::Mov => {
                    if let InstructionData::R { dst, src1, .. } = instr.data {
                        cur_regs[dst.0 as usize] = cur_regs[src1.0 as usize];
                    }
                }
                Opcode::Swap => {
                    if let InstructionData::R { dst, src1, .. } = instr.data {
                        cur_regs.swap(dst.0 as usize, src1.0 as usize);
                    }
                }
                Opcode::TypeAssert | Opcode::TypeCast => {
                    if let InstructionData::A { dst, imm12, .. } = instr.data {
                        let ty = TypeIndex::from_raw(imm12 as u32);
                        cur_regs[dst.0 as usize] = match input
                            .type_pool
                            .canonical_type(ty)
                            .and_then(|c| input.type_pool.as_intrinsic(c))?
                        {
                            Intrinsic::I64 => Some(ScalarType::I64),
                            Intrinsic::F64 => Some(ScalarType::F64),
                            Intrinsic::Bool => Some(ScalarType::Bool),
                            _ => return None,
                        };
                    }
                }
                Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::Div | Opcode::Mod => {
                    if let InstructionData::R { dst, src1, src2 } = instr.data {
                        let a = cur_regs[src1.0 as usize];
                        let b = cur_regs[src2.0 as usize];
                        let out = match (a, b) {
                            (Some(ScalarType::F64), _) | (_, Some(ScalarType::F64)) => {
                                Some(ScalarType::F64)
                            }
                            (Some(ScalarType::I64), _) | (_, Some(ScalarType::I64)) => {
                                Some(ScalarType::I64)
                            }
                            _ => None,
                        };
                        // Back-propagate operand hint when one side is known and the other came from a slot.
                        if let (Some(known), None) = (a, b) {
                            backprop_reg_source_slot(
                                decoded,
                                preds,
                                &pc_to_idx,
                                header_pc,
                                pc,
                                src2,
                                known,
                                &mut slot_types,
                                &mut changed,
                            );
                        } else if let (None, Some(known)) = (a, b) {
                            backprop_reg_source_slot(
                                decoded,
                                preds,
                                &pc_to_idx,
                                header_pc,
                                pc,
                                src1,
                                known,
                                &mut slot_types,
                                &mut changed,
                            );
                        }
                        cur_regs[dst.0 as usize] = out;
                    }
                }
                Opcode::BitAnd | Opcode::BitOr | Opcode::BitXor => {
                    if let InstructionData::R { dst, src1, src2 } = instr.data {
                        backprop_reg_source_slot(
                            decoded,
                            preds,
                            &pc_to_idx,
                            header_pc,
                            pc,
                            src1,
                            ScalarType::I64,
                            &mut slot_types,
                            &mut changed,
                        );
                        backprop_reg_source_slot(
                            decoded,
                            preds,
                            &pc_to_idx,
                            header_pc,
                            pc,
                            src2,
                            ScalarType::I64,
                            &mut slot_types,
                            &mut changed,
                        );
                        cur_regs[dst.0 as usize] = Some(ScalarType::I64);
                    }
                }
                Opcode::BitNot => {
                    if let InstructionData::R { dst, src1, .. } = instr.data {
                        backprop_reg_source_slot(
                            decoded,
                            preds,
                            &pc_to_idx,
                            header_pc,
                            pc,
                            src1,
                            ScalarType::I64,
                            &mut slot_types,
                            &mut changed,
                        );
                        cur_regs[dst.0 as usize] = Some(ScalarType::I64);
                    }
                }
                Opcode::Neg => {
                    if let InstructionData::R { dst, src1, .. } = instr.data {
                        cur_regs[dst.0 as usize] = cur_regs[src1.0 as usize];
                    }
                }
                Opcode::CmpEq
                | Opcode::CmpNe
                | Opcode::CmpLt
                | Opcode::CmpLe
                | Opcode::CmpGt
                | Opcode::CmpGe => {
                    if let InstructionData::R { dst, src1, src2 } = instr.data {
                        let a = cur_regs[src1.0 as usize];
                        let b = cur_regs[src2.0 as usize];
                        if let (Some(known), None) = (a, b) {
                            backprop_reg_source_slot(
                                decoded,
                                preds,
                                &pc_to_idx,
                                header_pc,
                                pc,
                                src2,
                                known,
                                &mut slot_types,
                                &mut changed,
                            );
                        } else if let (None, Some(known)) = (a, b) {
                            backprop_reg_source_slot(
                                decoded,
                                preds,
                                &pc_to_idx,
                                header_pc,
                                pc,
                                src1,
                                known,
                                &mut slot_types,
                                &mut changed,
                            );
                        }
                        cur_regs[dst.0 as usize] = Some(ScalarType::Bool);
                    }
                }
                _ => {}
            }
        }
        if !changed {
            break;
        }
    }

    // Any remaining unresolved slot defaults to `I64` only if guarded live-in; otherwise reject.
    for &slot in &slot_set {
        if let std::collections::btree_map::Entry::Vacant(e) = slot_types.entry(slot) {
            if live_in_slots.contains(&slot) {
                e.insert(ScalarType::I64);
            } else {
                return None;
            }
        }
    }

    // 4. Perform exact forward dataflow over `body_pcs` to verify type safety
    //    at EVERY instruction and compute `reg_in[idx]` and `exit_reg_types`!
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum RegAbs {
        Unset,
        Known(ScalarType),
        Conflict,
    }

    let mut visited = vec![false; loop_len];
    let mut state_in = vec![[RegAbs::Unset; GP_REGISTER_COUNT]; loop_len];
    for r in 0..GP_REGISTER_COUNT {
        if let Some(ty) = reg_live_in[r] {
            state_in[0][r] = RegAbs::Known(ty);
        }
    }
    let mut exit_abs: BTreeMap<usize, [RegAbs; GP_REGISTER_COUNT]> = BTreeMap::new();

    let merge_into = |dst_arr: &mut [RegAbs; GP_REGISTER_COUNT],
                      dst_visited: &mut bool,
                      incoming: &[RegAbs; GP_REGISTER_COUNT]|
     -> bool {
        if !*dst_visited {
            *dst_arr = *incoming;
            *dst_visited = true;
            return true;
        }
        let mut changed = false;
        for r in 0..GP_REGISTER_COUNT {
            let merged = match (dst_arr[r], incoming[r]) {
                (a, b) if a == b => a,
                (_, _) => RegAbs::Conflict,
            };
            if merged != dst_arr[r] {
                dst_arr[r] = merged;
                changed = true;
            }
        }
        changed
    };

    visited[0] = true;
    let mut worklist = VecDeque::new();
    worklist.push_back(header_pc);

    while let Some(pc) = worklist.pop_front() {
        let idx = pc_to_idx[&pc];
        let mut cur = state_in[idx];
        let instr = &decoded[pc];

        let req_reg = |cur: &[RegAbs; GP_REGISTER_COUNT], r: Reg| -> Option<ScalarType> {
            match cur[r.0 as usize] {
                RegAbs::Known(ty) => Some(ty),
                _ => None,
            }
        };

        // If `pc` is `Return` / `ReturnUnit`, treat `pc` itself as an exit edge BEFORE executing `Return`.
        if matches!(instr.opcode, Opcode::Return | Opcode::ReturnUnit) {
            let mut dummy_vis = exit_abs.contains_key(&pc);
            let entry = exit_abs
                .entry(pc)
                .or_insert([RegAbs::Unset; GP_REGISTER_COUNT]);
            merge_into(entry, &mut dummy_vis, &cur);
            continue;
        }

        match instr.opcode {
            Opcode::Safepoint | Opcode::Nop => {}
            Opcode::Load => {
                let InstructionData::A { dst, imm12, .. } = instr.data else {
                    return None;
                };
                let ty = match instr.amode {
                    AddrMode::Imm => ScalarType::I64,
                    AddrMode::Const => const_scalar_type(input, imm12 as u32)?,
                };
                cur[dst.0 as usize] = RegAbs::Known(ty);
            }
            Opcode::LoadConstWide => {
                let InstructionData::A { dst, base, imm12 } = instr.data else {
                    return None;
                };
                let ty = const_scalar_type(input, Instruction::wide_const_index(base, imm12))?;
                cur[dst.0 as usize] = RegAbs::Known(ty);
            }
            Opcode::LoadTrue | Opcode::LoadFalse => {
                let InstructionData::A { dst, .. } = instr.data else {
                    return None;
                };
                cur[dst.0 as usize] = RegAbs::Known(ScalarType::Bool);
            }
            Opcode::LoadSlot => {
                let InstructionData::A { dst, base, imm12 } = instr.data else {
                    return None;
                };
                let slot = ((base.0 as u32) << 12) | (imm12 as u32);
                let ty = *slot_types.get(&slot)?;
                cur[dst.0 as usize] = RegAbs::Known(ty);
            }
            Opcode::StoreSlot => {
                let InstructionData::A { dst, base, imm12 } = instr.data else {
                    return None;
                };
                let slot = ((base.0 as u32) << 12) | (imm12 as u32);
                let src_ty = req_reg(&cur, dst)?;
                let slot_ty = *slot_types.get(&slot)?;
                if src_ty != slot_ty {
                    return None;
                }
            }
            Opcode::Mov => {
                let InstructionData::R { dst, src1, .. } = instr.data else {
                    return None;
                };
                let ty = req_reg(&cur, src1)?;
                cur[dst.0 as usize] = RegAbs::Known(ty);
            }
            Opcode::Swap => {
                let InstructionData::R { dst, src1, .. } = instr.data else {
                    return None;
                };
                let a = req_reg(&cur, dst)?;
                let b = req_reg(&cur, src1)?;
                cur[dst.0 as usize] = RegAbs::Known(b);
                cur[src1.0 as usize] = RegAbs::Known(a);
            }
            Opcode::TypeAssert | Opcode::TypeCast => {
                let InstructionData::A {
                    dst,
                    base: src,
                    imm12,
                } = instr.data
                else {
                    return None;
                };
                let src_ty = req_reg(&cur, src)?;
                let target_kind = input
                    .type_pool
                    .canonical_type(TypeIndex::from_raw(imm12 as u32))
                    .and_then(|c| input.type_pool.as_intrinsic(c))?;
                let out_ty = match (src_ty, target_kind) {
                    (ScalarType::I64, Intrinsic::I64) => ScalarType::I64,
                    (ScalarType::Bool, Intrinsic::Bool) => ScalarType::Bool,
                    (ScalarType::F64, Intrinsic::F64) => ScalarType::F64,
                    (ScalarType::I64, Intrinsic::F64) => ScalarType::F64,
                    _ => return None,
                };
                cur[dst.0 as usize] = RegAbs::Known(out_ty);
            }
            Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::Div => {
                let InstructionData::R { dst, src1, src2 } = instr.data else {
                    return None;
                };
                let a = req_reg(&cur, src1)?;
                let b = req_reg(&cur, src2)?;
                let out = match (a, b) {
                    (ScalarType::I64, ScalarType::I64) => ScalarType::I64,
                    (ScalarType::F64, ScalarType::F64)
                    | (ScalarType::I64, ScalarType::F64)
                    | (ScalarType::F64, ScalarType::I64) => ScalarType::F64,
                    _ => return None,
                };
                cur[dst.0 as usize] = RegAbs::Known(out);
            }
            Opcode::Mod => {
                let InstructionData::R { dst, src1, src2 } = instr.data else {
                    return None;
                };
                if req_reg(&cur, src1)? != ScalarType::I64
                    || req_reg(&cur, src2)? != ScalarType::I64
                {
                    return None;
                }
                cur[dst.0 as usize] = RegAbs::Known(ScalarType::I64);
            }
            Opcode::BitAnd | Opcode::BitOr | Opcode::BitXor => {
                let InstructionData::R { dst, src1, src2 } = instr.data else {
                    return None;
                };
                if req_reg(&cur, src1)? != ScalarType::I64
                    || req_reg(&cur, src2)? != ScalarType::I64
                {
                    return None;
                }
                cur[dst.0 as usize] = RegAbs::Known(ScalarType::I64);
            }
            Opcode::BitNot => {
                let InstructionData::R { dst, src1, .. } = instr.data else {
                    return None;
                };
                if req_reg(&cur, src1)? != ScalarType::I64 {
                    return None;
                }
                cur[dst.0 as usize] = RegAbs::Known(ScalarType::I64);
            }
            Opcode::Neg => {
                let InstructionData::R { dst, src1, .. } = instr.data else {
                    return None;
                };
                let a = req_reg(&cur, src1)?;
                if !matches!(a, ScalarType::I64 | ScalarType::F64) {
                    return None;
                }
                cur[dst.0 as usize] = RegAbs::Known(a);
            }
            Opcode::CmpEq | Opcode::CmpNe => {
                let InstructionData::R { dst, src1, src2 } = instr.data else {
                    return None;
                };
                let a = req_reg(&cur, src1)?;
                let b = req_reg(&cur, src2)?;
                if a != b {
                    return None;
                }
                cur[dst.0 as usize] = RegAbs::Known(ScalarType::Bool);
            }
            Opcode::CmpLt | Opcode::CmpLe | Opcode::CmpGt | Opcode::CmpGe => {
                let InstructionData::R { dst, src1, src2 } = instr.data else {
                    return None;
                };
                let a = req_reg(&cur, src1)?;
                let b = req_reg(&cur, src2)?;
                if a != b || !matches!(a, ScalarType::I64 | ScalarType::F64) {
                    return None;
                }
                cur[dst.0 as usize] = RegAbs::Known(ScalarType::Bool);
            }
            Opcode::JmpIf | Opcode::JmpIfNot => {
                let InstructionData::J { cond, .. } = instr.data else {
                    return None;
                };
                let cty = req_reg(&cur, cond)?;
                if !matches!(cty, ScalarType::Bool | ScalarType::I64) {
                    return None;
                }
            }
            Opcode::Jmp | Opcode::JmpFar => {}
            _ => return None,
        }

        // Propagate to successors.
        let mut succs: Vec<usize> = Vec::with_capacity(2);
        if let InstructionData::J { offset, .. } = instr.data {
            let tgt = ((pc as i64) + (offset as i64)) as usize;
            succs.push(tgt);
        }
        if can_fall_through(instr) {
            succs.push(pc + 1);
        }

        for succ in succs {
            if let Some(&sidx) = pc_to_idx.get(&succ) {
                if merge_into(&mut state_in[sidx], &mut visited[sidx], &cur) {
                    worklist.push_back(succ);
                }
            } else {
                let mut vis = exit_abs.contains_key(&succ);
                let entry = exit_abs
                    .entry(succ)
                    .or_insert([RegAbs::Unset; GP_REGISTER_COUNT]);
                merge_into(entry, &mut vis, &cur);
            }
        }
    }

    // Verify that at every exit target `exit_pc` (and any potentially-deopting arithmetic PC),
    // any register modified in the loop and upward-exposed from that PC has a non-conflicting `Known(ty)`.
    let mut exit_reg_types: BTreeMap<usize, [Option<ScalarType>; GP_REGISTER_COUNT]> =
        BTreeMap::new();
    for (&exit_pc, abs_arr) in &exit_abs {
        let live_after = if exit_pc < decoded.len() {
            compute_upward_exposed_regs_from(decoded, exit_pc)
        } else {
            0
        };
        let mut resolved = [None; GP_REGISTER_COUNT];
        for r in 0..GP_REGISTER_COUNT {
            match abs_arr[r] {
                RegAbs::Known(ty) => resolved[r] = Some(ty),
                RegAbs::Conflict if reg_modified[r] && (live_after & (1u32 << r)) != 0 => {
                    return None;
                }
                _ => {}
            }
        }
        exit_reg_types.insert(exit_pc, resolved);
    }

    let mut reg_in = vec![[None; GP_REGISTER_COUNT]; loop_len];
    for (i, &pc) in body_pcs.iter().enumerate() {
        if !visited[i] {
            // Dead unreachable instruction inside loop region: stay conservative and skip Tier-2.
            return None;
        }
        let snapshot_resume_pc = match decoded[pc].opcode {
            Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::Div | Opcode::Mod | Opcode::Neg => {
                Some(pc)
            }
            Opcode::Safepoint => Some(pc + 1),
            _ => None,
        };
        let live_at_snapshot = if let Some(resume_pc) = snapshot_resume_pc {
            compute_upward_exposed_regs_from(decoded, resume_pc)
        } else {
            0
        };
        for r in 0..GP_REGISTER_COUNT {
            match state_in[i][r] {
                RegAbs::Known(ty) => reg_in[i][r] = Some(ty),
                RegAbs::Conflict
                    if snapshot_resume_pc.is_some()
                        && reg_modified[r]
                        && (live_at_snapshot & (1u32 << r)) != 0 =>
                {
                    return None;
                }
                _ => {}
            }
        }
    }

    // 5. Allocate physical GP / XMM registers & spill slots!
    let mut gp_cursor = 0usize;
    let mut xmm_cursor = 0usize;

    let mut alloc_ty = |ty: ScalarType,
                        need_f64_save: bool,
                        spill_counter: &mut usize|
     -> (PhysLoc, Option<i32>) {
        let loc = match ty {
            ScalarType::F64 => {
                if xmm_cursor < LOOP_XMM_POOL.len() {
                    let reg = LOOP_XMM_POOL[xmm_cursor];
                    xmm_cursor += 1;
                    PhysLoc::Xmm(reg)
                } else {
                    PhysLoc::Spill(next_spill_disp(spill_counter))
                }
            }
            ScalarType::I64 | ScalarType::Bool => {
                if gp_cursor < LOOP_GP_POOL.len() {
                    let reg = LOOP_GP_POOL[gp_cursor];
                    gp_cursor += 1;
                    PhysLoc::Gp(reg)
                } else {
                    PhysLoc::Spill(next_spill_disp(spill_counter))
                }
            }
        };
        let f64_save_disp = if ty == ScalarType::F64 && need_f64_save {
            Some(next_spill_disp(spill_counter))
        } else {
            None
        };
        (loc, f64_save_disp)
    };

    // Prioritize loop-carried `local_slots` first for physical GP/XMM registers!
    let mut slots = Vec::with_capacity(slot_set.len());
    let mut slot_map = BTreeMap::new();
    for &slot_idx in &slot_set {
        let ty = slot_types[&slot_idx];
        let live_in = live_in_slots.contains(&slot_idx);
        let modified = slot_modified_set.contains(&slot_idx);
        let (loc, f64_save_disp) = alloc_ty(ty, modified, spill_counter);
        let pos = slots.len();
        slot_map.insert(slot_idx, pos);
        slots.push(SlotVar {
            slot_idx,
            ty,
            live_in,
            modified,
            loc,
            f64_save_disp,
        });
    }

    // Next allocate `(reg, ty)` pairs observed inside the loop.
    let mut used_reg_tys: BTreeSet<(u8, ScalarType)> = BTreeSet::new();
    for (r, slot) in reg_live_in.iter().enumerate() {
        if let Some(ty) = *slot {
            used_reg_tys.insert((r as u8, ty));
        }
    }
    for arr in &reg_in {
        for (r, slot) in arr.iter().enumerate() {
            if let Some(ty) = *slot {
                used_reg_tys.insert((r as u8, ty));
            }
        }
    }
    for arr in exit_reg_types.values() {
        for (r, slot) in arr.iter().enumerate() {
            if let Some(ty) = *slot {
                used_reg_tys.insert((r as u8, ty));
            }
        }
    }
    // Ensure any written register at `pc` (including `Swap`) has a `reg_locs` entry for its produced type.
    for (idx, &pc) in body_pcs.iter().enumerate() {
        let instr = &decoded[pc];
        if instr.opcode == Opcode::Swap
            && let InstructionData::R { dst, src1, .. } = instr.data
        {
            if let Some(ta) = reg_in[idx][dst.0 as usize] {
                used_reg_tys.insert((src1.0, ta));
            }
            if let Some(tb) = reg_in[idx][src1.0 as usize] {
                used_reg_tys.insert((dst.0, tb));
            }
        } else if let Some((dst, ty)) = produced_reg_type(input, instr, &slot_types, &reg_in[idx]) {
            used_reg_tys.insert((dst.0, ty));
        }
    }

    let mut reg_locs = BTreeMap::new();
    for &(r, ty) in &used_reg_tys {
        let need_save = reg_modified[r as usize];
        let (loc, f64_save_disp) = alloc_ty(ty, need_save, spill_counter);
        reg_locs.insert((r, ty), RegTypedLoc { loc, f64_save_disp });
    }

    let slot_dirty_disp = next_spill_disp(spill_counter);
    let reg_dirty_disp = next_spill_disp(spill_counter);

    Some(LoopPlan {
        header_pc,
        body_pcs,
        pc_to_idx,
        slots,
        slot_map,
        reg_locs,
        reg_in,
        exit_reg_types,
        reg_live_in,
        reg_modified,
        slot_dirty_disp,
        reg_dirty_disp,
    })
}

#[allow(clippy::too_many_arguments)]
fn backprop_reg_source_slot(
    decoded: &[Instruction],
    preds: &[Vec<usize>],
    pc_to_idx: &BTreeMap<usize, usize>,
    header_pc: usize,
    use_pc: usize,
    reg: Reg,
    ty: ScalarType,
    slot_types: &mut BTreeMap<u32, ScalarType>,
    changed: &mut bool,
) {
    // Walk backward within the same straight-line basic block inside `pc_to_idx`.
    let mut scan = use_pc;
    while scan > header_pc {
        if preds[scan].as_slice() != [scan - 1] || !pc_to_idx.contains_key(&(scan - 1)) {
            break;
        }
        scan -= 1;
        let prev = &decoded[scan];
        if matches!(
            prev.opcode,
            Opcode::Jmp | Opcode::JmpFar | Opcode::JmpIf | Opcode::JmpIfNot
        ) {
            break;
        }
        if prev.opcode == Opcode::LoadSlot
            && let InstructionData::A { dst, base, imm12 } = prev.data
            && dst == reg
        {
            let slot = ((base.0 as u32) << 12) | (imm12 as u32);
            if let std::collections::btree_map::Entry::Vacant(e) = slot_types.entry(slot) {
                e.insert(ty);
                *changed = true;
            }
            break;
        }
        if let InstructionData::R { dst, .. } | InstructionData::A { dst, .. } = prev.data
            && prev.opcode != Opcode::StoreSlot
            && dst == reg
        {
            break;
        }
    }
}

fn produced_reg_type(
    input: &CompilationInput<'_>,
    instr: &Instruction,
    slot_types: &BTreeMap<u32, ScalarType>,
    cur_regs: &[Option<ScalarType>; GP_REGISTER_COUNT],
) -> Option<(Reg, ScalarType)> {
    match instr.opcode {
        Opcode::Load => {
            let InstructionData::A { dst, imm12, .. } = instr.data else {
                return None;
            };
            let ty = match instr.amode {
                AddrMode::Imm => ScalarType::I64,
                AddrMode::Const => const_scalar_type(input, imm12 as u32)?,
            };
            Some((dst, ty))
        }
        Opcode::LoadConstWide => {
            let InstructionData::A { dst, base, imm12 } = instr.data else {
                return None;
            };
            Some((
                dst,
                const_scalar_type(input, Instruction::wide_const_index(base, imm12))?,
            ))
        }
        Opcode::LoadTrue | Opcode::LoadFalse => {
            let InstructionData::A { dst, .. } = instr.data else {
                return None;
            };
            Some((dst, ScalarType::Bool))
        }
        Opcode::LoadSlot => {
            let InstructionData::A { dst, base, imm12 } = instr.data else {
                return None;
            };
            let slot = ((base.0 as u32) << 12) | (imm12 as u32);
            Some((dst, *slot_types.get(&slot)?))
        }
        Opcode::Mov => {
            let InstructionData::R { dst, src1, .. } = instr.data else {
                return None;
            };
            Some((dst, cur_regs[src1.0 as usize]?))
        }
        Opcode::TypeAssert | Opcode::TypeCast => {
            let InstructionData::A { dst, imm12, .. } = instr.data else {
                return None;
            };
            let ty = match input
                .type_pool
                .canonical_type(TypeIndex::from_raw(imm12 as u32))
                .and_then(|c| input.type_pool.as_intrinsic(c))?
            {
                Intrinsic::I64 => ScalarType::I64,
                Intrinsic::F64 => ScalarType::F64,
                Intrinsic::Bool => ScalarType::Bool,
                _ => return None,
            };
            Some((dst, ty))
        }
        Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::Div => {
            let InstructionData::R { dst, src1, src2 } = instr.data else {
                return None;
            };
            let ty = if cur_regs[src1.0 as usize] == Some(ScalarType::F64)
                || cur_regs[src2.0 as usize] == Some(ScalarType::F64)
            {
                ScalarType::F64
            } else {
                ScalarType::I64
            };
            Some((dst, ty))
        }
        Opcode::Mod | Opcode::BitAnd | Opcode::BitOr | Opcode::BitXor | Opcode::BitNot => {
            let InstructionData::R { dst, .. } = instr.data else {
                return None;
            };
            Some((dst, ScalarType::I64))
        }
        Opcode::Neg => {
            let InstructionData::R { dst, src1, .. } = instr.data else {
                return None;
            };
            Some((dst, cur_regs[src1.0 as usize]?))
        }
        Opcode::CmpEq
        | Opcode::CmpNe
        | Opcode::CmpLt
        | Opcode::CmpLe
        | Opcode::CmpGt
        | Opcode::CmpGe => {
            let InstructionData::R { dst, .. } = instr.data else {
                return None;
            };
            Some((dst, ScalarType::Bool))
        }
        _ => None,
    }
}

/// Backward dataflow inside `body_pcs` (without back-edges to `header_pc`)
/// finding all slots and virtual registers read on ANY path from `header_pc` before being written.
fn compute_loop_upward_exposed(
    decoded: &[Instruction],
    header_pc: usize,
    body_pcs: &[usize],
    pc_to_idx: &BTreeMap<usize, usize>,
) -> (BTreeSet<u32>, u32) {
    let n = body_pcs.len();
    let mut live_slots: Vec<BTreeSet<u32>> = vec![BTreeSet::new(); n];
    let mut live_regs: Vec<u32> = vec![0; n];
    let mut exit_exposed_cache: BTreeMap<usize, u32> = BTreeMap::new();

    // Seed exit edges with registers upward-exposed after the loop!
    let mut changed = true;
    while changed {
        changed = false;
        for (idx, &pc) in body_pcs.iter().enumerate().rev() {
            let instr = &decoded[pc];
            let mut out_slots = BTreeSet::new();
            let mut out_regs = 0u32;

            if matches!(instr.opcode, Opcode::Return | Opcode::ReturnUnit) {
                if let Opcode::Return = instr.opcode
                    && let InstructionData::C { payload } = instr.data
                {
                    let r = Instruction::c_return_reg(payload).0;
                    if (r as usize) < GP_REGISTER_COUNT {
                        out_regs |= 1u32 << r;
                    }
                }
            } else {
                let mut succs = Vec::with_capacity(2);
                if let InstructionData::J { offset, .. } = instr.data {
                    let tgt = ((pc as i64) + (offset as i64)) as usize;
                    succs.push(tgt);
                }
                if can_fall_through(instr) {
                    succs.push(pc + 1);
                }
                for succ in succs {
                    if succ == header_pc {
                        // Exclude back-edge so upward-exposed analysis stays intra-iteration acyclic!
                        continue;
                    }
                    if let Some(&sidx) = pc_to_idx.get(&succ) {
                        out_slots.extend(live_slots[sidx].iter().copied());
                        out_regs |= live_regs[sidx];
                    } else if succ < decoded.len() {
                        let mask = *exit_exposed_cache
                            .entry(succ)
                            .or_insert_with(|| compute_upward_exposed_regs_from(decoded, succ));
                        out_regs |= mask;
                    }
                }
            }

            // Apply transfer function `in = use U (out \ def)`.
            let (use_slot, def_slot, use_rmask, def_rmask) = instr_use_def(instr);
            if let Some(ds) = def_slot {
                out_slots.remove(&ds);
            }
            if let Some(us) = use_slot {
                out_slots.insert(us);
            }
            out_regs = (out_regs & !def_rmask) | use_rmask;

            if out_slots != live_slots[idx] || out_regs != live_regs[idx] {
                live_slots[idx] = out_slots;
                live_regs[idx] = out_regs;
                changed = true;
            }
        }
    }

    (live_slots[0].clone(), live_regs[0])
}

/// Conservative $O(N)$ upward-exposed register mask starting from `start_pc` across the function.
fn compute_upward_exposed_regs_from(decoded: &[Instruction], start_pc: usize) -> u32 {
    if start_pc >= decoded.len() {
        return 0;
    }
    let mut seen = vec![false; decoded.len()];
    let mut killed_in = vec![u32::MAX; decoded.len()];
    let mut queue = VecDeque::new();
    seen[start_pc] = true;
    killed_in[start_pc] = 0;
    queue.push_back(start_pc);
    let mut exposed = 0u32;

    while let Some(pc) = queue.pop_front() {
        let mut killed = killed_in[pc];
        let instr = &decoded[pc];
        let (_, _, use_r, def_r) = instr_use_def(instr);
        exposed |= use_r & !killed;
        killed |= def_r;

        if matches!(
            instr.opcode,
            Opcode::Return | Opcode::ReturnUnit | Opcode::Unreachable | Opcode::MatchFail
        ) {
            continue;
        }

        let mut propagate = |tgt: usize| {
            if tgt < decoded.len() {
                if !seen[tgt] {
                    seen[tgt] = true;
                    killed_in[tgt] = killed;
                    queue.push_back(tgt);
                } else {
                    let merged = killed_in[tgt] & killed;
                    if merged != killed_in[tgt] {
                        killed_in[tgt] = merged;
                        queue.push_back(tgt);
                    }
                }
            }
        };

        if let InstructionData::J { offset, .. } = instr.data {
            let tgt = ((pc as i64) + (offset as i64)) as isize;
            if tgt >= 0 {
                propagate(tgt as usize);
            }
        }
        if can_fall_through(instr) {
            propagate(pc + 1);
        }
    }
    exposed
}

fn instr_use_def(instr: &Instruction) -> (Option<u32>, Option<u32>, u32, u32) {
    let rbit = |r: Reg| -> u32 {
        if (r.0 as usize) < GP_REGISTER_COUNT {
            1u32 << r.0
        } else {
            0
        }
    };
    let arg_mask = |count: usize| -> u32 {
        if count == 0 {
            0
        } else if count >= GP_REGISTER_COUNT {
            u32::MAX
        } else {
            (1u32 << count) - 1
        }
    };
    match instr.opcode {
        Opcode::Safepoint
        | Opcode::Nop
        | Opcode::Jmp
        | Opcode::JmpFar
        | Opcode::ReturnUnit
        | Opcode::AllocateSlots
        | Opcode::MatchFail
        | Opcode::Unreachable => (None, None, 0, 0),
        Opcode::LoadSlot => {
            let InstructionData::A { dst, base, imm12 } = instr.data else {
                return (None, None, 0, 0);
            };
            let slot = ((base.0 as u32) << 12) | (imm12 as u32);
            (Some(slot), None, 0, rbit(dst))
        }
        Opcode::StoreSlot => {
            let InstructionData::A { dst, base, imm12 } = instr.data else {
                return (None, None, 0, 0);
            };
            let slot = ((base.0 as u32) << 12) | (imm12 as u32);
            (None, Some(slot), rbit(dst), 0)
        }
        Opcode::Load
        | Opcode::LoadConstWide
        | Opcode::LoadTrue
        | Opcode::LoadFalse
        | Opcode::LoadUnit
        | Opcode::LoadNull
        | Opcode::NewObject
        | Opcode::NewList
        | Opcode::NewMap
        | Opcode::LoadGlobal
        | Opcode::LoadGlobalWide => {
            let InstructionData::A { dst, .. } = instr.data else {
                return (None, None, 0, 0);
            };
            (None, None, 0, rbit(dst))
        }
        Opcode::Mov => {
            let InstructionData::R { dst, src1, .. } = instr.data else {
                return (None, None, 0, 0);
            };
            (None, None, rbit(src1), rbit(dst))
        }
        Opcode::Swap => {
            let InstructionData::R { dst, src1, .. } = instr.data else {
                return (None, None, 0, 0);
            };
            let m = rbit(dst) | rbit(src1);
            (None, None, m, m)
        }
        Opcode::Neg | Opcode::BitNot => {
            let InstructionData::R { dst, src1, .. } = instr.data else {
                return (None, None, 0, 0);
            };
            (None, None, rbit(src1), rbit(dst))
        }
        Opcode::Add
        | Opcode::Sub
        | Opcode::Mul
        | Opcode::Div
        | Opcode::Mod
        | Opcode::BitAnd
        | Opcode::BitOr
        | Opcode::BitXor
        | Opcode::Shl
        | Opcode::Shr
        | Opcode::UShr
        | Opcode::CmpEq
        | Opcode::CmpNe
        | Opcode::CmpLt
        | Opcode::CmpLe
        | Opcode::CmpGt
        | Opcode::CmpGe => {
            let InstructionData::R { dst, src1, src2 } = instr.data else {
                return (None, None, 0, 0);
            };
            (None, None, rbit(src1) | rbit(src2), rbit(dst))
        }
        Opcode::TypeCheck
        | Opcode::TypeCast
        | Opcode::TypeCastSafe
        | Opcode::TypeAssert
        | Opcode::LoadField
        | Opcode::ErrorOk
        | Opcode::ErrorErr
        | Opcode::ErrorIsOk
        | Opcode::ErrorPayload
        | Opcode::NewEnum
        | Opcode::EnumIs
        | Opcode::EnumField
        | Opcode::TraitProof
        | Opcode::TraitProject => {
            let InstructionData::A { dst, base, .. } = instr.data else {
                return (None, None, 0, 0);
            };
            (None, None, rbit(base), rbit(dst))
        }
        Opcode::TraitAssert => {
            let InstructionData::A { dst, base, .. } = instr.data else {
                return (None, None, 0, 0);
            };
            (None, None, rbit(base) | rbit(dst), rbit(dst))
        }
        Opcode::StoreField => {
            let InstructionData::A { dst, base, .. } = instr.data else {
                return (None, None, 0, 0);
            };
            (None, None, rbit(dst) | rbit(base), 0)
        }
        Opcode::LoadIndex => {
            let InstructionData::A { dst, base, imm12 } = instr.data else {
                return (None, None, 0, 0);
            };
            let idx_bit = if (imm12 as usize) < GP_REGISTER_COUNT {
                1u32 << imm12
            } else {
                u32::MAX
            };
            (None, None, rbit(base) | idx_bit, rbit(dst))
        }
        Opcode::StoreIndex => {
            let InstructionData::A { dst, base, imm12 } = instr.data else {
                return (None, None, 0, 0);
            };
            let idx_bit = if (imm12 as usize) < GP_REGISTER_COUNT {
                1u32 << imm12
            } else {
                u32::MAX
            };
            (None, None, rbit(dst) | rbit(base) | idx_bit, 0)
        }
        Opcode::StoreGlobal | Opcode::StoreGlobalWide => {
            let InstructionData::A { dst, .. } = instr.data else {
                return (None, None, 0, 0);
            };
            (None, None, rbit(dst), 0)
        }
        Opcode::NewClosure | Opcode::NewClosureWide => {
            let InstructionData::A { dst, base, .. } = instr.data else {
                return (None, None, 0, 0);
            };
            (None, None, arg_mask(base.0 as usize), rbit(dst))
        }
        Opcode::Call | Opcode::CallFar | Opcode::CallBuiltin => {
            let InstructionData::C { payload } = instr.data else {
                return (None, None, u32::MAX, 1);
            };
            let (arg_count, _) = Instruction::c_arg_count_func_id(payload);
            (None, None, arg_mask(arg_count as usize), 1)
        }
        Opcode::CallIndirect | Opcode::CallIndirectProof => {
            let InstructionData::C { payload } = instr.data else {
                return (None, None, u32::MAX, 1);
            };
            let (arg_count, closure_reg) = Instruction::c_call_indirect(payload);
            (
                None,
                None,
                arg_mask(arg_count as usize) | rbit(closure_reg),
                1,
            )
        }
        Opcode::CallMethod | Opcode::CallMethodFar | Opcode::TraitCall => {
            let InstructionData::C { payload } = instr.data else {
                return (None, None, u32::MAX, 1);
            };
            let (arg_count, recv_reg, _) = Instruction::c_call_method(payload);
            (None, None, arg_mask(arg_count as usize) | rbit(recv_reg), 1)
        }
        Opcode::JmpIf | Opcode::JmpIfNot | Opcode::JmpIfNull | Opcode::JmpIfNotNull => {
            let InstructionData::J { cond, .. } = instr.data else {
                return (None, None, 0, 0);
            };
            (None, None, rbit(cond), 0)
        }
        Opcode::Return => {
            let InstructionData::C { payload } = instr.data else {
                return (None, None, 0, 0);
            };
            (None, None, rbit(Instruction::c_return_reg(payload)), 0)
        }
        // Conservative default for effect/handler ops outside loops: all GP regs potentially read, r0 written.
        _ => (None, None, u32::MAX, 1),
    }
}

// ---------------------------------------------------------------------------
// Tier-2 Native Loop Code Generation
// ---------------------------------------------------------------------------

struct ExitStub {
    lbl: Label,
    target_pc: usize,
    reg_state: [Option<ScalarType>; GP_REGISTER_COUNT],
}

struct DeoptStub {
    lbl: Label,
    deopt_pc: usize,
    reg_state: [Option<ScalarType>; GP_REGISTER_COUNT],
}

struct SafepointStub {
    slow_lbl: Label,
    resume_lbl: Label,
    resume_pc: usize,
    reg_state: [Option<ScalarType>; GP_REGISTER_COUNT],
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn emit_specialized_loop(
    asm: &mut Assembler,
    input: &CompilationInput<'_>,
    decoded: &[Instruction],
    plan: &LoopPlan,
    baseline_labels: &[Label],
    _forward_targets: &[Label],
    finished_lbl: Label,
    epilogue_lbl: Label,
) -> Result<(), JitError> {
    let fallback_lbl = baseline_labels[plan.header_pc];

    // 1. Check `rt_ctx.allow_tier2_loops != 0` and `*func_deopt_ptr == 0`.
    asm.movzx_rm8(GpReg::Rax, GpReg::R12, JitRuntimeCtx::OFF_ALLOW_TIER2_LOOPS);
    asm.test_rr(GpReg::Rax, GpReg::Rax);
    asm.jcc(Cond::E, fallback_lbl);

    asm.mov_ri64(GpReg::Rax, input.func_deopt_ptr as usize as u64);
    asm.mov_rm32(GpReg::Rax, GpReg::Rax, 0);
    asm.test_rr(GpReg::Rax, GpReg::Rax);
    asm.jcc(Cond::Ne, fallback_lbl);

    // 2. Bounds-check `slots_len` once for the highest referenced slot index in the loop!
    if let Some(max_slot) = plan.slots.iter().map(|s| s.slot_idx).max() {
        asm.mov_rm64(GpReg::Rcx, GpReg::R12, JitRuntimeCtx::OFF_SLOTS_LEN);
        asm.mov_ri64(GpReg::Rax, (max_slot as u64) + 1);
        asm.cmp_rr(GpReg::Rcx, GpReg::Rax);
        asm.jcc(Cond::B, fallback_lbl);
    }

    // 3. Guard & unbox all `live_in` slots and `live_in` registers from `StackContext`!
    emit_guard_and_unbox_live_in(asm, plan, fallback_lbl);

    // Initialize dirty masks on the stack frame (`1` for every `live_in` variable).
    let mut init_slot_dirty = 0u64;
    for (idx, sv) in plan.slots.iter().enumerate() {
        if sv.live_in {
            init_slot_dirty |= 1u64 << idx;
        }
    }
    asm.mov_ri64(GpReg::Rax, init_slot_dirty);
    asm.mov_mr64(GpReg::Rbp, plan.slot_dirty_disp, GpReg::Rax);

    let mut init_reg_dirty = 0u64;
    for (r, slot) in plan.reg_live_in.iter().enumerate() {
        if slot.is_some() {
            init_reg_dirty |= 1u64 << r;
        }
    }
    asm.mov_ri64(GpReg::Rax, init_reg_dirty);
    asm.mov_mr64(GpReg::Rbp, plan.reg_dirty_disp, GpReg::Rax);

    // 4. Emit the unboxed fast loop instructions!
    let loop_len = plan.body_pcs.len();
    let fast_pc_labels: Vec<Label> = (0..loop_len).map(|_| asm.new_label()).collect();

    // Cold exit / deopt / safepoint stubs accumulated during loop emission.
    let mut exit_stubs: BTreeMap<usize, ExitStub> = BTreeMap::new();
    let mut deopt_stubs: Vec<DeoptStub> = Vec::new();
    let mut safepoint_stubs: Vec<SafepointStub> = Vec::new();

    for (&exit_pc, &reg_state) in &plan.exit_reg_types {
        exit_stubs.insert(
            exit_pc,
            ExitStub {
                lbl: asm.new_label(),
                target_pc: exit_pc,
                reg_state,
            },
        );
    }

    for (rel, &pc) in plan.body_pcs.iter().enumerate() {
        let next_emitted_pc = plan.body_pcs.get(rel + 1).copied();
        asm.bind(fast_pc_labels[rel]);
        let instr = &decoded[pc];
        let cur_regs = &plan.reg_in[rel];

        if matches!(instr.opcode, Opcode::Return | Opcode::ReturnUnit) {
            asm.jmp(exit_stubs[&pc].lbl);
            continue;
        }

        match instr.opcode {
            Opcode::Nop => {}

            Opcode::Safepoint => {
                let slow_lbl = asm.new_label();
                let budget_zero_lbl = asm.new_label();
                let resume_lbl = asm.new_label();

                asm.mov_rm64(GpReg::Rax, GpReg::R12, JitRuntimeCtx::OFF_GC_REQ_PTR);
                asm.movzx_rm8(GpReg::Rax, GpReg::Rax, 0);
                asm.mov_rm64(GpReg::Rcx, GpReg::R12, JitRuntimeCtx::OFF_PREEMPT_REQ_PTR);
                asm.movzx_rm8(GpReg::Rcx, GpReg::Rcx, 0);
                asm.or_rr(GpReg::Rax, GpReg::Rcx);
                asm.jcc(Cond::Ne, slow_lbl);

                asm.mov_rm32(GpReg::Rax, GpReg::R12, JitRuntimeCtx::OFF_SLICE_SAFEPOINTS);
                asm.sub_ri32(GpReg::Rax, 1);
                asm.jcc(Cond::Be, budget_zero_lbl);
                asm.mov_mr32(GpReg::R12, JitRuntimeCtx::OFF_SLICE_SAFEPOINTS, GpReg::Rax);
                asm.jmp(resume_lbl);

                asm.bind(budget_zero_lbl);
                asm.mov_mi32(GpReg::R12, JitRuntimeCtx::OFF_SLICE_SAFEPOINTS, 1);
                asm.jmp(slow_lbl);

                asm.bind(resume_lbl);
                safepoint_stubs.push(SafepointStub {
                    slow_lbl,
                    resume_lbl,
                    resume_pc: pc + 1,
                    reg_state: *cur_regs,
                });
            }

            Opcode::Load => {
                let InstructionData::A { dst, imm12, .. } = instr.data else {
                    unreachable!()
                };
                match instr.amode {
                    AddrMode::Imm => {
                        let v = Instruction::sext_imm12(imm12) as i64;
                        asm.mov_ri64(GpReg::Rax, v as u64);
                        store_reg_val(asm, plan, dst, ScalarType::I64, GpReg::Rax, XmmReg::Xmm6);
                    }
                    AddrMode::Const => {
                        emit_unboxed_const(asm, input, plan, dst, imm12 as u32);
                    }
                }
            }

            Opcode::LoadConstWide => {
                let InstructionData::A { dst, base, imm12 } = instr.data else {
                    unreachable!()
                };
                emit_unboxed_const(
                    asm,
                    input,
                    plan,
                    dst,
                    Instruction::wide_const_index(base, imm12),
                );
            }

            Opcode::LoadTrue => {
                let InstructionData::A { dst, .. } = instr.data else {
                    unreachable!()
                };
                asm.mov_ri64(GpReg::Rax, 1);
                store_reg_val(asm, plan, dst, ScalarType::Bool, GpReg::Rax, XmmReg::Xmm6);
            }

            Opcode::LoadFalse => {
                let InstructionData::A { dst, .. } = instr.data else {
                    unreachable!()
                };
                asm.mov_ri64(GpReg::Rax, 0);
                store_reg_val(asm, plan, dst, ScalarType::Bool, GpReg::Rax, XmmReg::Xmm6);
            }

            Opcode::LoadSlot => {
                let InstructionData::A { dst, base, imm12 } = instr.data else {
                    unreachable!()
                };
                let slot = ((base.0 as u32) << 12) | (imm12 as u32);
                let sv = &plan.slots[plan.slot_map[&slot]];
                let reg_loc = plan.reg_locs[&(dst.0, sv.ty)].loc;
                emit_move_loc_to_loc(asm, reg_loc, sv.loc, sv.ty);
                mark_reg_dirty(asm, plan, dst);
            }

            Opcode::StoreSlot => {
                let InstructionData::A { dst, base, imm12 } = instr.data else {
                    unreachable!()
                };
                let slot = ((base.0 as u32) << 12) | (imm12 as u32);
                let sidx = plan.slot_map[&slot];
                let sv = &plan.slots[sidx];
                let reg_loc = plan.reg_locs[&(dst.0, sv.ty)].loc;
                emit_move_loc_to_loc(asm, sv.loc, reg_loc, sv.ty);
                if !sv.live_in {
                    mark_slot_dirty(asm, plan, sidx);
                }
            }

            Opcode::Mov => {
                let InstructionData::R { dst, src1, .. } = instr.data else {
                    unreachable!()
                };
                let ty = cur_regs[src1.0 as usize].unwrap();
                let src_loc = plan.reg_locs[&(src1.0, ty)].loc;
                let dst_loc = plan.reg_locs[&(dst.0, ty)].loc;
                emit_move_loc_to_loc(asm, dst_loc, src_loc, ty);
                mark_reg_dirty(asm, plan, dst);
            }

            Opcode::Swap => {
                let InstructionData::R { dst, src1, .. } = instr.data else {
                    unreachable!()
                };
                if dst != src1 {
                    let ta = cur_regs[dst.0 as usize].unwrap();
                    let tb = cur_regs[src1.0 as usize].unwrap();
                    load_reg_gp_or_xmm(asm, plan, dst, ta, GpReg::Rax, XmmReg::Xmm6);
                    load_reg_gp_or_xmm(asm, plan, src1, tb, GpReg::Rcx, XmmReg::Xmm7);
                    store_reg_val(asm, plan, dst, tb, GpReg::Rcx, XmmReg::Xmm7);
                    store_reg_val(asm, plan, src1, ta, GpReg::Rax, XmmReg::Xmm6);
                }
            }

            Opcode::TypeAssert | Opcode::TypeCast => {
                let InstructionData::A {
                    dst,
                    base: src,
                    imm12,
                } = instr.data
                else {
                    unreachable!()
                };
                let src_ty = cur_regs[src.0 as usize].unwrap();
                let target_kind = input
                    .type_pool
                    .canonical_type(TypeIndex::from_raw(imm12 as u32))
                    .and_then(|c| input.type_pool.as_intrinsic(c))
                    .unwrap();
                match (src_ty, target_kind) {
                    (ScalarType::I64, Intrinsic::F64) => {
                        load_reg_gp(asm, plan, src, ScalarType::I64, GpReg::Rax);
                        asm.cvtsi2sd(XmmReg::Xmm6, GpReg::Rax);
                        store_reg_val(asm, plan, dst, ScalarType::F64, GpReg::Rax, XmmReg::Xmm6);
                    }
                    _ => {
                        let src_loc = plan.reg_locs[&(src.0, src_ty)].loc;
                        let dst_loc = plan.reg_locs[&(dst.0, src_ty)].loc;
                        emit_move_loc_to_loc(asm, dst_loc, src_loc, src_ty);
                        mark_reg_dirty(asm, plan, dst);
                    }
                }
            }

            Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::Div | Opcode::Mod => {
                let InstructionData::R { dst, src1, src2 } = instr.data else {
                    unreachable!()
                };
                let ta = cur_regs[src1.0 as usize].unwrap();
                let tb = cur_regs[src2.0 as usize].unwrap();
                if ta == ScalarType::F64 || tb == ScalarType::F64 {
                    load_reg_as_f64(asm, plan, src1, ta, XmmReg::Xmm6);
                    load_reg_as_f64(asm, plan, src2, tb, XmmReg::Xmm7);
                    if instr.opcode == Opcode::Div {
                        // In Nessa, float `Div` by `+0.0` or `-0.0` raises `DivisionByZero`!
                        // `ucomisd(xmm7, 0.0)`: `ZF == 1 && PF == 0` iff `xmm7 == ±0.0` (not `NaN`!).
                        let deopt_lbl = asm.new_label();
                        let non_zero_lbl = asm.new_label();
                        asm.mov_ri64(GpReg::Rax, 0);
                        asm.movq_xr(XmmReg::Xmm8, GpReg::Rax);
                        asm.ucomisd(XmmReg::Xmm7, XmmReg::Xmm8);
                        asm.jcc(Cond::P, non_zero_lbl); // NaN divisor produces NaN, not DivisionByZero
                        asm.jcc(Cond::E, deopt_lbl);
                        asm.bind(non_zero_lbl);
                        deopt_stubs.push(DeoptStub {
                            lbl: deopt_lbl,
                            deopt_pc: pc,
                            reg_state: *cur_regs,
                        });
                    }
                    match instr.opcode {
                        Opcode::Add => asm.addsd(XmmReg::Xmm6, XmmReg::Xmm7),
                        Opcode::Sub => asm.subsd(XmmReg::Xmm6, XmmReg::Xmm7),
                        Opcode::Mul => asm.mulsd(XmmReg::Xmm6, XmmReg::Xmm7),
                        Opcode::Div => asm.divsd(XmmReg::Xmm6, XmmReg::Xmm7),
                        _ => unreachable!(),
                    }
                    store_reg_val(asm, plan, dst, ScalarType::F64, GpReg::Rax, XmmReg::Xmm6);
                } else {
                    let deopt_lbl = asm.new_label();
                    load_reg_gp(asm, plan, src1, ScalarType::I64, GpReg::Rax);
                    load_reg_gp(asm, plan, src2, ScalarType::I64, GpReg::Rcx);
                    match instr.opcode {
                        Opcode::Add => {
                            asm.add_rr(GpReg::Rax, GpReg::Rcx);
                            asm.jcc(Cond::O, deopt_lbl);
                        }
                        Opcode::Sub => {
                            asm.sub_rr(GpReg::Rax, GpReg::Rcx);
                            asm.jcc(Cond::O, deopt_lbl);
                        }
                        Opcode::Mul => {
                            asm.imul_rr(GpReg::Rax, GpReg::Rcx);
                            asm.jcc(Cond::O, deopt_lbl);
                        }
                        Opcode::Div => {
                            asm.test_rr(GpReg::Rcx, GpReg::Rcx);
                            asm.jcc(Cond::E, deopt_lbl);
                            asm.cqo();
                            asm.idiv_r(GpReg::Rcx);
                        }
                        Opcode::Mod => {
                            asm.test_rr(GpReg::Rcx, GpReg::Rcx);
                            asm.jcc(Cond::E, deopt_lbl);
                            let not_neg1_lbl = asm.new_label();
                            let mod_done_lbl = asm.new_label();
                            asm.cmp_ri32(GpReg::Rcx, -1);
                            asm.jcc(Cond::Ne, not_neg1_lbl);
                            asm.mov_ri64(GpReg::Rax, 0);
                            asm.jmp(mod_done_lbl);
                            asm.bind(not_neg1_lbl);
                            asm.cqo();
                            asm.idiv_r(GpReg::Rcx);
                            asm.mov_rr(GpReg::Rax, GpReg::Rdx);
                            asm.bind(mod_done_lbl);
                        }
                        _ => unreachable!(),
                    }
                    // Verify 57-bit signed `IMM_I_SMALL` range `[-2^56, 2^56 - 1]`:
                    // `(rax >> 56)` must be `0` or `-1`, so `((rax >> 56) + 1) <= 1` unsigned!
                    asm.mov_rr(GpReg::Rdx, GpReg::Rax);
                    asm.sar_ri8(GpReg::Rdx, 56);
                    asm.add_ri32(GpReg::Rdx, 1);
                    asm.cmp_ri32(GpReg::Rdx, 1);
                    asm.jcc(Cond::A, deopt_lbl);

                    store_reg_val(asm, plan, dst, ScalarType::I64, GpReg::Rax, XmmReg::Xmm6);
                    deopt_stubs.push(DeoptStub {
                        lbl: deopt_lbl,
                        deopt_pc: pc,
                        reg_state: *cur_regs,
                    });
                }
            }

            Opcode::BitAnd | Opcode::BitOr | Opcode::BitXor => {
                let InstructionData::R { dst, src1, src2 } = instr.data else {
                    unreachable!()
                };
                load_reg_gp(asm, plan, src1, ScalarType::I64, GpReg::Rax);
                load_reg_gp(asm, plan, src2, ScalarType::I64, GpReg::Rcx);
                match instr.opcode {
                    Opcode::BitAnd => asm.and_rr(GpReg::Rax, GpReg::Rcx),
                    Opcode::BitOr => asm.or_rr(GpReg::Rax, GpReg::Rcx),
                    Opcode::BitXor => asm.xor_rr(GpReg::Rax, GpReg::Rcx),
                    _ => unreachable!(),
                }
                store_reg_val(asm, plan, dst, ScalarType::I64, GpReg::Rax, XmmReg::Xmm6);
            }

            Opcode::BitNot => {
                let InstructionData::R { dst, src1, .. } = instr.data else {
                    unreachable!()
                };
                load_reg_gp(asm, plan, src1, ScalarType::I64, GpReg::Rax);
                asm.not_r(GpReg::Rax);
                store_reg_val(asm, plan, dst, ScalarType::I64, GpReg::Rax, XmmReg::Xmm6);
            }

            Opcode::Neg => {
                let InstructionData::R { dst, src1, .. } = instr.data else {
                    unreachable!()
                };
                let ty = cur_regs[src1.0 as usize].unwrap();
                if ty == ScalarType::F64 {
                    load_reg_as_f64(asm, plan, src1, ScalarType::F64, XmmReg::Xmm6);
                    asm.mov_ri64(GpReg::Rax, 1u64 << 63);
                    asm.movq_xr(XmmReg::Xmm7, GpReg::Rax);
                    asm.xorpd(XmmReg::Xmm6, XmmReg::Xmm7);
                    store_reg_val(asm, plan, dst, ScalarType::F64, GpReg::Rax, XmmReg::Xmm6);
                } else {
                    let deopt_lbl = asm.new_label();
                    load_reg_gp(asm, plan, src1, ScalarType::I64, GpReg::Rax);
                    asm.neg_r(GpReg::Rax);
                    asm.jcc(Cond::O, deopt_lbl);
                    asm.mov_rr(GpReg::Rdx, GpReg::Rax);
                    asm.sar_ri8(GpReg::Rdx, 56);
                    asm.add_ri32(GpReg::Rdx, 1);
                    asm.cmp_ri32(GpReg::Rdx, 1);
                    asm.jcc(Cond::A, deopt_lbl);
                    store_reg_val(asm, plan, dst, ScalarType::I64, GpReg::Rax, XmmReg::Xmm6);
                    deopt_stubs.push(DeoptStub {
                        lbl: deopt_lbl,
                        deopt_pc: pc,
                        reg_state: *cur_regs,
                    });
                }
            }

            Opcode::CmpEq
            | Opcode::CmpNe
            | Opcode::CmpLt
            | Opcode::CmpLe
            | Opcode::CmpGt
            | Opcode::CmpGe => {
                let InstructionData::R { dst, src1, src2 } = instr.data else {
                    unreachable!()
                };
                let ty = cur_regs[src1.0 as usize].unwrap();
                if ty == ScalarType::F64 {
                    load_reg_as_f64(asm, plan, src1, ScalarType::F64, XmmReg::Xmm6);
                    load_reg_as_f64(asm, plan, src2, ScalarType::F64, XmmReg::Xmm7);
                    match instr.opcode {
                        Opcode::CmpGt => {
                            asm.mov_ri64(GpReg::Rax, 0);
                            asm.ucomisd(XmmReg::Xmm6, XmmReg::Xmm7);
                            asm.setcc_al(Cond::A);
                        }
                        Opcode::CmpGe => {
                            asm.mov_ri64(GpReg::Rax, 0);
                            asm.ucomisd(XmmReg::Xmm6, XmmReg::Xmm7);
                            asm.setcc_al(Cond::Ae);
                        }
                        Opcode::CmpLt => {
                            asm.mov_ri64(GpReg::Rax, 0);
                            asm.ucomisd(XmmReg::Xmm7, XmmReg::Xmm6);
                            asm.setcc_al(Cond::A);
                        }
                        Opcode::CmpLe => {
                            asm.mov_ri64(GpReg::Rax, 0);
                            asm.ucomisd(XmmReg::Xmm7, XmmReg::Xmm6);
                            asm.setcc_al(Cond::Ae);
                        }
                        Opcode::CmpEq | Opcode::CmpNe => {
                            let nan_lbl = asm.new_label();
                            asm.ucomisd(XmmReg::Xmm6, XmmReg::Xmm7);
                            asm.mov_ri64(GpReg::Rax, 0);
                            asm.jcc(Cond::P, nan_lbl);
                            asm.setcc_al(Cond::E);
                            asm.bind(nan_lbl);
                            if instr.opcode == Opcode::CmpNe {
                                asm.xor_ri32(GpReg::Rax, 1);
                            }
                        }
                        _ => unreachable!(),
                    }
                    store_reg_val(asm, plan, dst, ScalarType::Bool, GpReg::Rax, XmmReg::Xmm6);
                } else {
                    load_reg_gp(asm, plan, src1, ty, GpReg::Rax);
                    load_reg_gp(asm, plan, src2, ty, GpReg::Rcx);
                    asm.cmp_rr(GpReg::Rax, GpReg::Rcx);
                    asm.mov_ri64(GpReg::Rax, 0);
                    let cond = match instr.opcode {
                        Opcode::CmpEq => Cond::E,
                        Opcode::CmpNe => Cond::Ne,
                        Opcode::CmpLt => Cond::L,
                        Opcode::CmpLe => Cond::Le,
                        Opcode::CmpGt => Cond::G,
                        Opcode::CmpGe => Cond::Ge,
                        _ => unreachable!(),
                    };
                    asm.setcc_al(cond);
                    store_reg_val(asm, plan, dst, ScalarType::Bool, GpReg::Rax, XmmReg::Xmm6);
                }
            }

            Opcode::Jmp | Opcode::JmpFar => {
                let InstructionData::J { offset, .. } = instr.data else {
                    unreachable!()
                };
                emit_fast_loop_jump(
                    asm,
                    input,
                    decoded,
                    plan,
                    pc,
                    offset,
                    &fast_pc_labels,
                    &exit_stubs,
                    &mut safepoint_stubs,
                );
            }

            Opcode::JmpIf | Opcode::JmpIfNot => {
                let InstructionData::J { cond, offset } = instr.data else {
                    unreachable!()
                };
                let cty = cur_regs[cond.0 as usize].unwrap();
                load_reg_gp(asm, plan, cond, cty, GpReg::Rax);
                asm.test_rr(GpReg::Rax, GpReg::Rax);
                let fallthrough_lbl = asm.new_label();
                if instr.opcode == Opcode::JmpIf {
                    asm.jcc(Cond::E, fallthrough_lbl);
                } else {
                    asm.jcc(Cond::Ne, fallthrough_lbl);
                }
                emit_fast_loop_jump(
                    asm,
                    input,
                    decoded,
                    plan,
                    pc,
                    offset,
                    &fast_pc_labels,
                    &exit_stubs,
                    &mut safepoint_stubs,
                );
                asm.bind(fallthrough_lbl);
                if next_emitted_pc != Some(pc + 1) {
                    if let Some(&nidx) = plan.pc_to_idx.get(&(pc + 1)) {
                        asm.jmp(fast_pc_labels[nidx]);
                    } else {
                        asm.jmp(exit_stubs[&(pc + 1)].lbl);
                    }
                }
            }

            _ => unreachable!("filtered by analyze_loop_candidate"),
        }

        if can_fall_through(instr)
            && !matches!(instr.opcode, Opcode::JmpIf | Opcode::JmpIfNot)
            && next_emitted_pc != Some(pc + 1)
        {
            if let Some(&nidx) = plan.pc_to_idx.get(&(pc + 1)) {
                asm.jmp(fast_pc_labels[nidx]);
            } else {
                asm.jmp(exit_stubs[&(pc + 1)].lbl);
            }
        }
    }

    // 5. Emit cold Exit Stubs (box modified state back to `StackContext`, then jump to baseline!).
    for stub in exit_stubs.values() {
        asm.bind(stub.lbl);
        emit_box_state_to_stack_context(
            asm,
            input,
            plan,
            &stub.reg_state,
            stub.target_pc as u32,
            epilogue_lbl,
        );
        if stub.target_pc >= decoded.len() {
            asm.jmp(finished_lbl);
        } else {
            asm.jmp(baseline_labels[stub.target_pc]);
        }
    }

    // 6. Emit cold Deopt Stubs (box state as of `deopt_pc`, bump `deopt_count`, exit `InterpretAt`).
    for stub in &deopt_stubs {
        asm.bind(stub.lbl);
        emit_box_state_to_stack_context(
            asm,
            input,
            plan,
            &stub.reg_state,
            stub.deopt_pc as u32,
            epilogue_lbl,
        );
        // Increment global & per-function deoptimization counters.
        asm.mov_ri64(GpReg::Rax, input.global_deopt_ptr as usize as u64);
        asm.add_m64_i32(GpReg::Rax, 0, 1);
        asm.mov_ri64(GpReg::Rax, input.func_deopt_ptr as usize as u64);
        asm.mov_rm32(GpReg::Rcx, GpReg::Rax, 0);
        asm.add_ri32(GpReg::Rcx, 1);
        asm.mov_mr32(GpReg::Rax, 0, GpReg::Rcx);

        emit_exit(
            asm,
            JitExitKind::InterpretAt,
            stub.deopt_pc as u32,
            None,
            epilogue_lbl,
        );
    }

    // 7. Emit cold Safepoint Stubs (box state -> call `safepoint` -> reload from `StackContext`!).
    for stub in &safepoint_stubs {
        asm.bind(stub.slow_lbl);
        emit_box_state_to_stack_context(
            asm,
            input,
            plan,
            &stub.reg_state,
            stub.resume_pc as u32,
            epilogue_lbl,
        );
        asm.mov_rr(GpReg::Rdi, GpReg::R12);
        asm.mov_ri64(GpReg::Rsi, stub.resume_pc as u64);
        asm.mov_ri64(GpReg::Rax, input.helpers.safepoint as usize as u64);
        asm.call_r(GpReg::Rax);
        emit_after_helper_check(asm, stub.resume_pc as u32, epilogue_lbl);

        // Safepoint returned `Continue (0)` (e.g. STW GC ran): re-unbox dirty state from `StackContext`
        // and resume the fast loop directly at `resume_lbl`!
        emit_reload_unboxed_after_safepoint(asm, plan, &stub.reg_state);
        asm.jmp(stub.resume_lbl);
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn emit_fast_loop_jump(
    asm: &mut Assembler,
    input: &CompilationInput<'_>,
    decoded: &[Instruction],
    plan: &LoopPlan,
    pc: usize,
    offset: i32,
    fast_pc_labels: &[Label],
    exit_stubs: &BTreeMap<usize, ExitStub>,
    safepoint_stubs: &mut Vec<SafepointStub>,
) {
    let target_pc = ((pc as i64) + (offset as i64)) as usize;
    if let Some(&tidx) = plan.pc_to_idx.get(&target_pc) {
        if offset <= 0 {
            asm.mov_ri64(GpReg::Rax, input.hotness_ptr as usize as u64);
            asm.add_m64_i32(GpReg::Rax, 0, 1);

            if decoded[target_pc].opcode != Opcode::Safepoint {
                let slow_lbl = asm.new_label();
                let budget_zero_lbl = asm.new_label();
                let resume_lbl = asm.new_label();

                asm.mov_rm64(GpReg::Rax, GpReg::R12, JitRuntimeCtx::OFF_GC_REQ_PTR);
                asm.movzx_rm8(GpReg::Rax, GpReg::Rax, 0);
                asm.mov_rm64(GpReg::Rcx, GpReg::R12, JitRuntimeCtx::OFF_PREEMPT_REQ_PTR);
                asm.movzx_rm8(GpReg::Rcx, GpReg::Rcx, 0);
                asm.or_rr(GpReg::Rax, GpReg::Rcx);
                asm.jcc(Cond::Ne, slow_lbl);

                asm.mov_rm32(GpReg::Rax, GpReg::R12, JitRuntimeCtx::OFF_SLICE_SAFEPOINTS);
                asm.sub_ri32(GpReg::Rax, 1);
                asm.jcc(Cond::Be, budget_zero_lbl);
                asm.mov_mr32(GpReg::R12, JitRuntimeCtx::OFF_SLICE_SAFEPOINTS, GpReg::Rax);
                asm.jmp(resume_lbl);

                asm.bind(budget_zero_lbl);
                asm.mov_mi32(GpReg::R12, JitRuntimeCtx::OFF_SLICE_SAFEPOINTS, 1);
                asm.jmp(slow_lbl);

                asm.bind(resume_lbl);
                safepoint_stubs.push(SafepointStub {
                    slow_lbl,
                    resume_lbl,
                    resume_pc: target_pc,
                    reg_state: plan.reg_in[tidx],
                });
            }
        }
        asm.jmp(fast_pc_labels[tidx]);
    } else {
        asm.jmp(exit_stubs[&target_pc].lbl);
    }
}

fn emit_guard_and_unbox_live_in(asm: &mut Assembler, plan: &LoopPlan, fallback_lbl: Label) {
    for sv in &plan.slots {
        if sv.live_in {
            let disp = ((sv.slot_idx as usize) * 8) as i32;
            asm.mov_rm64(GpReg::Rax, GpReg::R14, disp);
            emit_guard_and_unbox_value(asm, sv.ty, sv.loc, fallback_lbl);
        }
    }
    for (r, slot) in plan.reg_live_in.iter().enumerate() {
        if let Some(ty) = *slot {
            let disp = (r * 8) as i32;
            asm.mov_rm64(GpReg::Rax, GpReg::R13, disp);
            let loc = plan.reg_locs[&(r as u8, ty)].loc;
            emit_guard_and_unbox_value(asm, ty, loc, fallback_lbl);
        }
    }
}

/// Expects raw `TaggedValue` in `Rax`. Clobbers only `Rax` and `Rcx`.
fn emit_guard_and_unbox_value(
    asm: &mut Assembler,
    ty: ScalarType,
    dst_loc: PhysLoc,
    fail_lbl: Label,
) {
    match ty {
        ScalarType::I64 => {
            asm.mov_rr(GpReg::Rcx, GpReg::Rax);
            asm.and_ri32(GpReg::Rcx, IMM_LOW7_MASK as i32);
            asm.cmp_ri32(GpReg::Rcx, IMM_I_SMALL_TAG as i32);
            asm.jcc(Cond::Ne, fail_lbl);
            asm.sar_ri8(GpReg::Rax, 7);
            match dst_loc {
                PhysLoc::Gp(g) => asm.mov_rr(g, GpReg::Rax),
                PhysLoc::Spill(disp) => asm.mov_mr64(GpReg::Rbp, disp, GpReg::Rax),
                PhysLoc::Xmm(_) => unreachable!(),
            }
        }
        ScalarType::Bool => {
            asm.mov_rr(GpReg::Rcx, GpReg::Rax);
            asm.and_ri32(GpReg::Rcx, IMM_BOOL_MASK_NO_BIT7);
            asm.cmp_ri32(GpReg::Rcx, TAG_FALSE as i32);
            asm.jcc(Cond::Ne, fail_lbl);
            asm.shr_ri8(GpReg::Rax, 7);
            asm.and_ri32(GpReg::Rax, 1);
            match dst_loc {
                PhysLoc::Gp(g) => asm.mov_rr(g, GpReg::Rax),
                PhysLoc::Spill(disp) => asm.mov_mr64(GpReg::Rbp, disp, GpReg::Rax),
                PhysLoc::Xmm(_) => unreachable!(),
            }
        }
        ScalarType::F64 => {
            let imm_lbl = asm.new_label();
            let done_lbl = asm.new_label();

            asm.mov_rr(GpReg::Rcx, GpReg::Rax);
            asm.and_ri32(GpReg::Rcx, IMM_LOW7_MASK as i32);
            asm.cmp_ri32(GpReg::Rcx, IMM_F64_TAG as i32);
            asm.jcc(Cond::E, imm_lbl);

            asm.test_rr(GpReg::Rax, GpReg::Rax);
            asm.jcc(Cond::E, fail_lbl);
            asm.test_ri32(GpReg::Rax, 0b111);
            asm.jcc(Cond::Ne, fail_lbl);
            asm.mov_rm32(GpReg::Rcx, GpReg::Rax, HDR_TYPE_INDEX_DISP);
            asm.cmp_ri32(GpReg::Rcx, INTRINSIC_F64_RAW as i32);
            asm.jcc(Cond::Ne, fail_lbl);
            asm.mov_rm32(GpReg::Rcx, GpReg::Rax, HDR_GC_META_DISP);
            asm.and_ri32(GpReg::Rcx, GC_META_ROLE_AND_WORDS_MASK);
            asm.cmp_ri32(GpReg::Rcx, GC_META_ORDINARY_1WORD);
            asm.jcc(Cond::Ne, fail_lbl);

            match dst_loc {
                PhysLoc::Xmm(x) => asm.movsd_xm(x, GpReg::Rax, 0),
                PhysLoc::Spill(disp) => {
                    asm.mov_rm64(GpReg::Rcx, GpReg::Rax, 0);
                    asm.mov_mr64(GpReg::Rbp, disp, GpReg::Rcx);
                }
                PhysLoc::Gp(_) => unreachable!(),
            }
            asm.jmp(done_lbl);

            asm.bind(imm_lbl);
            asm.and_ri32(GpReg::Rax, !(IMM_LOW7_MASK as i32));
            match dst_loc {
                PhysLoc::Xmm(x) => asm.movq_xr(x, GpReg::Rax),
                PhysLoc::Spill(disp) => asm.mov_mr64(GpReg::Rbp, disp, GpReg::Rax),
                PhysLoc::Gp(_) => unreachable!(),
            }

            asm.bind(done_lbl);
        }
    }
}

fn emit_test_dirty_bit(asm: &mut Assembler, mask_reg: GpReg, bit_idx: usize, zero_lbl: Label) {
    if bit_idx < 31 {
        asm.test_ri32(mask_reg, 1i32 << bit_idx);
    } else {
        asm.mov_rr(GpReg::Rax, mask_reg);
        asm.shr_ri8(GpReg::Rax, bit_idx as u8);
        asm.test_ri32(GpReg::Rax, 1);
    }
    asm.jcc(Cond::E, zero_lbl);
}

#[allow(clippy::too_many_lines)]
fn emit_box_state_to_stack_context(
    asm: &mut Assembler,
    input: &CompilationInput<'_>,
    plan: &LoopPlan,
    reg_state: &[Option<ScalarType>; GP_REGISTER_COUNT],
    exit_pc: u32,
    epilogue_lbl: Label,
) {
    // Phase A (no C calls: all `LOOP_GP_POOL` and `LOOP_XMM_POOL` registers remain intact):
    // 1. Box dirty modified `I64` & `Bool` local slots directly into `[r14 + slot*8]`.
    if plan
        .slots
        .iter()
        .any(|s| s.modified && matches!(s.ty, ScalarType::I64 | ScalarType::Bool))
    {
        asm.mov_rm64(GpReg::Rdx, GpReg::Rbp, plan.slot_dirty_disp);
        for (idx, sv) in plan.slots.iter().enumerate() {
            if !sv.modified || !matches!(sv.ty, ScalarType::I64 | ScalarType::Bool) {
                continue;
            }
            let skip_lbl = asm.new_label();
            emit_test_dirty_bit(asm, GpReg::Rdx, idx, skip_lbl);
            match sv.loc {
                PhysLoc::Gp(g) => asm.mov_rr(GpReg::Rax, g),
                PhysLoc::Spill(disp) => asm.mov_rm64(GpReg::Rax, GpReg::Rbp, disp),
                PhysLoc::Xmm(_) => unreachable!(),
            }
            asm.shl_ri8(GpReg::Rax, 7);
            let tag = if sv.ty == ScalarType::I64 {
                IMM_I_SMALL_TAG as i32
            } else {
                TAG_FALSE as i32
            };
            asm.or_ri32(GpReg::Rax, tag);
            asm.mov_mr64(GpReg::R14, ((sv.slot_idx as usize) * 8) as i32, GpReg::Rax);
            asm.bind(skip_lbl);
        }
    }

    // 2. Box dirty modified `I64` & `Bool` virtual registers directly into `[r13 + r*8]`.
    if reg_state
        .iter()
        .enumerate()
        .any(|(r, t)| plan.reg_modified[r] && matches!(t, Some(ScalarType::I64 | ScalarType::Bool)))
    {
        asm.mov_rm64(GpReg::Rdx, GpReg::Rbp, plan.reg_dirty_disp);
        for (r, slot) in reg_state.iter().enumerate() {
            let Some(ty @ (ScalarType::I64 | ScalarType::Bool)) = *slot else {
                continue;
            };
            if !plan.reg_modified[r] {
                continue;
            }
            let skip_lbl = asm.new_label();
            emit_test_dirty_bit(asm, GpReg::Rdx, r, skip_lbl);
            let loc = plan.reg_locs[&(r as u8, ty)].loc;
            match loc {
                PhysLoc::Gp(g) => asm.mov_rr(GpReg::Rax, g),
                PhysLoc::Spill(disp) => asm.mov_rm64(GpReg::Rax, GpReg::Rbp, disp),
                PhysLoc::Xmm(_) => unreachable!(),
            }
            asm.shl_ri8(GpReg::Rax, 7);
            let tag = if ty == ScalarType::I64 {
                IMM_I_SMALL_TAG as i32
            } else {
                TAG_FALSE as i32
            };
            asm.or_ri32(GpReg::Rax, tag);
            asm.mov_mr64(GpReg::R13, (r * 8) as i32, GpReg::Rax);
            asm.bind(skip_lbl);
        }
    }

    // 3. Spill all modified `F64` slots and registers onto native stack save slots BEFORE any C call!
    for sv in &plan.slots {
        if sv.modified && sv.ty == ScalarType::F64 {
            let save_disp = sv.f64_save_disp.unwrap();
            match sv.loc {
                PhysLoc::Xmm(x) => asm.movsd_mx(GpReg::Rbp, save_disp, x),
                PhysLoc::Spill(disp) => {
                    asm.mov_rm64(GpReg::Rax, GpReg::Rbp, disp);
                    asm.mov_mr64(GpReg::Rbp, save_disp, GpReg::Rax);
                }
                PhysLoc::Gp(_) => unreachable!(),
            }
        }
    }
    for (r, slot) in reg_state.iter().enumerate() {
        if plan.reg_modified[r] && *slot == Some(ScalarType::F64) {
            let rl = plan.reg_locs[&(r as u8, ScalarType::F64)];
            let save_disp = rl.f64_save_disp.unwrap();
            match rl.loc {
                PhysLoc::Xmm(x) => asm.movsd_mx(GpReg::Rbp, save_disp, x),
                PhysLoc::Spill(disp) => {
                    asm.mov_rm64(GpReg::Rax, GpReg::Rbp, disp);
                    asm.mov_mr64(GpReg::Rbp, save_disp, GpReg::Rax);
                }
                PhysLoc::Gp(_) => unreachable!(),
            }
        }
    }

    // Phase B: Box each dirty modified `F64` via `IMM_F64` fast path or `store_boxed_f64` helper.
    for (idx, sv) in plan.slots.iter().enumerate() {
        if !sv.modified || sv.ty != ScalarType::F64 {
            continue;
        }
        let skip_lbl = asm.new_label();
        let slow_lbl = asm.new_label();
        asm.mov_rm64(GpReg::Rdx, GpReg::Rbp, plan.slot_dirty_disp);
        emit_test_dirty_bit(asm, GpReg::Rdx, idx, skip_lbl);

        asm.mov_rm64(GpReg::Rax, GpReg::Rbp, sv.f64_save_disp.unwrap());
        asm.test_ri32(GpReg::Rax, IMM_LOW7_MASK as i32);
        asm.jcc(Cond::Ne, slow_lbl);
        asm.or_ri32(GpReg::Rax, IMM_F64_TAG as i32);
        asm.mov_mr64(GpReg::R14, ((sv.slot_idx as usize) * 8) as i32, GpReg::Rax);
        asm.jmp(skip_lbl);

        asm.bind(slow_lbl);
        asm.mov_rr(GpReg::Rdi, GpReg::R12);
        asm.mov_ri64(GpReg::Rsi, 1);
        asm.mov_ri64(GpReg::Rdx, sv.slot_idx as u64);
        asm.mov_rr(GpReg::Rcx, GpReg::Rax);
        asm.mov_ri64(GpReg::Rax, input.helpers.store_boxed_f64 as usize as u64);
        asm.call_r(GpReg::Rax);
        emit_after_helper_check(asm, exit_pc, epilogue_lbl);

        asm.bind(skip_lbl);
    }

    for (r, slot) in reg_state.iter().enumerate() {
        if !plan.reg_modified[r] || *slot != Some(ScalarType::F64) {
            continue;
        }
        let rl = plan.reg_locs[&(r as u8, ScalarType::F64)];
        let skip_lbl = asm.new_label();
        let slow_lbl = asm.new_label();
        asm.mov_rm64(GpReg::Rdx, GpReg::Rbp, plan.reg_dirty_disp);
        emit_test_dirty_bit(asm, GpReg::Rdx, r, skip_lbl);

        asm.mov_rm64(GpReg::Rax, GpReg::Rbp, rl.f64_save_disp.unwrap());
        asm.test_ri32(GpReg::Rax, IMM_LOW7_MASK as i32);
        asm.jcc(Cond::Ne, slow_lbl);
        asm.or_ri32(GpReg::Rax, IMM_F64_TAG as i32);
        asm.mov_mr64(GpReg::R13, (r * 8) as i32, GpReg::Rax);
        asm.jmp(skip_lbl);

        asm.bind(slow_lbl);
        asm.mov_rr(GpReg::Rdi, GpReg::R12);
        asm.mov_ri64(GpReg::Rsi, 0);
        asm.mov_ri64(GpReg::Rdx, r as u64);
        asm.mov_rr(GpReg::Rcx, GpReg::Rax);
        asm.mov_ri64(GpReg::Rax, input.helpers.store_boxed_f64 as usize as u64);
        asm.call_r(GpReg::Rax);
        emit_after_helper_check(asm, exit_pc, epilogue_lbl);

        asm.bind(skip_lbl);
    }
}

fn emit_reload_unboxed_after_safepoint(
    asm: &mut Assembler,
    plan: &LoopPlan,
    reg_state: &[Option<ScalarType>; GP_REGISTER_COUNT],
) {
    if !plan.slots.is_empty() {
        asm.mov_rm64(GpReg::Rdx, GpReg::Rbp, plan.slot_dirty_disp);
        for (idx, sv) in plan.slots.iter().enumerate() {
            let skip_lbl = asm.new_label();
            emit_test_dirty_bit(asm, GpReg::Rdx, idx, skip_lbl);
            asm.mov_rm64(GpReg::Rax, GpReg::R14, ((sv.slot_idx as usize) * 8) as i32);
            emit_guard_and_unbox_value(asm, sv.ty, sv.loc, skip_lbl);
            asm.bind(skip_lbl);
        }
    }

    if reg_state.iter().any(|t| t.is_some()) {
        asm.mov_rm64(GpReg::Rdx, GpReg::Rbp, plan.reg_dirty_disp);
        for (r, slot) in reg_state.iter().enumerate() {
            let Some(ty) = *slot else {
                continue;
            };
            let skip_lbl = asm.new_label();
            emit_test_dirty_bit(asm, GpReg::Rdx, r, skip_lbl);
            asm.mov_rm64(GpReg::Rax, GpReg::R13, (r * 8) as i32);
            let loc = plan.reg_locs[&(r as u8, ty)].loc;
            emit_guard_and_unbox_value(asm, ty, loc, skip_lbl);
            asm.bind(skip_lbl);
        }
    }
}

fn emit_unboxed_const(
    asm: &mut Assembler,
    input: &CompilationInput<'_>,
    plan: &LoopPlan,
    dst: Reg,
    const_idx: u32,
) {
    if let Some(v) = const_i64_val(input, const_idx) {
        asm.mov_ri64(GpReg::Rax, v as u64);
        store_reg_val(asm, plan, dst, ScalarType::I64, GpReg::Rax, XmmReg::Xmm6);
    } else if let Some(bits) = const_f64_bits(input, const_idx) {
        asm.mov_ri64(GpReg::Rax, bits);
        asm.movq_xr(XmmReg::Xmm6, GpReg::Rax);
        store_reg_val(asm, plan, dst, ScalarType::F64, GpReg::Rax, XmmReg::Xmm6);
    } else if let Some(CompileConstant::ImmediateValidated { raw, .. }) =
        input.constants.get(const_idx as usize).copied()
    {
        let b = u64::from(raw == TAG_TRUE);
        asm.mov_ri64(GpReg::Rax, b);
        store_reg_val(asm, plan, dst, ScalarType::Bool, GpReg::Rax, XmmReg::Xmm6);
    } else {
        unreachable!("validated by analyze_loop_candidate");
    }
}

fn emit_move_loc_to_loc(asm: &mut Assembler, dst: PhysLoc, src: PhysLoc, ty: ScalarType) {
    if dst == src {
        return;
    }
    match (dst, src, ty) {
        (PhysLoc::Gp(d), PhysLoc::Gp(s), _) => asm.mov_rr(d, s),
        (PhysLoc::Gp(d), PhysLoc::Spill(disp), _) => asm.mov_rm64(d, GpReg::Rbp, disp),
        (PhysLoc::Spill(disp), PhysLoc::Gp(s), _) => asm.mov_mr64(GpReg::Rbp, disp, s),
        (PhysLoc::Xmm(d), PhysLoc::Xmm(s), _) => asm.movsd_xx(d, s),
        (PhysLoc::Xmm(d), PhysLoc::Spill(disp), _) => asm.movsd_xm(d, GpReg::Rbp, disp),
        (PhysLoc::Spill(disp), PhysLoc::Xmm(s), _) => asm.movsd_mx(GpReg::Rbp, disp, s),
        (PhysLoc::Spill(d_disp), PhysLoc::Spill(s_disp), _) => {
            asm.mov_rm64(GpReg::Rax, GpReg::Rbp, s_disp);
            asm.mov_mr64(GpReg::Rbp, d_disp, GpReg::Rax);
        }
        _ => unreachable!(),
    }
}

fn load_reg_gp(asm: &mut Assembler, plan: &LoopPlan, reg: Reg, ty: ScalarType, dst_gp: GpReg) {
    match plan.reg_locs[&(reg.0, ty)].loc {
        PhysLoc::Gp(g) => {
            if g != dst_gp {
                asm.mov_rr(dst_gp, g);
            }
        }
        PhysLoc::Spill(disp) => asm.mov_rm64(dst_gp, GpReg::Rbp, disp),
        PhysLoc::Xmm(_) => unreachable!(),
    }
}

fn load_reg_as_f64(
    asm: &mut Assembler,
    plan: &LoopPlan,
    reg: Reg,
    ty: ScalarType,
    dst_xmm: XmmReg,
) {
    match ty {
        ScalarType::F64 => match plan.reg_locs[&(reg.0, ScalarType::F64)].loc {
            PhysLoc::Xmm(x) => {
                if x != dst_xmm {
                    asm.movsd_xx(dst_xmm, x);
                }
            }
            PhysLoc::Spill(disp) => asm.movsd_xm(dst_xmm, GpReg::Rbp, disp),
            PhysLoc::Gp(_) => unreachable!(),
        },
        ScalarType::I64 => {
            load_reg_gp(asm, plan, reg, ScalarType::I64, GpReg::Rdx);
            asm.cvtsi2sd(dst_xmm, GpReg::Rdx);
        }
        ScalarType::Bool => unreachable!(),
    }
}

fn load_reg_gp_or_xmm(
    asm: &mut Assembler,
    plan: &LoopPlan,
    reg: Reg,
    ty: ScalarType,
    gp_dst: GpReg,
    xmm_dst: XmmReg,
) {
    if ty == ScalarType::F64 {
        load_reg_as_f64(asm, plan, reg, ScalarType::F64, xmm_dst);
    } else {
        load_reg_gp(asm, plan, reg, ty, gp_dst);
    }
}

fn store_reg_val(
    asm: &mut Assembler,
    plan: &LoopPlan,
    dst: Reg,
    ty: ScalarType,
    gp_val: GpReg,
    xmm_val: XmmReg,
) {
    let loc = plan.reg_locs[&(dst.0, ty)].loc;
    match (loc, ty) {
        (PhysLoc::Gp(g), ScalarType::I64 | ScalarType::Bool) => {
            if g != gp_val {
                asm.mov_rr(g, gp_val);
            }
        }
        (PhysLoc::Spill(disp), ScalarType::I64 | ScalarType::Bool) => {
            asm.mov_mr64(GpReg::Rbp, disp, gp_val);
        }
        (PhysLoc::Xmm(x), ScalarType::F64) => {
            if x != xmm_val {
                asm.movsd_xx(x, xmm_val);
            }
        }
        (PhysLoc::Spill(disp), ScalarType::F64) => {
            asm.movsd_mx(GpReg::Rbp, disp, xmm_val);
        }
        _ => unreachable!(),
    }
    mark_reg_dirty(asm, plan, dst);
}

/// Uses only `Rdx` (and `Rax` only when `sidx >= 31`, where `StoreSlot` has no live `Rax`).
fn mark_slot_dirty(asm: &mut Assembler, plan: &LoopPlan, sidx: usize) {
    asm.mov_rm64(GpReg::Rdx, GpReg::Rbp, plan.slot_dirty_disp);
    if sidx < 31 {
        asm.or_ri32(GpReg::Rdx, 1i32 << sidx);
    } else {
        asm.mov_ri64(GpReg::Rax, 1u64 << sidx);
        asm.or_rr(GpReg::Rdx, GpReg::Rax);
    }
    asm.mov_mr64(GpReg::Rbp, plan.slot_dirty_disp, GpReg::Rdx);
}

/// Uses only `Rdx` (`dst.0 < GP_REGISTER_COUNT == 24 < 31`), preserving `Rax`, `Rcx`, `Xmm6`, `Xmm7`.
fn mark_reg_dirty(asm: &mut Assembler, plan: &LoopPlan, dst: Reg) {
    if plan.reg_live_in[dst.0 as usize].is_none() {
        asm.mov_rm64(GpReg::Rdx, GpReg::Rbp, plan.reg_dirty_disp);
        asm.or_ri32(GpReg::Rdx, 1i32 << dst.0);
        asm.mov_mr64(GpReg::Rbp, plan.reg_dirty_disp, GpReg::Rdx);
    }
}
