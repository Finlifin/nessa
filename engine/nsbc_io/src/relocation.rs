//! Relocate method names without treating arbitrary UInt values as string IDs.
//! Every disk method call uses a dedicated low constant slot. Reordering the
//! other constants widens narrow loads in place, keeping PCs and jumps stable.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use nsbc::{AddrMode, CodegenOutput, Constant, Instruction, InstructionData, Opcode};
use str_interner::StrId;

pub(super) struct RelocatedCode {
    pub instructions: Vec<Vec<u32>>,
    pub constants: Vec<Constant>,
    pub methods: Vec<(u32, String)>,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn capacity(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

fn method_name(instruction: &Instruction, constants: &[Constant]) -> io::Result<Option<String>> {
    if !matches!(
        instruction.opcode,
        Opcode::CallMethod | Opcode::CallMethodFar
    ) {
        return Ok(None);
    }
    let InstructionData::C { payload } = instruction.data else {
        return Err(invalid("invalid method instruction format"));
    };
    let (_, _, index) = Instruction::c_call_method(payload);
    let id = if instruction.opcode == Opcode::CallMethod {
        index
    } else {
        let value = match constants.get(index as usize) {
            Some(Constant::UInt(value)) => *value,
            Some(Constant::Int(value)) if *value >= 0 => *value as u64,
            _ => return Err(invalid("invalid method name constant")),
        };
        u32::try_from(value).map_err(|_| invalid("method name index overflow"))?
    };
    str_interner::try_get(StrId::from_raw(id))
        .map(Some)
        .ok_or_else(|| invalid("invalid interned method name"))
}

fn constant_reference(instruction: &Instruction) -> Option<u32> {
    match instruction.data {
        InstructionData::A { imm12, .. }
            if instruction.opcode == Opcode::Load && instruction.amode == AddrMode::Const
                || matches!(
                    instruction.opcode,
                    Opcode::NewClosureWide
                        | Opcode::NewEnum
                        | Opcode::EnumIs
                        | Opcode::ErrorOk
                        | Opcode::ErrorErr
                ) =>
        {
            Some(imm12 as u32)
        }
        InstructionData::A { base, imm12, .. } if instruction.opcode == Opcode::LoadConstWide => {
            Some(Instruction::wide_const_index(base, imm12))
        }
        InstructionData::C { payload } if instruction.opcode == Opcode::CallFar => {
            Some(payload & 0x3fff)
        }
        InstructionData::E { payload } if instruction.opcode == Opcode::PushCapturingHandler => {
            Some(payload & 0x1ffff)
        }
        InstructionData::E { payload } if instruction.opcode == Opcode::PushHandlerWide => {
            Some(payload)
        }
        _ => None,
    }
}

pub(super) fn encode(output: &CodegenOutput) -> io::Result<RelocatedCode> {
    let mut narrow = BTreeSet::new();
    let mut calls = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut method_constants = BTreeSet::new();
    let mut other_constants = BTreeSet::new();
    for word in output
        .functions
        .iter()
        .flat_map(|function| &function.instructions)
    {
        let instruction =
            Instruction::decode(*word).ok_or_else(|| invalid("invalid instruction"))?;
        if let Some(name) = method_name(&instruction, &output.constants)? {
            names.insert(name);
        }
        if instruction.opcode == Opcode::CallMethodFar
            && let InstructionData::C { payload } = instruction.data
        {
            method_constants.insert((payload & 0xfff) as usize);
        }
        if let Some(index) = constant_reference(&instruction) {
            other_constants.insert(index as usize);
        }
        match instruction.data {
            InstructionData::A { imm12, .. }
                if matches!(
                    instruction.opcode,
                    Opcode::NewClosureWide
                        | Opcode::NewEnum
                        | Opcode::EnumIs
                        | Opcode::ErrorOk
                        | Opcode::ErrorErr
                ) =>
            {
                narrow.insert(imm12 as usize);
            }
            InstructionData::C { payload } if instruction.opcode == Opcode::CallFar => {
                calls.insert((payload & 0x3fff) as usize);
            }
            _ => {}
        }
    }
    if narrow.len() + names.len() > 4096 {
        return Err(capacity(
            "method names and narrow metadata exceed the 12-bit constant capacity",
        ));
    }
    if narrow.len() + names.len() + calls.difference(&narrow).count() > 16384 {
        return Err(capacity(
            "relocated calls exceed the 14-bit constant capacity",
        ));
    }
    let mut constants = Vec::new();
    let mut indices = vec![None; output.constants.len()];
    let mut append = |old: usize, constants: &mut Vec<Constant>| -> io::Result<()> {
        let value = output
            .constants
            .get(old)
            .ok_or_else(|| invalid("invalid constant index"))?;
        indices[old] = Some(constants.len() as u32);
        constants.push(value.clone());
        Ok(())
    };
    for &old in &narrow {
        append(old, &mut constants)?;
    }
    let mut methods = Vec::new();
    let mut method_slots = BTreeMap::new();
    for name in names {
        let slot = constants.len() as u32;
        // A disk placeholder, overwritten only via the explicit method table.
        constants.push(Constant::UInt(0));
        method_slots.insert(name.clone(), slot);
        methods.push((slot, name));
    }
    for old in calls.difference(&narrow) {
        append(*old, &mut constants)?;
    }
    for old in 0..output.constants.len() {
        if !narrow.contains(&old)
            && !calls.contains(&old)
            && (!method_constants.contains(&old) || other_constants.contains(&old))
        {
            append(old, &mut constants)?;
        }
    }
    let relocated_index = |old: u32| {
        indices
            .get(old as usize)
            .copied()
            .flatten()
            .ok_or_else(|| invalid("invalid constant index during relocation"))
    };
    let mut functions = Vec::new();
    for function in &output.functions {
        let mut words = Vec::with_capacity(function.instructions.len());
        for &word in &function.instructions {
            let mut instruction =
                Instruction::decode(word).ok_or_else(|| invalid("invalid instruction"))?;
            let original = instruction;
            if let Some(name) = method_name(&instruction, &output.constants)? {
                let InstructionData::C { payload } = instruction.data else {
                    return Err(invalid("invalid method instruction format"));
                };
                let (count, receiver, _) = Instruction::c_call_method(payload);
                instruction =
                    Instruction::call_method_far(receiver, method_slots[&name] as u16, count);
            } else {
                match instruction.data {
                    InstructionData::A { dst, base, imm12 }
                        if instruction.opcode == Opcode::Load
                            && instruction.amode == AddrMode::Const
                            || instruction.opcode == Opcode::LoadConstWide =>
                    {
                        let old = if instruction.opcode == Opcode::LoadConstWide {
                            Instruction::wide_const_index(base, imm12)
                        } else {
                            imm12 as u32
                        };
                        let new = relocated_index(old)?;
                        if new >= (1 << 17) {
                            return Err(capacity(
                                "relocated loads exceed the 17-bit constant capacity",
                            ));
                        }
                        instruction = if new < 4096 {
                            Instruction::load_const(dst, new as u16)
                        } else {
                            Instruction::load_const_wide(dst, new)
                        };
                    }
                    InstructionData::A { dst, base, imm12 }
                        if matches!(
                            instruction.opcode,
                            Opcode::NewClosureWide
                                | Opcode::NewEnum
                                | Opcode::EnumIs
                                | Opcode::ErrorOk
                                | Opcode::ErrorErr
                        ) =>
                    {
                        let new = relocated_index(imm12 as u32)? as u16;
                        instruction = match instruction.opcode {
                            Opcode::NewClosureWide => {
                                Instruction::new_closure_wide(dst, new, base.0)
                            }
                            Opcode::NewEnum => Instruction::new_enum(dst, base, new),
                            Opcode::ErrorOk | Opcode::ErrorErr => Instruction::a_type(
                                instruction.opcode,
                                AddrMode::Imm,
                                dst,
                                base,
                                new,
                            ),
                            _ => Instruction::enum_is(dst, base, new),
                        };
                    }
                    InstructionData::C { payload } if instruction.opcode == Opcode::CallFar => {
                        let (count, old) = Instruction::c_arg_count_func_id(payload);
                        instruction = Instruction::call_far(relocated_index(old)? as u16, count);
                    }
                    InstructionData::E { payload }
                        if instruction.opcode == Opcode::PushCapturingHandler =>
                    {
                        let new = relocated_index(payload & 0x1ffff)?;
                        if new >= (1 << 17) {
                            return Err(capacity(
                                "relocated handler metadata exceeds the 17-bit constant capacity",
                            ));
                        }
                        instruction = Instruction::e_type(
                            Opcode::PushCapturingHandler,
                            (payload & !0x1ffff) | new,
                        );
                    }
                    InstructionData::E { payload }
                        if instruction.opcode == Opcode::PushHandlerWide =>
                    {
                        let new = relocated_index(payload)?;
                        if new >= (1 << 22) {
                            return Err(capacity(
                                "relocated handler metadata exceeds the 22-bit constant capacity",
                            ));
                        }
                        instruction = Instruction::e_type(Opcode::PushHandlerWide, new);
                    }
                    _ => {}
                }
            }
            // In particular, preserve all 22 bits of an existing JmpFar word;
            // unrelated opcodes must never change during constant relocation.
            words.push(if instruction == original {
                word
            } else {
                instruction.encode()
            });
        }
        functions.push(words);
    }
    Ok(RelocatedCode {
        instructions: functions,
        constants,
        methods,
    })
}

/// Patch only dedicated method slots after checking their exclusive use. Every
/// other constant, even a UInt containing the same numeric ID, keeps its value.
pub(super) fn restore(output: &mut CodegenOutput, methods: &[(u32, String)]) -> io::Result<()> {
    let corrupt = |message| io::Error::new(io::ErrorKind::InvalidData, message);
    let mut slots = BTreeMap::new();
    let mut names = BTreeSet::new();
    for (slot, name) in methods {
        if *slot >= 4096 || slots.insert(*slot, name).is_some() || !names.insert(name) {
            return Err(corrupt("invalid or duplicate method relocation"));
        }
        if !matches!(
            output.constants.get(*slot as usize),
            Some(Constant::UInt(0))
        ) {
            return Err(corrupt(
                "method relocation requires a dedicated UInt zero placeholder",
            ));
        }
    }
    let mut referenced = BTreeSet::new();
    for word in output
        .functions
        .iter()
        .flat_map(|function| &function.instructions)
    {
        let instruction =
            Instruction::decode(*word).ok_or_else(|| corrupt("invalid instruction"))?;
        match instruction.opcode {
            Opcode::CallMethod => {
                return Err(corrupt(
                    "artifact CODE must use relocatable far method calls",
                ));
            }
            Opcode::CallMethodFar => {
                let InstructionData::C { payload } = instruction.data else {
                    return Err(corrupt("invalid method instruction format"));
                };
                let slot = payload & 0xfff;
                if !slots.contains_key(&slot) {
                    return Err(corrupt("method call has no relocation record"));
                }
                referenced.insert(slot);
            }
            _ => {}
        }
        if constant_reference(&instruction).is_some_and(|slot| slots.contains_key(&slot)) {
            return Err(corrupt(
                "method relocation slot is also used as a numeric constant or metadata",
            ));
        }
    }
    if referenced.len() != slots.len() {
        return Err(corrupt("unused method relocation record"));
    }
    for (slot, name) in slots {
        output.constants[slot as usize] =
            Constant::UInt(str_interner::intern(name).as_u32() as u64);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nsbc::{CompiledFunction, FuncId, Reg};
    use type_pool::TypeIndex;

    #[test]
    fn enum_descriptors_relocate_with_closure_and_method_metadata() {
        let method = str_interner::intern("enum_metadata_method");
        let output = CodegenOutput {
            method_call_scopes: None,
            scope_coverage: nsbc::ScopeCoverage::Calls,
            globals: vec![],
            constants: vec![
                Constant::UInt(77),
                Constant::UInt(0),
                Constant::Enum {
                    type_index: TypeIndex::from_raw(42),
                    variant: 9,
                },
            ],
            functions: vec![CompiledFunction {
                display_owner: None,
                abi: None,
                func_id: FuncId(0),
                name: str_interner::intern("entry"),
                register_count: 32,
                param_count: 0,
                is_closure: false,
                function_type: TypeIndex::INVALID,
                safepoint_pcs: vec![],
                instructions: [
                    Instruction::new_enum(Reg(0), Reg(1), 2),
                    Instruction::enum_is(Reg(2), Reg(0), 2),
                    Instruction::new_closure_wide(Reg(3), 1, 0),
                    Instruction::call_method(Reg(0), method.as_u32(), 0),
                    Instruction::load_const(Reg(4), 0),
                    Instruction::match_fail(),
                ]
                .into_iter()
                .map(Instruction::encode)
                .collect(),
            }],
        };
        let relocated = encode(&output).unwrap();
        assert!(
            matches!(relocated.constants[1], Constant::Enum {type_index,variant:9} if type_index.as_u32()==42)
        );
        assert!(matches!(relocated.constants[3], Constant::UInt(77)));
        let words = &relocated.instructions[0];
        for (index, opcode) in [(0, Opcode::NewEnum), (1, Opcode::EnumIs)] {
            let instruction = Instruction::decode(words[index]).unwrap();
            assert_eq!(instruction.opcode, opcode);
            assert!(matches!(
                instruction.data,
                InstructionData::A { imm12: 1, .. }
            ));
        }
        assert!(matches!(
            Instruction::decode(words[2]).unwrap().data,
            InstructionData::A { imm12: 0, .. }
        ));
        assert_eq!(
            Instruction::decode(words[3]).unwrap().opcode,
            Opcode::CallMethodFar
        );
        assert!(matches!(
            Instruction::decode(words[4]).unwrap().data,
            InstructionData::A { imm12: 3, .. }
        ));
        assert_eq!(words[5], Instruction::match_fail().encode());
    }

    #[test]
    fn trait_proof_types_and_slots_are_not_method_or_constant_relocations() {
        let method = str_interner::intern("trait_relocation_method");
        let instructions = [
            Instruction::a_type(Opcode::TraitProof, AddrMode::Imm, Reg(3), Reg(2), 4095),
            Instruction::a_type(Opcode::TraitAssert, AddrMode::Imm, Reg(2), Reg(3), 4094),
            Instruction::a_type(Opcode::TraitProject, AddrMode::Imm, Reg(4), Reg(3), 4093),
            Instruction::c_type(Opcode::TraitCall, (2 << 17) | (4 << 12) | 4092),
            Instruction::call_indirect_proof(Reg(5), 2),
        ]
        .map(Instruction::encode);
        let mut words = instructions.to_vec();
        words.push(Instruction::new_closure_wide(Reg(6), 1, 0).encode());
        words.push(Instruction::call_method(Reg(2), method.as_u32(), 0).encode());
        words.push(Instruction::load_const(Reg(7), 0).encode());
        let mut output = CodegenOutput {
            method_call_scopes: None,
            scope_coverage: nsbc::ScopeCoverage::Calls,
            globals: vec![],
            constants: vec![Constant::UInt(77), Constant::UInt(0)],
            functions: vec![CompiledFunction {
                display_owner: None,
                abi: None,
                func_id: FuncId(0),
                name: str_interner::intern("proof_relocation"),
                register_count: 32,
                param_count: 0,
                is_closure: false,
                function_type: TypeIndex::INVALID,
                safepoint_pcs: vec![],
                instructions: words,
            }],
        };
        let relocated = encode(&output).unwrap();
        assert_eq!(
            &relocated.instructions[0][..instructions.len()],
            &instructions
        );
        assert_eq!(relocated.methods.len(), 1);
        assert!(matches!(relocated.constants[2], Constant::UInt(77)));
        output.constants = relocated.constants;
        output.functions[0].instructions = relocated.instructions[0].clone();
        restore(&mut output, &relocated.methods).unwrap();
        assert_eq!(
            &output.functions[0].instructions[..instructions.len()],
            &instructions
        );
    }
}
