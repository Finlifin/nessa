//! Both Error branches and checked capabilities survive fresh processes and source deletion.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use nsbc::{AddrMode, Constant, Instruction, Opcode, Reg, SectionKind};
use nsbc_io::{Archive, ArchiveWriter};
use type_pool::{Intrinsic, TypeId, TypeIndex, TypeInfo, TypeKind, TypePool};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let p = std::env::temp_dir().join(format!(
                "nessa-full-error-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&p) {
                Ok(()) => return Self(p),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => panic!("{e}"),
            }
        }
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn cli(command: &str, path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nessa"))
        .arg(command)
        .arg(path)
        .output()
        .unwrap()
}
fn success(out: &Output) {
    assert!(
        out.status.success(),
        "{} stdout={} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
fn compiled(source: &str) -> nsbc::CompiledArtifact {
    let result = driver::Driver::new().compile(source);
    assert!(!result.has_errors, "{source}: {:?}", result.diagnostics);
    result.into_artifact().unwrap()
}
fn roundtrip(source: &str, expected: &str) {
    let d = Directory::new();
    let p = d.0.join("program.ns");
    std::fs::write(&p, source).unwrap();
    let run = cli("run", &p);
    success(&run);
    assert_eq!(run.stdout, expected.as_bytes());
    success(&cli("build", &p));
    let archive = p.with_extension("nsbc");
    let bytes = std::fs::read(&archive).unwrap();
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 4);
    let from_source = compiled(source);
    let loaded = nsbc_io::read_artifact(&bytes).unwrap();
    assert_eq!(
        loaded.type_pool.identity_input(),
        from_source.type_pool.identity_input()
    );
    assert_eq!(loaded.type_pool.len(), from_source.type_pool.len());
    for i in 0..loaded.type_pool.len() {
        let t = TypeIndex::from_raw(i as u32);
        assert_eq!(
            loaded.type_pool.get(t).type_id,
            from_source.type_pool.get(t).type_id,
            "all128bits at {i}"
        );
    }
    std::fs::remove_file(&p).unwrap();
    assert!(!p.exists());
    let run = cli("run", &archive);
    success(&run);
    assert_eq!(run.stdout, expected.as_bytes());
}
fn rewrite(bytes: &[u8], kind: SectionKind, edit: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut archive = Archive::read_from(&mut &bytes[..]).unwrap();
    edit(
        &mut archive
            .sections
            .iter_mut()
            .find(|(e, _)| e.kind == kind as u8)
            .unwrap()
            .1,
    );
    let mut writer = ArchiveWriter::new();
    for (e, b) in archive.sections {
        writer.add_raw_section(SectionKind::from_u8(e.kind).unwrap(), b);
    }
    let mut out = vec![];
    writer.write_to(&mut out).unwrap();
    out
}
fn unique(bytes: &[u8], needle: &[u8]) -> usize {
    let found: Vec<_> = bytes
        .windows(needle.len())
        .enumerate()
        .filter_map(|(i, b)| (b == needle).then_some(i))
        .collect();
    assert_eq!(found.len(), 1, "ambiguous independent mutation: {found:?}");
    found[0]
}
fn rejects_before_entry(bytes: &[u8]) {
    let error = nsbc_io::read_artifact(bytes).expect_err("damaged executable accepted");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    let d = Directory::new();
    let p = d.0.join("damaged.nsbc");
    std::fs::write(&p, bytes).unwrap();
    let out = cli("run", &p);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "entry executed: {:?}", out.stdout);
}

#[test]
fn source_deleted_branches_propagation_elimination_and_defaults_replay_exact_values() {
    for source in [
        "enum E{bad};fn main(){let a:!E E=E.bad;let b:!E E=error E.bad;println((a match{E.bad! =>20,error E.bad=>0})+(b match{E.bad! =>0,error E.bad=>22}))}",
        "enum E{bad};global trace:i64=0;fn get()->!E i64{error E.bad};fn work()->!E i64{let n=get()!;trace=99;n};fn main(){println(work() match{error E.bad=>if trace==0{42}else{0},_! =>0})}",
        "enum E{a,b};global trace:i64=0;fn main(){let value:!E i64=error E.b;let kept=value!{E.a=>0};trace+=1;println(kept match{error E.b=>41+trace,error _=>0,_! =>0})}",
        "enum E{bad};fn main(){let value:!E i64=40;println(value!{ok! =>ok+2,E.* =>0})}",
        "enum E{value(text:String)};fn make()->fn()->i64{let value:!E i64=error E.value(\"abcd\"++\"efgh\");value match{error E.value(text)=>||text.len()+34,_! =>||0}};fn main(){println(make()())}",
        "enum E{bad};enum F{bad};struct P{};struct Q{};trait Read{assoc Err:Type=E;assoc Item:Type=i64;fn get(self)->!Err Item;derive fn answer(self)->!Err Item{self.get()!}};impl Read for P{pub fn get(self)->!E i64{40}};impl Read for Q{assoc Err:Type=F;pub fn get(self)->!F i64{2}};fn main(){println((P{}.answer()!{E.* =>0})+(Q{}.answer()!{F.* =>0}))}",
        "enum E{bad};fn again(v:Any)->!E i64{error v};fn main(){println(again(E.bad) match{error E.bad=>42,_! =>0})}",
        "enum E{bad};fn lift(v:Any)->!E E{v};fn main(){println(lift(E.bad) match{E.bad! =>42,error E.bad=>0})}",
        "enum E{bad};fn main(){let value:!E i64=error E.bad;let answer=value!{catch e=>{struct Local{n:i64};Local{n:40}.n},n! =>{struct Local{n:i64};Local{n:n}.n}};println(answer+2)}",
    ] {
        roundtrip(source, "42\n");
    }
}

#[test]
fn source_deleted_multishot_error_exit_and_success_frames_remain_independent() {
    roundtrip(
        "enum E{bad};effect pick(catch k)->!E i64;global trace:i64=0;fn work()->!E i64{let n=pick()#!;trace=trace*10+n;n+20};fn main(){let saved=work()#{pick(k)=>k};let failed:!E i64=saved(error E.bad).as(!E i64);let a:!E i64=saved(0).as(!E i64);let b:!E i64=saved(2).as(!E i64);let bad=failed match{error E.bad=>true,_! =>false};if bad and trace==2{println((a!{E.* =>0})+(b!{E.* =>0}))}else{println(0)}}",
        "42\n",
    );
}

#[test]
fn erased_wrong_domains_and_ordinary_shapes_keep_runtime_typeerror_after_source_deletion() {
    for source in [
        "enum E{bad};enum F{bad};fn again(v:Any)->!E i64{error v};fn main(){again(F.bad);println(42)}",
        "enum E{bad};enum F{bad};fn main(){let value:Any=error F.bad;value.as(!E i64);println(42)}",
        "enum E{bad};struct Fake{hi:i64,lo:i64,payload:i64};fn main(){let value:Any=Fake{hi:0,lo:0,payload:42};value.as(!E i64);println(42)}",
    ] {
        let d = Directory::new();
        let p = d.0.join("wrong.ns");
        std::fs::write(&p, source).unwrap();
        let direct = cli("run", &p);
        assert!(!direct.status.success());
        assert!(direct.stdout.is_empty());
        assert!(String::from_utf8_lossy(&direct.stderr).contains("TypeError"));
        success(&cli("build", &p));
        std::fs::remove_file(&p).unwrap();
        let archived = cli("run", &p.with_extension("nsbc"));
        assert!(!archived.status.success());
        assert!(archived.stdout.is_empty());
        assert!(String::from_utf8_lossy(&archived.stderr).contains("TypeError"));
    }
}

#[test]
fn invalid_sets_subset_holes_and_escape_boundaries_emit_no_archive() {
    for source in [
        "fn main()->!Any i64{42}",
        "enum E{bad};fn main(){let value:!E i64=42;if value matches n!{n}else{0}}",
        "enum E{bad};fn main(){let value:!E i64=error E.bad;value match{error E.bad=>42}}",
        "enum E{a,b};fn main(){let value:!E i64=42;value match{n! =>n,error E.a=>0}}",
        "enum E{bad};fn main(){let value:!E (i64,i64)=(20,22);value match{((a,b) as whole)! if whole.0==a=>a+b,error _=>0}}",
        "enum E{bad};fn dynamic(value:Any){error value};fn main(){dynamic(E.bad) match{_! =>0,error E.bad=>42}}",
        "const A=B;const B=A;fn main()->!A i64{42}",
        "enum E{bad};enum F{bad};fn narrow(v:!F Any)->!E Any{v};fn main(){42}",
        "enum E{bad};fn read()->i64{let v:!E i64=error E.bad;v!};fn main(){42}",
        "enum E{bad};fn get()->!E i64{error E.bad};fn call(.n:i64=get()!){n};fn main(){42}",
        "enum E{bad};fn get()->!E i64{error E.bad};struct P{n:i64=get()!};fn main(){42}",
        "enum E{bad};fn get()->!E i64{error E.bad};global n:i64=get()!;fn main(){42}",
    ] {
        let d = Directory::new();
        let p = d.0.join("invalid.ns");
        std::fs::write(&p, source).unwrap();
        let out = cli("build", &p);
        assert!(!out.status.success(), "accepted {source}");
        assert!(!p.with_extension("nsbc").exists());
    }
}

#[test]
fn valid_checksum_bad_error_opcode_targets_modes_registers_and_reserved_fields_reject() {
    // Keep construction in a small separate function, so a validated tighter
    // register bound can test Reg(31) without changing executable instructions.
    let source = "enum E{bad};fn make(){error E.bad};fn main(){let value:!E i64=make();value match{error E.bad=>println(42),_! =>println(0)}}";
    let mut artifact = compiled(source);
    let constructors: Vec<_> = artifact
        .codegen_output
        .functions
        .iter()
        .flat_map(|f| f.instructions.iter().copied())
        .filter(|&word| Instruction::decode(word).is_some_and(|i| i.opcode == Opcode::ErrorErr))
        .collect();
    assert_eq!(constructors.len(), 1);
    let original = constructors[0];
    let owner = artifact
        .codegen_output
        .functions
        .iter_mut()
        .find(|f| f.instructions.contains(&original))
        .unwrap();
    owner.register_count = owner.register_count.min(31);
    // write_artifact checks all explicit and implicit register uses; success
    // proves this exact unmodified instruction body is valid below Reg(31).
    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
    let decoded = Instruction::decode(original).unwrap();
    let nsbc::InstructionData::A { dst, base, imm12 } = decoded.data else {
        panic!("ErrorErr must be A-type")
    };
    assert!(artifact.codegen_output.constants.len() < 4095);
    for mutation in 0..7 {
        let bad = match mutation {
            0 => (original & 0x00ff_ffff) | 0xff00_0000,
            1 => Instruction::a_type(Opcode::ErrorErr, AddrMode::Imm, dst, base, 4095).encode(),
            2 => Instruction::a_type(Opcode::ErrorErr, AddrMode::Const, dst, base, imm12).encode(),
            3 => {
                Instruction::a_type(Opcode::ErrorErr, AddrMode::Imm, Reg(31), base, imm12).encode()
            }
            4 => Instruction::a_type(Opcode::ErrorIsOk, AddrMode::Imm, dst, base, 1).encode(),
            5 => Instruction::a_type(Opcode::ErrorPayload, AddrMode::Imm, dst, base, 1).encode(),
            6 => original | (3 << 22),
            _ => unreachable!(),
        };
        let corrupted = rewrite(&bytes, SectionKind::Code, |code| {
            let at = unique(code, &original.to_le_bytes());
            code[at..at + 4].copy_from_slice(&bad.to_le_bytes());
        });
        rejects_before_entry(&corrupted);
    }
    let mut target = nsbc_io::read_artifact(&bytes).unwrap();
    let bad_index: u16 = target.codegen_output.constants.len().try_into().unwrap();
    target
        .codegen_output
        .constants
        .push(Constant::Type(Intrinsic::I64.type_index()));
    let with_constant = nsbc_io::write_artifact(&target).unwrap();
    let bad = Instruction::a_type(Opcode::ErrorErr, AddrMode::Imm, dst, base, bad_index).encode();
    let corrupted = rewrite(&with_constant, SectionKind::Code, |code| {
        let at = unique(code, &original.to_le_bytes());
        code[at..at + 4].copy_from_slice(&bad.to_le_bytes());
    });
    rejects_before_entry(&corrupted);
    let mut at_capacity = nsbc_io::read_artifact(&bytes).unwrap();
    let valid_target = at_capacity.codegen_output.constants[imm12 as usize].clone();
    while at_capacity.codegen_output.constants.len() < 4095 {
        at_capacity.codegen_output.constants.push(Constant::Int(0));
    }
    at_capacity.codegen_output.constants.push(valid_target);
    for function in &mut at_capacity.codegen_output.functions {
        for word in &mut function.instructions {
            if *word == original {
                *word =
                    Instruction::a_type(Opcode::ErrorErr, AddrMode::Imm, dst, base, 4095).encode();
            }
        }
    }
    let valid = nsbc_io::write_artifact(&at_capacity).unwrap();
    let d = Directory::new();
    let p = d.0.join("max-index.nsbc");
    std::fs::write(&p, valid).unwrap();
    let out = cli("run", &p);
    success(&out);
    assert_eq!(out.stdout, b"42\n");
}

#[test]
fn authenticated_error_carrier_layout_and_tag_ids_reject_metadata_changes() {
    let artifact = compiled(
        "enum E{bad};fn main(){let value:!E i64=error E.bad;value match{error E.bad=>println(42),_! =>println(0)}}",
    );
    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
    let pool = &artifact.type_pool;
    let enum_members: std::collections::BTreeSet<_> = (0..pool.len())
        .map(|i| TypeIndex::from_raw(i as u32))
        .filter(|&t| {
            matches!(&pool.get(t).kind,
            TypeKind::Enum { name, .. } if str_interner::get(*name) == "E")
        })
        .map(|t| pool.canonical_type(t).unwrap())
        .collect();
    assert_eq!(enum_members.len(), 1, "fixture must have one canonical E");
    let error_member = *enum_members.iter().next().unwrap();
    // Error construction initially produces !E NoReturn; select the exact
    // lifted !E i64 carrier, independently of constant registration order.
    let targets: Vec<_> = (0..pool.len())
        .map(|i| TypeIndex::from_raw(i as u32))
        .filter(|&t| {
            let info = pool.get(t);
            info.size == 24
                && info.align == 8
                && matches!(&info.kind, TypeKind::ErrorQualified { errors, inner }
                    if errors.len() == 1
                        && pool.canonical_type(errors[0]) == Some(error_member)
                        && pool.canonical_type(*inner) == Some(Intrinsic::I64.type_index()))
        })
        .collect();
    assert_eq!(
        targets.len(),
        1,
        "fixture must have one exact !E i64 descriptor"
    );
    let target = targets[0];
    assert_eq!(pool.canonical_type(target), Some(target));
    assert!(
        artifact
            .codegen_output
            .functions
            .iter()
            .flat_map(|f| f.instructions.iter().copied())
            .filter_map(Instruction::decode)
            .any(|instruction| {
                matches!(instruction.opcode, Opcode::TypeCast | Opcode::TypeAssert)
                    && matches!(instruction.data, nsbc::InstructionData::A { imm12, .. }
                    if pool.canonical_type(TypeIndex::from_raw(imm12 as u32)) == Some(target))
            }),
        "exact !E i64 carrier must be an executable checked conversion target"
    );
    let id = artifact.type_pool.get(target).type_id;
    assert_ne!(id, TypeId::ZERO);
    let needle = [id.hi().to_le_bytes(), id.lo().to_le_bytes()].concat();
    let TypeKind::ErrorQualified { errors, inner } = &artifact.type_pool.get(target).kind else {
        unreachable!()
    };
    assert_eq!(errors.len(), 1);
    assert_eq!(*inner, Intrinsic::I64.type_index());
    for mutation in 0..6 {
        let corrupted = rewrite(&bytes, SectionKind::Metadata, |metadata| {
            let at = unique(metadata, &needle);
            assert_eq!(
                u32::from_le_bytes(metadata[at + 16..at + 20].try_into().unwrap()),
                24
            );
            match mutation {
                0 => metadata[at] ^= 1,
                1 => metadata[at + 8] ^= 1,
                2 => metadata[at + 16..at + 20].copy_from_slice(&16u32.to_le_bytes()),
                3 => metadata[at + 20..at + 24].copy_from_slice(&4u32.to_le_bytes()),
                4 | 5 => {
                    // TPOL11 kind 9: member count, member indices, then inner index.
                    // Verify the complete fixture record before editing any semantic edge.
                    assert_eq!(metadata[at + 24], 9);
                    assert_eq!(&metadata[at + 25..at + 29], &1u32.to_le_bytes());
                    assert_eq!(
                        &metadata[at + 29..at + 33],
                        &errors[0].as_u32().to_le_bytes()
                    );
                    assert_eq!(&metadata[at + 33..at + 37], &inner.as_u32().to_le_bytes());
                    let offset = if mutation == 4 { 29 } else { 33 };
                    metadata[at + offset..at + offset + 4]
                        .copy_from_slice(&Intrinsic::Bool.type_index().as_u32().to_le_bytes());
                }
                _ => unreachable!(),
            }
        });
        rejects_before_entry(&corrupted);
    }
}

#[test]
fn nsbc3_cannot_smuggle_error_success_lifts_conversions_or_typed_function_capability() {
    for source in [
        "enum E{bad};fn main(){let value:!E i64=42;println(value!{E.* =>0})}",
        "enum E{bad};fn convert(value:Any)->!E i64{value.as(!E i64)};fn main(){println(42)}",
        "enum E{bad};fn consume(value:!E i64)->i64{42};fn main(){println(42)}",
    ] {
        let artifact = compiled(source);
        let mut bytes = nsbc_io::write_artifact(&artifact).unwrap();
        assert_eq!(&bytes[4..8], &4u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&3u32.to_le_bytes());
        rejects_before_entry(&bytes);
    }
}

#[test]
fn genuine_unfinalized_nsbc3_preserves_legacy_error_metadata_but_never_reinterprets_payloads() {
    use nsbc::{
        BuiltinImport, CodegenOutput, CompiledArtifact, CompiledFunction, FuncId, ScopeCoverage,
    };
    let mut pool = TypePool::with_intrinsics();
    let error = pool.register(TypeInfo {
        kind: TypeKind::Enum {
            name: str_interner::intern("LegacyError"),
            variants: vec![type_pool::VariantInfo {
                name: str_interner::intern("bad"),
                tag: 0,
                fields: vec![],
            }],
        },
        type_id: TypeId(1234, 5678),
        size: 0,
        align: 0,
    });
    let legacy = pool.intern_structural(TypeKind::ErrorQualified {
        errors: vec![error],
        inner: Intrinsic::I64.type_index(),
    });
    assert_eq!(pool.get(legacy).size, 0);
    assert!(pool.identity_input().is_none());
    let signature = pool.intern_structural(TypeKind::Function {
        params: vec![],
        ret: Intrinsic::Unit.type_index(),
    });
    let artifact = CompiledArtifact {
        codegen_output: CodegenOutput {
            method_call_scopes: None,
            scope_coverage: ScopeCoverage::Calls,
            functions: vec![CompiledFunction {
                display_owner: None,
                func_id: FuncId(0),
                name: str_interner::intern("legacy_entry"),
                instructions: vec![
                    Instruction::load_imm(Reg(0), 42).encode(),
                    Instruction::call_builtin(1, 1).encode(),
                    Instruction::return_unit().encode(),
                ],
                register_count: 2,
                param_count: 0,
                is_closure: false,
                function_type: signature,
                abi: None,
                safepoint_pcs: vec![],
            }],
            constants: vec![Constant::Type(legacy)],
            globals: vec![],
        },
        type_pool: pool,
        entry: Some(FuncId(0)),
        builtin_abi_version: 1,
        builtins: vec![BuiltinImport {
            id: 1,
            name: "println".into(),
        }],
    };
    let mut bytes = nsbc_io::write_artifact(&artifact).unwrap();
    bytes[4..8].copy_from_slice(&3u32.to_le_bytes());
    let restored = nsbc_io::read_artifact(&bytes).unwrap();
    assert!(restored.type_pool.identity_input().is_none());
    assert_eq!(restored.type_pool.get(legacy).size, 0);
    assert_eq!(restored.type_pool.get(error).type_id, TypeId(1234, 5678));
    let d = Directory::new();
    let p = d.0.join("legacy.nsbc");
    std::fs::write(&p, &bytes).unwrap();
    let run = cli("run", &p);
    success(&run);
    assert_eq!(run.stdout, b"42\n");
    // Old descriptor reflection is metadata, not permission to instantiate a carrier.
    let mut invalid = nsbc_io::read_artifact(&bytes).unwrap();
    invalid.codegen_output.functions[0]
        .instructions
        .insert(0, Instruction::new_object(Reg(1), legacy).encode());
    assert!(nsbc_io::write_artifact(&invalid).is_err());
    for version in [1u32, 2, 5, u32::MAX] {
        let mut bad = bytes.clone();
        bad[4..8].copy_from_slice(&version.to_le_bytes());
        rejects_before_entry(&bad);
    }
}
