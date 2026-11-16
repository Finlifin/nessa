//! Back-end NSBC bytecode peephole optimizer and constant-pool deduplicator (`O1+`).
//!
//! Transformations:
//! 1. Module-wide constant-pool deduplication during emission (preserving all narrow
//!    12/14/17-bit descriptor invariants by mapping duplicates to lower indices).
//! 2. Same-block `StoreSlot(s, rSrc)` -> `LoadSlot(rDst, s)` forwarding (`rDst == rSrc`
//!    eliminates the redundant reload; `rDst != rSrc` replaces memory reload with `Mov`).
//! 3. Self-move `Mov(r, r)` elimination.
//! 4. 3-instruction conditional branch sequence `[JmpIfNot(c, 2), Jmp(then), Jmp(else)]`
//!    compaction into 1-instruction `JmpIf` / `JmpIfNot` when either arm falls through
//!    (or 2-instruction `[JmpIfNot(c, else), Jmp(then)]` otherwise).
//! 5. Fallthrough unconditional `Jmp(offset == 1)` / `JmpFar(offset == 1)` elimination,
//!    with exact lockstep PC relocation of all jumps, `safepoint_pcs`, and `method_call_scopes`.

use std::collections::HashSet;

use nsbc::{
    AddrMode, CodegenOutput, CompiledFunction, Constant, Instruction, InstructionData,
    MethodCallScope, Opcode, Reg,
};

#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum ConstantKey {
    Int(i64),
    UInt(u64),
    Int128(i128),
    UInt128(u128),
    Type(u32),
    Enum(u32, u32),
    FloatBits(u64),
    Str(String),
    Char(char),
}

impl ConstantKey {
    pub(crate) fn from_constant(c: &Constant) -> Self {
        match c {
            Constant::Int(v) => Self::Int(*v),
            Constant::UInt(v) => Self::UInt(*v),
            Constant::Int128(v) => Self::Int128(*v),
            Constant::UInt128(v) => Self::UInt128(*v),
            Constant::Type(t) => Self::Type(t.as_u32()),
            Constant::Enum {
                type_index,
                variant,
            } => Self::Enum(type_index.as_u32(), *variant),
            Constant::Float(f) => Self::FloatBits(f.to_bits()),
            Constant::Str(s) => Self::Str(s.clone()),
            Constant::Char(ch) => Self::Char(*ch),
        }
    }
}

#[derive(Clone, Copy)]
enum SymInstr {
    Raw(u32),
    UncondJump {
        target_old_pc: usize,
    },
    CondJump {
        opcode: Opcode,
        cond: Reg,
        target_old_pc: usize,
    },
}

/// Optimize all compiled functions in `output`, updating `method_call_scopes` and
/// `safepoint_pcs` in exact lockstep.
pub(crate) fn optimize_bytecode(output: &mut CodegenOutput) {
    // Group method_call_scopes by function ID for fast per-function relocation.
    let mut scopes_by_func: std::collections::HashMap<u32, Vec<MethodCallScope>> =
        std::collections::HashMap::new();
    for entry in output.method_call_scopes.drain(..) {
        scopes_by_func
            .entry(entry.func_id.0)
            .or_default()
            .push(entry);
    }

    for func in &mut output.functions {
        let mut fn_scopes = scopes_by_func.remove(&func.func_id.0).unwrap_or_default();
        optimize_function_bytecode(func, &mut fn_scopes);
        output.method_call_scopes.extend(fn_scopes);
    }
}

fn decode_slot_op(word: u32) -> Option<(Opcode, u32, Reg)> {
    let instr = Instruction::decode(word)?;
    match (instr.opcode, instr.data) {
        (Opcode::LoadSlot | Opcode::StoreSlot, InstructionData::A { dst, base, imm12 }) => {
            let slot = ((base.0 as u32) << 12) | (imm12 as u32);
            Some((instr.opcode, slot, dst))
        }
        _ => None,
    }
}

fn decode_jump_target(pc: usize, word: u32) -> Option<(Opcode, Reg, usize)> {
    let instr = Instruction::decode(word)?;
    match (instr.opcode, instr.data) {
        (
            Opcode::Jmp | Opcode::JmpFar | Opcode::JmpIf | Opcode::JmpIfNot,
            InstructionData::J { cond, offset },
        ) => {
            let target = (pc as i64) + (offset as i64);
            if target >= 0 {
                Some((instr.opcode, cond, target as usize))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn optimize_function_bytecode(func: &mut CompiledFunction, fn_scopes: &mut [MethodCallScope]) {
    let len = func.instructions.len();
    if len <= 2 {
        return;
    }

    // Run up to 2 passes (branch compaction + store-load forwarding can expose fallthrough Jmps).
    for _ in 0..2 {
        let cur_len = func.instructions.len();
        if cur_len <= 2 {
            break;
        }

        // Collect all PCs targeted by jumps (excluding internal `pc -> pc + 2` inside intact
        // 3-instruction conditional branch sequences when checking whether `pc + 2` has external preds).
        let mut jump_target_counts = vec![0usize; cur_len + 1];
        for (pc, &word) in func.instructions.iter().enumerate() {
            if let Some((_, _, target)) = decode_jump_target(pc, word)
                && target <= cur_len
            {
                jump_target_counts[target] += 1;
            }
        }
        let safepoint_set: HashSet<usize> =
            func.safepoint_pcs.iter().map(|&p| p as usize).collect();

        let mut sym: Vec<Option<SymInstr>> = Vec::with_capacity(cur_len);
        for (pc, &word) in func.instructions.iter().enumerate() {
            if let Some((op, cond, target)) = decode_jump_target(pc, word) {
                if matches!(op, Opcode::Jmp | Opcode::JmpFar) {
                    sym.push(Some(SymInstr::UncondJump {
                        target_old_pc: target,
                    }));
                } else {
                    sym.push(Some(SymInstr::CondJump {
                        opcode: op,
                        cond,
                        target_old_pc: target,
                    }));
                }
            } else {
                sym.push(Some(SymInstr::Raw(word)));
            }
        }

        // Pass A: Peephole StoreSlot -> LoadSlot forwarding and self-Mov removal (never touching pc == 0).
        let mut pc = 1usize;
        while pc < cur_len {
            let word = func.instructions[pc];
            // Delete `Mov(r, r)` when `pc` is not a safepoint.
            if let Some(instr) = Instruction::decode(word)
                && instr.opcode == Opcode::Mov
                && let InstructionData::R { dst, src1, .. } = instr.data
                && dst == src1
                && !safepoint_set.contains(&pc)
            {
                sym[pc] = None;
                pc += 1;
                continue;
            }

            // `StoreSlot(slot, rSrc)` immediately followed by `LoadSlot(rDst, slot)` with no jump landing on `pc + 1`.
            if pc + 1 < cur_len
                && jump_target_counts[pc + 1] == 0
                && !safepoint_set.contains(&(pc + 1))
                && let Some((Opcode::StoreSlot, s1, r_src)) = decode_slot_op(word)
                && let Some((Opcode::LoadSlot, s2, r_dst)) =
                    decode_slot_op(func.instructions[pc + 1])
                && s1 == s2
            {
                if r_dst == r_src {
                    sym[pc + 1] = None;
                } else {
                    sym[pc + 1] = Some(SymInstr::Raw(Instruction::mov(r_dst, r_src).encode()));
                }
                pc += 2;
                continue;
            }
            pc += 1;
        }

        // Pass B: Compact 3-instruction conditional branches `[JmpIfNot(c, +2), Jmp(then), Jmp(else)]`.
        let mut b_pc = 1usize;
        while b_pc + 2 < cur_len {
            if let (
                Some(SymInstr::CondJump {
                    opcode: Opcode::JmpIfNot,
                    cond,
                    target_old_pc: hop_pc,
                }),
                Some(SymInstr::UncondJump {
                    target_old_pc: then_target,
                }),
                Some(SymInstr::UncondJump {
                    target_old_pc: else_target,
                }),
            ) = (sym[b_pc], sym[b_pc + 1], sym[b_pc + 2])
                && hop_pc == b_pc + 2
                && jump_target_counts[b_pc + 1] == 0
                && jump_target_counts[b_pc + 2] == 1
                && !safepoint_set.contains(&(b_pc + 1))
                && !safepoint_set.contains(&(b_pc + 2))
            {
                let fallthrough = b_pc + 3;
                let dist_then = (then_target as i64 - b_pc as i64).abs();
                let dist_else = (else_target as i64 - b_pc as i64).abs();
                if else_target == fallthrough && dist_then < 60_000 {
                    sym[b_pc] = Some(SymInstr::CondJump {
                        opcode: Opcode::JmpIf,
                        cond,
                        target_old_pc: then_target,
                    });
                    sym[b_pc + 1] = None;
                    sym[b_pc + 2] = None;
                    b_pc += 3;
                    continue;
                } else if then_target == fallthrough && dist_else < 60_000 {
                    sym[b_pc] = Some(SymInstr::CondJump {
                        opcode: Opcode::JmpIfNot,
                        cond,
                        target_old_pc: else_target,
                    });
                    sym[b_pc + 1] = None;
                    sym[b_pc + 2] = None;
                    b_pc += 3;
                    continue;
                } else if dist_else < 60_000 {
                    sym[b_pc] = Some(SymInstr::CondJump {
                        opcode: Opcode::JmpIfNot,
                        cond,
                        target_old_pc: else_target,
                    });
                    sym[b_pc + 1] = Some(SymInstr::UncondJump {
                        target_old_pc: then_target,
                    });
                    sym[b_pc + 2] = None;
                    b_pc += 3;
                    continue;
                }
            }
            b_pc += 1;
        }

        // Pass C: Delete fallthrough unconditional jumps (`UncondJump` whose target has no surviving
        // instructions between `pc + 1` and `target_old_pc`).
        // Precompute `next_surviving_at_or_after[i]` = index of first `sym[j].is_some()` for `j >= i` (or `cur_len`).
        let mut next_live = vec![cur_len; cur_len + 1];
        for i in (0..cur_len).rev() {
            next_live[i] = if sym[i].is_some() {
                i
            } else {
                next_live[i + 1]
            };
        }
        for i in 1..cur_len {
            if let Some(SymInstr::UncondJump { target_old_pc }) = sym[i]
                && target_old_pc <= cur_len
                && next_live[i + 1] == next_live[target_old_pc]
                && !safepoint_set.contains(&i)
            {
                sym[i] = None;
            }
        }

        // Build exact `new_pc_of_old[0..=cur_len]` mapping.
        let mut new_pc_of_old = vec![0u32; cur_len + 1];
        let mut new_count = 0u32;
        for i in 0..cur_len {
            new_pc_of_old[i] = new_count;
            if sym[i].is_some() {
                new_count += 1;
            }
        }
        new_pc_of_old[cur_len] = new_count;

        if new_count as usize == cur_len
            && sym
                .iter()
                .enumerate()
                .all(|(i, s)| matches!(s, Some(SymInstr::Raw(w)) if *w == func.instructions[i]))
        {
            break;
        }

        // Re-encode surviving instructions with relocated jump offsets.
        let mut new_instrs = Vec::with_capacity(new_count as usize);
        let mut encode_ok = true;
        for item in sym.iter().flatten() {
            let new_pc = new_instrs.len() as i64;
            match *item {
                SymInstr::Raw(word) => new_instrs.push(word),
                SymInstr::UncondJump { target_old_pc } => {
                    let target_new_pc = new_pc_of_old[target_old_pc] as i64;
                    let offset = (target_new_pc - new_pc) as i32;
                    let word = if (-(1 << 16)..(1 << 16)).contains(&(offset as i64)) {
                        Instruction::jmp(offset).encode()
                    } else if (-(1 << 21)..(1 << 21)).contains(&(offset as i64)) {
                        Instruction::jmp_far(offset).encode()
                    } else {
                        encode_ok = false;
                        break;
                    };
                    new_instrs.push(word);
                }
                SymInstr::CondJump {
                    opcode,
                    cond,
                    target_old_pc,
                } => {
                    let target_new_pc = new_pc_of_old[target_old_pc] as i64;
                    let offset = target_new_pc - new_pc;
                    if !(-(1 << 16)..(1 << 16)).contains(&offset) {
                        encode_ok = false;
                        break;
                    }
                    let word = Instruction {
                        opcode,
                        amode: AddrMode::Imm,
                        data: InstructionData::J {
                            cond,
                            offset: offset as i32,
                        },
                    }
                    .encode();
                    new_instrs.push(word);
                }
            }
        }

        if !encode_ok {
            // Leave `func` untouched if an extreme synthetic jump could not re-encode.
            break;
        }

        func.instructions = new_instrs;
        for sp in &mut func.safepoint_pcs {
            *sp = new_pc_of_old[*sp as usize];
        }
        func.safepoint_pcs.sort_unstable();
        func.safepoint_pcs.dedup();

        for sc in fn_scopes.iter_mut() {
            sc.pc = new_pc_of_old[sc.pc as usize];
        }
    }
}
