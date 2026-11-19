//! Binary codec for `SectionKind::Imports` (`5`) and `SectionKind::Exports` (`6`) in modular `.nsbc` archives.

use std::io;

use nsbc::{
    ArtifactExports, ArtifactImports, CaptureAbi, ExportTarget, ExportedRuntimeSymbol, FuncId,
    FunctionAbi, GlobalId, ImportedFunction, ImportedGlobal, ImportedModuleRef, ImportedPackageRef,
    InterfaceModuleStub, ModularArtifact, ModuleDescriptor, ParameterAbi, SectionKind,
    SymbolLinkSelector, validate_modular_artifact,
};
use type_pool::{TypeId, TypeIndex};

use crate::Archive;
use crate::artifact::{
    MAX_ITEMS, Reader, Writer, decode_core_sections, encode_core_sections, invalid,
};

const IMPORTS_MAGIC: &[u8; 4] = b"NSIM";
const IMPORTS_REVISION: u32 = 1;

const EXPORTS_MAGIC: &[u8; 4] = b"NSEX";
const EXPORTS_REVISION: u32 = 1;

fn write_type_id(writer: &mut Writer, id: TypeId) -> io::Result<()> {
    writer.u64(id.hi())?;
    writer.u64(id.lo())
}

fn read_type_id(reader: &mut Reader<'_>) -> io::Result<TypeId> {
    Ok(TypeId::from_pair(reader.u64()?, reader.u64()?))
}

fn write_opt_type_id(writer: &mut Writer, opt: Option<TypeId>) -> io::Result<()> {
    match opt {
        None => writer.data(&[0]),
        Some(id) => {
            writer.data(&[1])?;
            write_type_id(writer, id)
        }
    }
}

fn read_opt_type_id(reader: &mut Reader<'_>) -> io::Result<Option<TypeId>> {
    if reader.presence("optional TypeId")? {
        Ok(Some(read_type_id(reader)?))
    } else {
        Ok(None)
    }
}

fn write_string_vec(writer: &mut Writer, items: &[String]) -> io::Result<()> {
    writer.count(items.len())?;
    for item in items {
        writer.string(item)?;
    }
    Ok(())
}

fn read_string_vec(reader: &mut Reader<'_>, budget: &mut usize) -> io::Result<Vec<String>> {
    let count = reader.abi_count(4, budget)?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(reader.string()?);
    }
    Ok(out)
}

fn write_u32_vec(writer: &mut Writer, items: &[u32]) -> io::Result<()> {
    writer.count(items.len())?;
    for &item in items {
        writer.u32(item)?;
    }
    Ok(())
}

fn read_u32_vec(reader: &mut Reader<'_>, budget: &mut usize) -> io::Result<Vec<u32>> {
    let count = reader.abi_count(4, budget)?;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(reader.u32()?);
    }
    Ok(out)
}

fn write_abi(writer: &mut Writer, abi: &FunctionAbi) -> io::Result<()> {
    writer.count(abi.captures.len())?;
    for cap in &abi.captures {
        match cap {
            CaptureAbi::Value => writer.data(&[0])?,
            CaptureAbi::TraitProof { view } => {
                writer.data(&[1])?;
                writer.u32(view.as_u32())?;
            }
        }
    }
    writer.count(abi.parameters.len())?;
    for param in &abi.parameters {
        match param {
            ParameterAbi::Value => writer.data(&[0])?,
            ParameterAbi::Trait { view } => {
                writer.data(&[1])?;
                writer.u32(view.as_u32())?;
            }
            ParameterAbi::TraitSelf { view } => {
                writer.data(&[2])?;
                writer.u32(view.as_u32())?;
            }
        }
    }
    Ok(())
}

fn read_abi(reader: &mut Reader<'_>, budget: &mut usize) -> io::Result<FunctionAbi> {
    let cap_count = reader.abi_count(1, budget)?;
    if cap_count > nsbc::Reg::MAX_GP as usize {
        return Err(invalid("capture ABI count exceeds register limit"));
    }
    let mut captures = Vec::with_capacity(cap_count);
    for _ in 0..cap_count {
        captures.push(match reader.take(1)?[0] {
            0 => CaptureAbi::Value,
            1 => CaptureAbi::TraitProof {
                view: TypeIndex::from_raw(reader.u32()?),
            },
            _ => return Err(invalid("invalid capture ABI tag in modular section")),
        });
    }
    let param_count = reader.abi_count(1, budget)?;
    if param_count > nsbc::Reg::MAX_GP as usize {
        return Err(invalid("parameter ABI count exceeds register limit"));
    }
    let mut parameters = Vec::with_capacity(param_count);
    for _ in 0..param_count {
        parameters.push(match reader.take(1)?[0] {
            0 => ParameterAbi::Value,
            1 => ParameterAbi::Trait {
                view: TypeIndex::from_raw(reader.u32()?),
            },
            2 => ParameterAbi::TraitSelf {
                view: TypeIndex::from_raw(reader.u32()?),
            },
            _ => return Err(invalid("invalid parameter ABI tag in modular section")),
        });
    }
    Ok(FunctionAbi {
        captures,
        parameters,
    })
}

fn write_selector(writer: &mut Writer, selector: &SymbolLinkSelector) -> io::Result<()> {
    match selector {
        SymbolLinkSelector::ScopedSymbol {
            module_path,
            owner_type_id,
            name,
        } => {
            writer.data(&[0])?;
            write_string_vec(writer, module_path)?;
            write_opt_type_id(writer, *owner_type_id)?;
            writer.string(name)
        }
        SymbolLinkSelector::InherentMethod {
            receiver_type_id,
            name,
        } => {
            writer.data(&[1])?;
            write_type_id(writer, *receiver_type_id)?;
            writer.string(name)
        }
        SymbolLinkSelector::TraitMethod {
            trait_type_id,
            implementor_type_id,
            name,
        } => {
            writer.data(&[2])?;
            write_type_id(writer, *trait_type_id)?;
            write_type_id(writer, *implementor_type_id)?;
            writer.string(name)
        }
        SymbolLinkSelector::DerivedJsonFrom {
            implementor_type_id,
        } => {
            writer.data(&[3])?;
            write_type_id(writer, *implementor_type_id)
        }
        SymbolLinkSelector::DerivedJsonHelper {
            implementor_type_id,
            is_write,
        } => {
            writer.data(&[4, u8::from(*is_write)])?;
            write_type_id(writer, *implementor_type_id)
        }
        SymbolLinkSelector::DefaultTraitHelper {
            trait_type_id,
            implementor_type_id,
            method_name,
            nested_name,
        } => {
            writer.data(&[5])?;
            write_type_id(writer, *trait_type_id)?;
            write_type_id(writer, *implementor_type_id)?;
            writer.string(method_name)?;
            writer.string(nested_name)
        }
    }
}

fn read_selector(reader: &mut Reader<'_>, budget: &mut usize) -> io::Result<SymbolLinkSelector> {
    match reader.take(1)?[0] {
        0 => {
            let module_path = read_string_vec(reader, budget)?;
            let owner_type_id = read_opt_type_id(reader)?;
            let name = reader.string()?;
            Ok(SymbolLinkSelector::ScopedSymbol {
                module_path,
                owner_type_id,
                name,
            })
        }
        1 => {
            let receiver_type_id = read_type_id(reader)?;
            let name = reader.string()?;
            Ok(SymbolLinkSelector::InherentMethod {
                receiver_type_id,
                name,
            })
        }
        2 => {
            let trait_type_id = read_type_id(reader)?;
            let implementor_type_id = read_type_id(reader)?;
            let name = reader.string()?;
            Ok(SymbolLinkSelector::TraitMethod {
                trait_type_id,
                implementor_type_id,
                name,
            })
        }
        3 => Ok(SymbolLinkSelector::DerivedJsonFrom {
            implementor_type_id: read_type_id(reader)?,
        }),
        4 => {
            let is_write = match reader.take(1)?[0] {
                0 => false,
                1 => true,
                _ => return Err(invalid("invalid DerivedJsonHelper direction flag")),
            };
            let implementor_type_id = read_type_id(reader)?;
            Ok(SymbolLinkSelector::DerivedJsonHelper {
                implementor_type_id,
                is_write,
            })
        }
        5 => {
            let trait_type_id = read_type_id(reader)?;
            let implementor_type_id = read_type_id(reader)?;
            let method_name = reader.string()?;
            let nested_name = reader.string()?;
            Ok(SymbolLinkSelector::DefaultTraitHelper {
                trait_type_id,
                implementor_type_id,
                method_name,
                nested_name,
            })
        }
        _ => Err(invalid("invalid SymbolLinkSelector tag")),
    }
}

pub(crate) fn encode_imports_section(imports: &ArtifactImports) -> io::Result<Vec<u8>> {
    let mut writer = Writer(Vec::new());
    writer.data(IMPORTS_MAGIC)?;
    writer.u32(IMPORTS_REVISION)?;

    writer.count(imports.packages.len())?;
    for pkg in &imports.packages {
        writer.string(&pkg.qualified_name)?;
        writer.string(&pkg.version)?;
        writer.data(&pkg.identity)?;
        writer.data(&pkg.interface_hash)?;
    }

    writer.count(imports.functions.len())?;
    for func in &imports.functions {
        writer.u32(func.package_index)?;
        write_selector(&mut writer, &func.selector)?;
        writer.u32(func.function_type.as_u32())?;
        writer.data(&[func.param_count])?;
        write_abi(&mut writer, &func.abi)?;
        match func.display_owner {
            None => writer.data(&[0])?,
            Some(owner) => {
                writer.data(&[1])?;
                writer.u32(owner.as_u32())?;
            }
        }
    }

    writer.count(imports.globals.len())?;
    for glob in &imports.globals {
        writer.u32(glob.package_index)?;
        write_string_vec(&mut writer, &glob.module_path)?;
        write_opt_type_id(&mut writer, glob.owner_type_id)?;
        writer.string(&glob.name)?;
        writer.u32(glob.type_index.as_u32())?;
        writer.data(&[u8::from(glob.is_mutable)])?;
    }

    writer.count(imports.module_dependencies.len())?;
    for mdep in &imports.module_dependencies {
        writer.u32(mdep.package_index)?;
        write_string_vec(&mut writer, &mdep.module_path)?;
    }

    Ok(writer.0)
}

pub(crate) fn decode_imports_section(bytes: &[u8]) -> io::Result<ArtifactImports> {
    let mut reader = Reader { remaining: bytes };
    if reader.take(4)? != IMPORTS_MAGIC {
        return Err(invalid("invalid Imports section magic"));
    }
    let rev = reader.u32()?;
    if rev != IMPORTS_REVISION {
        return Err(invalid(format!(
            "unsupported Imports section revision {rev}; expected {IMPORTS_REVISION}"
        )));
    }
    let mut budget = MAX_ITEMS;

    let pkg_count = reader.abi_count(56, &mut budget)?;
    let mut packages = Vec::with_capacity(pkg_count);
    for _ in 0..pkg_count {
        let qualified_name = reader.string()?;
        let version = reader.string()?;
        let mut identity = [0u8; 16];
        identity.copy_from_slice(reader.take(16)?);
        let mut interface_hash = [0u8; 32];
        interface_hash.copy_from_slice(reader.take(32)?);
        packages.push(ImportedPackageRef {
            qualified_name,
            version,
            identity,
            interface_hash,
        });
    }

    let fn_count = reader.abi_count(16, &mut budget)?;
    let mut functions = Vec::with_capacity(fn_count);
    for _ in 0..fn_count {
        let package_index = reader.u32()?;
        let selector = read_selector(&mut reader, &mut budget)?;
        let function_type = TypeIndex::from_raw(reader.u32()?);
        let param_count = reader.take(1)?[0];
        let abi = read_abi(&mut reader, &mut budget)?;
        let display_owner = if reader.presence("imported Display owner")? {
            Some(TypeIndex::from_raw(reader.u32()?))
        } else {
            None
        };
        functions.push(ImportedFunction {
            package_index,
            selector,
            function_type,
            param_count,
            abi,
            display_owner,
        });
    }

    let glob_count = reader.abi_count(18, &mut budget)?;
    let mut globals = Vec::with_capacity(glob_count);
    for _ in 0..glob_count {
        let package_index = reader.u32()?;
        let module_path = read_string_vec(&mut reader, &mut budget)?;
        let owner_type_id = read_opt_type_id(&mut reader)?;
        let name = reader.string()?;
        let type_index = TypeIndex::from_raw(reader.u32()?);
        let is_mutable = reader.presence("imported global mutability")?;
        globals.push(ImportedGlobal {
            package_index,
            module_path,
            owner_type_id,
            name,
            type_index,
            is_mutable,
        });
    }

    let mdep_count = reader.abi_count(8, &mut budget)?;
    let mut module_dependencies = Vec::with_capacity(mdep_count);
    for _ in 0..mdep_count {
        let package_index = reader.u32()?;
        let module_path = read_string_vec(&mut reader, &mut budget)?;
        module_dependencies.push(ImportedModuleRef {
            package_index,
            module_path,
        });
    }

    reader.finish()?;
    Ok(ArtifactImports {
        packages,
        functions,
        globals,
        module_dependencies,
    })
}

pub(crate) fn encode_exports_section(exports: &ArtifactExports) -> io::Result<Vec<u8>> {
    let mut writer = Writer(Vec::new());
    writer.data(EXPORTS_MAGIC)?;
    writer.u32(EXPORTS_REVISION)?;
    writer.string(&exports.qualified_name)?;
    writer.string(&exports.version)?;
    writer.data(&exports.identity)?;
    writer.data(&exports.interface_hash)?;
    writer.string(&exports.manifest_toml)?;
    writer.data(&[u8::from(exports.is_privileged_std)])?;

    writer.count(exports.modules.len())?;
    for module in &exports.modules {
        write_string_vec(&mut writer, &module.module_path)?;
        write_opt_type_id(&mut writer, module.module_type_id)?;
        writer.count(module.owned_functions.len())?;
        for &fid in &module.owned_functions {
            writer.u32(fid.0)?;
        }
        writer.count(module.owned_globals.len())?;
        for &gid in &module.owned_globals {
            writer.u32(gid.0)?;
        }
        writer.count(module.scope_initializers.len())?;
        for &init_fid in &module.scope_initializers {
            writer.u32(init_fid.0)?;
        }
        write_u32_vec(&mut writer, &module.local_module_deps)?;
        write_u32_vec(&mut writer, &module.local_init_prereqs)?;
        write_u32_vec(&mut writer, &module.external_module_deps)?;
        write_u32_vec(&mut writer, &module.external_init_prereqs)?;
        match module.main_func {
            None => writer.data(&[0])?,
            Some(fid) => {
                writer.data(&[1])?;
                writer.u32(fid.0)?;
            }
        }
    }

    writer.count(exports.symbols.len())?;
    for sym in &exports.symbols {
        write_selector(&mut writer, &sym.selector)?;
        writer.u32(sym.owner_module_index)?;
        match &sym.target {
            ExportTarget::Function {
                func_id,
                function_type,
                param_count,
                abi,
                display_owner,
            } => {
                writer.data(&[0])?;
                writer.u32(func_id.0)?;
                writer.u32(function_type.as_u32())?;
                writer.data(&[*param_count])?;
                write_abi(&mut writer, abi)?;
                match display_owner {
                    None => writer.data(&[0])?,
                    Some(owner) => {
                        writer.data(&[1])?;
                        writer.u32(owner.as_u32())?;
                    }
                }
            }
            ExportTarget::Global {
                global_id,
                type_index,
                is_mutable,
            } => {
                writer.data(&[1])?;
                writer.u32(global_id.0)?;
                writer.u32(type_index.as_u32())?;
                writer.data(&[u8::from(*is_mutable)])?;
            }
            ExportTarget::Type {
                type_index,
                type_id,
            } => {
                writer.data(&[2])?;
                writer.u32(type_index.as_u32())?;
                write_type_id(&mut writer, *type_id)?;
            }
        }
    }

    writer.count(exports.interface_modules.len())?;
    for imod in &exports.interface_modules {
        write_string_vec(&mut writer, &imod.module_path)?;
        writer.string(&imod.stub_source)?;
    }

    Ok(writer.0)
}

pub(crate) fn decode_exports_section(bytes: &[u8]) -> io::Result<ArtifactExports> {
    let mut reader = Reader { remaining: bytes };
    if reader.take(4)? != EXPORTS_MAGIC {
        return Err(invalid("invalid Exports section magic"));
    }
    let rev = reader.u32()?;
    if rev != EXPORTS_REVISION {
        return Err(invalid(format!(
            "unsupported Exports section revision {rev}; expected {EXPORTS_REVISION}"
        )));
    }
    let mut budget = MAX_ITEMS;

    let qualified_name = reader.string()?;
    let version = reader.string()?;
    let mut identity = [0u8; 16];
    identity.copy_from_slice(reader.take(16)?);
    let mut interface_hash = [0u8; 32];
    interface_hash.copy_from_slice(reader.take(32)?);
    let manifest_toml = reader.string()?;
    let is_privileged_std = reader.presence("privileged std flag")?;

    let mod_count = reader.abi_count(30, &mut budget)?;
    let mut modules = Vec::with_capacity(mod_count);
    for _ in 0..mod_count {
        let module_path = read_string_vec(&mut reader, &mut budget)?;
        let module_type_id = read_opt_type_id(&mut reader)?;
        let owned_functions = read_u32_vec(&mut reader, &mut budget)?
            .into_iter()
            .map(FuncId)
            .collect();
        let owned_globals = read_u32_vec(&mut reader, &mut budget)?
            .into_iter()
            .map(GlobalId)
            .collect();
        let scope_initializers = read_u32_vec(&mut reader, &mut budget)?
            .into_iter()
            .map(FuncId)
            .collect();
        let local_module_deps = read_u32_vec(&mut reader, &mut budget)?;
        let local_init_prereqs = read_u32_vec(&mut reader, &mut budget)?;
        let external_module_deps = read_u32_vec(&mut reader, &mut budget)?;
        let external_init_prereqs = read_u32_vec(&mut reader, &mut budget)?;
        let main_func = if reader.presence("module main_func")? {
            Some(FuncId(reader.u32()?))
        } else {
            None
        };
        modules.push(ModuleDescriptor {
            module_path,
            module_type_id,
            owned_functions,
            owned_globals,
            scope_initializers,
            local_module_deps,
            local_init_prereqs,
            external_module_deps,
            external_init_prereqs,
            main_func,
        });
    }

    let sym_count = reader.abi_count(14, &mut budget)?;
    let mut symbols = Vec::with_capacity(sym_count);
    for _ in 0..sym_count {
        let selector = read_selector(&mut reader, &mut budget)?;
        let owner_module_index = reader.u32()?;
        let target = match reader.take(1)?[0] {
            0 => {
                let func_id = FuncId(reader.u32()?);
                let function_type = TypeIndex::from_raw(reader.u32()?);
                let param_count = reader.take(1)?[0];
                let abi = read_abi(&mut reader, &mut budget)?;
                let display_owner = if reader.presence("exported Display owner")? {
                    Some(TypeIndex::from_raw(reader.u32()?))
                } else {
                    None
                };
                ExportTarget::Function {
                    func_id,
                    function_type,
                    param_count,
                    abi,
                    display_owner,
                }
            }
            1 => {
                let global_id = GlobalId(reader.u32()?);
                let type_index = TypeIndex::from_raw(reader.u32()?);
                let is_mutable = reader.presence("exported global mutability")?;
                ExportTarget::Global {
                    global_id,
                    type_index,
                    is_mutable,
                }
            }
            2 => {
                let type_index = TypeIndex::from_raw(reader.u32()?);
                let type_id = read_type_id(&mut reader)?;
                ExportTarget::Type {
                    type_index,
                    type_id,
                }
            }
            _ => return Err(invalid("invalid ExportTarget tag")),
        };
        symbols.push(ExportedRuntimeSymbol {
            selector,
            owner_module_index,
            target,
        });
    }

    let imod_count = reader.abi_count(8, &mut budget)?;
    let mut interface_modules = Vec::with_capacity(imod_count);
    for _ in 0..imod_count {
        let module_path = read_string_vec(&mut reader, &mut budget)?;
        let stub_source = reader.string()?;
        interface_modules.push(InterfaceModuleStub {
            module_path,
            stub_source,
        });
    }

    reader.finish()?;
    Ok(ArtifactExports {
        qualified_name,
        version,
        identity,
        interface_hash,
        manifest_toml,
        is_privileged_std,
        modules,
        symbols,
        interface_modules,
    })
}

/// Serialize a validated [`ModularArtifact`] to `.nsbc` container bytes with
/// `Metadata`, `Code`, `Constants`, `StackMaps`, `Imports`, `Exports`, and optional `Wasm`.
pub fn write_modular_artifact(artifact: &ModularArtifact) -> io::Result<Vec<u8>> {
    validate_modular_artifact(artifact)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    let main_entry = artifact.exports.modules.iter().find_map(|m| m.main_func);
    let mut writer = encode_core_sections(
        &artifact.codegen_output,
        &artifact.type_pool,
        main_entry,
        artifact.builtin_abi_version,
        &artifact.builtins,
        &artifact.wasm_modules,
    )?;
    writer.add_raw_section(
        SectionKind::Imports,
        encode_imports_section(&artifact.imports)?,
    );
    writer.add_raw_section(
        SectionKind::Exports,
        encode_exports_section(&artifact.exports)?,
    );
    let mut bytes = Vec::new();
    writer.write_to(&mut bytes)?;
    Ok(bytes)
}

/// Decode and validate a complete [`ModularArtifact`] from `.nsbc` bytes.
pub fn read_modular_artifact(bytes: &[u8]) -> io::Result<ModularArtifact> {
    let mut input = bytes;
    let archive = Archive::read_from(&mut input)?;
    crate::container::validate_target(&archive.header)?;
    let mut imports_count = 0usize;
    let mut exports_count = 0usize;
    for (entry, _) in &archive.sections {
        if entry.name_idx != 0 {
            return Err(invalid(
                "named sections are not supported in modular artifact",
            ));
        }
        match SectionKind::from_u8(entry.kind) {
            Some(
                SectionKind::Metadata
                | SectionKind::Code
                | SectionKind::Constants
                | SectionKind::StackMaps
                | SectionKind::Wasm,
            ) => {}
            Some(SectionKind::Imports) => imports_count += 1,
            Some(SectionKind::Exports) => exports_count += 1,
            _ => return Err(invalid("unsupported section in modular artifact")),
        }
    }
    if imports_count != 1 || exports_count != 1 {
        return Err(invalid(
            "modular artifact requires exactly one Imports and one Exports section",
        ));
    }
    let core = decode_core_sections(&archive)?;
    let imports = decode_imports_section(
        archive
            .find_section(SectionKind::Imports)
            .ok_or_else(|| invalid("missing Imports section"))?,
    )?;
    let exports = decode_exports_section(
        archive
            .find_section(SectionKind::Exports)
            .ok_or_else(|| invalid("missing Exports section"))?,
    )?;
    let artifact = ModularArtifact {
        codegen_output: core.codegen_output,
        type_pool: core.type_pool,
        builtin_abi_version: core.builtin_abi_version,
        builtins: core.builtins,
        wasm_modules: core.wasm_modules,
        imports,
        exports,
    };
    validate_modular_artifact(&artifact).map_err(|error| invalid(error.to_string()))?;
    Ok(artifact)
}

/// Fast header-only read of `SectionKind::Exports` from a modular `.nsbc` archive,
/// skipping `Code`, `Constants`, `StackMaps`, and `TypePool` deserialization when checking
/// `interface_hash` or extracting compile-time interface stubs.
pub fn read_modular_exports_only(bytes: &[u8]) -> io::Result<ArtifactExports> {
    let mut input = bytes;
    let archive = Archive::read_from(&mut input)?;
    crate::container::validate_target(&archive.header)?;
    let exports_bytes = archive
        .find_section(SectionKind::Exports)
        .ok_or_else(|| invalid("missing Exports section in modular archive"))?;
    decode_exports_section(exports_bytes)
}

/// Returns `true` when `bytes` is an `.nsbc` container holding `SectionKind::Exports` (`6`).
pub fn is_modular_archive(bytes: &[u8]) -> bool {
    let mut input = bytes;
    let Ok(archive) = Archive::read_from(&mut input) else {
        return false;
    };
    archive.find_section(SectionKind::Exports).is_some()
        && archive.find_section(SectionKind::Imports).is_some()
}

#[cfg(test)]
mod tests {
    use nsbc::{
        CodegenOutput, CompiledFunction, Constant, Instruction, MethodCallScope, Reg,
        instruction_requires_scope,
    };
    use type_pool::{
        IdentityPathSegment, Intrinsic, NominalTypeProvenance, PackageTypeContext,
        TypeIdentityInput, TypeKind, TypePool,
    };

    use super::*;

    fn sample_modular_artifact() -> ModularArtifact {
        let mut pool = TypePool::with_intrinsics();
        pool.install_scopes(vec![type_pool::ScopeContext {
            parent: None,
            package: 0,
            assoc_type: None,
        }])
        .unwrap();
        let mod_ty = pool.register(type_pool::TypeInfo {
            kind: TypeKind::Module {
                name: str_interner::intern("sample_mod"),
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let fn_sig = pool.intern_structural(TypeKind::Function {
            params: vec![Intrinsic::I64.type_index()],
            ret: Intrinsic::I64.type_index(),
        });
        let init_sig = pool.intern_structural(TypeKind::Function {
            params: vec![],
            ret: Intrinsic::Unit.type_index(),
        });
        pool.finalize_type_identities(TypeIdentityInput {
            schema: 1,
            packages: vec![PackageTypeContext {
                identity_schema: 1,
                identity: [0x11; 16],
                qualified_name: "org.demo/sample".into(),
                version: "0.1.0".into(),
            }],
            declarations: vec![NominalTypeProvenance {
                type_index: mod_ty,
                package: 0,
                path: vec![
                    IdentityPathSegment::Lexical {
                        kind: 0,
                        ordinal: mod_ty.as_u32(),
                    },
                    IdentityPathSegment::Named("sample_mod".into()),
                ],
                last_stable_version: "0.1.0".into(),
            }],
        })
        .unwrap();
        let mod_tid = pool.get(mod_ty).type_id;

        let functions = vec![
            CompiledFunction {
                display_owner: None,
                func_id: FuncId(0),
                name: str_interner::intern("compute"),
                // Calls imported function FuncId(2) and loads imported global GlobalId(1)!
                instructions: vec![
                    Instruction::allocate_slots(1).encode(),
                    Instruction::call(2, 1).encode(),
                    Instruction::load_global(Reg(1), 1).encode(),
                    Instruction::add(Reg(0), Reg(0), Reg(1)).encode(),
                    Instruction::ret(Reg(0)).encode(),
                ],
                register_count: 2,
                param_count: 1,
                is_closure: false,
                function_type: fn_sig,
                abi: FunctionAbi::value_only(1),
                safepoint_pcs: vec![],
            },
            CompiledFunction {
                display_owner: None,
                func_id: FuncId(1),
                name: str_interner::intern("__module_init_0"),
                instructions: vec![
                    Instruction::load_imm(Reg(0), 10).encode(),
                    Instruction::store_global(0, Reg(0)).encode(),
                    Instruction::return_unit().encode(),
                ],
                register_count: 1,
                param_count: 0,
                is_closure: false,
                function_type: init_sig,
                abi: FunctionAbi::value_only(0),
                safepoint_pcs: vec![],
            },
        ];

        let mut method_call_scopes = Vec::new();
        for f in &functions {
            for (pc, &w) in f.instructions.iter().enumerate() {
                if let Some(inst) = Instruction::decode(w)
                    && instruction_requires_scope(inst.opcode)
                {
                    method_call_scopes.push(MethodCallScope {
                        func_id: f.func_id,
                        pc: pc as u32,
                        scope: 0,
                    });
                }
            }
        }

        ModularArtifact {
            codegen_output: CodegenOutput {
                functions,
                constants: vec![Constant::Int(99)],
                globals: vec![nsbc::GlobalInfo {
                    type_index: Intrinsic::I64.type_index(),
                    is_mutable: true,
                }],
                method_call_scopes,
            },
            type_pool: pool,
            builtin_abi_version: 1,
            builtins: vec![],
            wasm_modules: vec![],
            imports: ArtifactImports {
                packages: vec![ImportedPackageRef {
                    qualified_name: "org.demo/dep".into(),
                    version: "0.1.0".into(),
                    identity: [0x22; 16],
                    interface_hash: [0xAB; 32],
                }],
                functions: vec![ImportedFunction {
                    package_index: 0,
                    selector: SymbolLinkSelector::ScopedSymbol {
                        module_path: vec![],
                        owner_type_id: None,
                        name: "dep_double".into(),
                    },
                    function_type: fn_sig,
                    param_count: 1,
                    abi: FunctionAbi::value_only(1),
                    display_owner: None,
                }],
                globals: vec![ImportedGlobal {
                    package_index: 0,
                    module_path: vec![],
                    owner_type_id: None,
                    name: "DEP_OFFSET".into(),
                    type_index: Intrinsic::I64.type_index(),
                    is_mutable: false,
                }],
                module_dependencies: vec![ImportedModuleRef {
                    package_index: 0,
                    module_path: vec![],
                }],
            },
            exports: ArtifactExports {
                qualified_name: "org.demo/sample".into(),
                version: "0.1.0".into(),
                identity: [0x11; 16],
                interface_hash: [0xCD; 32],
                manifest_toml: "[package]\nname = \"sample\"\ndomain = \"org.demo\"\nversion = \"0.1.0\"\ntype = \"lib\"\n".into(),
                is_privileged_std: false,
                modules: vec![ModuleDescriptor {
                    module_path: vec![],
                    module_type_id: Some(mod_tid),
                    owned_functions: vec![FuncId(0), FuncId(1)],
                    owned_globals: vec![GlobalId(0)],
                    scope_initializers: vec![FuncId(1)],
                    local_module_deps: vec![],
                    local_init_prereqs: vec![],
                    external_module_deps: vec![0],
                    external_init_prereqs: vec![0],
                    main_func: None,
                }],
                symbols: vec![ExportedRuntimeSymbol {
                    selector: SymbolLinkSelector::ScopedSymbol {
                        module_path: vec![],
                        owner_type_id: Some(mod_tid),
                        name: "compute".into(),
                    },
                    owner_module_index: 0,
                    target: ExportTarget::Function {
                        func_id: FuncId(0),
                        function_type: fn_sig,
                        param_count: 1,
                        abi: FunctionAbi::value_only(1),
                        display_owner: None,
                    },
                }],
                interface_modules: vec![InterfaceModuleStub {
                    module_path: vec![],
                    stub_source: "pub fn compute(x: i64) -> i64 {}\n".into(),
                }],
            },
        }
    }

    #[test]
    fn modular_artifact_roundtrips_and_validates_imports_and_exports() {
        let artifact = sample_modular_artifact();
        let bytes = write_modular_artifact(&artifact).unwrap();
        assert!(is_modular_archive(&bytes));

        let exports_only = read_modular_exports_only(&bytes).unwrap();
        assert_eq!(exports_only, artifact.exports);

        let decoded = read_modular_artifact(&bytes).unwrap();
        assert_eq!(decoded.imports, artifact.imports);
        assert_eq!(decoded.exports, artifact.exports);
        assert_eq!(
            decoded.codegen_output.functions.len(),
            artifact.codegen_output.functions.len()
        );
        assert_eq!(write_modular_artifact(&decoded).unwrap(), bytes);

        // Standalone `read_artifact` must reject unlinked modular archives.
        assert!(crate::read_artifact(&bytes).is_err());
    }

    #[test]
    fn modular_artifact_rejects_corrupted_imports_exports_and_unclaimed_functions() {
        let mut bad_unclaimed = sample_modular_artifact();
        bad_unclaimed.exports.modules[0].owned_functions.remove(0);
        assert!(
            write_modular_artifact(&bad_unclaimed)
                .unwrap_err()
                .to_string()
                .contains("every local function must belong to a module descriptor")
        );

        let mut bad_import_pkg = sample_modular_artifact();
        bad_import_pkg.imports.functions[0].package_index = 99;
        assert!(
            write_modular_artifact(&bad_import_pkg)
                .unwrap_err()
                .to_string()
                .contains("package_index out of range")
        );

        let valid_bytes = write_modular_artifact(&sample_modular_artifact()).unwrap();
        let mut archive_in = valid_bytes.as_slice();
        let mut archive = Archive::read_from(&mut archive_in).unwrap();
        archive
            .sections
            .iter_mut()
            .find(|(e, _)| e.kind == SectionKind::Exports as u8)
            .unwrap()
            .1
            .push(0xFF);
        let mut trailing_bytes = Vec::new();
        let mut w = crate::ArchiveWriter::new();
        for (entry, data) in archive.sections {
            w.add_raw_section(SectionKind::from_u8(entry.kind).unwrap(), data);
        }
        w.write_to(&mut trailing_bytes).unwrap();
        assert!(
            read_modular_artifact(&trailing_bytes)
                .unwrap_err()
                .to_string()
                .contains("trailing bytes")
        );
    }
}
