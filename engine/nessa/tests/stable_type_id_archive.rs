//! Independent fresh-process identity replay and checksum-rebuilt metadata corruption.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use nsbc::SectionKind;
use nsbc_io::{Archive, ArchiveWriter};
use type_pool::{Intrinsic, TypeId, TypeIndex, TypeInfo, TypeKind, TypePool};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let p = std::env::temp_dir().join(format!(
                "nessa-stable-id-{}-{}",
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
fn success(o: &Output) {
    assert!(
        o.status.success(),
        "{}: stdout={} stderr={}",
        o.status,
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
}
fn declared_id(pool: &TypePool, name: &str) -> (TypeIndex, TypeId) {
    let found: Vec<_> = (0..pool.len())
        .filter_map(|i| {
            let t = TypeIndex::from_raw(i as u32);
            let n = match pool.get(t).kind {
                TypeKind::Struct { name, .. }
                | TypeKind::Enum { name, .. }
                | TypeKind::Typealias { name, .. } => name,
                _ => return None,
            };
            if str_interner::get(n)!=name{return None;}
            // This archive fixture's Step alias denotes specifically Step(Payload),
            // distinct from standard-library aliases with the same simple name.
            if name=="Step" {
                let item=pool.checked_iteration_step_item(t).ok().flatten()?;
                if !matches!(pool.get(item).kind,TypeKind::Struct{name,..}if str_interner::get(name)=="Payload"){return None;}
            }
            Some(t)
        })
        .collect();
    assert!(!found.is_empty(), "{name}: no semantic source type");
    let canonical: std::collections::HashSet<_> = found
        .iter()
        .map(|&t| pool.canonical_type(t).unwrap())
        .collect();
    assert_eq!(
        canonical.len(),
        1,
        "{name}: ambiguous semantic target {found:?}"
    );
    let t = *canonical.iter().next().unwrap();
    let id = pool.stable_type_id(t).unwrap();
    assert_ne!(id, TypeId::ZERO);
    for alias in found {
        assert_eq!(pool.stable_type_id(alias).unwrap(), id);
        assert_eq!(pool.get(alias).type_id, id);
    }
    (t, id)
}
fn rewrite_metadata(bytes: &[u8], edit: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
    let mut archive = Archive::read_from(&mut &bytes[..]).unwrap();
    edit(
        &mut archive
            .sections
            .iter_mut()
            .find(|(entry, _)| entry.kind == SectionKind::Metadata as u8)
            .unwrap()
            .1,
    );
    let mut writer = ArchiveWriter::new();
    for (entry, body) in archive.sections {
        writer.add_raw_section(SectionKind::from_u8(entry.kind).unwrap(), body);
    }
    let mut result = vec![];
    writer.write_to(&mut result).unwrap();
    result
}
fn occurrence(bytes: &[u8], needle: &[u8]) -> usize {
    let positions: Vec<_> = bytes
        .windows(needle.len())
        .enumerate()
        .filter_map(|(i, b)| (b == needle).then_some(i))
        .collect();
    assert_eq!(
        positions.len(),
        1,
        "ambiguous metadata test fixture: {positions:?}"
    );
    positions[0]
}
fn serialized_id(id: TypeId) -> Vec<u8> {
    [id.hi().to_le_bytes(), id.lo().to_le_bytes()].concat()
}

#[test]
fn source_deleted_archives_replay_full_ids_and_concrete_payloads_in_polluted_process() {
    for (source, name) in [
        (
            "struct Payload{n:i64};typealias Alias=Payload;fn main(){let value:Any=Payload{n:42};println(value.as(Alias).n)}",
            "Payload",
        ),
        (
            "struct Node{next:?Node,n:i64};fn main(){let root=Node{next:Node{next:null,n:42},n:0};println(root.next.unwrap().n)}",
            "Node",
        ),
        (
            "struct Payload{n:i64};trait Source{assoc Item:Type=Self;derive fn next(self)->IterationStep(Item){IterationStep(Item).yielded(self)}};impl Source for Payload{};typealias Step=IterationStep(Payload);fn main(){Payload{n:42}.next() match{Step.yielded(value)=>println(value.n),_=>println(0)}}",
            "Step",
        ),
        (
            "struct Payload{n:i64};fn make()->fn()->i64{let value=Payload{n:42};||value.n};fn main(){println(make()())}",
            "Payload",
        ),
    ] {
        let directory = Directory::new();
        let source_path = directory.0.join("program.ns");
        std::fs::write(&source_path, source).unwrap();
        let direct = cli("run", &source_path);
        success(&direct);
        assert_eq!(direct.stdout, b"42\n");
        success(&cli("build", &source_path));
        let archive_path = source_path.with_extension("nsbc");
        let bytes = std::fs::read(&archive_path).unwrap();
        let artifact = nsbc_io::read_artifact(&bytes).unwrap();
        let (index, id) = declared_id(&artifact.type_pool, name);
        let compiled = driver::Driver::new()
            .compile(source)
            .into_artifact()
            .unwrap();
        assert_eq!(
            declared_id(&compiled.type_pool, name).1,
            id,
            "source compilation and CLI archive must agree on all128bits"
        );
        assert!(artifact.type_pool.identity_input().is_some());
        let archive = Archive::read_from(&mut &bytes[..]).unwrap();
        let metadata = archive.find_section(SectionKind::Metadata).unwrap();
        let at = occurrence(metadata, b"TPOL");
        assert_eq!(
            u32::from_le_bytes(metadata[at + 4..at + 8].try_into().unwrap()),
            11
        );
        std::fs::remove_file(&source_path).unwrap();
        assert!(!source_path.exists());
        let run = cli("run", &archive_path);
        success(&run);
        assert_eq!(run.stdout, b"42\n");
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "identity_replay_child", "--nocapture"])
            .env("NESSA_STABLE_ID_ARCHIVE", &archive_path)
            .env("NESSA_STABLE_ID_NAME", name)
            .env("NESSA_STABLE_ID_INDEX", index.as_u32().to_string())
            .env("NESSA_STABLE_ID_HI", id.hi().to_string())
            .env("NESSA_STABLE_ID_LO", id.lo().to_string())
            .output()
            .unwrap();
        success(&child);
        assert!(String::from_utf8_lossy(&child.stdout).contains("42\n"));
    }
}

#[test]
fn identity_replay_child() {
    let Some(path) = std::env::var_os("NESSA_STABLE_ID_ARCHIVE") else {
        return;
    };
    for i in 0..6000 {
        str_interner::intern(&format!("independent-identity-name-{i}"));
    }
    let _ = driver::Driver::new().compile("struct Unrelated{v:bool};fn main(){false}");
    let bytes = std::fs::read(&path).unwrap();
    let artifact = nsbc_io::read_artifact(&bytes).unwrap();
    let name = std::env::var("NESSA_STABLE_ID_NAME").unwrap();
    let (index, id) = declared_id(&artifact.type_pool, &name);
    assert_eq!(
        index.as_u32(),
        std::env::var("NESSA_STABLE_ID_INDEX")
            .unwrap()
            .parse::<u32>()
            .unwrap()
    );
    assert_eq!(
        id.hi(),
        std::env::var("NESSA_STABLE_ID_HI")
            .unwrap()
            .parse::<u64>()
            .unwrap()
    );
    assert_eq!(
        id.lo(),
        std::env::var("NESSA_STABLE_ID_LO")
            .unwrap()
            .parse::<u64>()
            .unwrap()
    );
    assert!(matches!(
        driver::Driver::new().run_artifact(artifact),
        driver::RunResult::Ok
    ));
}

#[test]
fn unique_identity_halves_and_layout_tampering_fail_after_checksum_rebuild() {
    let directory = Directory::new();
    let source_path = directory.0.join("tamper.ns");
    std::fs::write(
        &source_path,
        "struct Payload{n:i64};fn main(){println(Payload{n:42}.n)}",
    )
    .unwrap();
    success(&cli("build", &source_path));
    let bytes = std::fs::read(source_path.with_extension("nsbc")).unwrap();
    let artifact = nsbc_io::read_artifact(&bytes).unwrap();
    let (_, id) = declared_id(&artifact.type_pool, "Payload");
    // This fixture has no alias: each full identity occurs exactly once, and
    // its persisted provenance must expose either half changing independently.
    for mutation in 0..4 {
        let damaged = rewrite_metadata(&bytes, |metadata| {
            let encoded = serialized_id(id);
            let at = occurrence(metadata, &encoded);
            match mutation {
                0 => metadata[at] ^= 0x80,
                1 => metadata[at + 15] ^= 0x40,
                2 => metadata[at + 16..at + 20].copy_from_slice(&16u32.to_le_bytes()),
                3 => metadata[at..at + 16].fill(0),
                _ => unreachable!(),
            }
        });
        let error = nsbc_io::read_artifact(&damaged).expect_err("tamper accepted");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        let path = directory.0.join(format!("bad-{mutation}.nsbc"));
        std::fs::write(&path, &damaged).unwrap();
        let output = cli("run", &path);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty(), "main must not execute");
    }
    // A unique source identity has no alias collision to help the reader. This
    // regression specifically requires persisted recomputation of both halves.
    let unique = driver::Driver::new()
        .compile("struct Unique{n:i64};fn main(){Unique{n:42}.n}")
        .into_artifact()
        .unwrap();
    let (unique_index, unique_id) = declared_id(&unique.type_pool, "Unique");
    let unique_bytes = nsbc_io::write_artifact(&unique).unwrap();
    for half in [0, 8] {
        let damaged = rewrite_metadata(&unique_bytes, |metadata| {
            let at = occurrence(metadata, &serialized_id(unique_id));
            metadata[at + half] ^= 1;
        });
        assert!(nsbc_io::read_artifact(&damaged).is_err());
    }
    let input = unique.type_pool.identity_input().unwrap();
    let declaration = input
        .declarations
        .iter()
        .find(|d| d.type_index == unique_index)
        .unwrap();
    let package = &input.packages[declaration.package as usize];
    let damaged = rewrite_metadata(&unique_bytes, |metadata| {
        let at = occurrence(metadata, &package.identity);
        metadata[at + 15] ^= 1;
    });
    assert!(
        nsbc_io::read_artifact(&damaged).is_err(),
        "claimed package changed without recomputing IDs"
    );
    let corrupted = rewrite_metadata(&unique_bytes, |metadata| {
        let at = occurrence(metadata, b"TPOL");
        metadata[at + 4..at + 8].copy_from_slice(&u32::MAX.to_le_bytes());
    });
    assert!(nsbc_io::read_artifact(&corrupted).is_err());
}

#[test]
fn malformed_source_never_emits_archive() {
    for source in [
        "newtype Distance=i64;fn main(){42}",
        "typealias A=B;typealias B=A;fn main(){42}",
        "trait T{assoc Item:Type=i64};fn main(){IterationStep(T.Item)}",
    ] {
        let directory = Directory::new();
        let path = directory.0.join("bad.ns");
        std::fs::write(&path, source).unwrap();
        let output = cli("build", &path);
        assert!(!output.status.success(), "accepted {source}");
        assert!(!path.with_extension("nsbc").exists());
    }
}

#[test]
fn genuinely_unfinalized_legacy_pool_retains_zero_and_exact_indices() {
    use nsbc::{
        BuiltinImport, CodegenOutput, CompiledArtifact, CompiledFunction, FuncId, Instruction, Reg,
        ScopeCoverage,
    };
    let mut pool = TypePool::with_intrinsics();
    let mut indices = vec![];
    for name in ["LegacyA", "LegacyB"] {
        indices.push(pool.register(TypeInfo {
            kind: TypeKind::Struct {
                name: str_interner::intern(name),
                fields: vec![],
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        }));
    }
    let signature = pool.intern_structural(TypeKind::Function {
        params: vec![],
        ret: Intrinsic::Unit.type_index(),
    });
    assert!(pool.identity_input().is_none());
    assert_eq!(pool.lookup_by_id(TypeId::ZERO), None);
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
                register_count: 1,
                param_count: 0,
                is_closure: false,
                function_type: signature,
                abi: None,
                safepoint_pcs: vec![],
            }],
            constants: vec![],
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
    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
    let archive = Archive::read_from(&mut &bytes[..]).unwrap();
    let metadata = archive.find_section(SectionKind::Metadata).unwrap();
    let tpol = occurrence(metadata, b"TPOL");
    let revision = u32::from_le_bytes(metadata[tpol + 4..tpol + 8].try_into().unwrap());
    assert!((1..=10).contains(&revision));
    let restored = nsbc_io::read_artifact(&bytes).unwrap();
    assert!(restored.type_pool.identity_input().is_none());
    for &index in &indices {
        assert_eq!(restored.type_pool.get(index).type_id, TypeId::ZERO);
        assert!(restored.type_pool.stable_type_id(index).is_err());
    }
    assert_ne!(indices[0], indices[1]);
    assert_eq!(restored.type_pool.lookup_by_id(TypeId::ZERO), None);
    assert_eq!(nsbc_io::write_artifact(&restored).unwrap(), bytes);
    let directory = Directory::new();
    let path = directory.0.join("legacy.nsbc");
    std::fs::write(&path, bytes).unwrap();
    let result = cli("run", &path);
    success(&result);
    assert_eq!(result.stdout, b"42\n");
}

// Transport-only encoder from the frozen TPOL11 schema, used to locate and replace
// complete provenance records after rebuilding the container checksum. It is not
// an oracle for the TypeId hash algorithm.
fn input_bytes(input: &type_pool::TypeIdentityInput) -> Vec<u8> {
    fn number(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }
    fn text(out: &mut Vec<u8>, value: &str) {
        number(out, value.len().try_into().unwrap());
        out.extend_from_slice(value.as_bytes());
    }
    let mut out = vec![];
    number(&mut out, input.schema);
    number(&mut out, input.packages.len().try_into().unwrap());
    for package in &input.packages {
        number(&mut out, package.identity_schema);
        out.extend_from_slice(&package.identity);
        text(&mut out, &package.qualified_name);
        text(&mut out, &package.version);
    }
    number(&mut out, input.declarations.len().try_into().unwrap());
    for d in &input.declarations {
        number(&mut out, d.type_index.as_u32());
        number(&mut out, d.package);
        number(&mut out, d.path.len().try_into().unwrap());
        for segment in &d.path {
            match segment {
                type_pool::IdentityPathSegment::Named(name) => {
                    out.push(0);
                    text(&mut out, name);
                }
                type_pool::IdentityPathSegment::Lexical { kind, ordinal } => {
                    out.extend_from_slice(&[1, *kind]);
                    number(&mut out, *ordinal);
                }
            }
        }
        text(&mut out, &d.last_stable_version);
    }
    out
}

#[test]
fn provenance_schema_paths_versions_and_completeness_reject_with_valid_container_checksum() {
    let directory = Directory::new();
    let artifact = driver::Driver::new()
        .compile("struct Unique{n:i64};fn main(){println(Unique{n:42}.n)}")
        .into_artifact()
        .unwrap();
    let (unique_index, _) = declared_id(&artifact.type_pool, "Unique");
    let original = artifact.type_pool.identity_input().unwrap();
    let original_bytes = input_bytes(original);
    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
    let d = original
        .declarations
        .iter()
        .position(|d| d.type_index == unique_index)
        .unwrap();
    for mutation in 0..9 {
        let mut changed = original.clone();
        match mutation {
            0 => changed.schema = 999,
            1 => changed.packages[changed.declarations[d].package as usize].identity_schema = 999,
            2 => changed.declarations[d]
                .path
                .push(type_pool::IdentityPathSegment::Named("forged".into())),
            3 => changed.declarations[d].last_stable_version = "9.8.7".into(),
            4 => changed.declarations[d].package = u32::MAX,
            5 => {
                changed.declarations.remove(d);
            }
            6 => changed.declarations.push(changed.declarations[d].clone()),
            7 => changed.declarations[d].type_index = TypeIndex::from_raw(u32::MAX),
            8 => {
                changed.packages[changed.declarations[d].package as usize].version =
                    "invalid".into()
            }
            _ => unreachable!(),
        }
        let damaged = rewrite_metadata(&bytes, |metadata| {
            let at = occurrence(metadata, &original_bytes);
            metadata.splice(at..at + original_bytes.len(), input_bytes(&changed));
        });
        let error = nsbc_io::read_artifact(&damaged).expect_err("provenance accepted");
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::InvalidData,
            "mutation {mutation}"
        );
        let path = directory.0.join(format!("provenance-{mutation}.nsbc"));
        std::fs::write(&path, &damaged).unwrap();
        let output = cli("run", &path);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty(), "must reject before entry");
    }
    for mutation in 0..2 {
        let damaged = rewrite_metadata(&bytes, |metadata| {
            if mutation == 0 {
                metadata.pop();
            } else {
                metadata.push(0);
            }
        });
        assert!(
            nsbc_io::read_artifact(&damaged).is_err(),
            "truncated or trailing finalized input accepted"
        );
    }
}

#[test]
fn finalized_abstract_holder_roundtrips_metadata_but_never_becomes_executable() {
    use nsbc::{
        AddrMode, CodegenOutput, CompiledArtifact, CompiledFunction, Constant, FuncId, GlobalInfo,
        Instruction, Opcode, Reg, ScopeCoverage,
    };
    use type_pool::{
        FieldInfo, IdentityPathSegment, NominalTypeProvenance, PackageTypeContext,
        TypeIdentityInput,
    };
    let mut pool = TypePool::with_intrinsics();
    let item_name = str_interner::intern("Item");
    let owner = pool.register(TypeInfo {
        kind: TypeKind::Trait {
            name: str_interner::intern("Source"),
            parents: vec![],
            assoc_types: vec![(item_name, Intrinsic::Any.type_index())],
        },
        type_id: TypeId::ZERO,
        size: 0,
        align: 0,
    });
    let item = pool.register(TypeInfo {
        kind: TypeKind::AssociatedType {
            trait_owner: owner,
            name: item_name,
        },
        type_id: TypeId::ZERO,
        size: 0,
        align: 0,
    });
    if let TypeKind::Trait { assoc_types, .. } = &mut pool.get_mut(owner).kind {
        assoc_types[0].1 = item;
    }
    pool.register_associated_default(type_pool::AssociatedTypeDefault {
        trait_owner: owner,
        name: item_name,
        expression: type_pool::AssociatedTypeExpr::Required,
    })
    .unwrap();
    let holder = pool.register(TypeInfo {
        kind: TypeKind::Struct {
            name: str_interner::intern("Holder"),
            fields: vec![FieldInfo {
                name: str_interner::intern("value"),
                ty: item,
                offset: 0,
                has_default: false,
            }],
        },
        type_id: TypeId::ZERO,
        size: 8,
        align: 8,
    });
    let signature = pool.intern_structural(TypeKind::Function {
        params: vec![],
        ret: Intrinsic::Unit.type_index(),
    });
    let abstract_signature = pool.intern_structural(TypeKind::Function {
        params: vec![],
        ret: holder,
    });
    let declarations = [(owner, "Source"), (holder, "Holder")]
        .into_iter()
        .map(|(type_index, name)| NominalTypeProvenance {
            type_index,
            package: 0,
            path: vec![IdentityPathSegment::Named(name.into())],
            last_stable_version: "1.2.3".into(),
        })
        .collect();
    pool.finalize_type_identities(TypeIdentityInput {
        schema: 1,
        packages: vec![PackageTypeContext {
            identity_schema: 1,
            identity: [0x63; 16],
            qualified_name: "example.org/abstract-security".into(),
            version: "1.2.3".into(),
        }],
        declarations,
    })
    .unwrap();
    assert_eq!(pool.get(holder).type_id, TypeId::ZERO);
    assert!(pool.stable_type_id(holder).is_err());
    pool.validate().unwrap();
    let artifact = CompiledArtifact {
        codegen_output: CodegenOutput {
            method_call_scopes: None,
            scope_coverage: ScopeCoverage::Calls,
            functions: vec![CompiledFunction {
                display_owner: None,
                func_id: FuncId(0),
                name: str_interner::intern("metadata_entry"),
                instructions: vec![Instruction::return_unit().encode()],
                register_count: 2,
                param_count: 0,
                is_closure: false,
                function_type: signature,
                abi: None,
                safepoint_pcs: vec![],
            }],
            constants: vec![],
            globals: vec![],
        },
        type_pool: pool,
        entry: Some(FuncId(0)),
        builtin_abi_version: 1,
        builtins: vec![],
    };
    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
    let restored = nsbc_io::read_artifact(&bytes).unwrap();
    assert!(restored.type_pool.identity_input().is_some());
    assert_eq!(restored.type_pool.get(holder).type_id, TypeId::ZERO);
    assert!(restored.type_pool.stable_type_id(holder).is_err());
    let archive = Archive::read_from(&mut &bytes[..]).unwrap();
    let metadata = archive.find_section(SectionKind::Metadata).unwrap();
    let at = occurrence(metadata, b"TPOL");
    assert_eq!(
        u32::from_le_bytes(metadata[at + 4..at + 8].try_into().unwrap()),
        11
    );
    let TypeKind::Struct { fields, .. } = &restored.type_pool.get(holder).kind else {
        panic!("Holder descriptor lost")
    };
    assert_eq!(fields[0].ty, item);
    assert_eq!(nsbc_io::write_artifact(&restored).unwrap(), bytes);

    // Replace only the executable function's type reference; the finalized pool
    // and its valid abstract metadata remain unchanged. Rebuild the checksum so
    // this exercises the reader's executable-type validation, not framing.
    let damaged = rewrite_metadata(&bytes, |metadata| {
        let name = b"metadata_entry";
        let at = occurrence(metadata, name) + name.len();
        assert_eq!(&metadata[at..at + 4], &signature.as_u32().to_le_bytes());
        metadata[at..at + 4].copy_from_slice(&abstract_signature.as_u32().to_le_bytes());
    });
    let error = nsbc_io::read_artifact(&damaged)
        .expect_err("reader accepted abstract executable signature");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        error
            .to_string()
            .contains("unresolved associated type is not an executable type"),
        "{error}"
    );
    let directory = Directory::new();
    let path = directory.0.join("abstract-entry.nsbc");
    std::fs::write(&path, &damaged).unwrap();
    let output = cli("run", &path);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    for mutation in 0..5 {
        let mut invalid = nsbc_io::read_artifact(&bytes).unwrap();
        match mutation {
            0 => invalid.codegen_output.globals.push(GlobalInfo {
                type_index: holder,
                is_mutable: false,
            }),
            1 => invalid
                .codegen_output
                .constants
                .push(Constant::Type(holder)),
            2 => invalid.codegen_output.functions[0].function_type = abstract_signature,
            3 => invalid.codegen_output.functions[0]
                .instructions
                .insert(0, Instruction::new_object(Reg(0), holder).encode()),
            4 => invalid.codegen_output.functions[0].instructions.insert(
                0,
                Instruction::a_type(
                    Opcode::TypeCheck,
                    AddrMode::Imm,
                    Reg(1),
                    Reg(0),
                    holder.as_u32().try_into().unwrap(),
                )
                .encode(),
            ),
            _ => unreachable!(),
        }
        let error = nsbc_io::write_artifact(&invalid)
            .expect_err("finalized abstract Holder became executable");
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::InvalidInput,
            "mutation {mutation}: {error}"
        );
        assert!(
            error
                .to_string()
                .contains("unresolved associated type is not an executable type"),
            "mutation {mutation}: {error}"
        );
    }
}
