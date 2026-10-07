//! Complete artifact codec, explicitly distinguished from legacy CODE layout.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use nsbc::{
    BuiltinImport, CaptureAbi, CodegenOutput, CompiledArtifact, CompiledFunction, FuncId,
    FunctionAbi, GlobalInfo, MethodCallScope, ParameterAbi, ScopeCoverage, SectionKind,
    validate_artifact,
};
use type_pool::TypeIndex;

use crate::{Archive, ArchiveWriter, MAX_ARCHIVE_SIZE, relocation, type_metadata};

const METADATA_MAGIC: &[u8; 4] = b"NSAM";
const METADATA_REVISION: u32 = 6;
const TAGGED_ROOT_SCAN: u32 = 0;
const MAX_ITEMS: usize = 1_000_000;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

struct Writer(Vec<u8>);

impl Writer {
    fn data(&mut self, bytes: &[u8]) -> io::Result<()> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|length| length > MAX_ARCHIVE_SIZE)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "artifact section exceeds MAX_ARCHIVE_SIZE",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(())
    }
    fn u32(&mut self, value: u32) -> io::Result<()> {
        self.data(&value.to_le_bytes())
    }
    fn count(&mut self, count: usize) -> io::Result<()> {
        let count = u32::try_from(count)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "artifact count overflow"))?;
        self.u32(count)
    }
    fn blob(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.count(bytes.len())?;
        self.data(bytes)
    }
    fn string(&mut self, value: &str) -> io::Result<()> {
        self.blob(value.as_bytes())
    }
}

struct Reader<'a> {
    remaining: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> io::Result<&'a [u8]> {
        if length > self.remaining.len() {
            return Err(invalid("truncated artifact section"));
        }
        let (bytes, remaining) = self.remaining.split_at(length);
        self.remaining = remaining;
        Ok(bytes)
    }
    fn u32(&mut self) -> io::Result<u32> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }
    fn count(&mut self, minimum_size: usize) -> io::Result<usize> {
        let count = self.u32()? as usize;
        if count > MAX_ITEMS || count > self.remaining.len() / minimum_size {
            return Err(invalid(
                "artifact count exceeds section data or resource limit",
            ));
        }
        Ok(count)
    }
    fn presence(&mut self, context: &str) -> io::Result<bool> {
        match self.take(1)?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid(format!("invalid {context} presence tag"))),
        }
    }
    fn abi_count(&mut self, minimum: usize, budget: &mut usize) -> io::Result<usize> {
        let count = self.count(minimum)?;
        *budget = budget
            .checked_sub(count)
            .ok_or_else(|| invalid("function ABI cumulative item limit exceeded"))?;
        Ok(count)
    }
    fn blob(&mut self) -> io::Result<&'a [u8]> {
        let count = self.u32()? as usize;
        self.take(count)
    }
    fn string(&mut self) -> io::Result<String> {
        std::str::from_utf8(self.blob()?)
            .map(str::to_owned)
            .map_err(|_| invalid("invalid UTF-8 in artifact name"))
    }
    fn finish(&self) -> io::Result<()> {
        if !self.remaining.is_empty() {
            return Err(invalid("trailing bytes in artifact section"));
        }
        Ok(())
    }
}

/// Serialize a self-contained artifact with exact-index type metadata and
/// method-name relocation. Unsupported encoding capacities return an error.
pub fn write_artifact(artifact: &CompiledArtifact) -> io::Result<Vec<u8>> {
    let pool_bytes = type_metadata::encode_pool(&artifact.type_pool)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    validate_artifact(artifact)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    let relocated = relocation::encode(&artifact.codegen_output)?;
    let mut metadata = Writer(Vec::new());
    metadata.data(METADATA_MAGIC)?;
    let revision = match (
        artifact.codegen_output.scope_coverage,
        &artifact.codegen_output.method_call_scopes,
    ) {
        (ScopeCoverage::Calls, None) => 1,
        (ScopeCoverage::Calls, Some(_)) => 2,
        (ScopeCoverage::CallsAndTypes, Some(_)) => 3,
        (ScopeCoverage::CallsAndTypes, None) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "complete call and type contexts require a scope table",
            ));
        }
    };
    let trait_self = artifact
        .codegen_output
        .functions
        .iter()
        .filter_map(|function| function.abi.as_ref())
        .any(|abi| {
            abi.parameters
                .iter()
                .any(|parameter| matches!(parameter, ParameterAbi::TraitSelf { .. }))
        });
    let revision = if artifact
        .codegen_output
        .functions
        .iter()
        .any(|function| function.display_owner.is_some())
    {
        METADATA_REVISION
    } else if trait_self {
        5
    } else if artifact
        .codegen_output
        .functions
        .iter()
        .any(|function| function.abi.is_some())
    {
        4
    } else {
        revision
    };
    metadata.u32(revision)?;
    metadata.u32(TAGGED_ROOT_SCAN)?;
    metadata.u32(artifact.builtin_abi_version)?;
    metadata.u32(artifact.entry.map_or(u32::MAX, |entry| entry.0))?;
    metadata.blob(&pool_bytes)?;
    metadata.count(artifact.codegen_output.globals.len())?;
    for global in &artifact.codegen_output.globals {
        metadata.u32(global.type_index.as_u32())?;
        metadata.data(&[u8::from(global.is_mutable)])?;
    }
    metadata.count(artifact.codegen_output.functions.len())?;
    for function in &artifact.codegen_output.functions {
        metadata.u32(function.func_id.0)?;
        let name = str_interner::try_get(function.name).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid interned function name",
            )
        })?;
        metadata.string(&name)?;
        metadata.u32(function.function_type.as_u32())?;
    }
    metadata.count(relocated.methods.len())?;
    for (slot, name) in &relocated.methods {
        metadata.u32(*slot)?;
        metadata.string(name)?;
    }
    metadata.count(artifact.builtins.len())?;
    for builtin in &artifact.builtins {
        metadata.u32(builtin.id)?;
        metadata.string(&builtin.name)?;
    }

    if revision >= 4 {
        metadata.data(&[match artifact.codegen_output.scope_coverage {
            ScopeCoverage::Calls => 0,
            ScopeCoverage::CallsAndTypes => 1,
        }])?;
        metadata.data(&[u8::from(
            artifact.codegen_output.method_call_scopes.is_some(),
        )])?;
    }
    if let Some(scopes) = &artifact.codegen_output.method_call_scopes {
        metadata.count(scopes.len())?;
        for callsite in scopes {
            metadata.u32(callsite.func_id.0)?;
            metadata.u32(callsite.pc)?;
            metadata.u32(callsite.scope)?;
        }
    }
    if revision >= 4 {
        let mut items = artifact.codegen_output.functions.len();
        for function in &artifact.codegen_output.functions {
            if let Some(abi) = &function.abi {
                items = items
                    .checked_add(abi.captures.len())
                    .and_then(|items| items.checked_add(abi.parameters.len()))
                    .filter(|&items| items <= MAX_ITEMS)
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "function ABI cumulative item limit exceeded",
                        )
                    })?;
            }
        }
        metadata.count(artifact.codegen_output.functions.len())?;
        for function in &artifact.codegen_output.functions {
            metadata.u32(function.func_id.0)?;
            metadata.data(&[u8::from(function.abi.is_some())])?;
            if let Some(abi) = &function.abi {
                metadata.count(abi.captures.len())?;
                for capture in &abi.captures {
                    match capture {
                        CaptureAbi::Value => metadata.data(&[0])?,
                        CaptureAbi::TraitProof { view } => {
                            metadata.data(&[1])?;
                            metadata.u32(view.as_u32())?;
                        }
                    }
                }
                metadata.count(abi.parameters.len())?;
                for parameter in &abi.parameters {
                    match parameter {
                        ParameterAbi::Value => metadata.data(&[0])?,
                        ParameterAbi::Trait { view } => {
                            metadata.data(&[1])?;
                            metadata.u32(view.as_u32())?;
                        }
                        ParameterAbi::TraitSelf { view } => {
                            metadata.data(&[2])?;
                            metadata.u32(view.as_u32())?;
                        }
                    }
                }
            }
        }
    }

    if revision >= 6 {
        metadata.count(artifact.codegen_output.functions.len())?;
        for function in &artifact.codegen_output.functions {
            metadata.u32(function.func_id.0)?;
            metadata.data(&[u8::from(function.display_owner.is_some())])?;
            if let Some(owner) = function.display_owner {
                metadata.u32(owner.as_u32())?;
            }
        }
    }

    let mut code = Writer(Vec::new());
    code.count(artifact.codegen_output.functions.len())?;
    let mut offset = 4usize
        .checked_add(
            artifact
                .codegen_output
                .functions
                .len()
                .checked_mul(16)
                .ok_or_else(|| invalid("CODE table overflow"))?,
        )
        .ok_or_else(|| invalid("CODE table overflow"))?;
    for (function, words) in artifact
        .codegen_output
        .functions
        .iter()
        .zip(&relocated.instructions)
    {
        let size = words
            .len()
            .checked_mul(4)
            .ok_or_else(|| invalid("CODE size overflow"))?;
        let next = offset
            .checked_add(size)
            .filter(|&end| end <= MAX_ARCHIVE_SIZE)
            .ok_or_else(|| invalid("CODE exceeds MAX_ARCHIVE_SIZE"))?;
        code.u32(function.func_id.0)?;
        code.count(offset)?;
        code.count(size)?;
        code.data(&[function.register_count, function.param_count])?;
        code.data(&(if function.is_closure { 2u16 } else { 0 }).to_le_bytes())?;
        offset = next;
    }
    for words in &relocated.instructions {
        for &word in words {
            code.u32(word)?;
        }
    }
    let mut stack_maps = Writer(Vec::new());
    stack_maps.count(artifact.codegen_output.functions.len())?;
    for function in &artifact.codegen_output.functions {
        stack_maps.u32(function.func_id.0)?;
        stack_maps.count(function.safepoint_pcs.len())?;
        for &pc in &function.safepoint_pcs {
            stack_maps.u32(pc)?;
        }
    }
    let mut writer = ArchiveWriter::new();
    writer.add_raw_section(SectionKind::Metadata, metadata.0);
    writer.add_raw_section(SectionKind::Code, code.0);
    writer.add_constants_section(&relocated.constants)?;
    writer.add_raw_section(SectionKind::StackMaps, stack_maps.0);
    let mut bytes = Vec::new();
    writer.write_to(&mut bytes)?;
    Ok(bytes)
}

struct FunctionMetadata {
    name: str_interner::StrId,
    signature: TypeIndex,
    abi: Option<FunctionAbi>,
    display_owner: Option<TypeIndex>,
}

struct Metadata {
    type_pool: type_pool::TypePool,
    globals: Vec<GlobalInfo>,
    functions: BTreeMap<u32, FunctionMetadata>,
    methods: Vec<(u32, String)>,
    entry: Option<FuncId>,
    builtin_abi_version: u32,
    builtins: Vec<BuiltinImport>,
    method_call_scopes: Option<Vec<MethodCallScope>>,
    scope_coverage: ScopeCoverage,
}

fn decode_metadata(bytes: &[u8]) -> io::Result<Metadata> {
    let mut reader = Reader { remaining: bytes };
    if reader.take(4)? != METADATA_MAGIC {
        return Err(invalid("missing complete artifact metadata magic"));
    }
    let revision = reader.u32()?;
    if !matches!(revision, 1 | 2 | 3 | 4 | 5 | METADATA_REVISION) {
        return Err(invalid("unsupported artifact metadata revision"));
    }
    if reader.u32()? != TAGGED_ROOT_SCAN {
        return Err(invalid("unsupported artifact root scan mode"));
    }
    let builtin_abi_version = reader.u32()?;
    let entry = match reader.u32()? {
        u32::MAX => None,
        value => Some(FuncId(value)),
    };
    let type_pool = type_metadata::decode_pool(reader.blob()?)?;
    let count = reader.count(5)?;
    let mut globals = Vec::with_capacity(count);
    for _ in 0..count {
        let type_index = TypeIndex::from_raw(reader.u32()?);
        let is_mutable = match reader.take(1)?[0] {
            0 => false,
            1 => true,
            _ => return Err(invalid("invalid global mutability flag")),
        };
        globals.push(GlobalInfo {
            type_index,
            is_mutable,
        });
    }
    let count = reader.count(12)?;
    let mut functions = BTreeMap::new();
    for _ in 0..count {
        let id = reader.u32()?;
        let name = str_interner::intern(&reader.string()?);
        let signature = TypeIndex::from_raw(reader.u32()?);
        if functions
            .insert(
                id,
                FunctionMetadata {
                    name,
                    signature,
                    abi: None,
                    display_owner: None,
                },
            )
            .is_some()
        {
            return Err(invalid("duplicate function metadata"));
        }
    }
    let count = reader.count(8)?;
    let mut methods = Vec::with_capacity(count);
    for _ in 0..count {
        methods.push((reader.u32()?, reader.string()?));
    }
    let count = reader.count(8)?;
    let mut builtins = Vec::with_capacity(count);
    for _ in 0..count {
        builtins.push(BuiltinImport {
            id: reader.u32()?,
            name: reader.string()?,
        });
    }
    let scope_coverage = if revision >= 4 {
        match reader.take(1)?[0] {
            0 => ScopeCoverage::Calls,
            1 => ScopeCoverage::CallsAndTypes,
            _ => return Err(invalid("invalid scope coverage tag")),
        }
    } else if revision == 3 {
        ScopeCoverage::CallsAndTypes
    } else {
        ScopeCoverage::Calls
    };
    let scopes_present = if revision >= 4 {
        reader.presence("scope table")?
    } else {
        revision >= 2
    };
    let method_call_scopes = if scopes_present {
        let count = reader.count(12)?;
        let mut scopes = Vec::with_capacity(count);
        for _ in 0..count {
            scopes.push(MethodCallScope {
                func_id: FuncId(reader.u32()?),
                pc: reader.u32()?,
                scope: reader.u32()?,
            });
        }
        Some(scopes)
    } else {
        None
    };
    if revision >= 4 {
        let mut budget = MAX_ITEMS;
        let count = reader.abi_count(5, &mut budget)?;
        if count != functions.len() {
            return Err(invalid("function ABI count differs from function metadata"));
        }
        let mut seen = BTreeSet::new();
        for _ in 0..count {
            let id = reader.u32()?;
            if !seen.insert(id) {
                return Err(invalid("duplicate function ABI metadata"));
            }
            let function = functions
                .get_mut(&id)
                .ok_or_else(|| invalid("unknown function ABI target"))?;
            if reader.presence("function ABI")? {
                let count = reader.abi_count(1, &mut budget)?;
                if count > nsbc::Reg::MAX_GP as usize {
                    return Err(invalid("capture ABI count exceeds the register limit"));
                }
                let mut captures = Vec::new();
                captures
                    .try_reserve_exact(count)
                    .map_err(|_| invalid("cannot allocate capture ABI"))?;
                for _ in 0..count {
                    captures.push(match reader.take(1)?[0] {
                        0 => CaptureAbi::Value,
                        1 => CaptureAbi::TraitProof {
                            view: TypeIndex::from_raw(reader.u32()?),
                        },
                        _ => return Err(invalid("invalid capture ABI tag")),
                    });
                }
                let count = reader.abi_count(1, &mut budget)?;
                if count > nsbc::Reg::MAX_GP as usize {
                    return Err(invalid("parameter ABI count exceeds the register limit"));
                }
                let mut parameters = Vec::new();
                parameters
                    .try_reserve_exact(count)
                    .map_err(|_| invalid("cannot allocate parameter ABI"))?;
                for _ in 0..count {
                    parameters.push(match reader.take(1)?[0] {
                        0 => ParameterAbi::Value,
                        1 => ParameterAbi::Trait {
                            view: TypeIndex::from_raw(reader.u32()?),
                        },
                        2 if revision >= 5 => ParameterAbi::TraitSelf {
                            view: TypeIndex::from_raw(reader.u32()?),
                        },
                        2 => {
                            return Err(invalid(
                                "TraitSelf parameter requires metadata revision 5",
                            ));
                        }
                        _ => return Err(invalid("invalid parameter ABI tag")),
                    });
                }
                function.abi = Some(FunctionAbi {
                    captures,
                    parameters,
                });
            }
        }
    }
    if revision >= 6 {
        let count = reader.count(5)?;
        if count != functions.len() {
            return Err(invalid(
                "Display owner count differs from function metadata",
            ));
        }
        let mut seen = BTreeSet::new();
        for _ in 0..count {
            let id = reader.u32()?;
            if !seen.insert(id) {
                return Err(invalid("duplicate Display owner metadata"));
            }
            let function = functions
                .get_mut(&id)
                .ok_or_else(|| invalid("unknown Display owner target"))?;
            if reader.presence("Display owner")? {
                function.display_owner = Some(TypeIndex::from_raw(reader.u32()?));
            }
        }
    }
    reader.finish()?;
    Ok(Metadata {
        type_pool,
        globals,
        functions,
        methods,
        entry,
        builtin_abi_version,
        builtins,
        method_call_scopes,
        scope_coverage,
    })
}

fn decode_stack_maps(bytes: &[u8]) -> io::Result<BTreeMap<u32, Vec<u32>>> {
    let mut reader = Reader { remaining: bytes };
    let count = reader.count(8)?;
    let mut functions = BTreeMap::new();
    for _ in 0..count {
        let id = reader.u32()?;
        let count = reader.count(4)?;
        let mut pcs = Vec::with_capacity(count);
        for _ in 0..count {
            pcs.push(reader.u32()?);
        }
        if functions.insert(id, pcs).is_some() {
            return Err(invalid("duplicate function stack maps"));
        }
    }
    reader.finish()?;
    Ok(functions)
}

fn decode_code(
    bytes: &[u8],
    metadata: &mut BTreeMap<u32, FunctionMetadata>,
    stack_maps: &mut BTreeMap<u32, Vec<u32>>,
) -> io::Result<Vec<CompiledFunction>> {
    let mut reader = Reader { remaining: bytes };
    let count = reader.count(16)?;
    let data_start = 4 + count * 16;
    let mut functions = Vec::with_capacity(count);
    let mut ranges = Vec::with_capacity(count);
    for _ in 0..count {
        let id = reader.u32()?;
        let offset = reader.u32()? as usize;
        let size = reader.u32()? as usize;
        let counts = reader.take(2)?;
        let mut flags = [0; 2];
        flags.copy_from_slice(reader.take(2)?);
        let flags = u16::from_le_bytes(flags);
        if flags & !2 != 0 {
            return Err(invalid("unsupported artifact function flags"));
        }
        let end = offset
            .checked_add(size)
            .filter(|&end| offset >= data_start && end <= bytes.len())
            .ok_or_else(|| invalid("CODE function is outside section data"))?;
        if !offset.is_multiple_of(4) || !size.is_multiple_of(4) {
            return Err(invalid("CODE function is not instruction aligned"));
        }
        let function = metadata
            .remove(&id)
            .ok_or_else(|| invalid("missing or duplicate function metadata for CODE entry"))?;
        let safepoint_pcs = stack_maps
            .remove(&id)
            .ok_or_else(|| invalid("missing or duplicate function stack maps for CODE entry"))?;
        functions.push(CompiledFunction {
            func_id: FuncId(id),
            name: function.name,
            instructions: Vec::new(),
            register_count: counts[0],
            param_count: counts[1],
            is_closure: flags & 2 != 0,
            function_type: function.signature,
            abi: function.abi,
            display_owner: function.display_owner,
            safepoint_pcs,
        });
        ranges.push((offset, end));
    }
    // Validate the entire partition before copying code. Overlapping entries
    // must not amplify a bounded section into many copies of the same payload.
    let mut sorted_ranges = ranges.clone();
    sorted_ranges.sort_unstable();
    let mut previous = data_start;
    for (start, end) in sorted_ranges {
        if start != previous {
            return Err(invalid("CODE functions overlap or leave unclaimed bytes"));
        }
        previous = end;
    }
    if previous != bytes.len() || !metadata.is_empty() || !stack_maps.is_empty() {
        return Err(invalid(
            "trailing CODE bytes or unmatched function metadata",
        ));
    }
    for (function, (start, end)) in functions.iter_mut().zip(ranges) {
        function.instructions = bytes[start..end]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|word| u32::from_le_bytes(*word))
            .collect();
    }
    Ok(functions)
}

/// Decode and validate a complete artifact before it can be installed in a VM.
/// Legacy archives with missing metadata or a zero checksum cannot execute.
pub fn read_artifact(bytes: &[u8]) -> io::Result<CompiledArtifact> {
    let mut input = bytes;
    let archive = Archive::read_from(&mut input)?;
    if archive.header.checksum == [0; 32] {
        return Err(invalid("complete artifacts require a checksum"));
    }
    crate::container::validate_target(&archive.header)?;
    for (entry, _) in &archive.sections {
        if entry.name_idx != 0 {
            return Err(invalid(
                "named sections are not supported by complete artifact metadata",
            ));
        }
        if !matches!(
            SectionKind::from_u8(entry.kind),
            Some(
                SectionKind::Metadata
                    | SectionKind::Code
                    | SectionKind::Constants
                    | SectionKind::StackMaps
            )
        ) {
            return Err(invalid("unsupported section in complete artifact"));
        }
    }
    let section = |kind| {
        archive
            .find_section(kind)
            .ok_or_else(|| invalid(format!("missing {kind:?} artifact section")))
    };
    let mut metadata = decode_metadata(section(SectionKind::Metadata)?)?;
    let mut stack_maps = decode_stack_maps(section(SectionKind::StackMaps)?)?;
    let functions = decode_code(
        section(SectionKind::Code)?,
        &mut metadata.functions,
        &mut stack_maps,
    )?;
    let mut output = CodegenOutput {
        method_call_scopes: metadata.method_call_scopes,
        scope_coverage: metadata.scope_coverage,
        functions,
        constants: archive.constants()?,
        globals: metadata.globals,
    };
    relocation::restore(&mut output, &metadata.methods)?;
    let artifact = CompiledArtifact {
        codegen_output: output,
        type_pool: metadata.type_pool,
        entry: metadata.entry,
        builtin_abi_version: metadata.builtin_abi_version,
        builtins: metadata.builtins,
    };
    validate_artifact(&artifact).map_err(|error| invalid(error.to_string()))?;
    if archive.header.version == 3
        && nsbc::uses_error_envelopes(&artifact).map_err(|error| invalid(error.to_string()))?
    {
        return Err(invalid(
            "executable Error capability requires NSBC version 4",
        ));
    }
    Ok(artifact)
}

#[cfg(test)]
#[path = "artifact_tests.rs"]
mod tests;
