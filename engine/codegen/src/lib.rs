mod validation;

use std::collections::HashMap;

pub use validation::EncodingError;

use nir::{
    BasicBlock, BinOp, BlockId, NirExpr, NirFunction, NirLocal, NirModule, NirStmt, NirValue,
    Terminator, UnaryOp,
};
use nsbc::{CodegenOutput, CompiledFunction, Constant, Instruction, MethodCallScope, Opcode, Reg};

// ---------------------------------------------------------------------------
// Local storage — frame-owned slots, independent of call argument registers
// ---------------------------------------------------------------------------

const SCRATCH: Reg = Reg(Reg::SCRATCH0);
const SECOND_SCRATCH: Reg = Reg(Reg::SCRATCH1);
const RESULT: Reg = Reg(8);

/// Stable local indices also identify value slots. This conservative layout
/// keeps every local alive across calls without register aliasing. CFG liveness
/// can later promote locals into callee-saved registers and reuse dead slots.
struct FrameLayout {
    count: u32,
}

impl FrameLayout {
    fn new(count: u32) -> Self {
        Self { count }
    }

    fn slot(&self, local: NirLocal) -> u32 {
        debug_assert!(local.0 < self.count);
        local.0
    }
}

// ---------------------------------------------------------------------------
// Emitter — generates instructions from NIR
// ---------------------------------------------------------------------------

struct Emitter {
    instructions: Vec<u32>,
    /// Maps BlockId → instruction offset.
    block_offsets: Vec<u32>,
    /// Pending fixups: (instruction_index, target_block).
    fixups: Vec<(usize, BlockId)>,
    safepoint_pcs: Vec<u32>,
    constants: Vec<Constant>,
    /// Base index into the global constant pool for this function's constants.
    const_base: u32,
    call_scope: Option<u32>,
    method_call_scopes: Vec<(u32, u32)>,
}

impl Emitter {
    fn new(const_base: u32) -> Self {
        Self {
            instructions: Vec::new(),
            block_offsets: Vec::new(),
            fixups: Vec::new(),
            safepoint_pcs: Vec::new(),
            constants: Vec::new(),
            const_base,
            call_scope: None,
            method_call_scopes: Vec::new(),
        }
    }

    fn add_constant(&mut self, c: Constant) -> u32 {
        let global_idx = self.const_base + self.constants.len() as u32;
        self.constants.push(c);
        global_idx
    }

    fn emit(&mut self, instr: Instruction) {
        if nsbc::ScopeCoverage::CallsAndTypes.covers(instr.opcode)
            && let Some(scope) = self.call_scope
        {
            self.method_call_scopes.push((self.current_pc(), scope));
        }
        self.instructions.push(instr.encode());
    }

    fn current_pc(&self) -> u32 {
        self.instructions.len() as u32
    }

    fn emit_safepoint(&mut self) {
        self.safepoint_pcs.push(self.current_pc());
        self.emit(Instruction::safepoint());
    }
}

// ---------------------------------------------------------------------------
// Codegen — the main code generator
// ---------------------------------------------------------------------------

pub struct Codegen {
    output_constants: Vec<Constant>,
    method_call_scopes: Vec<MethodCallScope>,
    encoding_errors: Vec<EncodingError>,
    enum_descriptors: HashMap<(type_pool::TypeIndex, u32), u16>,
    error_descriptors: HashMap<type_pool::TypeIndex, u16>,
}

impl Codegen {
    pub fn new() -> Self {
        Self {
            output_constants: Vec::new(),
            method_call_scopes: Vec::new(),
            encoding_errors: Vec::new(),
            enum_descriptors: HashMap::new(),
            error_descriptors: HashMap::new(),
        }
    }

    /// Compile an entire NIR module whose layout has already been validated.
    ///
    /// # Panics
    /// Panics if the NIR exceeds encoding capacities. Use `try_codegen` for
    /// source input or externally constructed NIR.
    pub fn compile(self, module: &NirModule) -> CodegenOutput {
        self.compile_checked(module)
            .expect("NIR exceeds bytecode capacity; use try_codegen for checked compilation")
    }

    fn compile_checked(mut self, module: &NirModule) -> Result<CodegenOutput, Vec<EncodingError>> {
        // Reserve narrow enum metadata operands before ordinary value constants.
        // The archive writer likewise preserves/reorders these checked operands.
        for function in &module.functions {
            for statement in function.blocks.iter().flat_map(|block| &block.stmts) {
                let NirStmt::Assign(_, expression) = statement else {
                    continue;
                };
                if let NirExpr::ErrorOk(_, ty) | NirExpr::ErrorErr(_, ty) = expression.unscoped() {
                    if !self.error_descriptors.contains_key(ty) {
                        if self.output_constants.len() >= 4096 {
                            return Err(vec![EncodingError {
                                function: function.func_id,
                                message: "Error and enum descriptor operands exceed 4096 constants"
                                    .into(),
                            }]);
                        }
                        self.error_descriptors
                            .insert(*ty, self.output_constants.len() as u16);
                        self.output_constants.push(Constant::Type(*ty));
                    }
                    continue;
                }
                let key = match expression.unscoped() {
                    NirExpr::NewEnum(ty, tag, _) | NirExpr::EnumIs(_, ty, tag) => (*ty, *tag),
                    _ => continue,
                };
                if self.enum_descriptors.contains_key(&key) {
                    continue;
                }
                if self.output_constants.len() >= 1 << 12 {
                    return Err(vec![EncodingError {
                        function: function.func_id,
                        message:
                            "enum instructions support at most 4096 distinct variant descriptors"
                                .into(),
                    }]);
                }
                let index = self.output_constants.len() as u16;
                self.enum_descriptors.insert(key, index);
                self.output_constants.push(Constant::Enum {
                    type_index: key.0,
                    variant: key.1,
                });
            }
        }
        let mut functions = Vec::new();
        for func in &module.functions {
            let compiled = self.compile_function(func);
            functions.push(compiled);
        }
        if !self.encoding_errors.is_empty() {
            return Err(self.encoding_errors);
        }
        Ok(CodegenOutput {
            scope_coverage: nsbc::ScopeCoverage::CallsAndTypes,
            functions,
            constants: self.output_constants,
            globals: module.globals.clone(),
            method_call_scopes: Some(self.method_call_scopes),
        })
    }

    fn compile_function(&mut self, func: &NirFunction) -> CompiledFunction {
        let mut layout = FrameLayout::new(func.local_count);
        let mut emitter = Emitter::new(self.output_constants.len() as u32);

        emitter.emit(Instruction::allocate_slots(func.local_count));
        // Store every incoming parameter before reusing any argument register.
        for (i, param) in func.params.iter().enumerate() {
            emitter.emit(Instruction::store_slot(
                layout.slot(param.local),
                Reg(i as u8),
            ));
        }
        emitter.call_scope = func.entry_scope;
        // An indirect/dynamic caller may have no static signature. Validate
        // required parameters after saving every incoming value, so any boxing
        // during widening cannot lose another argument's GC root.
        for (index, param) in func.params.iter().enumerate() {
            let proof = match param.role {
                nir::NirParamRole::TraitProof { .. }
                | nir::NirParamRole::TraitSelfProof { .. }
                | nir::NirParamRole::CaptureProof { .. } => continue,
                nir::NirParamRole::User => {
                    index
                        .checked_sub(1)
                        .and_then(|previous| match func.params[previous].role {
                            nir::NirParamRole::TraitProof { view }
                            | nir::NirParamRole::TraitSelfProof { view } => {
                                Some((&func.params[previous], view))
                            }
                            _ => None,
                        })
                }
                nir::NirParamRole::Capture => {
                    func.params.get(index + 1).and_then(|next| match next.role {
                        nir::NirParamRole::CaptureProof { view } => Some((next, view)),
                        _ => None,
                    })
                }
            };
            if let Some((proof, view)) = proof {
                emitter.emit(Instruction::load_slot(RESULT, layout.slot(param.local)));
                emitter.emit(Instruction::load_slot(SCRATCH, layout.slot(proof.local)));
                emitter.emit(Instruction::trait_assert(RESULT, SCRATCH, view));
                if param.role == nir::NirParamRole::User
                    && index > 0
                    && matches!(
                        func.params[index - 1].role,
                        nir::NirParamRole::TraitSelfProof { .. }
                    )
                {
                    emitter.emit(Instruction::a_type(
                        Opcode::TypeAssert,
                        nsbc::AddrMode::Imm,
                        RESULT,
                        RESULT,
                        param.type_index.as_u32() as u16,
                    ));
                }
                emitter.emit(Instruction::store_slot(layout.slot(param.local), RESULT));
                continue;
            }
            if param.type_index == type_pool::TypeIndex::INVALID
                || param.type_index == type_pool::Intrinsic::Any.type_index()
            {
                continue;
            }
            emitter.emit(Instruction::load_slot(RESULT, layout.slot(param.local)));
            emitter.emit(Instruction::a_type(
                Opcode::TypeAssert,
                nsbc::AddrMode::Imm,
                RESULT,
                RESULT,
                param.type_index.as_u32() as u16,
            ));
            emitter.emit(Instruction::store_slot(layout.slot(param.local), RESULT));
        }

        emitter.call_scope = None;

        // Compute block order (simple linear order).
        let block_count = func.blocks.len();
        emitter.block_offsets = vec![0u32; block_count];

        // First pass: emit code for each block.
        for block in &func.blocks {
            emitter.block_offsets[block.id.0 as usize] = emitter.current_pc();
            self.emit_block(block, &mut emitter, &mut layout);
        }

        // Second pass: fix up jump offsets.
        for (instr_idx, target_block) in &emitter.fixups {
            let target_pc = emitter.block_offsets[target_block.0 as usize] as i64;
            let source_pc = (*instr_idx + 1) as i64; // PC after the jump instruction
            // VM jump semantics use `pc = pc + offset - 1` after pre-increment,
            // so codegen stores a +1-biased relative offset.
            let offset = target_pc - source_pc + 1;
            if !(-(1 << 21)..(1 << 21)).contains(&offset) {
                self.encoding_errors.push(EncodingError {
                    function: func.func_id,
                    message: "jump distance exceeds the signed 22-bit bytecode capacity".into(),
                });
                continue;
            }
            let offset = offset as i32;

            // Re-encode the jump instruction with the correct offset.
            let old_word = emitter.instructions[*instr_idx];
            if let Some(old_instr) = Instruction::decode(old_word) {
                // Preserve the cond register from the original instruction.
                let cond_reg = match old_instr.data {
                    nsbc::InstructionData::J { cond, .. } => cond,
                    _ => Reg(0),
                };
                let new_instr = if (-(1 << 16)..(1 << 16)).contains(&offset) {
                    Instruction::j_type(old_instr.opcode, cond_reg, offset)
                } else {
                    Instruction::jmp_far(offset)
                };
                emitter.instructions[*instr_idx] = new_instr.encode();
            }
        }

        self.output_constants.append(&mut emitter.constants);
        self.method_call_scopes
            .extend(
                emitter
                    .method_call_scopes
                    .iter()
                    .map(|&(pc, scope)| MethodCallScope {
                        func_id: func.func_id,
                        pc,
                        scope,
                    }),
            );

        CompiledFunction {
            func_id: func.func_id,
            name: func.name,
            instructions: emitter.instructions,
            register_count: Reg::MAX_GP,
            param_count: func.params.len() as u8,
            is_closure: func.is_closure,
            function_type: func.function_type,
            display_owner: func.display_owner,
            abi: Some(func.entry_abi()),
            safepoint_pcs: emitter.safepoint_pcs,
        }
    }

    fn emit_block(&mut self, block: &BasicBlock, emitter: &mut Emitter, layout: &mut FrameLayout) {
        // Emit safepoint at block entry (for GC coordination).
        emitter.emit_safepoint();

        // Emit statements.
        for stmt in &block.stmts {
            self.emit_stmt(stmt, emitter, layout);
        }

        // Emit terminator.
        self.emit_terminator(&block.terminator, emitter, layout);
    }

    fn emit_stmt(&mut self, stmt: &NirStmt, emitter: &mut Emitter, layout: &mut FrameLayout) {
        match stmt {
            NirStmt::Scoped { scope, statement } => {
                let previous = emitter.call_scope.replace(*scope);
                self.emit_stmt(statement, emitter, layout);
                emitter.call_scope = previous;
            }
            NirStmt::StoreField(object, index, value) => {
                self.load_value(object, Reg(0), emitter, layout);
                self.load_value(value, Reg(1), emitter, layout);
                emitter.emit(Instruction::store_field(Reg(0), *index, Reg(1)));
            }
            NirStmt::StoreIndex(object, index, value) => {
                self.load_value(object, Reg(0), emitter, layout);
                self.load_value(index, Reg(1), emitter, layout);
                self.load_value(value, Reg(2), emitter, layout);
                emitter.emit(Instruction::a_type(
                    Opcode::StoreIndex,
                    nsbc::AddrMode::Imm,
                    Reg(2),
                    Reg(0),
                    1,
                ));
            }
            NirStmt::StoreGlobal(global, value) => {
                let source = self.value_to_reg(value, emitter, layout);
                let instruction = if global.0 < (1 << 12) {
                    Instruction::a_type(
                        Opcode::StoreGlobal,
                        nsbc::AddrMode::Imm,
                        Reg(0),
                        source,
                        global.0 as u16,
                    )
                } else {
                    Instruction::a_type(
                        Opcode::StoreGlobalWide,
                        nsbc::AddrMode::Imm,
                        source,
                        Reg((global.0 >> 12) as u8),
                        (global.0 & 0xfff) as u16,
                    )
                };
                emitter.emit(instruction);
            }
            NirStmt::Assign(local, expr) => {
                self.emit_expr(expr, RESULT, emitter, layout);
                emitter.emit(Instruction::store_slot(layout.slot(*local), RESULT));
            }
            NirStmt::Drop(_local) => {
                // For now, GC handles collection. No explicit drop instruction.
            }
            NirStmt::PushHandler {
                effect,
                closure,
                continuation_param,
            } => {
                let register = self.value_to_reg(closure, emitter, layout);
                if let Some(position) = continuation_param {
                    let metadata = emitter.add_constant(Constant::UInt(
                        effect.as_u32() as u64 | ((*position as u64) << 32),
                    ));
                    emitter.emit(Instruction::push_capturing_handler(register, metadata));
                } else {
                    emitter.emit(Instruction::push_handler_closure(*effect, register));
                }
            }
            NirStmt::PopHandler => emitter.emit(Instruction::e_type(Opcode::PopHandler, 0)),
            NirStmt::EffectCall {
                evidence: _,
                operation: _,
                args: _,
                result,
            } => {
                // TODO: emit effect call via evidence
                emitter.emit(Instruction::load_unit(RESULT));
                emitter.emit(Instruction::store_slot(layout.slot(*result), RESULT));
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
        layout: &mut FrameLayout,
    ) {
        match expr {
            NirExpr::LoadGlobal(global) => {
                emitter.emit(Instruction::a_type(
                    if global.0 < (1 << 12) {
                        Opcode::LoadGlobal
                    } else {
                        Opcode::LoadGlobalWide
                    },
                    nsbc::AddrMode::Imm,
                    dst,
                    Reg((global.0 >> 12) as u8),
                    (global.0 & 0xfff) as u16,
                ));
            }
            NirExpr::Use(val) => {
                self.load_value(val, dst, emitter, layout);
            }
            NirExpr::BinOp(op, lhs, rhs) => {
                let lr = self.value_to_reg(lhs, emitter, layout);
                let rr = self.value_to_reg_in(rhs, SECOND_SCRATCH, emitter, layout);
                if matches!(op, BinOp::And | BinOp::Or) {
                    // NIR's eager boolean operators select between already
                    // evaluated bool values; source short-circuiting uses CFG.
                    emitter.emit(Instruction::mov(dst, lr));
                    emitter.emit(if *op == BinOp::And {
                        Instruction::jmp_if_not(lr, 2)
                    } else {
                        Instruction::jmp_if(lr, 2)
                    });
                    emitter.emit(Instruction::mov(dst, rr));
                    return;
                }
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
                    BinOp::And | BinOp::Or => unreachable!("boolean operators were emitted above"),
                };
                emitter.emit(Instruction::r_type(opcode, dst, lr, rr));
            }
            NirExpr::UnaryOp(op, val) => {
                let src = self.value_to_reg(val, emitter, layout);
                match op {
                    UnaryOp::Neg => {
                        emitter.emit(Instruction::r_type(Opcode::Neg, dst, src, Reg(0)));
                    }
                    UnaryOp::Not => {
                        emitter.emit(Instruction::load_false(SECOND_SCRATCH));
                        emitter.emit(Instruction::r_type(Opcode::CmpEq, dst, src, SECOND_SCRATCH));
                    }
                    UnaryOp::BitNot => {
                        emitter.emit(Instruction::r_type(Opcode::BitNot, dst, src, Reg(0)));
                    }
                }
            }
            NirExpr::ScopedCall { scope, call } => {
                let previous = emitter.call_scope.replace(*scope);
                self.emit_expr(call, dst, emitter, layout);
                emitter.call_scope = previous;
            }
            NirExpr::Call(func_id, args) | NirExpr::CallWithProof(func_id, args) => {
                // Place args in r0..rN (up to 8 arg regs).
                for (i, arg) in args.iter().enumerate() {
                    let r = Reg(i as u8);
                    self.load_value(arg, r, emitter, layout);
                }
                self.emit_call(emitter, func_id.0, args.len() as u8);
                // Result is in r0.
                if dst.0 != 0 {
                    emitter.emit(Instruction::mov(dst, Reg(0)));
                }
            }
            NirExpr::CallBuiltin(builtin_id, args) => {
                // Place args in r0..rN.
                for (i, arg) in args.iter().enumerate() {
                    let r = Reg(i as u8);
                    self.load_value(arg, r, emitter, layout);
                }
                emitter.emit(Instruction::call_builtin(*builtin_id, args.len() as u8));
                // Result is in r0.
                if dst.0 != 0 {
                    emitter.emit(Instruction::mov(dst, Reg(0)));
                }
            }
            NirExpr::EffectCall(effect, args) => {
                for (index, arg) in args.iter().enumerate() {
                    self.load_value(arg, Reg(index as u8), emitter, layout);
                }
                emitter.emit(Instruction::effect_call(*effect, args.len() as u8));
                if dst.0 != 0 {
                    emitter.emit(Instruction::mov(dst, Reg(0)));
                }
            }
            NirExpr::DelimitedCall {
                body,
                handler_count,
            } => {
                let body = self.value_to_reg(body, emitter, layout);
                emitter.emit(Instruction::reset_closure(body, *handler_count));
                emitter.emit(Instruction::mov(dst, Reg(0)));
            }
            NirExpr::ResumeContinuation(continuation, value)
            | NirExpr::ResumeContinuationOnce(continuation, value) => {
                let continuation = self.value_to_reg(continuation, emitter, layout);
                let value = self.value_to_reg_in(value, SECOND_SCRATCH, emitter, layout);
                emitter.emit(if matches!(expr, NirExpr::ResumeContinuationOnce(..)) {
                    Instruction::resume_continuation_once(continuation, value)
                } else {
                    Instruction::resume_continuation(continuation, value)
                });
                emitter.emit(Instruction::mov(dst, Reg(0)));
            }
            NirExpr::MethodCall(receiver, method, args) => {
                // Place args in r0..rN-1.
                for (i, arg) in args.iter().enumerate() {
                    let r = Reg(i as u8);
                    self.load_value(arg, r, emitter, layout);
                }
                // Load receiver into a register outside the arg range.
                let recv_reg = Reg(args.len() as u8);
                self.load_value(receiver, recv_reg, emitter, layout);
                self.emit_call_method(emitter, recv_reg, method.as_u32(), args.len() as u8);
                // Result is in r0.
                if dst.0 != 0 {
                    emitter.emit(Instruction::mov(dst, Reg(0)));
                }
            }
            NirExpr::FieldAccess(obj, field_idx) => {
                let obj_reg = self.value_to_reg(obj, emitter, layout);
                emitter.emit(Instruction::load_field(dst, obj_reg, *field_idx));
            }
            NirExpr::IndexAccess(obj, idx) => {
                let obj_reg = self.value_to_reg(obj, emitter, layout);
                let idx_reg = self.value_to_reg_in(idx, SECOND_SCRATCH, emitter, layout);
                // Index register encoded in imm12 (independent opcode, not amode).
                emitter.emit(Instruction::a_type(
                    Opcode::LoadIndex,
                    nsbc::AddrMode::Imm,
                    dst,
                    obj_reg,
                    idx_reg.0 as u16,
                ));
            }
            NirExpr::NewList(length) => {
                emitter.emit(Instruction::a_type(
                    Opcode::NewList,
                    nsbc::AddrMode::Imm,
                    dst,
                    Reg(0),
                    *length,
                ));
            }
            NirExpr::ErrorOk(value, ty) | NirExpr::ErrorErr(value, ty) => {
                self.load_value(value, SCRATCH, emitter, layout);
                emitter.emit(Instruction::a_type(
                    if matches!(expr, NirExpr::ErrorOk(..)) {
                        Opcode::ErrorOk
                    } else {
                        Opcode::ErrorErr
                    },
                    nsbc::AddrMode::Imm,
                    dst,
                    SCRATCH,
                    self.error_descriptors[ty],
                ));
            }
            NirExpr::ErrorIsOk(value) | NirExpr::ErrorPayload(value) => {
                self.load_value(value, SCRATCH, emitter, layout);
                emitter.emit(Instruction::a_type(
                    if matches!(expr, NirExpr::ErrorIsOk(..)) {
                        Opcode::ErrorIsOk
                    } else {
                        Opcode::ErrorPayload
                    },
                    nsbc::AddrMode::Imm,
                    dst,
                    SCRATCH,
                    0,
                ));
            }
            NirExpr::NewEnum(ty, tag, tuple) => {
                let tuple = self.value_to_reg(tuple, emitter, layout);
                emitter.emit(Instruction::new_enum(
                    dst,
                    tuple,
                    self.enum_descriptors[&(*ty, *tag)],
                ));
            }
            NirExpr::EnumIs(value, ty, tag) => {
                let value = self.value_to_reg(value, emitter, layout);
                emitter.emit(Instruction::enum_is(
                    dst,
                    value,
                    self.enum_descriptors[&(*ty, *tag)],
                ));
            }
            NirExpr::EnumField(value, index) => {
                let value = self.value_to_reg(value, emitter, layout);
                emitter.emit(Instruction::enum_field(dst, value, *index as u16));
            }
            NirExpr::NewObject(type_idx, fields) => {
                emitter.emit(Instruction::new_object(dst, *type_idx));
                for (i, field_val) in fields.iter().enumerate() {
                    let val_reg = self.value_to_reg(field_val, emitter, layout);
                    emitter.emit(Instruction::store_field(dst, i as u32, val_reg));
                }
            }
            NirExpr::NewClosure(func_id, captures) => {
                // Place captured values in r0..rN for the closure to pick up.
                for (i, cap) in captures.iter().enumerate() {
                    let r = Reg(i as u8);
                    self.load_value(cap, r, emitter, layout);
                }
                self.emit_new_closure(emitter, dst, func_id.0, captures.len() as u8);
            }
            NirExpr::CallIndirect(callee, args) | NirExpr::CallIndirectProof(callee, args) => {
                // Load the closure value first, before args clobber r0..rN-1.
                let closure_reg = Reg(args.len() as u8);
                self.load_value(callee, closure_reg, emitter, layout);
                // Place explicit args in r0..r(N-1).
                for (i, arg) in args.iter().enumerate() {
                    let r = Reg(i as u8);
                    self.load_value(arg, r, emitter, layout);
                }
                emitter.emit(if matches!(expr, NirExpr::CallIndirectProof(..)) {
                    Instruction::call_indirect_proof(closure_reg, args.len() as u8)
                } else {
                    Instruction::call_indirect(closure_reg, args.len() as u8)
                });
                // Result is in r0.
                if dst.0 != 0 {
                    emitter.emit(Instruction::mov(dst, Reg(0)));
                }
            }
            NirExpr::TraitProof(value, view) => {
                self.load_value(value, SCRATCH, emitter, layout);
                emitter.emit(Instruction::trait_proof(dst, SCRATCH, *view));
            }
            NirExpr::TraitProject(proof, view) => {
                self.load_value(proof, SCRATCH, emitter, layout);
                emitter.emit(Instruction::trait_project(dst, SCRATCH, *view));
            }
            NirExpr::TraitAssert(value, proof, view) => {
                self.load_value(value, dst, emitter, layout);
                self.load_value(proof, SCRATCH, emitter, layout);
                emitter.emit(Instruction::trait_assert(dst, SCRATCH, *view));
            }
            NirExpr::TraitCall {
                receiver,
                proof,
                slot,
                args,
                ..
            } => {
                self.load_value(receiver, Reg(0), emitter, layout);
                for (index, arg) in args.iter().enumerate() {
                    self.load_value(arg, Reg((index + 1) as u8), emitter, layout);
                }
                let proof_register = Reg((args.len() + 1) as u8);
                self.load_value(proof, proof_register, emitter, layout);
                emitter.emit(Instruction::trait_call(
                    proof_register,
                    *slot,
                    (args.len() + 1) as u8,
                ));
                if dst.0 != 0 {
                    emitter.emit(Instruction::mov(dst, Reg(0)));
                }
            }
            NirExpr::TypeCheck(val, type_idx) => {
                let src = self.value_to_reg(val, emitter, layout);
                let idx = type_idx.as_u32();
                debug_assert!(idx < (1 << 12));
                emitter.emit(Instruction::a_type(
                    Opcode::TypeCheck,
                    nsbc::AddrMode::Imm,
                    dst,
                    src,
                    idx as u16,
                ));
            }
            NirExpr::TypeCast(val, type_idx) | NirExpr::TypeAssert(val, type_idx) => {
                let src = self.value_to_reg(val, emitter, layout);
                let idx = type_idx.as_u32();
                debug_assert!(idx < (1 << 12));
                emitter.emit(Instruction::a_type(
                    if matches!(expr, NirExpr::TypeAssert(_, _)) {
                        Opcode::TypeAssert
                    } else {
                        Opcode::TypeCast
                    },
                    nsbc::AddrMode::Imm,
                    dst,
                    src,
                    idx as u16,
                ));
            }
        }
    }

    fn emit_call(&self, emitter: &mut Emitter, func_id: u32, arg_count: u8) {
        if func_id < (1 << 14) {
            emitter.emit(Instruction::call(func_id, arg_count));
        } else {
            let idx = emitter.add_constant(Constant::UInt(func_id as u64));
            debug_assert!(idx < (1 << 14));
            emitter.emit(Instruction::call_far(idx as u16, arg_count));
        }
    }

    fn emit_call_method(
        &self,
        emitter: &mut Emitter,
        recv: Reg,
        method_str_id: u32,
        arg_count: u8,
    ) {
        if method_str_id < (1 << 12) && arg_count < 32 {
            emitter.emit(Instruction::call_method(recv, method_str_id, arg_count));
        } else {
            let idx = emitter.add_constant(Constant::UInt(method_str_id as u64));
            debug_assert!(idx < (1 << 12));
            emitter.emit(Instruction::call_method_far(recv, idx as u16, arg_count));
        }
    }

    fn emit_new_closure(&self, emitter: &mut Emitter, dst: Reg, func_id: u32, capture_count: u8) {
        if func_id < (1 << 12) {
            emitter.emit(Instruction::new_closure(dst, func_id, capture_count));
        } else {
            let idx = emitter.add_constant(Constant::UInt(func_id as u64));
            debug_assert!(idx < (1 << 12));
            emitter.emit(Instruction::new_closure_wide(
                dst,
                idx as u16,
                capture_count,
            ));
        }
    }

    fn emit_load_const(&self, emitter: &mut Emitter, dst: Reg, global_idx: u32) {
        if global_idx < (1 << 12) {
            emitter.emit(Instruction::load_const(dst, global_idx as u16));
        } else {
            // 19-bit wide index
            debug_assert!(global_idx < (1 << 19));
            emitter.emit(Instruction::load_const_wide(dst, global_idx));
        }
    }

    /// Emit Load with Imm if value fits in signed 12-bit; otherwise pool + Const.
    fn emit_load_integer(&self, emitter: &mut Emitter, dst: Reg, bits: u64, signed: bool) {
        let as_i64 = bits as i64;
        // Load's immediate operand creates a signed value. Unsigned constants
        // must retain their tag through the constant pool even when small.
        let fits_imm = signed && (-2048..2048).contains(&as_i64);
        if fits_imm {
            emitter.emit(Instruction::load_imm(dst, (bits as u16) & 0xFFF));
        } else if signed {
            let idx = emitter.add_constant(Constant::Int(bits as i64));
            self.emit_load_const(emitter, dst, idx);
        } else {
            let idx = emitter.add_constant(Constant::UInt(bits));
            self.emit_load_const(emitter, dst, idx);
        }
    }

    fn emit_terminator(
        &mut self,
        term: &Terminator,
        emitter: &mut Emitter,
        layout: &mut FrameLayout,
    ) {
        match term {
            Terminator::Goto(target) => {
                let pc = emitter.current_pc() as usize;
                emitter.fixups.push((pc, *target));
                emitter.emit(Instruction::jmp(0)); // placeholder offset
            }
            Terminator::Branch(cond, then_block, else_block) => {
                let cond_reg = self.value_to_reg(cond, emitter, layout);
                // Keep the conditional hop short. Either destination may need
                // a far jump; widening the conditional itself would lose its
                // condition because the ISA's JmpFar is unconditional.
                emitter.emit(Instruction::jmp_if_not(cond_reg, 2));
                let pc_then = emitter.current_pc() as usize;
                emitter.fixups.push((pc_then, *then_block));
                emitter.emit(Instruction::jmp(0)); // placeholder
                let pc_else = emitter.current_pc() as usize;
                emitter.fixups.push((pc_else, *else_block));
                emitter.emit(Instruction::jmp(0)); // placeholder
            }
            Terminator::Return(val) => match val {
                NirValue::Unit => {
                    emitter.emit(Instruction::return_unit());
                }
                _ => {
                    let src = self.value_to_reg(val, emitter, layout);
                    emitter.emit(Instruction::ret(src));
                }
            },
            Terminator::Unreachable => {
                // Emit a trap / debug break.
                emitter.emit(Instruction::e_type(Opcode::DebugBreak, 0));
            }
            Terminator::MatchFail => emitter.emit(Instruction::match_fail()),
        }
    }

    /// Load a NirValue into a register.
    fn load_value(
        &mut self,
        val: &NirValue,
        dst: Reg,
        emitter: &mut Emitter,
        layout: &mut FrameLayout,
    ) {
        match val {
            NirValue::Local(local) => {
                emitter.emit(Instruction::load_slot(dst, layout.slot(*local)));
            }
            NirValue::ConstInt(v) => {
                self.emit_load_integer(emitter, dst, *v as u64, true);
            }
            NirValue::ConstUInt(v) => {
                self.emit_load_integer(emitter, dst, *v, false);
            }
            NirValue::ConstI128(value) => {
                let index = emitter.add_constant(Constant::Int128(*value));
                self.emit_load_const(emitter, dst, index);
            }
            NirValue::ConstU128(value) => {
                let index = emitter.add_constant(Constant::UInt128(*value));
                self.emit_load_const(emitter, dst, index);
            }
            NirValue::ConstBool(b) => {
                if *b {
                    emitter.emit(Instruction::load_true(dst));
                } else {
                    emitter.emit(Instruction::load_false(dst));
                }
            }
            NirValue::ConstChar(value) => {
                let index = emitter.add_constant(Constant::Char(*value));
                self.emit_load_const(emitter, dst, index);
            }
            NirValue::Unit => {
                emitter.emit(Instruction::load_unit(dst));
            }
            NirValue::Null => {
                emitter.emit(Instruction::load_null(dst));
            }
            NirValue::ConstFloat(v) => {
                let idx = emitter.add_constant(Constant::Float(*v));
                self.emit_load_const(emitter, dst, idx);
            }
            NirValue::ConstType(ty) => {
                let index = emitter.add_constant(Constant::Type(*ty));
                self.emit_load_const(emitter, dst, index);
            }
            NirValue::ConstEnum(ty, variant) => {
                let index = emitter.add_constant(Constant::Enum {
                    type_index: *ty,
                    variant: *variant,
                });
                self.emit_load_const(emitter, dst, index);
            }
            NirValue::ConstStr(str_id) => {
                // Strip surrounding quotes and process escape sequences.
                let raw = str_interner::get(*str_id);
                let content = if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
                    process_string_escapes(&raw[1..raw.len() - 1])
                } else {
                    raw.to_string()
                };
                let global_idx = emitter.add_constant(Constant::Str(content));
                self.emit_load_const(emitter, dst, global_idx);
            }
        }
    }

    /// Materialize a NirValue into a register, returning which register.
    fn value_to_reg(
        &mut self,
        val: &NirValue,
        emitter: &mut Emitter,
        layout: &mut FrameLayout,
    ) -> Reg {
        self.value_to_reg_in(val, SCRATCH, emitter, layout)
    }

    fn value_to_reg_in(
        &mut self,
        val: &NirValue,
        scratch: Reg,
        emitter: &mut Emitter,
        layout: &mut FrameLayout,
    ) -> Reg {
        self.load_value(val, scratch, emitter, layout);
        scratch
    }
}

impl Default for Codegen {
    fn default() -> Self {
        Self::new()
    }
}

/// Process Nessa string escape sequences: \n \t \r \\ \" \' \xHH \uHHHH.
fn process_string_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some('x') => {
                    let h1 = chars.next().unwrap_or('0');
                    let h2 = chars.next().unwrap_or('0');
                    let hex = format!("{}{}", h1, h2);
                    if let Ok(v) = u8::from_str_radix(&hex, 16) {
                        out.push(v as char);
                    }
                }
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(v) = u32::from_str_radix(&hex, 16)
                        && let Some(ch) = char::from_u32(v)
                    {
                        out.push(ch);
                    }
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Compile a NIR module into bytecode.
pub fn codegen(module: &NirModule) -> CodegenOutput {
    Codegen::new().compile(module)
}

/// Validate source-generated NIR before any encoding assertions can execute.
pub fn try_codegen(module: &NirModule) -> Result<CodegenOutput, Vec<EncodingError>> {
    let errors = validation::validate(module);
    if !errors.is_empty() {
        return Err(errors);
    }
    Codegen::new().compile_checked(module)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nsbc::FuncId;

    fn make_simple_nir() -> NirModule {
        use nir::*;
        use type_pool::TypeIndex;
        NirModule {
            derived_methods: Vec::new(),
            function_symbols: Default::default(),
            globals: Vec::new(),
            entry: None,
            functions: vec![NirFunction {
                entry_scope: Some(0),
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
                function_type: type_pool::TypeIndex::INVALID,
                display_owner: None,
            }],
        }
    }

    #[test]
    fn entry_abi_is_explicit_for_ordinary_functions_and_closure_captures() {
        let mut module = make_simple_nir();
        let output = try_codegen(&module).unwrap();
        let abi = output.functions[0].abi.as_ref().unwrap();
        assert_eq!(
            abi,
            &nsbc::FunctionAbi {
                captures: vec![],
                parameters: vec![]
            }
        );
        let function = &mut module.functions[0];
        function.is_closure = true;
        function.params = vec![
            nir::NirParam {
                local: NirLocal(0),
                name: str_interner::intern("capture"),
                type_index: type_pool::TypeIndex::INVALID,
                role: nir::NirParamRole::Capture,
            },
            nir::NirParam {
                local: NirLocal(1),
                name: str_interner::intern("arg"),
                type_index: type_pool::TypeIndex::INVALID,
                role: nir::NirParamRole::User,
            },
        ];
        let output = try_codegen(&module).unwrap();
        let function = &output.functions[0];
        let abi = function.abi.as_ref().unwrap();
        assert_eq!(abi.captures, vec![nsbc::CaptureAbi::Value]);
        assert_eq!(abi.parameters, vec![nsbc::ParameterAbi::Value]);
        assert_eq!(
            abi.physical_parameter_count(),
            function.param_count as usize
        );
        assert!(!abi.has_trait_proofs());
        let stores: Vec<_> = function
            .instructions
            .iter()
            .map(|&word| Instruction::decode(word).unwrap())
            .take(3)
            .filter(|instruction| instruction.opcode == Opcode::StoreSlot)
            .collect();
        assert_eq!(
            stores.len(),
            2,
            "both physical parameters are saved before body execution"
        );
    }

    #[test]
    fn trait_entry_saves_both_parameters_then_asserts_the_provided_proof() {
        let mut module = make_simple_nir();
        let view = type_pool::TypeIndex::from_raw(25);
        module.functions[0].params = vec![
            nir::NirParam {
                local: NirLocal(0),
                name: str_interner::intern("proof"),
                type_index: type_pool::TypeIndex::INVALID,
                role: nir::NirParamRole::TraitProof { view },
            },
            nir::NirParam {
                local: NirLocal(1),
                name: str_interner::intern("data"),
                type_index: view,
                role: nir::NirParamRole::User,
            },
        ];
        let output = try_codegen(&module).unwrap();
        assert_eq!(
            output.functions[0].abi.as_ref().unwrap().parameters,
            vec![nsbc::ParameterAbi::Trait { view }]
        );
        let instructions: Vec<_> = output.functions[0]
            .instructions
            .iter()
            .map(|&word| Instruction::decode(word).unwrap())
            .collect();
        assert_eq!(instructions[1].opcode, Opcode::StoreSlot);
        assert_eq!(instructions[2].opcode, Opcode::StoreSlot);
        let assert = instructions
            .iter()
            .position(|instruction| instruction.opcode == Opcode::TraitAssert)
            .unwrap();
        assert!(assert > 2);
        assert!(
            !instructions[..=assert]
                .iter()
                .any(|instruction| instruction.opcode == Opcode::TypeAssert
                    || instruction.opcode == Opcode::TraitProof)
        );
        module.functions[0].params[0].role = nir::NirParamRole::TraitSelfProof { view };
        module.functions[0].params[1].type_index = type_pool::Intrinsic::I64.type_index();
        let output = try_codegen(&module).unwrap();
        assert_eq!(
            output.functions[0].abi.as_ref().unwrap().parameters,
            vec![nsbc::ParameterAbi::TraitSelf { view }]
        );
        let checks: Vec<_> = output.functions[0]
            .instructions
            .iter()
            .map(|&word| Instruction::decode(word).unwrap())
            .filter(|instruction| {
                matches!(
                    instruction.opcode,
                    Opcode::TraitAssert | Opcode::TypeAssert | Opcode::TraitProof
                )
            })
            .collect();
        assert_eq!(
            checks
                .iter()
                .map(|instruction| instruction.opcode)
                .collect::<Vec<_>>(),
            vec![Opcode::TraitAssert, Opcode::TypeAssert]
        );
        assert!(
            matches!(checks[1].data, nsbc::InstructionData::A { imm12, .. } if imm12 == type_pool::Intrinsic::I64.type_index().as_u32() as u16)
        );
        assert!(
            output
                .method_call_scopes
                .as_ref()
                .unwrap()
                .iter()
                .any(|entry| entry.pc as usize == assert && entry.scope == 0)
        );
    }

    #[test]
    fn proof_opcodes_preserve_physical_call_layout_and_lexical_acquisition_scope() {
        let mut module = make_simple_nir();
        let view = type_pool::TypeIndex::from_raw(25);
        module.functions[0].local_count = 5;
        module.functions[0].blocks[0].stmts = vec![
            NirStmt::Assign(
                NirLocal(1),
                NirExpr::TraitProof(NirValue::Local(NirLocal(0)), view).in_scope(7),
            ),
            NirStmt::Assign(
                NirLocal(2),
                NirExpr::TraitProject(NirValue::Local(NirLocal(1)), view),
            ),
            NirStmt::Assign(
                NirLocal(3),
                NirExpr::TraitCall {
                    receiver: NirValue::Local(NirLocal(0)),
                    proof: NirValue::Local(NirLocal(2)),
                    view,
                    slot: 3,
                    args: vec![NirValue::ConstInt(42)],
                }
                .in_scope(8),
            ),
            NirStmt::Assign(
                NirLocal(4),
                NirExpr::CallIndirectProof(
                    NirValue::Local(NirLocal(0)),
                    vec![NirValue::Local(NirLocal(1)), NirValue::Local(NirLocal(0))],
                )
                .in_scope(9),
            ),
        ];
        let output = try_codegen(&module).unwrap();
        let decoded: Vec<_> = output.functions[0]
            .instructions
            .iter()
            .map(|&word| Instruction::decode(word).unwrap())
            .collect();
        let trait_call = decoded
            .iter()
            .find(|instruction| instruction.opcode == Opcode::TraitCall)
            .unwrap();
        let nsbc::InstructionData::C { payload } = trait_call.data else {
            panic!("trait call uses C encoding")
        };
        assert_eq!(Instruction::c_call_method(payload), (2, Reg(2), 3));
        let indirect = decoded
            .iter()
            .find(|instruction| instruction.opcode == Opcode::CallIndirectProof)
            .unwrap();
        let nsbc::InstructionData::C { payload } = indirect.data else {
            panic!("indirect call uses C encoding")
        };
        assert_eq!(Instruction::c_call_indirect(payload), (2, Reg(2)));
        let scopes: Vec<_> = output
            .method_call_scopes
            .as_ref()
            .unwrap()
            .iter()
            .map(|entry| entry.scope)
            .collect();
        assert_eq!(scopes, vec![7, 8, 9]);
    }

    #[test]
    fn malformed_capture_prefixes_are_rejected_before_codegen() {
        let mut module = make_simple_nir();
        module.functions[0].params = vec![nir::NirParam {
            local: NirLocal(0),
            name: str_interner::intern("capture"),
            type_index: type_pool::TypeIndex::INVALID,
            role: nir::NirParamRole::Capture,
        }];
        assert!(
            try_codegen(&module)
                .unwrap_err()
                .iter()
                .any(|error| error.message.contains("capture"))
        );
        module.functions[0].is_closure = true;
        module.functions[0].params.insert(
            0,
            nir::NirParam {
                local: NirLocal(1),
                name: str_interner::intern("arg"),
                type_index: type_pool::TypeIndex::INVALID,
                role: nir::NirParamRole::User,
            },
        );
        assert!(
            try_codegen(&module)
                .unwrap_err()
                .iter()
                .any(|error| error.message.contains("capture"))
        );
    }

    #[test]
    fn type_query_contexts_cover_declaration_prologues_and_lexical_expressions() {
        let mut module = make_simple_nir();
        let function = &mut module.functions[0];
        function.entry_scope = Some(42);
        function.is_closure = true;
        function.params = vec![nir::NirParam {
            local: NirLocal(0),
            name: str_interner::intern("capture"),
            type_index: type_pool::Intrinsic::I64.type_index(),
            role: nir::NirParamRole::User,
        }];
        function.blocks[0].stmts = vec![
            NirStmt::Assign(
                NirLocal(1),
                NirExpr::TypeCheck(
                    NirValue::Local(NirLocal(0)),
                    type_pool::Intrinsic::I64.type_index(),
                )
                .in_scope(7),
            ),
            NirStmt::Assign(
                NirLocal(1),
                NirExpr::TypeCast(
                    NirValue::Local(NirLocal(0)),
                    type_pool::Intrinsic::I64.type_index(),
                )
                .in_scope(8),
            ),
            NirStmt::Assign(
                NirLocal(1),
                NirExpr::TypeAssert(
                    NirValue::Local(NirLocal(0)),
                    type_pool::Intrinsic::I64.type_index(),
                )
                .in_scope(9),
            ),
        ];
        let output = try_codegen(&module).unwrap();
        assert_eq!(output.scope_coverage, nsbc::ScopeCoverage::CallsAndTypes);
        let pcs: Vec<_> = output.functions[0]
            .instructions
            .iter()
            .enumerate()
            .filter_map(|(pc, &word)| {
                let opcode = Instruction::decode(word).unwrap().opcode;
                matches!(
                    opcode,
                    Opcode::TypeCheck | Opcode::TypeCast | Opcode::TypeAssert
                )
                .then_some(pc as u32)
            })
            .collect();
        assert_eq!(pcs.len(), 4);
        assert_eq!(
            output.method_call_scopes,
            Some(
                pcs.into_iter()
                    .zip([42, 7, 8, 9])
                    .map(|(pc, scope)| MethodCallScope {
                        func_id: FuncId(0),
                        pc,
                        scope
                    })
                    .collect()
            )
        );
        module.functions[0].entry_scope = None;
        assert!(
            try_codegen(&module)
                .unwrap_err()
                .iter()
                .any(|error| error.message.contains("declaration scope"))
        );
    }

    #[test]
    fn safe_cast_emission_uses_the_same_type_query_context_table() {
        let mut emitter = Emitter::new(0);
        emitter.call_scope = Some(7);
        emitter.emit(Instruction::a_type(
            Opcode::TypeCastSafe,
            nsbc::AddrMode::Imm,
            Reg(0),
            Reg(1),
            0,
        ));
        assert_eq!(emitter.method_call_scopes, vec![(0, 7)]);
    }

    #[test]
    fn dynamic_call_contexts_use_the_actual_emitted_pc_for_every_encoding() {
        let mut module = make_simple_nir();
        let function = &mut module.functions[0];
        function.blocks[0].stmts = vec![
            NirStmt::Assign(NirLocal(0), NirExpr::Use(NirValue::Unit)),
            NirStmt::Assign(NirLocal(1), NirExpr::Call(FuncId(0), vec![])),
            NirStmt::Assign(
                NirLocal(1),
                NirExpr::MethodCall(
                    NirValue::Local(NirLocal(0)),
                    str_interner::StrId::from_raw(17),
                    vec![NirValue::ConstInt(42)],
                )
                .in_scope(7),
            ),
            NirStmt::Assign(
                NirLocal(1),
                NirExpr::MethodCall(
                    NirValue::Local(NirLocal(0)),
                    str_interner::StrId::from_raw(4096),
                    vec![],
                )
                .in_scope(8),
            ),
            NirStmt::Assign(
                NirLocal(1),
                NirExpr::CallIndirect(NirValue::Local(NirLocal(0)), vec![NirValue::ConstInt(42)])
                    .in_scope(9),
            ),
        ];
        let output = try_codegen(&module).unwrap();
        let calls: Vec<_> = output.functions[0]
            .instructions
            .iter()
            .enumerate()
            .filter_map(|(pc, &word)| {
                let instruction = Instruction::decode(word).unwrap();
                matches!(
                    instruction.opcode,
                    Opcode::CallMethod | Opcode::CallMethodFar | Opcode::CallIndirect
                )
                .then_some((pc as u32, instruction.opcode))
            })
            .collect();
        assert_eq!(
            calls.iter().map(|&(_, op)| op).collect::<Vec<_>>(),
            vec![
                Opcode::CallMethod,
                Opcode::CallMethodFar,
                Opcode::CallIndirect
            ]
        );
        assert_eq!(
            output.method_call_scopes,
            Some(
                calls
                    .iter()
                    .zip([7, 8, 9])
                    .map(|(&(pc, _), scope)| MethodCallScope {
                        func_id: FuncId(0),
                        pc,
                        scope
                    })
                    .collect()
            )
        );
        assert_eq!(
            try_codegen(&make_simple_nir()).unwrap().method_call_scopes,
            Some(vec![])
        );
    }

    #[test]
    fn incomplete_or_malformed_dynamic_scope_metadata_is_rejected() {
        for expression in [
            NirExpr::CallIndirect(NirValue::Unit, vec![]),
            NirExpr::MethodCall(NirValue::Unit, str_interner::StrId::from_raw(17), vec![]),
            NirExpr::Use(NirValue::Unit).in_scope(0),
            NirExpr::CallIndirect(NirValue::Unit, vec![])
                .in_scope(0)
                .in_scope(1),
            NirExpr::CallIndirect(NirValue::Local(NirLocal(99)), vec![]).in_scope(0),
            NirExpr::CallIndirect(NirValue::Unit, vec![NirValue::Unit; 32]).in_scope(0),
        ] {
            let mut module = make_simple_nir();
            module.functions[0].blocks[0].stmts = vec![NirStmt::Assign(NirLocal(0), expression)];
            assert!(try_codegen(&module).is_err());
        }
    }

    #[test]
    fn distant_branch_destinations_keep_their_condition() {
        for condition in [false, true] {
            let mut module = make_simple_nir();
            let function = &mut module.functions[0];
            function.local_count = 1;
            function.blocks = vec![
                BasicBlock {
                    id: BlockId(0),
                    stmts: vec![],
                    terminator: Terminator::Branch(
                        NirValue::ConstBool(condition),
                        BlockId(2),
                        BlockId(1),
                    ),
                },
                BasicBlock {
                    id: BlockId(1),
                    stmts: vec![
                        NirStmt::Assign(NirLocal(0), NirExpr::Use(NirValue::ConstInt(0)));
                        33000
                    ],
                    terminator: Terminator::Return(NirValue::ConstInt(0)),
                },
                BasicBlock {
                    id: BlockId(2),
                    stmts: vec![],
                    terminator: Terminator::Return(NirValue::ConstInt(42)),
                },
            ];
            let output = try_codegen(&module).unwrap();
            let words = &output.functions[0].instructions;
            let branch_pc = words
                .iter()
                .position(|&word| Instruction::decode(word).unwrap().opcode == Opcode::JmpIfNot)
                .unwrap();
            let nsbc::InstructionData::J { offset: skip, .. } =
                Instruction::decode(words[branch_pc]).unwrap().data
            else {
                panic!("expected short conditional hop")
            };
            assert_eq!(skip, 2);
            let hop_pc = branch_pc + if condition { 1 } else { 2 };
            let hop = Instruction::decode(words[hop_pc]).unwrap();
            let nsbc::InstructionData::J { offset, .. } = hop.data else {
                panic!("expected destination jump")
            };
            assert_eq!(
                hop.opcode,
                if condition {
                    Opcode::JmpFar
                } else {
                    Opcode::Jmp
                }
            );
            let destination = (hop_pc as i64 + offset as i64) as usize;
            assert_eq!(
                Instruction::decode(words[destination]).unwrap().opcode,
                Opcode::Safepoint
            );
            let nsbc::InstructionData::A { imm12, .. } =
                Instruction::decode(words[destination + 1]).unwrap().data
            else {
                panic!("expected result load")
            };
            assert_eq!(imm12, if condition { 42 } else { 0 });
        }
    }

    #[test]
    fn malformed_nir_block_targets_fail_before_emission() {
        let mut module = make_simple_nir();
        module.functions[0].blocks[0].terminator = Terminator::Goto(BlockId(u32::MAX));
        assert!(
            try_codegen(&module)
                .unwrap_err()
                .iter()
                .any(|error| error.message.contains("outside the function"))
        );
        module.functions[0].blocks[0].id = BlockId(1);
        assert!(
            try_codegen(&module)
                .unwrap_err()
                .iter()
                .any(|error| error.message.contains("dense from zero"))
        );
    }

    #[test]
    fn globals_preserve_schema_and_select_narrow_or_wide_encoding() {
        use nir::{NirLocal, NirStmt, NirValue};
        use nsbc::{GlobalId, GlobalInfo, InstructionData};
        for index in [4095u32, 4096, (1 << 17) - 1] {
            let mut module = make_simple_nir();
            module.globals = vec![
                GlobalInfo {
                    type_index: type_pool::Intrinsic::I64.type_index(),
                    is_mutable: false
                };
                index as usize + 1
            ];
            let function = &mut module.functions[0];
            function.local_count = 1;
            function.blocks[0].stmts = vec![
                NirStmt::StoreGlobal(GlobalId(index), NirValue::ConstInt(42)).in_scope(0),
                NirStmt::Assign(NirLocal(0), NirExpr::LoadGlobal(GlobalId(index))),
            ];
            function.blocks[0].terminator = Terminator::Return(NirValue::Local(NirLocal(0)));
            let output = try_codegen(&module).unwrap();
            assert_eq!(output.globals, module.globals);
            let instructions: Vec<_> = output.functions[0]
                .instructions
                .iter()
                .filter_map(|&word| Instruction::decode(word))
                .filter(|instruction| {
                    matches!(
                        instruction.opcode,
                        Opcode::LoadGlobal
                            | Opcode::LoadGlobalWide
                            | Opcode::StoreGlobal
                            | Opcode::StoreGlobalWide
                    )
                })
                .collect();
            assert_eq!(instructions.len(), 2);
            let wide = index >= 4096;
            assert_eq!(
                instructions[0].opcode,
                if wide {
                    Opcode::StoreGlobalWide
                } else {
                    Opcode::StoreGlobal
                }
            );
            assert_eq!(
                instructions[1].opcode,
                if wide {
                    Opcode::LoadGlobalWide
                } else {
                    Opcode::LoadGlobal
                }
            );
            for instruction in instructions {
                let InstructionData::A { base, imm12, .. } = instruction.data else {
                    panic!("global instructions must use A format");
                };
                assert_eq!(
                    if wide {
                        Instruction::wide_const_index(base, imm12)
                    } else {
                        imm12 as u32
                    },
                    index
                );
            }
        }
    }

    #[test]
    fn checked_codegen_rejects_missing_or_unencodable_globals() {
        use nir::{NirLocal, NirStmt, NirValue};
        use nsbc::{GlobalId, GlobalInfo};
        for index in [0u32, 1 << 17] {
            let mut module = make_simple_nir();
            module.globals.clear();
            module.functions[0].blocks[0].stmts = vec![
                NirStmt::StoreGlobal(GlobalId(index), NirValue::ConstInt(42)).in_scope(0),
                NirStmt::Assign(NirLocal(0), NirExpr::LoadGlobal(GlobalId(index))),
            ];
            let errors = try_codegen(&module).unwrap_err();
            assert!(
                errors
                    .iter()
                    .any(|error| error.message.contains("global store"))
            );
            assert!(
                errors
                    .iter()
                    .any(|error| error.message.contains("global load"))
            );
        }
        let mut module = make_simple_nir();
        module.functions.clear();
        module.globals = vec![
            GlobalInfo {
                type_index: type_pool::Intrinsic::Any.type_index(),
                is_mutable: true
            };
            (1 << 17) + 1
        ];
        assert!(
            try_codegen(&module)
                .unwrap_err()
                .iter()
                .any(|error| error.message.contains("131072 global"))
        );
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

    #[test]
    fn checked_codegen_rejects_out_of_range_local_references() {
        let mut module = make_simple_nir();
        module.functions[0].local_count = 1;
        let errors = try_codegen(&module).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|error| error.message.contains("outside the function's slot layout"))
        );
    }

    #[test]
    fn wide_and_unsigned_constants_keep_every_bit_in_the_pool() {
        for value in [
            NirValue::ConstUInt(42),
            NirValue::ConstUInt(u64::MAX),
            NirValue::ConstI128(42),
            NirValue::ConstU128(42),
            NirValue::ConstI128(i128::MIN),
            NirValue::ConstI128(i128::MAX),
            NirValue::ConstU128(u128::MAX),
        ] {
            let mut module = make_simple_nir();
            module.functions[0].blocks[0].stmts.clear();
            module.functions[0].blocks[0].terminator = Terminator::Return(value);
            let output = try_codegen(&module).unwrap();
            assert_eq!(output.constants.len(), 1);
            match (&value, &output.constants[0]) {
                (NirValue::ConstUInt(expected), Constant::UInt(actual)) => {
                    assert_eq!(actual, expected)
                }
                (NirValue::ConstI128(expected), Constant::Int128(actual)) => {
                    assert_eq!(actual, expected)
                }
                (NirValue::ConstU128(expected), Constant::UInt128(actual)) => {
                    assert_eq!(actual, expected)
                }
                _ => panic!("constant kind changed: {:?}", output.constants[0]),
            }
        }
    }

    #[test]
    fn character_constants_keep_their_scalar_kind_and_all_unicode_bits() {
        for value in ['\0', '\n', '\'', '界', '🦀', '\u{10ffff}'] {
            let mut module = make_simple_nir();
            module.functions[0].blocks[0].stmts.clear();
            module.functions[0].blocks[0].terminator =
                Terminator::Return(NirValue::ConstChar(value));
            let output = try_codegen(&module).unwrap();
            assert!(
                matches!(output.constants.as_slice(), [Constant::Char(actual)] if *actual == value)
            );
            assert!(output.functions[0].instructions.iter().any(|&word| {
                let instruction = Instruction::decode(word).unwrap();
                instruction.opcode == Opcode::Load && instruction.amode == nsbc::AddrMode::Const
            }));
        }
    }

    #[test]
    fn logical_not_compares_bool_with_false_instead_of_inverting_integer_bits() {
        for value in [false, true] {
            let mut emitter = Emitter::new(0);
            Codegen::new().emit_expr(
                &NirExpr::UnaryOp(UnaryOp::Not, NirValue::ConstBool(value)),
                RESULT,
                &mut emitter,
                &mut FrameLayout::new(0),
            );
            let instructions: Vec<_> = emitter
                .instructions
                .into_iter()
                .map(|word| Instruction::decode(word).unwrap())
                .collect();
            assert_eq!(
                instructions.last(),
                Some(&Instruction::r_type(
                    Opcode::CmpEq,
                    RESULT,
                    SCRATCH,
                    SECOND_SCRATCH
                ))
            );
            assert!(
                instructions
                    .iter()
                    .any(|instruction| *instruction == Instruction::load_false(SECOND_SCRATCH))
            );
            assert!(
                !instructions
                    .iter()
                    .any(|instruction| instruction.opcode == Opcode::BitNot)
            );
        }
    }

    #[test]
    fn eager_boolean_nir_selects_bool_values_without_numeric_instructions() {
        for operator in [BinOp::And, BinOp::Or] {
            for left in [false, true] {
                for right in [false, true] {
                    let mut emitter = Emitter::new(0);
                    Codegen::new().emit_expr(
                        &NirExpr::BinOp(
                            operator,
                            NirValue::ConstBool(left),
                            NirValue::ConstBool(right),
                        ),
                        RESULT,
                        &mut emitter,
                        &mut FrameLayout::new(0),
                    );
                    let instructions: Vec<_> = emitter
                        .instructions
                        .into_iter()
                        .map(|word| Instruction::decode(word).unwrap())
                        .collect();
                    let branch = if operator == BinOp::And {
                        Instruction::jmp_if_not(SCRATCH, 2)
                    } else {
                        Instruction::jmp_if(SCRATCH, 2)
                    };
                    assert_eq!(
                        instructions[instructions.len() - 3..],
                        [
                            Instruction::mov(RESULT, SCRATCH),
                            branch,
                            Instruction::mov(RESULT, SECOND_SCRATCH),
                        ]
                    );
                    assert!(!instructions.iter().any(|instruction| matches!(
                        instruction.opcode,
                        Opcode::BitAnd | Opcode::BitOr
                    )));
                }
            }
        }
    }
}
