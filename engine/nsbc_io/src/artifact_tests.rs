use std::collections::BTreeSet;

use nsbc::{
    AddrMode, Constant, Instruction, InstructionData, Opcode, Reg, instruction_requires_scope,
};
use sha2::{Digest, Sha256};
use type_pool::{
    IdentityPathSegment, Intrinsic, NominalTypeProvenance, PackageTypeContext, TypeId,
    TypeIdentityInput, TypeIndex, TypeInfo, TypeKind, TypePool,
};

use super::*;

fn finalize_pool(pool: &mut TypePool) {
    let snapshot = pool.snapshot();
    let structural: BTreeSet<_> = snapshot.structural_types.iter().copied().collect();
    let mut declarations = Vec::new();
    for (index, info) in snapshot.types.iter().enumerate() {
        let ty = TypeIndex::from_raw(index as u32);
        if pool.is_reserved_type(ty) {
            continue;
        }
        let path = match &info.kind {
            TypeKind::Struct { name, .. }
            | TypeKind::Newtype { name, .. }
            | TypeKind::Trait { name, .. }
            | TypeKind::Module { name } => Some(vec![
                IdentityPathSegment::Lexical {
                    kind: 0,
                    ordinal: index as u32,
                },
                IdentityPathSegment::Named(str_interner::get(*name)),
            ]),
            TypeKind::Enum { name, .. } if !structural.contains(&ty) => Some(vec![
                IdentityPathSegment::Lexical {
                    kind: 0,
                    ordinal: index as u32,
                },
                IdentityPathSegment::Named(str_interner::get(*name)),
            ]),
            TypeKind::Effect { .. } if !structural.contains(&ty) => {
                Some(vec![IdentityPathSegment::Lexical {
                    kind: 6,
                    ordinal: index as u32,
                }])
            }
            _ => None,
        };
        if let Some(path) = path {
            declarations.push(NominalTypeProvenance {
                type_index: ty,
                package: 0,
                path,
                last_stable_version: "1.0.0".into(),
            });
        }
    }
    pool.finalize_type_identities(TypeIdentityInput {
        schema: 1,
        packages: vec![PackageTypeContext {
            identity_schema: 1,
            identity: [0x22; 16],
            qualified_name: "org.test/artifact".into(),
            version: "1.0.0".into(),
        }],
        declarations,
    })
    .unwrap();
}

fn collect_scopes(functions: &[CompiledFunction], scope: u32) -> Vec<MethodCallScope> {
    let mut scopes = Vec::new();
    for function in functions {
        for (pc, &word) in function.instructions.iter().enumerate() {
            if let Some(inst) = Instruction::decode(word)
                && instruction_requires_scope(inst.opcode)
            {
                scopes.push(MethodCallScope {
                    func_id: function.func_id,
                    pc: pc as u32,
                    scope,
                });
            }
        }
    }
    scopes
}

fn fixture() -> CompiledArtifact {
    let mut pool = TypePool::with_intrinsics();
    pool.install_scopes(vec![
        type_pool::ScopeContext {
            parent: None,
            package: 0,
            assoc_type: None,
        },
        type_pool::ScopeContext {
            parent: Some(0),
            package: 0,
            assoc_type: Some(Intrinsic::I64.type_index()),
        },
    ])
    .unwrap();
    let signature = pool.intern_structural(TypeKind::Function {
        params: vec![],
        ret: Intrinsic::I64.type_index(),
    });
    let optional = pool.intern_structural(TypeKind::Optional {
        inner: Intrinsic::I64.type_index(),
    });
    let alias = pool.register(TypeInfo {
        kind: TypeKind::Typealias {
            name: str_interner::intern("PersistedInteger"),
            target: Intrinsic::I64.type_index(),
        },
        type_id: TypeId::ZERO,
        size: 0,
        align: 0,
    });
    finalize_pool(&mut pool);
    let functions = vec![
        CompiledFunction {
            display_owner: None,
            abi: FunctionAbi::default(),
            func_id: FuncId(0),
            name: str_interner::intern("artifact_entry"),
            instructions: [
                Instruction::allocate_slots(1),
                Instruction::safepoint(),
                Instruction::load_const(Reg(0), 0),
                Instruction::a_type(Opcode::StoreGlobal, AddrMode::Imm, Reg(0), Reg(0), 0),
                Instruction::a_type(Opcode::LoadGlobal, AddrMode::Imm, Reg(1), Reg(0), 0),
                Instruction::ret(Reg(1)),
            ]
            .iter()
            .map(|instruction| instruction.encode())
            .collect(),
            register_count: 2,
            param_count: 0,
            is_closure: false,
            function_type: signature,
            safepoint_pcs: vec![1],
        },
        CompiledFunction {
            display_owner: None,
            abi: FunctionAbi::default(),
            func_id: FuncId(1),
            name: str_interner::intern("artifact_closure"),
            instructions: vec![
                Instruction::load_imm(Reg(0), 7).encode(),
                Instruction::ret(Reg(0)).encode(),
            ],
            register_count: 1,
            param_count: 0,
            is_closure: true,
            function_type: signature,
            safepoint_pcs: vec![],
        },
    ];
    let method_call_scopes = collect_scopes(&functions, 0);
    CompiledArtifact {
        codegen_output: CodegenOutput {
            method_call_scopes,
            globals: vec![GlobalInfo {
                type_index: Intrinsic::I64.type_index(),
                is_mutable: false,
            }],
            constants: vec![
                Constant::Int(42),
                Constant::Str("中文\0persistent".into()),
                Constant::Int128(i128::MIN),
                Constant::UInt128(u128::MAX),
                Constant::Type(optional),
                Constant::Type(alias),
                Constant::Float(f64::from_bits(0x7ff8_0000_0000_0123)),
            ],
            functions,
        },
        type_pool: pool,
        entry: Some(FuncId(0)),
        builtin_abi_version: 1,
        builtins: vec![],
    }
}

fn rewrite_section(
    mut bytes: &[u8],
    kind: SectionKind,
    edit: impl FnOnce(&mut Vec<u8>),
) -> Vec<u8> {
    let archive = Archive::read_from(&mut bytes).unwrap();
    let mut sections = archive.sections;
    edit(
        &mut sections
            .iter_mut()
            .find(|(entry, _)| entry.kind == kind as u8)
            .unwrap()
            .1,
    );
    let mut writer = ArchiveWriter::new();
    for (entry, data) in sections {
        writer.add_raw_section(SectionKind::from_u8(entry.kind).unwrap(), data);
    }
    let mut bytes = Vec::new();
    writer.write_to(&mut bytes).unwrap();
    bytes
}

fn rejects(bytes: &[u8], message: &str) {
    let error = read_artifact(bytes).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains(message), "{error}");
}

fn methods_offset(metadata: &[u8]) -> usize {
    let mut reader = Reader {
        remaining: metadata,
    };
    reader.take(20).unwrap();
    reader.blob().unwrap();
    let globals = reader.u32().unwrap() as usize;
    reader.take(globals * 5).unwrap();
    let functions = reader.u32().unwrap();
    for _ in 0..functions {
        reader.u32().unwrap();
        reader.string().unwrap();
        reader.u32().unwrap();
    }
    metadata.len() - reader.remaining.len()
}

fn scopes_offset(metadata: &[u8]) -> usize {
    let mut reader = Reader {
        remaining: &metadata[methods_offset(metadata)..],
    };
    let methods = reader.u32().unwrap();
    for _ in 0..methods {
        reader.u32().unwrap();
        reader.string().unwrap();
    }
    let builtins = reader.u32().unwrap();
    for _ in 0..builtins {
        reader.u32().unwrap();
        reader.string().unwrap();
    }
    metadata.len() - reader.remaining.len()
}

fn abis_offset(metadata: &[u8]) -> usize {
    let start = scopes_offset(metadata);
    let mut reader = Reader {
        remaining: &metadata[start..],
    };
    let scopes = reader.u32().unwrap() as usize;
    reader.take(scopes * 12).unwrap();
    metadata.len() - reader.remaining.len()
}

#[test]
fn complete_artifacts_roundtrip_every_runtime_metadata_component() {
    let artifact = fixture();
    let bytes = write_artifact(&artifact).unwrap();
    let archive = Archive::read_from(&mut bytes.as_slice()).unwrap();
    assert_eq!(archive.header.version, nsbc::VERSION);
    assert_eq!(archive.sections.len(), 4);
    assert_ne!(archive.header.checksum, [0; 32]);
    let metadata = archive.find_section(SectionKind::Metadata).unwrap();
    assert_eq!(&metadata[..8], b"NSAM\x06\x00\x00\x00");
    assert_eq!(&metadata[24..32], b"TPOL\x0b\x00\x00\x00");
    let code = archive.find_section(SectionKind::Code).unwrap();
    assert_eq!(u32::from_le_bytes(code[8..12].try_into().unwrap()), 36);
    assert_eq!(u16::from_le_bytes(code[34..36].try_into().unwrap()), 2);

    let mut loaded = read_artifact(&bytes).unwrap();
    assert_eq!(loaded.entry, artifact.entry);
    assert_eq!(loaded.builtin_abi_version, 1);
    assert_eq!(
        loaded.codegen_output.globals,
        artifact.codegen_output.globals
    );
    assert_eq!(
        loaded.codegen_output.method_call_scopes,
        artifact.codegen_output.method_call_scopes
    );
    assert_eq!(loaded.type_pool.len(), artifact.type_pool.len());
    assert_eq!(loaded.type_pool.scopes(), artifact.type_pool.scopes());
    assert_eq!(loaded.type_pool.null_type(), artifact.type_pool.null_type());
    assert_eq!(
        loaded.type_pool.snapshot().structural_types,
        artifact.type_pool.snapshot().structural_types
    );
    assert_eq!(
        loaded.type_pool.intern_structural(TypeKind::Optional {
            inner: Intrinsic::I64.type_index()
        }),
        match artifact.codegen_output.constants[4] {
            Constant::Type(index) => index,
            _ => panic!("fixture must contain its optional type"),
        }
    );
    for (actual, expected) in loaded
        .codegen_output
        .functions
        .iter()
        .zip(&artifact.codegen_output.functions)
    {
        assert_eq!(actual.func_id, expected.func_id);
        assert_eq!(actual.instructions, expected.instructions);
        assert_eq!(
            str_interner::try_get(actual.name),
            str_interner::try_get(expected.name)
        );
        assert_eq!(actual.function_type, expected.function_type);
        assert_eq!(actual.abi, expected.abi);
        assert_eq!(actual.display_owner, expected.display_owner);
        assert_eq!(actual.is_closure, expected.is_closure);
        assert_eq!(actual.param_count, expected.param_count);
        assert_eq!(actual.register_count, expected.register_count);
        assert_eq!(actual.safepoint_pcs, expected.safepoint_pcs);
    }
    assert!(matches!(
        loaded.codegen_output.constants[2],
        Constant::Int128(i128::MIN)
    ));
    assert!(matches!(
        loaded.codegen_output.constants[3],
        Constant::UInt128(u128::MAX)
    ));
    assert!(
        matches!(loaded.codegen_output.constants[6], Constant::Float(value) if value.to_bits() == 0x7ff8_0000_0000_0123)
    );
    assert_eq!(write_artifact(&loaded).unwrap(), bytes);
}

#[test]
fn rejects_legacy_container_versions_zero_checksums_and_legacy_metadata_revisions() {
    let bytes = write_artifact(&fixture()).unwrap();

    // Container versions != 4 (even with recomputed SHA-256) are rejected.
    for version in [0u32, 1, 2, 3, 5, u32::MAX] {
        let mut bad = bytes.clone();
        bad[4..8].copy_from_slice(&version.to_le_bytes());
        let checksum = Sha256::digest(&bad[nsbc::FILE_HEADER_SIZE..]);
        bad[8..40].copy_from_slice(&checksum);
        rejects(&bad, "unsupported NSBC version");
    }

    // Zero checksum and missing Metadata section are rejected.
    let mut zeroed = bytes.clone();
    zeroed[8..40].fill(0);
    rejects(&zeroed, "checksum mismatch");

    let mut incomplete_writer = ArchiveWriter::new();
    incomplete_writer.add_raw_section(SectionKind::Code, vec![0; 4]);
    incomplete_writer.add_constants_section(&[]).unwrap();
    incomplete_writer.add_raw_section(SectionKind::StackMaps, vec![0; 4]);
    let mut incomplete = Vec::new();
    incomplete_writer.write_to(&mut incomplete).unwrap();
    rejects(&incomplete, "missing Metadata");

    // NSAM revisions != 6 (including legacy 1..=5) are rejected.
    for revision in [0u32, 1, 2, 3, 4, 5, 7, u32::MAX] {
        rejects(
            &rewrite_section(&bytes, SectionKind::Metadata, |metadata| {
                metadata[4..8].copy_from_slice(&revision.to_le_bytes());
            }),
            "unsupported artifact metadata revision",
        );
    }

    // TPOL versions != 11 (including legacy 1..=10) are rejected.
    for version in [0u32, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, u32::MAX] {
        rejects(
            &rewrite_section(&bytes, SectionKind::Metadata, |metadata| {
                assert_eq!(&metadata[24..28], b"TPOL");
                metadata[28..32].copy_from_slice(&version.to_le_bytes());
            }),
            "unsupported type metadata version",
        );
    }

    // Retired constant tag 4 (BigInt) and truncated tag 5 (Int128) are rejected.
    rejects(
        &rewrite_section(&bytes, SectionKind::Constants, |constants| {
            *constants = vec![1, 0, 0, 0, 4, 1, 0, 0, 0, 42];
        }),
        "unknown constant tag",
    );
    rejects(
        &rewrite_section(&bytes, SectionKind::Constants, |constants| {
            *constants = vec![1, 0, 0, 0, 5, 0, 0, 0, 0];
        }),
        "truncated constant payload",
    );

    // Unfinalized TypePool is rejected on write_artifact.
    let mut unfinalized = fixture();
    unfinalized.type_pool = TypePool::with_intrinsics();
    let error = write_artifact(&unfinalized).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("TPOL11 requires finalized"));
}

#[test]
fn code_tables_reject_bad_offsets_overlaps_counts_flags_and_trailing_bytes() {
    let bytes = write_artifact(&fixture()).unwrap();
    for (offset, value, message) in [
        (8, 0u32, "outside section"),
        (24, 36, "overlap"),
        (0, u32::MAX, "count"),
        (20, 0, "duplicate function metadata"),
    ] {
        let corrupt = rewrite_section(&bytes, SectionKind::Code, |code| {
            code[offset..offset + 4].copy_from_slice(&value.to_le_bytes())
        });
        rejects(&corrupt, message);
    }
    rejects(
        &rewrite_section(&bytes, SectionKind::Code, |code| code[18] = 1),
        "function flags",
    );
    rejects(
        &rewrite_section(&bytes, SectionKind::Code, |code| code.extend([0; 4])),
        "trailing CODE",
    );
    rejects(
        &rewrite_section(&bytes, SectionKind::StackMaps, |maps| maps.push(0)),
        "trailing bytes",
    );
    rejects(
        &rewrite_section(&bytes, SectionKind::Metadata, |metadata| metadata[0] ^= 1),
        "metadata magic",
    );
    rejects(
        &rewrite_section(&bytes, SectionKind::Metadata, |metadata| metadata.push(0)),
        "trailing bytes",
    );
}

#[test]
fn code_partition_validation_preserves_table_order_and_rejects_amplifying_ranges() {
    let artifact = fixture();
    let bytes = write_artifact(&artifact).unwrap();
    let reversed = rewrite_section(&bytes, SectionKind::Code, |code| {
        let (first, second) = code[4..36].split_at_mut(16);
        first.swap_with_slice(second);
    });
    let loaded = read_artifact(&reversed).unwrap();
    assert_eq!(loaded.codegen_output.functions[0].func_id, FuncId(1));
    assert_eq!(
        loaded.codegen_output.functions[0].instructions,
        artifact.codegen_output.functions[1].instructions
    );
    assert_eq!(
        loaded.codegen_output.functions[1].instructions,
        artifact.codegen_output.functions[0].instructions
    );

    let mut artifact = fixture();
    let mut instructions = vec![Instruction::nop().encode(); 1024];
    instructions[1023] = Instruction::return_unit().encode();
    artifact.codegen_output.functions = (0..100)
        .map(|id| CompiledFunction {
            display_owner: None,
            abi: FunctionAbi::default(),
            func_id: FuncId(id),
            name: str_interner::intern("overlapping_code"),
            instructions: instructions.clone(),
            register_count: 0,
            param_count: 0,
            is_closure: false,
            function_type: TypeIndex::INVALID,
            safepoint_pcs: Vec::new(),
        })
        .collect();
    artifact.codegen_output.method_call_scopes.clear();
    let bytes = write_artifact(&artifact).unwrap();
    let overlapping = rewrite_section(&bytes, SectionKind::Code, |code| {
        let data_start = 4 + 16 * 100;
        let size = (code.len() - data_start) as u32;
        for entry in code[4..data_start].as_chunks_mut::<16>().0 {
            entry[4..8].copy_from_slice(&(data_start as u32).to_le_bytes());
            entry[8..12].copy_from_slice(&size.to_le_bytes());
        }
    });
    rejects(&overlapping, "overlap");
}

fn method_fixture() -> CompiledArtifact {
    let mut artifact = fixture();
    // StrId zero is always representable by the narrow method form. Preserve a
    // numeric UInt with that same value and also use it as old far metadata.
    artifact.codegen_output.constants = vec![Constant::UInt(0)];
    let function = &mut artifact.codegen_output.functions[0];
    function.safepoint_pcs.clear();
    function.instructions = [
        Instruction::load_const(Reg(0), 0),
        Instruction::load_imm(Reg(1), 1),
        Instruction::call_method(Reg(1), 0, 0),
        Instruction::call_method_far(Reg(1), 0, 0),
        Instruction::ret(Reg(0)),
    ]
    .iter()
    .map(|instruction| instruction.encode())
    .collect();
    artifact.codegen_output.method_call_scopes = vec![
        MethodCallScope {
            func_id: FuncId(0),
            pc: 2,
            scope: 0,
        },
        MethodCallScope {
            func_id: FuncId(0),
            pc: 3,
            scope: 1,
        },
    ];
    artifact
}

#[test]
fn method_relocation_preserves_numeric_uints_callsite_scopes_and_resaves() {
    let artifact = method_fixture();
    let bytes = write_artifact(&artifact).unwrap();
    let loaded = read_artifact(&bytes).unwrap();
    assert_eq!(loaded.codegen_output.constants.len(), 2);
    assert_eq!(
        loaded.codegen_output.method_call_scopes,
        artifact.codegen_output.method_call_scopes
    );
    let words = &loaded.codegen_output.functions[0].instructions;
    let load = Instruction::decode(words[0]).unwrap();
    let InstructionData::A {
        imm12: numeric_slot,
        ..
    } = load.data
    else {
        panic!("expected load");
    };
    let method = Instruction::decode(words[2]).unwrap();
    assert_eq!(method.opcode, Opcode::CallMethodFar);
    let InstructionData::C { payload } = method.data else {
        panic!("expected method call");
    };
    let (_, _, method_slot) = Instruction::c_call_method(payload);
    assert_ne!(numeric_slot as u32, method_slot);
    assert!(matches!(
        loaded.codegen_output.constants[numeric_slot as usize],
        Constant::UInt(0)
    ));
    assert_eq!(write_artifact(&loaded).unwrap(), bytes);

    let name = format!(
        "{}::relocated",
        str_interner::try_get(str_interner::StrId::from_raw(0)).unwrap()
    );
    let modified = rewrite_section(&bytes, SectionKind::Metadata, |metadata| {
        let offset = methods_offset(metadata);
        let old_length =
            u32::from_le_bytes(metadata[offset + 8..offset + 12].try_into().unwrap()) as usize;
        let replacement: Vec<_> = (name.len() as u32)
            .to_le_bytes()
            .into_iter()
            .chain(name.bytes())
            .collect();
        metadata.splice(offset + 8..offset + 12 + old_length, replacement);
    });
    let loaded = read_artifact(&modified).unwrap();
    assert!(matches!(
        loaded.codegen_output.constants[numeric_slot as usize],
        Constant::UInt(0)
    ));
    assert!(
        matches!(loaded.codegen_output.constants[method_slot as usize], Constant::UInt(value) if value == str_interner::intern(&name).as_u32() as u64)
    );
}

#[test]
fn method_slots_cannot_alias_numeric_or_handler_metadata_uses() {
    let bytes = write_artifact(&method_fixture()).unwrap();
    let corrupt = rewrite_section(&bytes, SectionKind::Code, |code| {
        let offset = u32::from_le_bytes(code[8..12].try_into().unwrap()) as usize;
        code[offset..offset + 4]
            .copy_from_slice(&Instruction::load_const(Reg(0), 0).encode().to_le_bytes());
    });
    rejects(&corrupt, "also used as a numeric");
    let corrupt = rewrite_section(&bytes, SectionKind::Code, |code| {
        let offset = u32::from_le_bytes(code[8..12].try_into().unwrap()) as usize;
        code[offset..offset + 4].copy_from_slice(
            &Instruction::push_capturing_handler(Reg(0), 0)
                .encode()
                .to_le_bytes(),
        );
    });
    rejects(&corrupt, "also used as a numeric");
}

#[test]
fn method_relocation_widens_loads_and_moves_capturing_handler_metadata_in_place() {
    let mut artifact = method_fixture();
    let effect = artifact.type_pool.register(TypeInfo {
        kind: TypeKind::Effect {
            params: vec![Intrinsic::I64.type_index()],
            ret: Intrinsic::I64.type_index(),
            is_async: false,
        },
        type_id: TypeId::ZERO,
        size: 0,
        align: 0,
    });
    finalize_pool(&mut artifact.type_pool);
    let packed = (1u64 << 32) | u64::from(effect.as_u32());
    artifact
        .codegen_output
        .constants
        .resize(4096, Constant::Int(9));
    artifact.codegen_output.constants[1] = Constant::UInt(packed);
    artifact.codegen_output.constants[4095] = Constant::Int(99);
    artifact.codegen_output.functions[1].is_closure = false;
    artifact.codegen_output.functions[0].instructions = [
        Instruction::load_const(Reg(0), 0),
        Instruction::load_const(Reg(0), 4095),
        Instruction::load_imm(Reg(1), 1),
        Instruction::call_method(Reg(1), 0, 0),
        Instruction::push_capturing_handler(Reg(1), 1),
        Instruction::e_type(Opcode::PopHandler, 0),
        Instruction::ret(Reg(0)),
    ]
    .iter()
    .map(|instruction| instruction.encode())
    .collect();
    artifact.codegen_output.method_call_scopes = vec![MethodCallScope {
        func_id: FuncId(0),
        pc: 3,
        scope: 0,
    }];
    let bytes = write_artifact(&artifact).unwrap();
    let loaded = read_artifact(&bytes).unwrap();
    let words = &loaded.codegen_output.functions[0].instructions;
    assert_eq!(
        words.len(),
        artifact.codegen_output.functions[0].instructions.len()
    );
    let load = Instruction::decode(words[1]).unwrap();
    assert_eq!(load.opcode, Opcode::LoadConstWide);
    let InstructionData::A { base, imm12, .. } = load.data else {
        panic!("expected load");
    };
    let index = Instruction::wide_const_index(base, imm12);
    assert_eq!(index, 4096);
    assert!(matches!(
        loaded.codegen_output.constants[index as usize],
        Constant::Int(99)
    ));
    let handler = Instruction::decode(words[4]).unwrap();
    let InstructionData::E { payload } = handler.data else {
        panic!("expected handler");
    };
    assert_eq!(handler.opcode, Opcode::PushCapturingHandler);
    let relocated_handler = payload & 0x1ffff;
    assert_ne!(relocated_handler, 1);
    assert!(
        matches!(loaded.codegen_output.constants[relocated_handler as usize], Constant::UInt(value) if value == packed)
    );
}

#[test]
fn unrelated_far_jump_words_and_safepoint_pcs_survive_relocation() {
    let mut artifact = method_fixture();
    let distance = (1 << 17) + 1;
    let function = &mut artifact.codegen_output.functions[0];
    function.instructions = vec![Instruction::nop().encode(); distance + 3];
    let far_word = ((Opcode::JmpFar as u32) << 24) | distance as u32;
    function.instructions[0] = far_word;
    function.instructions[distance] = Instruction::safepoint().encode();
    function.instructions[distance + 1] = Instruction::call_method(Reg(1), 0, 0).encode();
    function.instructions[distance + 2] = Instruction::ret(Reg(0)).encode();
    function.safepoint_pcs = vec![distance as u32];
    artifact.codegen_output.method_call_scopes = vec![MethodCallScope {
        func_id: FuncId(0),
        pc: (distance + 1) as u32,
        scope: 0,
    }];
    let loaded = read_artifact(&write_artifact(&artifact).unwrap()).unwrap();
    let function = &loaded.codegen_output.functions[0];
    assert_eq!(function.instructions[0], far_word);
    assert_eq!(function.safepoint_pcs, [distance as u32]);
    assert_eq!(
        function.instructions[distance],
        Instruction::safepoint().encode()
    );
    assert_eq!(
        Instruction::decode(function.instructions[distance + 1])
            .unwrap()
            .opcode,
        Opcode::CallMethodFar
    );
}

#[test]
fn builtin_manifest_roundtrips_and_must_cover_real_call_operands() {
    let mut artifact = fixture();
    artifact.builtins = vec![BuiltinImport {
        id: 3,
        name: "to_i64".into(),
    }];
    artifact.codegen_output.functions[0].safepoint_pcs.clear();
    artifact.codegen_output.functions[0].instructions = [
        Instruction::load_imm(Reg(0), 42),
        Instruction::call_builtin(3, 1),
        Instruction::ret(Reg(0)),
    ]
    .iter()
    .map(|instruction| instruction.encode())
    .collect();
    artifact.codegen_output.method_call_scopes.clear();
    let bytes = write_artifact(&artifact).unwrap();
    assert_eq!(read_artifact(&bytes).unwrap().builtins, artifact.builtins);
    let corrupt = rewrite_section(&bytes, SectionKind::Metadata, |metadata| {
        let builtins_start = methods_offset(metadata) + 4;
        assert_eq!(
            &metadata[builtins_start..builtins_start + 8],
            &[1, 0, 0, 0, 3, 0, 0, 0]
        );
        metadata[builtins_start + 4..builtins_start + 8].copy_from_slice(&1u32.to_le_bytes());
    });
    rejects(&corrupt, "undeclared builtin");
    artifact.builtins.push(artifact.builtins[0].clone());
    assert_eq!(
        write_artifact(&artifact).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn corrupt_global_flags_types_and_entry_are_rejected_before_execution() {
    let bytes = write_artifact(&fixture()).unwrap();
    for (flag, type_index, message) in [
        (2, Intrinsic::I64.type_index().as_u32(), "mutability"),
        (0, u32::MAX, "invalid type index"),
    ] {
        let corrupt = rewrite_section(&bytes, SectionKind::Metadata, |metadata| {
            let mut reader = Reader {
                remaining: metadata,
            };
            reader.take(20).unwrap();
            reader.blob().unwrap();
            let offset = metadata.len() - reader.remaining.len();
            metadata[offset + 4..offset + 8].copy_from_slice(&type_index.to_le_bytes());
            metadata[offset + 8] = flag;
        });
        rejects(&corrupt, message);
    }
    rejects(
        &rewrite_section(&bytes, SectionKind::Metadata, |metadata| {
            metadata[16..20].copy_from_slice(&999u32.to_le_bytes());
        }),
        "unknown function",
    );
}

#[test]
fn resolved_derived_methods_roundtrip_while_unresolved_sentinels_are_rejected() {
    let mut artifact = fixture();
    let owner = Intrinsic::I64.type_index();
    let trait_type = artifact.type_pool.register(TypeInfo {
        kind: TypeKind::Trait {
            name: str_interner::intern("DerivedEqView"),
            parents: vec![],
            assoc_types: vec![],
        },
        type_id: TypeId::ZERO,
        size: 0,
        align: 0,
    });
    let declaration = artifact.type_pool.intern_structural(TypeKind::Function {
        params: vec![trait_type],
        ret: Intrinsic::Bool.type_index(),
    });
    let actual = artifact.type_pool.intern_structural(TypeKind::Function {
        params: vec![owner],
        ret: Intrinsic::Bool.type_index(),
    });
    let name = str_interner::intern("eq");
    artifact
        .type_pool
        .register_trait_schema(type_pool::TraitDispatchSchema {
            trait_type,
            slots: vec![type_pool::TraitMethodKey {
                trait_owner: trait_type,
                name,
                signature: Some(type_pool::TraitMethodSignature {
                    associated_paths: vec![],
                    declaration,
                    self_paths: vec![vec![type_pool::TraitTypeStep::Parameter(0)]],
                    parameter_kinds: vec![type_pool::TraitParameterKind::Receiver],
                }),
            }],
        })
        .unwrap();
    let impl_fn = &mut artifact.codegen_output.functions[1];
    impl_fn.function_type = actual;
    impl_fn.is_closure = false;
    impl_fn.param_count = 1;
    impl_fn.abi = FunctionAbi {
        captures: vec![],
        parameters: vec![ParameterAbi::Value],
    };
    impl_fn.instructions = vec![
        Instruction::a_type(Opcode::LoadTrue, AddrMode::Imm, Reg(0), Reg(0), 0).encode(),
        Instruction::ret(Reg(0)).encode(),
    ];

    let method = type_pool::MethodSlot {
        name,
        func_id: type_pool::DERIVE_FUNC_ID,
        access: type_pool::MethodAccess::Public,
        trait_impl: Some(trait_type),
        visible_scope: None,
    };
    artifact.type_pool.add_method(owner, method.clone());
    artifact
        .type_pool
        .add_trait_impl(type_pool::TraitImplRecord {
            trait_type,
            implementor: owner,
            visible_scope: None,
            methods: vec![method],
        });
    artifact.type_pool.add_vtable(type_pool::VTable {
        trait_type,
        implementor: owner,
        visible_scope: None,
        entries: vec![type_pool::DERIVE_FUNC_ID],
    });
    finalize_pool(&mut artifact.type_pool);

    // Unresolved DERIVE_FUNC_ID must be rejected before producing an archive.
    let unresolved_err = write_artifact(&artifact).unwrap_err();
    assert_eq!(unresolved_err.kind(), io::ErrorKind::InvalidInput);
    assert!(
        unresolved_err
            .to_string()
            .contains("unresolved derived method sentinel")
    );

    // Once resolved to a real function, roundtrip succeeds cleanly.
    artifact
        .type_pool
        .resolve_derived_methods([(owner, trait_type, name, 1)])
        .unwrap();
    finalize_pool(&mut artifact.type_pool);
    let loaded = read_artifact(&write_artifact(&artifact).unwrap()).unwrap();
    assert_eq!(
        loaded.type_pool.find_method(owner, name).unwrap().func_id,
        1
    );
    assert_eq!(
        loaded
            .type_pool
            .find_trait_method(owner, trait_type, name)
            .unwrap()
            .func_id,
        1
    );
    assert_eq!(
        loaded
            .type_pool
            .find_vtable(owner, trait_type)
            .unwrap()
            .entries,
        vec![1]
    );
    assert_eq!(loaded.codegen_output.functions[1].func_id, FuncId(1));
}

#[test]
fn damaged_callsite_counts_scopes_pcs_and_duplicates_are_rejected() {
    let artifact = method_fixture();
    let bytes = write_artifact(&artifact).unwrap();
    for mutation in 0..6 {
        let damaged = rewrite_section(&bytes, SectionKind::Metadata, |metadata| {
            let count = scopes_offset(metadata);
            let first = count + 4;
            let second = first + 12;
            match mutation {
                0 => metadata[count..count + 4].copy_from_slice(&u32::MAX.to_le_bytes()),
                1 => metadata[first..first + 4].copy_from_slice(&99_u32.to_le_bytes()),
                2 => metadata[first + 4..first + 8].copy_from_slice(&0_u32.to_le_bytes()),
                3 => metadata[first + 8..first + 12].copy_from_slice(&99_u32.to_le_bytes()),
                4 => metadata[second + 4..second + 8].copy_from_slice(&2_u32.to_le_bytes()),
                _ => {
                    metadata[count..count + 4].copy_from_slice(&1_u32.to_le_bytes());
                    metadata.drain(first..second);
                }
            }
        });
        let error = read_artifact(&damaged).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            error
                .to_string()
                .contains(if mutation == 0 { "count" } else { "context" }),
            "mutation {mutation}: {error}"
        );
    }

    // Missing required scope on write_artifact is also rejected.
    let mut missing_scope = method_fixture();
    missing_scope.codegen_output.method_call_scopes.pop();
    assert_eq!(
        write_artifact(&missing_scope).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

fn explicit_capture_fixture() -> CompiledArtifact {
    let mut artifact = fixture();
    artifact.codegen_output.functions[0].abi = FunctionAbi::default();
    artifact.codegen_output.functions[0]
        .instructions
        .insert(5, Instruction::new_closure(Reg(0), 1, 1).encode());
    let closure = &mut artifact.codegen_output.functions[1];
    closure.param_count = 1;
    closure.abi = FunctionAbi {
        captures: vec![CaptureAbi::Value],
        parameters: vec![],
    };
    artifact
}

#[test]
fn mandatory_function_abi_preserves_captures_parameters_and_rejects_corruption() {
    let artifact = explicit_capture_fixture();
    let bytes = write_artifact(&artifact).unwrap();
    let loaded = read_artifact(&bytes).unwrap();
    for (original, restored) in artifact
        .codegen_output
        .functions
        .iter()
        .zip(&loaded.codegen_output.functions)
    {
        assert_eq!(restored.abi, original.abi);
        assert_eq!(restored.func_id, original.func_id);
    }
    assert_eq!(write_artifact(&loaded).unwrap(), bytes);

    // ABI section layout for explicit_capture_fixture():
    // count=2 (4B), f0=[id=0(4B), cap=0(4B), param=0(4B)] (12B),
    // f1=[id=1(4B), cap=1(4B), tag=0(1B), param=0(4B)] (13B).
    for (offset, value, message) in [
        (0, 1u32, "ABI count"),
        (4, u32::MAX, "unknown function ABI"),
        (16, 0, "duplicate function ABI"),
        (20, u32::MAX, "count"),
    ] {
        rejects(
            &rewrite_section(&bytes, SectionKind::Metadata, |metadata| {
                let start = abis_offset(metadata);
                metadata[start + offset..start + offset + 4].copy_from_slice(&value.to_le_bytes());
            }),
            message,
        );
    }
    rejects(
        &rewrite_section(&bytes, SectionKind::Metadata, |metadata| {
            let start = abis_offset(metadata);
            assert_eq!(metadata[start + 24], 0);
            metadata[start + 24] = 255;
        }),
        "capture ABI tag",
    );
    rejects(
        &rewrite_section(&bytes, SectionKind::Metadata, |metadata| {
            let start = abis_offset(metadata);
            metadata[start + 20..start + 24].copy_from_slice(&33u32.to_le_bytes());
            metadata.splice(start + 25..start + 25, [0; 32]);
        }),
        "capture ABI count exceeds the register limit",
    );
    for view in [
        u32::MAX,
        Intrinsic::I64.type_index().as_u32(),
        artifact.type_pool.well_known.eq.as_u32(),
    ] {
        let damaged = rewrite_section(&bytes, SectionKind::Metadata, |metadata| {
            let start = abis_offset(metadata);
            let tag = start + 24;
            metadata[tag] = 1;
            metadata.splice(tag + 1..tag + 1, view.to_le_bytes());
        });
        let valid_view = view == artifact.type_pool.well_known.eq.as_u32();
        let damaged = if valid_view {
            rewrite_section(&damaged, SectionKind::Code, |code| code[33] = 0)
        } else {
            damaged
        };
        let error = read_artifact(&damaged).err().unwrap().to_string();
        if valid_view {
            assert!(
                error.contains("disagrees") || error.contains("capture"),
                "{error}"
            );
        } else {
            assert!(error.contains("trait") || error.contains("type"), "{error}");
        }
    }
}

fn trait_proof_fixture() -> CompiledArtifact {
    let mut artifact = fixture();
    let view = artifact.type_pool.register(TypeInfo {
        kind: TypeKind::Trait {
            name: str_interner::intern("CodecProofView"),
            parents: vec![],
            assoc_types: vec![],
        },
        type_id: TypeId::ZERO,
        size: 0,
        align: 0,
    });
    let signature = artifact.type_pool.intern_structural(TypeKind::Function {
        params: vec![view],
        ret: Intrinsic::I64.type_index(),
    });
    let owner = Intrinsic::I64.type_index();
    let actual = artifact.type_pool.intern_structural(TypeKind::Function {
        params: vec![owner],
        ret: owner,
    });
    let method = type_pool::MethodSlot {
        access: type_pool::MethodAccess::Public,
        name: str_interner::intern("read"),
        func_id: 1,
        trait_impl: Some(view),
        visible_scope: None,
    };
    artifact.type_pool.add_method(owner, method.clone());
    artifact
        .type_pool
        .add_trait_impl(type_pool::TraitImplRecord {
            visible_scope: None,
            trait_type: view,
            implementor: owner,
            methods: vec![method],
        });
    artifact.type_pool.add_vtable(type_pool::VTable {
        visible_scope: None,
        trait_type: view,
        implementor: owner,
        entries: vec![1],
    });
    artifact
        .type_pool
        .register_trait_schema(type_pool::TraitDispatchSchema {
            trait_type: view,
            slots: vec![type_pool::TraitMethodKey {
                trait_owner: view,
                name: str_interner::intern("read"),
                signature: Some(type_pool::TraitMethodSignature {
                    associated_paths: vec![],
                    declaration: signature,
                    self_paths: vec![vec![type_pool::TraitTypeStep::Parameter(0)]],
                    parameter_kinds: vec![type_pool::TraitParameterKind::Receiver],
                }),
            }],
        })
        .unwrap();
    finalize_pool(&mut artifact.type_pool);

    let entry = &mut artifact.codegen_output.functions[0];
    entry.abi = FunctionAbi::default();
    entry.safepoint_pcs.clear();
    entry.instructions = [
        Instruction::load_const(Reg(0), 0),
        Instruction::trait_proof(Reg(1), Reg(0), view),
        Instruction::trait_assert(Reg(0), Reg(1), view),
        Instruction::trait_call(Reg(1), 0, 1),
        Instruction::ret(Reg(0)),
    ]
    .map(Instruction::encode)
    .to_vec();
    let method = &mut artifact.codegen_output.functions[1];
    method.function_type = actual;
    method.is_closure = false;
    method.param_count = 1;
    method.abi = FunctionAbi {
        captures: vec![],
        parameters: vec![ParameterAbi::Value],
    };
    artifact.codegen_output.method_call_scopes = [1, 2, 3]
        .map(|pc| MethodCallScope {
            func_id: FuncId(0),
            pc,
            scope: 0,
        })
        .to_vec();
    artifact
}

#[test]
fn trait_proof_instructions_specialized_self_proofs_and_slot_validation_roundtrip() {
    let mut artifact = trait_proof_fixture();
    let bytes = write_artifact(&artifact).unwrap();
    let loaded = read_artifact(&bytes).unwrap();
    assert_eq!(
        loaded.codegen_output.functions[0].instructions,
        artifact.codegen_output.functions[0].instructions
    );
    assert_eq!(
        loaded.codegen_output.method_call_scopes,
        artifact.codegen_output.method_call_scopes
    );
    assert_eq!(
        loaded.type_pool.snapshot().trait_schemas,
        artifact.type_pool.snapshot().trait_schemas
    );
    assert_eq!(write_artifact(&loaded).unwrap(), bytes);

    for (pc, replacement, error) in [
        (
            1,
            Instruction::trait_proof(Reg(1), Reg(0), Intrinsic::I64.type_index()),
            "trait view",
        ),
        (
            1,
            Instruction::trait_proof(Reg(1), Reg(0), TypeIndex::from_raw(4095)),
            "type index",
        ),
        (3, Instruction::trait_call(Reg(1), 4095, 1), "slot"),
        (
            3,
            Instruction::trait_call(Reg(1), 0, 0),
            "requires receiver data",
        ),
    ] {
        rejects(
            &rewrite_section(&bytes, SectionKind::Code, |code| {
                let start = u32::from_le_bytes(code[8..12].try_into().unwrap()) as usize;
                code[start + pc * 4..start + (pc + 1) * 4]
                    .copy_from_slice(&replacement.encode().to_le_bytes());
            }),
            error,
        );
    }
    rejects(&bytes[..bytes.len() - 1], "section");

    // Specialized ParameterAbi::TraitSelf roundtrip and parameter tag corruption.
    let view = artifact.type_pool.trait_schemas_snapshot()[0].trait_type;
    let entry = &mut artifact.codegen_output.functions[0];
    entry.register_count = 3;
    entry.instructions[1] = Instruction::trait_proof(Reg(2), Reg(0), view).encode();
    entry.instructions[2] = Instruction::trait_assert(Reg(0), Reg(2), view).encode();
    entry.instructions[3] = Instruction::trait_call(Reg(2), 0, 2).encode();
    let method = &mut artifact.codegen_output.functions[1];
    method.register_count = 2;
    method.param_count = 2;
    method.abi.parameters = vec![ParameterAbi::TraitSelf { view }];
    let expected_abi = method.abi.clone();
    let self_bytes = write_artifact(&artifact).unwrap();
    let restored = read_artifact(&self_bytes).unwrap();
    assert_eq!(restored.codegen_output.functions[1].abi, expected_abi);
    assert_eq!(write_artifact(&restored).unwrap(), self_bytes);

    rejects(
        &rewrite_section(&self_bytes, SectionKind::Metadata, |metadata| {
            let start = abis_offset(metadata);
            // count(4) + f0(12) + f1_id(4) + f1_cap(4) + f1_param_count(4) = 28
            assert_eq!(metadata[start + 28], 2);
            metadata[start + 28] = 255;
        }),
        "invalid parameter ABI tag",
    );
    rejects(
        &rewrite_section(&self_bytes, SectionKind::Metadata, |metadata| {
            let start = abis_offset(metadata) + 29;
            metadata[start..start + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        }),
        "type index",
    );
}

fn generated_display_fixture() -> CompiledArtifact {
    let mut artifact = fixture();
    let owner = artifact.type_pool.register(TypeInfo {
        kind: TypeKind::Struct {
            name: str_interner::intern("PersistedDisplay"),
            fields: vec![],
        },
        type_id: TypeId::ZERO,
        size: 0,
        align: 8,
    });
    let view = artifact.type_pool.well_known.display;
    let name = str_interner::intern("to_string");
    let declaration = artifact.type_pool.intern_structural(TypeKind::Function {
        params: vec![view],
        ret: Intrinsic::Str.type_index(),
    });
    artifact
        .type_pool
        .register_trait_schema(type_pool::TraitDispatchSchema {
            trait_type: view,
            slots: vec![type_pool::TraitMethodKey {
                trait_owner: view,
                name,
                signature: Some(type_pool::TraitMethodSignature {
                    declaration,
                    self_paths: vec![vec![type_pool::TraitTypeStep::Parameter(0)]],
                    associated_paths: vec![],
                    parameter_kinds: vec![type_pool::TraitParameterKind::Receiver],
                }),
            }],
        })
        .unwrap();
    let method = type_pool::MethodSlot {
        name,
        func_id: 1,
        trait_impl: Some(view),
        visible_scope: None,
        access: type_pool::MethodAccess::Public,
    };
    artifact.type_pool.add_method(owner, method.clone());
    artifact
        .type_pool
        .add_trait_impl(type_pool::TraitImplRecord {
            trait_type: view,
            implementor: owner,
            visible_scope: None,
            methods: vec![method],
        });
    artifact.type_pool.add_vtable(type_pool::VTable {
        trait_type: view,
        implementor: owner,
        visible_scope: None,
        entries: vec![1],
    });
    let signature = artifact.type_pool.intern_structural(TypeKind::Function {
        params: vec![owner],
        ret: Intrinsic::Str.type_index(),
    });
    finalize_pool(&mut artifact.type_pool);

    let function = &mut artifact.codegen_output.functions[1];
    function.display_owner = Some(owner);
    function.function_type = signature;
    function.is_closure = false;
    function.param_count = 1;
    function.abi = FunctionAbi {
        captures: vec![],
        parameters: vec![ParameterAbi::Value],
    };
    function.instructions = [Instruction::load_const(Reg(0), 1), Instruction::ret(Reg(0))]
        .map(Instruction::encode)
        .to_vec();
    artifact
}

#[test]
fn generated_display_metadata_roundtrips_and_rejects_corrupt_tables_and_contracts() {
    let artifact = generated_display_fixture();
    let bytes = write_artifact(&artifact).unwrap();
    let archive = Archive::read_from(&mut bytes.as_slice()).unwrap();
    let metadata = archive.find_section(SectionKind::Metadata).unwrap();
    assert_eq!(&metadata[..8], b"NSAM\x06\x00\x00\x00");
    let restored = read_artifact(&bytes).unwrap();
    for (original, loaded) in artifact
        .codegen_output
        .functions
        .iter()
        .zip(&restored.codegen_output.functions)
    {
        assert_eq!(loaded.func_id, original.func_id);
        assert_eq!(loaded.display_owner, original.display_owner);
        assert_eq!(loaded.abi, original.abi);
    }
    assert_eq!(write_artifact(&restored).unwrap(), bytes);

    for (offset, replacement, message) in [
        (18, vec![1, 0, 0, 0], "count"),
        (14, vec![9, 0, 0, 0], "target"),
        (9, vec![0, 0, 0, 0], "duplicate"),
        (5, vec![2], "presence"),
        (4, u32::MAX.to_le_bytes().to_vec(), "Display"),
    ] {
        rejects(
            &rewrite_section(&bytes, SectionKind::Metadata, |metadata| {
                let start = metadata.len() - offset;
                metadata[start..start + replacement.len()].copy_from_slice(&replacement);
            }),
            message,
        );
    }
    rejects(
        &rewrite_section(&bytes, SectionKind::Metadata, |metadata| {
            metadata.pop();
        }),
        "truncated",
    );

    for case in 0..4 {
        let mut bad = generated_display_fixture();
        let method = &mut bad.codegen_output.functions[1];
        match case {
            0 => method.display_owner = Some(Intrinsic::I64.type_index()),
            1 => method.abi = FunctionAbi::default(),
            2 => method.is_closure = true,
            _ => bad.codegen_output.functions[0].display_owner = method.display_owner,
        }
        assert!(
            write_artifact(&bad).is_err(),
            "invalid Display contract case {case}"
        );
    }
}

#[test]
fn error_opcodes_and_error_layouts_roundtrip_and_validate_authenticated_types() {
    let mut value = fixture();
    let qualified = value
        .type_pool
        .normalize_error_type(
            vec![Intrinsic::I64.type_index()],
            Intrinsic::I64.type_index(),
        )
        .unwrap();
    finalize_pool(&mut value.type_pool);
    value.codegen_output.constants = vec![Constant::Type(qualified)];
    let entry = &mut value.codegen_output.functions[0];
    entry.instructions = vec![
        Instruction::load_imm(Reg(0), 42).encode(),
        Instruction::a_type(Opcode::ErrorOk, AddrMode::Imm, Reg(1), Reg(0), 0).encode(),
        Instruction::a_type(Opcode::ErrorPayload, AddrMode::Imm, Reg(0), Reg(1), 0).encode(),
        Instruction::ret(Reg(0)).encode(),
    ];
    entry.register_count = 3;
    entry.safepoint_pcs.clear();
    let expected = entry.instructions.clone();
    value.codegen_output.method_call_scopes = collect_scopes(&value.codegen_output.functions, 0);
    let bytes = write_artifact(&value).unwrap();
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 4);
    let read = read_artifact(&bytes).unwrap();
    assert_eq!(read.codegen_output.functions[0].instructions, expected);
    assert_eq!(
        read.type_pool.stable_type_id(qualified).unwrap(),
        value.type_pool.stable_type_id(qualified).unwrap()
    );

    value.codegen_output.functions[0].instructions[2] =
        Instruction::a_type(Opcode::ErrorPayload, AddrMode::Imm, Reg(0), Reg(1), 1).encode();
    assert!(
        write_artifact(&value)
            .unwrap_err()
            .to_string()
            .contains("unused Error operands")
    );

    // Zero-sized unnormalized ErrorQualified descriptor is rejected in executable slots.
    let mut bad_layout = fixture();
    let zero_sized = bad_layout.type_pool.register(TypeInfo {
        kind: TypeKind::ErrorQualified {
            errors: vec![Intrinsic::I64.type_index()],
            inner: Intrinsic::I64.type_index(),
        },
        type_id: TypeId::ZERO,
        size: 0,
        align: 0,
    });
    finalize_pool(&mut bad_layout.type_pool);
    bad_layout.codegen_output.globals[0].type_index = zero_sized;
    assert!(
        write_artifact(&bad_layout)
            .unwrap_err()
            .to_string()
            .contains("unsupported legacy Error layout")
    );
}
