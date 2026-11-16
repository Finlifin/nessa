//! Deterministic human-readable text disassembler for NSBC artifacts and archives.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::io;

use nsbc::{
    AddrMode, CaptureAbi, CompiledArtifact, CompiledFunction, Constant, Instruction,
    InstructionData, Opcode, ParameterAbi,
};
use type_pool::{TypeIndex, TypeKind, TypePool};

use crate::read_artifact;

/// Decode and validate an `.nsbc` archive byte slice, then render its disassembly.
pub fn disassemble_bytes(bytes: &[u8]) -> io::Result<String> {
    let artifact = read_artifact(bytes)?;
    Ok(disassemble_artifact(&artifact))
}

/// Format a [`CompiledArtifact`] into a deterministic, human-readable text disassembly.
pub fn disassemble_artifact(artifact: &CompiledArtifact) -> String {
    let pool = &artifact.type_pool;
    let codegen = &artifact.codegen_output;

    let mut func_names = BTreeMap::new();
    for func in &codegen.functions {
        func_names.insert(func.func_id.0, resolve_str_id(func.name));
    }

    let mut builtins: Vec<_> = artifact.builtins.iter().collect();
    builtins.sort_by_key(|b| b.id);
    let builtin_names: BTreeMap<u32, &str> =
        builtins.iter().map(|b| (b.id, b.name.as_str())).collect();

    let mut out = String::new();
    let _ = writeln!(out, "; NSBC Disassembly");
    let _ = writeln!(out, "builtin_abi_version: {}", artifact.builtin_abi_version);
    match artifact.entry {
        Some(entry) => {
            let name = func_names
                .get(&entry.0)
                .map(String::as_str)
                .unwrap_or("<unknown>");
            let _ = writeln!(out, "entry: fn #{} ({name})", entry.0);
        }
        None => {
            let _ = writeln!(out, "entry: none");
        }
    }
    let _ = writeln!(out, "types: {}", pool.len());
    let _ = writeln!(out, "globals: {}", codegen.globals.len());
    let _ = writeln!(out, "constants: {}", codegen.constants.len());
    let _ = writeln!(out, "functions: {}", codegen.functions.len());
    let _ = writeln!(out, "builtins: {}", builtins.len());
    if !artifact.wasm_modules.is_empty() {
        let _ = writeln!(out, "wasm_modules: {}", artifact.wasm_modules.len());
    }

    // Builtin import table
    let _ = writeln!(out, "\nsection .builtins ({}):", builtins.len());
    for builtin in &builtins {
        let _ = writeln!(out, "  [{}] {}", builtin.id, builtin.name);
    }

    // Embedded WASM module & binding table
    let mut wasm_bindings = Vec::new();
    if !artifact.wasm_modules.is_empty() {
        let _ = writeln!(
            out,
            "\nsection .wasm ({} modules):",
            artifact.wasm_modules.len()
        );
        for (midx, wmod) in artifact.wasm_modules.iter().enumerate() {
            let _ = writeln!(
                out,
                "  module [{midx}] {:?} ({} bytes, {} bindings):",
                wmod.name,
                wmod.wasm_bytes.len(),
                wmod.bindings.len()
            );
            for binding in &wmod.bindings {
                let bid = wasm_bindings.len();
                let sig = format_type(pool, binding.signature);
                let _ = writeln!(
                    out,
                    "    binding [{bid}] {} -> {}::{} ({sig})",
                    binding.nessa_name, wmod.name, binding.wasm_export
                );
                wasm_bindings.push((
                    wmod.name.as_str(),
                    binding.nessa_name.as_str(),
                    binding.wasm_export.as_str(),
                ));
            }
        }
    }

    // Globals table
    let _ = writeln!(out, "\nsection .globals ({}):", codegen.globals.len());
    for (idx, global) in codegen.globals.iter().enumerate() {
        let mutability = if global.is_mutable { "mut" } else { "imm" };
        let ty = format_type(pool, global.type_index);
        let _ = writeln!(out, "  [{idx}] {mutability} {ty}");
    }

    // Constants table
    let mut method_constants = BTreeSet::new();
    let mut non_method_constants = BTreeSet::new();
    for word in codegen.functions.iter().flat_map(|f| &f.instructions) {
        if let Some(instr) = Instruction::decode(*word) {
            if instr.opcode == Opcode::CallMethodFar
                && let InstructionData::C { payload } = instr.data
            {
                let (_, _, const_idx) = Instruction::c_call_method(payload);
                method_constants.insert(const_idx as usize);
            }
            if let Some(const_idx) = crate::relocation::constant_reference(&instr) {
                non_method_constants.insert(const_idx as usize);
            }
        }
    }
    let _ = writeln!(out, "\nsection .constants ({}):", codegen.constants.len());
    for (idx, constant) in codegen.constants.iter().enumerate() {
        let formatted = if method_constants.contains(&idx)
            && !non_method_constants.contains(&idx)
            && let Some(id) = constant_u32(&codegen.constants, idx as u32)
        {
            let name = resolve_str_id(str_interner::StrId::from_raw(id));
            format!("MethodSym({name:?})")
        } else {
            format_constant(pool, constant)
        };
        let _ = writeln!(out, "  [{idx}] {formatted}");
    }

    // Functions
    let mut functions: Vec<&CompiledFunction> = codegen.functions.iter().collect();
    functions.sort_by_key(|f| f.func_id.0);

    let _ = writeln!(out, "\nsection .code ({} functions):", functions.len());
    let ctx = DisasmContext {
        pool,
        constants: &codegen.constants,
        func_names: &func_names,
        builtin_names: &builtin_names,
        wasm_bindings: &wasm_bindings,
    };
    for (i, func) in functions.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        format_function(&mut out, func, &ctx);
    }

    out
}

struct DisasmContext<'a> {
    pool: &'a TypePool,
    constants: &'a [Constant],
    func_names: &'a BTreeMap<u32, String>,
    builtin_names: &'a BTreeMap<u32, &'a str>,
    wasm_bindings: &'a [(&'a str, &'a str, &'a str)],
}

fn resolve_str_id(id: str_interner::StrId) -> String {
    str_interner::try_get(id).unwrap_or_else(|| format!("<str#{}>", id.as_u32()))
}

fn format_type(pool: &TypePool, index: TypeIndex) -> String {
    if index == TypeIndex::INVALID {
        return "type#invalid".to_owned();
    }
    let raw = index.as_u32();
    if (raw as usize) < pool.len()
        && let Some(name) = pool.display_name(index)
    {
        return format!("type#{raw} ({name})");
    }
    format!("type#{raw}")
}

fn format_enum_descriptor(pool: &TypePool, type_index: TypeIndex, variant: u32) -> String {
    let raw = type_index.as_u32();
    if type_index != TypeIndex::INVALID
        && (raw as usize) < pool.len()
        && let Some(canon) = pool.canonical_type(type_index)
        && let TypeKind::Enum { name, variants, .. } = &pool.get(canon).kind
    {
        let enum_name = resolve_str_id(*name);
        if let Some(info) = variants.iter().find(|v| v.tag == variant) {
            let var_name = resolve_str_id(info.name);
            return format!("Enum(type#{raw}: {enum_name}::{var_name}, tag={variant})");
        }
        return format!("Enum(type#{raw}: {enum_name}, tag={variant})");
    }
    format!("Enum(type#{raw}, tag={variant})")
}

fn format_constant(pool: &TypePool, constant: &Constant) -> String {
    match constant {
        Constant::Int(v) => format!("Int({v})"),
        Constant::UInt(v) => format!("UInt({v})"),
        Constant::Int128(v) => format!("Int128({v})"),
        Constant::UInt128(v) => format!("UInt128({v})"),
        Constant::Float(v) => {
            if v.is_nan() {
                "Float(NaN)".to_owned()
            } else if v.is_infinite() {
                if v.is_sign_positive() {
                    "Float(inf)".to_owned()
                } else {
                    "Float(-inf)".to_owned()
                }
            } else {
                format!("Float({v:?})")
            }
        }
        Constant::Str(s) => format!("Str({s:?})"),
        Constant::Char(c) => format!("Char({c:?})"),
        Constant::Type(ty) => format!("Type({})", format_type(pool, *ty)),
        Constant::Enum {
            type_index,
            variant,
        } => format_enum_descriptor(pool, *type_index, *variant),
    }
}

fn extract_slot_count(instructions: &[u32]) -> u32 {
    let Some(&first) = instructions.first() else {
        return 0;
    };
    if let Some(decoded) = Instruction::decode(first)
        && decoded.opcode == Opcode::AllocateSlots
        && let InstructionData::E { payload } = decoded.data
    {
        return payload;
    }
    0
}

fn format_capture_abi(pool: &TypePool, capture: &CaptureAbi) -> String {
    match capture {
        CaptureAbi::Value => "Value".to_owned(),
        CaptureAbi::TraitProof { view } => format!("TraitProof({})", format_type(pool, *view)),
    }
}

fn format_parameter_abi(pool: &TypePool, param: &ParameterAbi) -> String {
    match param {
        ParameterAbi::Value => "Value".to_owned(),
        ParameterAbi::Trait { view } => format!("Trait({})", format_type(pool, *view)),
        ParameterAbi::TraitSelf { view } => format!("TraitSelf({})", format_type(pool, *view)),
    }
}

fn format_function(out: &mut String, func: &CompiledFunction, ctx: &DisasmContext<'_>) {
    let fn_name = resolve_str_id(func.name);
    let slot_count = extract_slot_count(&func.instructions);
    let _ = writeln!(out, "fn #{} {fn_name}:", func.func_id.0);
    let _ = writeln!(
        out,
        "  signature: {}",
        format_type(ctx.pool, func.function_type)
    );
    let _ = writeln!(
        out,
        "  params: {}, registers: {}, slots: {slot_count}, closure: {}",
        func.param_count, func.register_count, func.is_closure
    );
    match func.display_owner {
        Some(owner) => {
            let _ = writeln!(out, "  display_owner: {}", format_type(ctx.pool, owner));
        }
        None => {
            let _ = writeln!(out, "  display_owner: none");
        }
    }
    let captures: Vec<String> = func
        .abi
        .captures
        .iter()
        .map(|c| format_capture_abi(ctx.pool, c))
        .collect();
    let params: Vec<String> = func
        .abi
        .parameters
        .iter()
        .map(|p| format_parameter_abi(ctx.pool, p))
        .collect();
    let _ = writeln!(
        out,
        "  abi: captures=[{}], params=[{}]",
        captures.join(", "),
        params.join(", ")
    );
    if !func.safepoint_pcs.is_empty() {
        let pcs: Vec<String> = func
            .safepoint_pcs
            .iter()
            .map(|pc| format!("{pc:04}"))
            .collect();
        let _ = writeln!(out, "  safepoints: [{}]", pcs.join(", "));
    }
    for (pc, &word) in func.instructions.iter().enumerate() {
        let rendered = format_instruction(pc, word, ctx);
        let _ = writeln!(out, "  {pc:04}: {rendered}");
    }
}

fn constant_u32(constants: &[Constant], idx: u32) -> Option<u32> {
    match constants.get(idx as usize)? {
        Constant::UInt(v) => u32::try_from(*v).ok(),
        Constant::Int(v) if *v >= 0 => u32::try_from(*v as u64).ok(),
        _ => None,
    }
}

fn format_const_ref(ctx: &DisasmContext<'_>, idx: u32) -> String {
    match ctx.constants.get(idx as usize) {
        Some(constant) => format!("const[{idx}] ; {}", format_constant(ctx.pool, constant)),
        None => format!("const[{idx}]"),
    }
}

fn format_fn_target(ctx: &DisasmContext<'_>, func_id: u32) -> String {
    match ctx.func_names.get(&func_id) {
        Some(name) => format!("fn #{func_id} ({name})"),
        None => format!("fn #{func_id}"),
    }
}

fn format_instruction(pc: usize, word: u32, ctx: &DisasmContext<'_>) -> String {
    let Some(instr) = Instruction::decode(word) else {
        return format!("<Invalid {word:#010x}>");
    };
    if (!instr.opcode.uses_amode() && (word & (0b11 << 22)) != 0) || instr.encode() != word {
        return format!("<Invalid {word:#010x}>");
    }

    let op = format!("{:?}", instr.opcode);
    match instr.data {
        InstructionData::R { dst, src1, src2 } => match instr.opcode {
            Opcode::Neg | Opcode::BitNot | Opcode::Mov | Opcode::Swap => {
                format!("{op} r{}, r{}", dst.0, src1.0)
            }
            _ => format!("{op} r{}, r{}, r{}", dst.0, src1.0, src2.0),
        },
        InstructionData::A { dst, base, imm12 } => {
            let idx = imm12 as u32;
            let wide = Instruction::wide_const_index(base, imm12);
            match instr.opcode {
                Opcode::Load => match instr.amode {
                    AddrMode::Imm => {
                        format!("{op} r{}, imm={}", dst.0, Instruction::sext_imm12(imm12))
                    }
                    AddrMode::Const => {
                        format!("{op} r{}, {}", dst.0, format_const_ref(ctx, idx))
                    }
                },
                Opcode::LoadConstWide => {
                    format!("{op} r{}, {}", dst.0, format_const_ref(ctx, wide))
                }
                Opcode::LoadUnit | Opcode::LoadTrue | Opcode::LoadFalse | Opcode::LoadNull => {
                    format!("{op} r{}", dst.0)
                }
                Opcode::TypeCheck
                | Opcode::TypeCast
                | Opcode::TypeCastSafe
                | Opcode::TypeAssert => {
                    format!(
                        "{op} r{}, r{}, {}",
                        dst.0,
                        base.0,
                        format_type(ctx.pool, TypeIndex::from_raw(idx))
                    )
                }
                Opcode::TraitProof | Opcode::TraitProject => {
                    format!(
                        "{op} r{}, r{}, {}",
                        dst.0,
                        base.0,
                        format_type(ctx.pool, TypeIndex::from_raw(idx))
                    )
                }
                Opcode::TraitAssert => {
                    format!(
                        "{op} data=r{}, proof=r{}, {}",
                        dst.0,
                        base.0,
                        format_type(ctx.pool, TypeIndex::from_raw(idx))
                    )
                }
                Opcode::ErrorOk | Opcode::ErrorErr => {
                    format!(
                        "{op} r{}, r{}, {}",
                        dst.0,
                        base.0,
                        format_const_ref(ctx, idx)
                    )
                }
                Opcode::ErrorIsOk | Opcode::ErrorPayload => {
                    format!("{op} r{}, r{}", dst.0, base.0)
                }
                Opcode::LoadField => {
                    format!("{op} r{}, r{}.field[{idx}]", dst.0, base.0)
                }
                Opcode::StoreField => {
                    format!("{op} r{}.field[{idx}], r{}", base.0, dst.0)
                }
                Opcode::LoadIndex => {
                    format!("{op} r{}, r{}[r{idx}]", dst.0, base.0)
                }
                Opcode::StoreIndex => {
                    format!("{op} r{}[r{idx}], r{}", base.0, dst.0)
                }
                Opcode::LoadGlobal => {
                    format!("{op} r{}, global[{idx}]", dst.0)
                }
                Opcode::StoreGlobal => {
                    format!("{op} global[{idx}], r{}", dst.0)
                }
                Opcode::LoadGlobalWide => {
                    format!("{op} r{}, global[{wide}]", dst.0)
                }
                Opcode::StoreGlobalWide => {
                    format!("{op} global[{wide}], r{}", dst.0)
                }
                Opcode::LoadSlot => {
                    format!("{op} r{}, slot[{wide}]", dst.0)
                }
                Opcode::StoreSlot => {
                    format!("{op} slot[{wide}], r{}", dst.0)
                }
                Opcode::NewObject => {
                    format!(
                        "{op} r{}, {}",
                        dst.0,
                        format_type(ctx.pool, TypeIndex::from_raw(idx))
                    )
                }
                Opcode::NewList | Opcode::NewMap => {
                    format!("{op} r{}, len={idx}", dst.0)
                }
                Opcode::NewClosure => {
                    format!(
                        "{op} r{}, {}, captures={}",
                        dst.0,
                        format_fn_target(ctx, idx),
                        base.0
                    )
                }
                Opcode::NewClosureWide => {
                    let target = constant_u32(ctx.constants, idx)
                        .map(|fid| format!(" ; {}", format_fn_target(ctx, fid)))
                        .unwrap_or_default();
                    format!("{op} r{}, const[{idx}], captures={}{target}", dst.0, base.0)
                }
                Opcode::NewEnum => {
                    format!(
                        "{op} r{}, tuple=r{}, {}",
                        dst.0,
                        base.0,
                        format_const_ref(ctx, idx)
                    )
                }
                Opcode::EnumIs => {
                    format!(
                        "{op} r{}, r{}, {}",
                        dst.0,
                        base.0,
                        format_const_ref(ctx, idx)
                    )
                }
                Opcode::EnumField => {
                    format!("{op} r{}, r{}.field[{idx}]", dst.0, base.0)
                }
                _ => format!("{op} r{}, r{}, {idx}", dst.0, base.0),
            }
        }
        InstructionData::J { cond, offset } => {
            let target = pc as i64 + offset as i64;
            let target_str = if target >= 0 {
                format!("{target:04}")
            } else {
                format!("{target}")
            };
            match instr.opcode {
                Opcode::Jmp | Opcode::JmpFar => {
                    format!("{op} {offset:+} -> {target_str}")
                }
                _ => {
                    format!("{op} r{}, {offset:+} -> {target_str}", cond.0)
                }
            }
        }
        InstructionData::C { payload } => match instr.opcode {
            Opcode::Call => {
                let (argc, fid) = Instruction::c_arg_count_func_id(payload);
                format!("{op} {}, argc={argc}", format_fn_target(ctx, fid))
            }
            Opcode::CallFar => {
                let (argc, const_idx) = Instruction::c_arg_count_func_id(payload);
                let target = constant_u32(ctx.constants, const_idx)
                    .map(|fid| format!(" ; {}", format_fn_target(ctx, fid)))
                    .unwrap_or_default();
                format!("{op} const[{const_idx}], argc={argc}{target}")
            }
            Opcode::CallWasm => {
                let (argc, bid) = Instruction::c_arg_count_func_id(payload);
                match ctx.wasm_bindings.get(bid as usize) {
                    Some((mname, nname, ename)) => {
                        format!("{op} wasm[{bid}] ({nname} -> {mname}::{ename}), argc={argc}")
                    }
                    None => format!("{op} wasm[{bid}], argc={argc}"),
                }
            }
            Opcode::CallBuiltin => {
                let (argc, bid) = Instruction::c_arg_count_func_id(payload);
                match ctx.builtin_names.get(&bid) {
                    Some(name) => format!("{op} builtin[{bid}] ({name}), argc={argc}"),
                    None => format!("{op} builtin[{bid}], argc={argc}"),
                }
            }
            Opcode::CallIndirect | Opcode::CallIndirectProof => {
                let (argc, closure) = Instruction::c_call_indirect(payload);
                format!("{op} r{}, argc={argc}", closure.0)
            }
            Opcode::CallMethod => {
                let (argc, recv, method_id) = Instruction::c_call_method(payload);
                let name = resolve_str_id(str_interner::StrId::from_raw(method_id));
                format!("{op} recv=r{}, method={name:?}, argc={argc}", recv.0)
            }
            Opcode::CallMethodFar => {
                let (argc, recv, const_idx) = Instruction::c_call_method(payload);
                let name = constant_u32(ctx.constants, const_idx)
                    .map(|id| resolve_str_id(str_interner::StrId::from_raw(id)))
                    .map(|s| format!(" ; method={s:?}"))
                    .unwrap_or_default();
                format!(
                    "{op} recv=r{}, const[{const_idx}], argc={argc}{name}",
                    recv.0
                )
            }
            Opcode::TraitCall => {
                let (argc, proof, slot) = Instruction::c_call_method(payload);
                format!("{op} proof=r{}, slot={slot}, argc={argc}", proof.0)
            }
            Opcode::Return => {
                let src = Instruction::c_return_reg(payload);
                format!("{op} r{}", src.0)
            }
            Opcode::ReturnUnit => op,
            _ => format!("{op} payload={payload:#x}"),
        },
        InstructionData::E { payload } => match instr.opcode {
            Opcode::AllocateSlots => format!("{op} slots={payload}"),
            Opcode::EffectCallDyn => {
                let argc = payload >> 17;
                let effect_ty = TypeIndex::from_raw(payload & 0x1FFFF);
                format!(
                    "{op} effect={}, argc={argc}",
                    format_type(ctx.pool, effect_ty)
                )
            }
            Opcode::PushHandlerClosure => {
                let closure = (payload >> 17) & 0x1F;
                let effect_ty = TypeIndex::from_raw(payload & 0x1FFFF);
                format!(
                    "{op} closure=r{closure}, effect={}",
                    format_type(ctx.pool, effect_ty)
                )
            }
            Opcode::PushCapturingHandler => {
                let closure = (payload >> 17) & 0x1F;
                let const_idx = payload & 0x1FFFF;
                format!(
                    "{op} closure=r{closure}, {}",
                    format_const_ref(ctx, const_idx)
                )
            }
            Opcode::ResetClosure => {
                let (body, handlers) = Instruction::e_control_regs(payload);
                format!("{op} body=r{}, handlers={}", body.0, handlers.0)
            }
            Opcode::ResumeContinuation | Opcode::ResumeContinuationOnce => {
                let (cont, val) = Instruction::e_control_regs(payload);
                format!("{op} cont=r{}, val=r{}", cont.0, val.0)
            }
            Opcode::AwaitFuture => {
                let (dst, src) = Instruction::e_control_regs(payload);
                format!("{op} r{}, r{}", dst.0, src.0)
            }
            Opcode::PopHandler
            | Opcode::AwaitPopHandler
            | Opcode::Safepoint
            | Opcode::Unreachable
            | Opcode::Nop
            | Opcode::MatchFail => op,
            _ => format!("{op} payload={payload:#x}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nsbc::{BuiltinImport, CodegenOutput, FuncId, FunctionAbi, GlobalInfo, Reg};

    #[test]
    fn disassembles_header_constants_globals_builtins_and_instructions_deterministically() {
        let pool = TypePool::with_intrinsics();
        let i64_ty = type_pool::Intrinsic::I64.type_index();
        let unit_ty = type_pool::Intrinsic::Unit.type_index();

        let helper_name = str_interner::intern("helper");
        let main_name = str_interner::intern("main");

        let helper = CompiledFunction {
            display_owner: None,
            func_id: FuncId(0),
            name: helper_name,
            instructions: vec![
                Instruction::allocate_slots(1).encode(),
                Instruction::store_slot(0, Reg(0)).encode(),
                Instruction::load_slot(Reg(1), 0).encode(),
                Instruction::load_const(Reg(2), 0).encode(),
                Instruction::add(Reg(0), Reg(1), Reg(2)).encode(),
                Instruction::ret(Reg(0)).encode(),
            ],
            register_count: 3,
            param_count: 1,
            is_closure: false,
            function_type: TypeIndex::INVALID,
            abi: FunctionAbi::value_only(1),
            safepoint_pcs: Vec::new(),
        };

        let main_fn = CompiledFunction {
            display_owner: None,
            func_id: FuncId(1),
            name: main_name,
            instructions: vec![
                Instruction::load_imm(Reg(0), 40).encode(),
                Instruction::call(0, 1).encode(),
                Instruction::store_global(0, Reg(0)).encode(),
                Instruction::load_global(Reg(0), 0).encode(),
                Instruction::call_builtin(1, 1).encode(),
                Instruction::jmp_if_not(Reg(0), 1).encode(),
                Instruction::safepoint().encode(),
                Instruction::return_unit().encode(),
            ],
            register_count: 2,
            param_count: 0,
            is_closure: false,
            function_type: unit_ty,
            abi: FunctionAbi::value_only(0),
            safepoint_pcs: vec![6],
        };

        let artifact = CompiledArtifact {
            codegen_output: CodegenOutput {
                functions: vec![main_fn, helper],
                constants: vec![
                    Constant::Int(2),
                    Constant::Str("hello\nworld".into()),
                    Constant::Char('λ'),
                    Constant::Float(3.5),
                    Constant::Type(i64_ty),
                ],
                globals: vec![GlobalInfo {
                    type_index: i64_ty,
                    is_mutable: true,
                }],
                method_call_scopes: Vec::new(),
            },
            type_pool: pool,
            entry: Some(FuncId(1)),
            builtin_abi_version: 9,
            builtins: vec![BuiltinImport {
                id: 1,
                name: "println".into(),
            }],
            wasm_modules: Vec::new(),
        };

        let first = disassemble_artifact(&artifact);
        let second = disassemble_artifact(&artifact);
        assert_eq!(first, second);
        assert!(first.contains("builtin_abi_version: 9"), "{first}");
        assert!(first.contains("entry: fn #1 (main)"), "{first}");
        assert!(first.contains("[1] println"), "{first}");
        assert!(first.contains("[0] Int(2)"), "{first}");
        assert!(first.contains("[1] Str(\"hello\\nworld\")"), "{first}");
        assert!(first.contains("[2] Char('λ')"), "{first}");
        assert!(first.contains("[3] Float(3.5)"), "{first}");
        assert!(first.contains("[4] Type(type#"), "{first}");
        assert!(first.contains("fn #0 helper:"), "{first}");
        assert!(
            first.contains("params: 1, registers: 3, slots: 1, closure: false"),
            "{first}"
        );
        assert!(first.contains("0000: AllocateSlots slots=1"), "{first}");
        assert!(first.contains("0001: StoreSlot slot[0], r0"), "{first}");
        assert!(first.contains("0002: LoadSlot r1, slot[0]"), "{first}");
        assert!(
            first.contains("0003: Load r2, const[0] ; Int(2)"),
            "{first}"
        );
        assert!(first.contains("0004: Add r0, r1, r2"), "{first}");
        assert!(first.contains("0005: Return r0"), "{first}");
        assert!(first.contains("fn #1 main:"), "{first}");
        assert!(
            first.contains("0001: Call fn #0 (helper), argc=1"),
            "{first}"
        );
        assert!(
            first.contains("0004: CallBuiltin builtin[1] (println), argc=1"),
            "{first}"
        );
        assert!(first.contains("0005: JmpIfNot r0, +1 -> 0006"), "{first}");
    }

    #[test]
    fn disassemble_bytes_rejects_corrupt_archives_and_roundtrips_valid_archives() {
        let mut pool = TypePool::with_intrinsics();
        let fn_ty = pool.intern_structural(TypeKind::Function {
            params: Vec::new(),
            ret: type_pool::Intrinsic::Unit.type_index(),
        });
        pool.finalize_type_identities(type_pool::TypeIdentityInput {
            schema: 1,
            packages: vec![type_pool::PackageTypeContext {
                identity_schema: 1,
                identity: [0x22; 16],
                qualified_name: "org.test/disasm".into(),
                version: "1.0.0".into(),
            }],
            declarations: Vec::new(),
        })
        .unwrap();
        let main_fn = CompiledFunction {
            display_owner: None,
            func_id: FuncId(0),
            name: str_interner::intern("main"),
            instructions: vec![Instruction::return_unit().encode()],
            register_count: 1,
            param_count: 0,
            is_closure: false,
            function_type: fn_ty,
            abi: FunctionAbi::value_only(0),
            safepoint_pcs: Vec::new(),
        };
        let artifact = CompiledArtifact {
            codegen_output: CodegenOutput {
                functions: vec![main_fn],
                constants: vec![Constant::Int(99)],
                globals: Vec::new(),
                method_call_scopes: Vec::new(),
            },
            type_pool: pool,
            entry: Some(FuncId(0)),
            builtin_abi_version: 9,
            builtins: Vec::new(),
            wasm_modules: Vec::new(),
        };
        let bytes = crate::write_artifact(&artifact).unwrap();
        let text = disassemble_bytes(&bytes).unwrap();
        assert!(text.contains("entry: fn #0 (main)"), "{text}");
        assert!(text.contains("[0] Int(99)"), "{text}");
        assert!(text.contains("0000: ReturnUnit"), "{text}");

        assert!(disassemble_bytes(b"not an nsbc archive").is_err());
        let mut corrupted = bytes.clone();
        *corrupted.last_mut().unwrap() ^= 0xFF;
        assert!(disassemble_bytes(&corrupted).is_err());
    }

    #[test]
    fn call_method_far_constants_render_method_sym_instead_of_process_local_str_id() {
        let mut pool = TypePool::with_intrinsics();
        let fn_ty = pool.intern_structural(TypeKind::Function {
            params: Vec::new(),
            ret: type_pool::Intrinsic::Unit.type_index(),
        });
        let method_sym = str_interner::intern("custom_far_method_name");
        let main_fn = CompiledFunction {
            display_owner: None,
            func_id: FuncId(0),
            name: str_interner::intern("main"),
            instructions: vec![
                Instruction::load_unit(nsbc::Reg(0)).encode(),
                Instruction::call_method_far(nsbc::Reg(0), 0, 0).encode(),
                Instruction::return_unit().encode(),
            ],
            register_count: 2,
            param_count: 0,
            is_closure: false,
            function_type: fn_ty,
            abi: FunctionAbi::value_only(0),
            safepoint_pcs: Vec::new(),
        };
        let artifact = CompiledArtifact {
            codegen_output: CodegenOutput {
                functions: vec![main_fn],
                constants: vec![Constant::UInt(u64::from(method_sym.as_u32()))],
                globals: Vec::new(),
                method_call_scopes: Vec::new(),
            },
            type_pool: pool,
            entry: Some(FuncId(0)),
            builtin_abi_version: 9,
            builtins: Vec::new(),
            wasm_modules: Vec::new(),
        };
        let text = disassemble_artifact(&artifact);
        assert!(
            text.contains("[0] MethodSym(\"custom_far_method_name\")"),
            "{text}"
        );
        assert!(
            !text.contains(&format!("UInt({})", method_sym.as_u32())),
            "{text}"
        );
    }
}
