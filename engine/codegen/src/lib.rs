use nir::{
    BasicBlock, BinOp, BlockId, NirExpr, NirFunction, NirLocal, NirModule, NirStmt, NirValue,
    Terminator, UnaryOp,
};
use nsbc::{CodegenOutput, CompiledFunction, Constant, FuncId, Instruction, Opcode, Reg};

// ---------------------------------------------------------------------------
// Register allocator — simple linear scan
// ---------------------------------------------------------------------------

const MAX_REGS: u8 = 20;

struct RegAlloc {
    /// Maps NirLocal → register number.
    local_to_reg: Vec<Option<u8>>,
    /// Next free register.
    next_reg: u8,
}

impl RegAlloc {
    fn new(local_count: u32) -> Self {
        Self {
            local_to_reg: vec![None; local_count as usize],
            next_reg: 0,
        }
    }

    fn alloc(&mut self, local: NirLocal) -> Reg {
        if let Some(r) = self.local_to_reg[local.0 as usize] {
            return Reg(r);
        }
        let r = self.next_reg % MAX_REGS;
        self.next_reg = self.next_reg.wrapping_add(1);
        self.local_to_reg[local.0 as usize] = Some(r);
        Reg(r)
    }

    fn get(&mut self, local: NirLocal) -> Reg {
        self.alloc(local)
    }

    fn max_used(&self) -> u8 {
        self.next_reg.min(MAX_REGS)
    }
}

// ---------------------------------------------------------------------------
// Emitter — generates instructions from NIR
// ---------------------------------------------------------------------------

struct Emitter {
    instructions: Vec<u64>,
    /// Maps BlockId → instruction offset.
    block_offsets: Vec<u32>,
    /// Pending fixups: (instruction_index, target_block).
    fixups: Vec<(usize, BlockId)>,
    safepoint_pcs: Vec<u32>,
    constants: Vec<Constant>,
}

impl Emitter {
    fn new() -> Self {
        Self {
            instructions: Vec::new(),
            block_offsets: Vec::new(),
            fixups: Vec::new(),
            safepoint_pcs: Vec::new(),
            constants: Vec::new(),
        }
    }

    fn emit(&mut self, instr: Instruction) {
        self.instructions.push(instr.encode());
    }

    fn current_pc(&self) -> u32 {
        self.instructions.len() as u32
    }

    fn emit_safepoint(&mut self) {
        self.safepoint_pcs.push(self.current_pc());
        self.emit(Instruction::safepoint());
    }

    fn _add_constant(&mut self, c: Constant) -> u32 {
        let idx = self.constants.len() as u32;
        self.constants.push(c);
        idx
    }
}

// ---------------------------------------------------------------------------
// Codegen — the main code generator
// ---------------------------------------------------------------------------

pub struct Codegen {
    output_constants: Vec<Constant>,
}

impl Codegen {
    pub fn new() -> Self {
        Self {
            output_constants: Vec::new(),
        }
    }

    /// Compile an entire NIR module.
    pub fn compile(mut self, module: &NirModule) -> CodegenOutput {
        let mut functions = Vec::new();
        for func in &module.functions {
            let compiled = self.compile_function(func);
            functions.push(compiled);
        }
        CodegenOutput {
            functions,
            constants: self.output_constants,
        }
    }

    fn compile_function(&mut self, func: &NirFunction) -> CompiledFunction {
        let mut regalloc = RegAlloc::new(func.local_count);
        let mut emitter = Emitter::new();

        // Pre-assign registers for parameters.
        for (i, param) in func.params.iter().enumerate() {
            let r = regalloc.alloc(param.local);
            // Params arrive in r0..rN by calling convention.
            if r.0 != i as u8 {
                emitter.emit(Instruction::mov(r, Reg(i as u8)));
            }
        }

        // Compute block order (simple linear order).
        let block_count = func.blocks.len();
        emitter.block_offsets = vec![0u32; block_count];

        // First pass: emit code for each block.
        for block in &func.blocks {
            emitter.block_offsets[block.id.0 as usize] = emitter.current_pc();
            self.emit_block(block, &mut emitter, &mut regalloc);
        }

        // Second pass: fix up jump offsets.
        for (instr_idx, target_block) in &emitter.fixups {
            let target_pc = emitter.block_offsets[target_block.0 as usize] as i64;
            let source_pc = (*instr_idx + 1) as i64; // PC after the jump instruction
            // VM jump semantics use `pc = pc + offset - 1` after pre-increment,
            // so codegen stores a +1-biased relative offset.
            let offset = target_pc - source_pc + 1;

            // Re-encode the jump instruction with the correct offset.
            let old_word = emitter.instructions[*instr_idx];
            if let Some(old_instr) = Instruction::decode(old_word) {
                // Preserve the cond register from the original instruction.
                let cond_reg = match old_instr.data {
                    nsbc::InstructionData::J { cond, .. } => cond,
                    _ => Reg(0),
                };
                let new_instr = Instruction::j_type(old_instr.opcode, cond_reg, offset);
                emitter.instructions[*instr_idx] = new_instr.encode();
            }
        }

        self.output_constants.extend(emitter.constants.drain(..));

        CompiledFunction {
            func_id: func.func_id,
            name: func.name,
            instructions: emitter.instructions,
            register_count: regalloc.max_used(),
            param_count: func.params.len() as u8,
            is_closure: func.is_closure,
            safepoint_pcs: emitter.safepoint_pcs,
        }
    }

    fn emit_block(&mut self, block: &BasicBlock, emitter: &mut Emitter, regalloc: &mut RegAlloc) {
        // Emit safepoint at block entry (for GC coordination).
        emitter.emit_safepoint();

        // Emit statements.
        for stmt in &block.stmts {
            self.emit_stmt(stmt, emitter, regalloc);
        }

        // Emit terminator.
        self.emit_terminator(&block.terminator, emitter, regalloc);
    }

    fn emit_stmt(&mut self, stmt: &NirStmt, emitter: &mut Emitter, regalloc: &mut RegAlloc) {
        match stmt {
            NirStmt::Assign(local, expr) => {
                let dst = regalloc.alloc(*local);
                self.emit_expr(expr, dst, emitter, regalloc);
            }
            NirStmt::Drop(_local) => {
                // For now, GC handles collection. No explicit drop instruction.
            }
            NirStmt::EffectCall {
                evidence: _,
                operation: _,
                args: _,
                result,
            } => {
                // TODO: emit effect call via evidence
                let dst = regalloc.alloc(*result);
                emitter.emit(Instruction::load_unit(dst));
            }
            NirStmt::Nop => {
                emitter.emit(Instruction::nop());
            }
        }
    }

    fn emit_expr(
        &mut self,
        expr: &NirExpr,
        dst: Reg,
        emitter: &mut Emitter,
        regalloc: &mut RegAlloc,
    ) {
        match expr {
            NirExpr::Use(val) => {
                self.load_value(val, dst, emitter, regalloc);
            }
            NirExpr::BinOp(op, lhs, rhs) => {
                let lr = self.value_to_reg(lhs, emitter, regalloc);
                let rr = self.value_to_reg(rhs, emitter, regalloc);
                let opcode = match op {
                    BinOp::Add => Opcode::Add,
                    BinOp::Sub => Opcode::Sub,
                    BinOp::Mul => Opcode::Mul,
                    BinOp::Div => Opcode::Div,
                    BinOp::Mod => Opcode::Mod,
                    BinOp::BitAnd => Opcode::BitAnd,
                    BinOp::BitOr => Opcode::BitOr,
                    BinOp::BitXor => Opcode::BitXor,
                    BinOp::Shl => Opcode::Shl,
                    BinOp::Shr => Opcode::Shr,
                    BinOp::Eq => Opcode::CmpEq,
                    BinOp::Ne => Opcode::CmpNe,
                    BinOp::Lt => Opcode::CmpLt,
                    BinOp::Le => Opcode::CmpLe,
                    BinOp::Gt => Opcode::CmpGt,
                    BinOp::Ge => Opcode::CmpGe,
                    BinOp::And => Opcode::BitAnd, // TODO: short-circuit
                    BinOp::Or => Opcode::BitOr,
                };
                emitter.emit(Instruction::r_type(opcode, dst, lr, rr));
            }
            NirExpr::UnaryOp(op, val) => {
                let src = self.value_to_reg(val, emitter, regalloc);
                match op {
                    UnaryOp::Neg => {
                        emitter.emit(Instruction::r_type(Opcode::Neg, dst, src, Reg(0)));
                    }
                    UnaryOp::Not | UnaryOp::BitNot => {
                        emitter.emit(Instruction::r_type(Opcode::BitNot, dst, src, Reg(0)));
                    }
                }
            }
            NirExpr::Call(func_id, args) => {
                // Place args in r0..rN.
                for (i, arg) in args.iter().enumerate() {
                    let r = Reg(i as u8);
                    self.load_value(arg, r, emitter, regalloc);
                }
                emitter.emit(Instruction::call(func_id.0, args.len() as u8));
                // Result is in r0.
                if dst.0 != 0 {
                    emitter.emit(Instruction::mov(dst, Reg(0)));
                }
            }
            NirExpr::CallIntrinsic(intrinsic_fn, args) => {
                // Place args in r0..rN.
                for (i, arg) in args.iter().enumerate() {
                    let r = Reg(i as u8);
                    self.load_value(arg, r, emitter, regalloc);
                }
                emitter.emit(Instruction::call_intrinsic(*intrinsic_fn, args.len() as u8));
                // Result is in r0.
                if dst.0 != 0 {
                    emitter.emit(Instruction::mov(dst, Reg(0)));
                }
            }
            NirExpr::MethodCall(receiver, method, args) => {
                // Place args in r0..rN-1.
                for (i, arg) in args.iter().enumerate() {
                    let r = Reg(i as u8);
                    self.load_value(arg, r, emitter, regalloc);
                }
                // Load receiver into a register outside the arg range.
                let recv_reg = Reg(args.len() as u8);
                self.load_value(receiver, recv_reg, emitter, regalloc);
                emitter.emit(Instruction::call_method(
                    recv_reg,
                    method.as_u32(),
                    args.len() as u8,
                ));
                // Result is in r0.
                if dst.0 != 0 {
                    emitter.emit(Instruction::mov(dst, Reg(0)));
                }
            }
            NirExpr::FieldAccess(obj, field_idx) => {
                let obj_reg = self.value_to_reg(obj, emitter, regalloc);
                emitter.emit(Instruction::load_field(dst, obj_reg, *field_idx));
            }
            NirExpr::IndexAccess(obj, idx) => {
                let obj_reg = self.value_to_reg(obj, emitter, regalloc);
                let idx_reg = self.value_to_reg(idx, emitter, regalloc);
                emitter.emit(Instruction::i_type(
                    Opcode::LoadIndex,
                    dst,
                    obj_reg,
                    idx_reg.0 as u64,
                ));
            }
            NirExpr::NewObject(type_idx, fields) => {
                emitter.emit(Instruction::new_object(dst, *type_idx));
                for (i, field_val) in fields.iter().enumerate() {
                    let val_reg = self.value_to_reg(field_val, emitter, regalloc);
                    emitter.emit(Instruction::store_field(dst, i as u32, val_reg));
                }
            }
            NirExpr::NewClosure(func_id, captures) => {
                // Place captured values in r0..rN for the closure to pick up.
                for (i, cap) in captures.iter().enumerate() {
                    let r = Reg(i as u8);
                    self.load_value(cap, r, emitter, regalloc);
                }
                emitter.emit(Instruction::new_closure(
                    dst,
                    func_id.0,
                    captures.len() as u8,
                ));
            }
            NirExpr::CallIndirect(callee, args) => {
                // Load the closure value first, before args clobber r0..rN-1.
                let closure_reg = Reg(args.len() as u8);
                self.load_value(callee, closure_reg, emitter, regalloc);
                // Place explicit args in r0..r(N-1).
                for (i, arg) in args.iter().enumerate() {
                    let r = Reg(i as u8);
                    self.load_value(arg, r, emitter, regalloc);
                }
                emitter.emit(Instruction::call_indirect(closure_reg, args.len() as u8));
                // Result is in r0.
                if dst.0 != 0 {
                    emitter.emit(Instruction::mov(dst, Reg(0)));
                }
            }
            NirExpr::TypeCheck(val, type_idx) => {
                let src = self.value_to_reg(val, emitter, regalloc);
                emitter.emit(Instruction::i_type(
                    Opcode::TypeCheck,
                    dst,
                    src,
                    type_idx.as_u32() as u64,
                ));
            }
            NirExpr::TypeCast(val, type_idx) => {
                let src = self.value_to_reg(val, emitter, regalloc);
                emitter.emit(Instruction::i_type(
                    Opcode::TypeCast,
                    dst,
                    src,
                    type_idx.as_u32() as u64,
                ));
            }
        }
    }

    fn emit_terminator(
        &mut self,
        term: &Terminator,
        emitter: &mut Emitter,
        regalloc: &mut RegAlloc,
    ) {
        match term {
            Terminator::Goto(target) => {
                let pc = emitter.current_pc() as usize;
                emitter.fixups.push((pc, *target));
                emitter.emit(Instruction::jmp(0)); // placeholder offset
            }
            Terminator::Branch(cond, then_block, else_block) => {
                let cond_reg = self.value_to_reg(cond, emitter, regalloc);
                let pc_then = emitter.current_pc() as usize;
                emitter.fixups.push((pc_then, *then_block));
                emitter.emit(Instruction::jmp_if(cond_reg, 0)); // placeholder
                let pc_else = emitter.current_pc() as usize;
                emitter.fixups.push((pc_else, *else_block));
                emitter.emit(Instruction::jmp(0)); // placeholder
            }
            Terminator::Return(val) => match val {
                NirValue::Unit => {
                    emitter.emit(Instruction::return_unit());
                }
                _ => {
                    let src = self.value_to_reg(val, emitter, regalloc);
                    emitter.emit(Instruction::ret(src));
                }
            },
            Terminator::Unreachable => {
                // Emit a trap / debug break.
                emitter.emit(Instruction::e_type(Opcode::DebugBreak, 0));
            }
        }
    }

    /// Load a NirValue into a register.
    fn load_value(
        &mut self,
        val: &NirValue,
        dst: Reg,
        emitter: &mut Emitter,
        regalloc: &mut RegAlloc,
    ) {
        match val {
            NirValue::Local(local) => {
                let src = regalloc.get(*local);
                if src != dst {
                    emitter.emit(Instruction::mov(dst, src));
                }
            }
            NirValue::ConstInt(v) => {
                emitter.emit(Instruction::load_imm(dst, *v as u64));
            }
            NirValue::ConstUInt(v) => {
                emitter.emit(Instruction::load_imm(dst, *v));
            }
            NirValue::ConstBool(b) => {
                if *b {
                    emitter.emit(Instruction::load_true(dst));
                } else {
                    emitter.emit(Instruction::load_false(dst));
                }
            }
            NirValue::Unit => {
                emitter.emit(Instruction::load_unit(dst));
            }
            NirValue::Null => {
                emitter.emit(Instruction::load_null(dst));
            }
            NirValue::ConstFloat(v) => {
                // Store float as its bit pattern in an immediate.
                emitter.emit(Instruction::load_imm(dst, v.to_bits()));
            }
            NirValue::ConstStr(_str_id) => {
                // TODO: load string constant from constant pool.
                emitter.emit(Instruction::load_null(dst));
            }
        }
    }

    /// Materialize a NirValue into a register, returning which register.
    fn value_to_reg(
        &mut self,
        val: &NirValue,
        emitter: &mut Emitter,
        regalloc: &mut RegAlloc,
    ) -> Reg {
        match val {
            NirValue::Local(local) => regalloc.get(*local),
            _ => {
                // Use a scratch register.
                let scratch = Reg(19); // r19 as scratch
                self.load_value(val, scratch, emitter, regalloc);
                scratch
            }
        }
    }
}

impl Default for Codegen {
    fn default() -> Self {
        Self::new()
    }
}

/// Compile a NIR module into bytecode.
pub fn codegen(module: &NirModule) -> CodegenOutput {
    Codegen::new().compile(module)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_simple_nir() -> NirModule {
        use nir::*;
        use type_pool::TypeIndex;
        NirModule {
            functions: vec![NirFunction {
                func_id: FuncId(0),
                name: str_interner::StrId::from_raw(0),
                params: vec![],
                return_type: TypeIndex::INVALID,
                blocks: vec![BasicBlock {
                    id: BlockId(0),
                    stmts: vec![
                        NirStmt::Assign(NirLocal(0), NirExpr::Use(NirValue::ConstInt(10))),
                        NirStmt::Assign(NirLocal(1), NirExpr::Use(NirValue::ConstInt(32))),
                        NirStmt::Assign(
                            NirLocal(2),
                            NirExpr::BinOp(
                                BinOp::Add,
                                NirValue::Local(NirLocal(0)),
                                NirValue::Local(NirLocal(1)),
                            ),
                        ),
                    ],
                    terminator: Terminator::Return(NirValue::Local(NirLocal(2))),
                }],
                entry_block: BlockId(0),
                local_count: 3,
                is_closure: false,
            }],
        }
    }

    #[test]
    fn codegen_produces_instructions() {
        let module = make_simple_nir();
        let output = codegen(&module);
        assert_eq!(output.functions.len(), 1);
        let func = &output.functions[0];
        assert!(!func.instructions.is_empty());
    }

    #[test]
    fn codegen_has_safepoints() {
        let module = make_simple_nir();
        let output = codegen(&module);
        let func = &output.functions[0];
        assert!(
            !func.safepoint_pcs.is_empty(),
            "should have safepoint at block entry"
        );
    }

    #[test]
    fn codegen_instructions_decode() {
        let module = make_simple_nir();
        let output = codegen(&module);
        let func = &output.functions[0];
        for &word in &func.instructions {
            assert!(
                Instruction::decode(word).is_some(),
                "all emitted instructions should decode: {word:#x}"
            );
        }
    }
}
