//! Complete artifact codec on the single current NSAM v6 baseline.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use nsbc::{
    BuiltinImport, CaptureAbi, CodegenOutput, CompiledArtifact, CompiledFunction, FuncId,
    FunctionAbi, GlobalInfo, MethodCallScope, ParameterAbi, SectionKind, WasmBindingDef,
    WasmModuleDef, validate_artifact,
};
use type_pool::TypeIndex;

use crate::{Archive, ArchiveWriter, MAX_ARCHIVE_SIZE, relocation, type_metadata};

const METADATA_MAGIC: &[u8; 4] = b"NSAM";
const METADATA_REVISION: u32 = 6;
const TAGGED_ROOT_SCAN: u32 = 0;
pub(super) const MAX_ITEMS: usize = 1_000_000;

pub(super) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(super) struct Writer(pub(super) Vec<u8>);

impl Writer {
    pub(super) fn data(&mut self, bytes: &[u8]) -> io::Result<()> {
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
    pub(super) fn u32(&mut self, value: u32) -> io::Result<()> {
        self.data(&value.to_le_bytes())
    }
    pub(super) fn u64(&mut self, value: u64) -> io::Result<()> {
        self.data(&value.to_le_bytes())
    }
    pub(super) fn count(&mut self, count: usize) -> io::Result<()> {
        let count = u32::try_from(count)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "artifact count overflow"))?;
        self.u32(count)
    }
    pub(super) fn blob(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.count(bytes.len())?;
        self.data(bytes)
    }
    pub(super) fn string(&mut self, value: &str) -> io::Result<()> {
        self.blob(value.as_bytes())
    }
}

pub(super) struct Reader<'a> {
    pub(super) remaining: &'a [u8],
}

impl<'a> Reader<'a> {
    pub(super) fn take(&mut self, length: usize) -> io::Result<&'a [u8]> {
        if length > self.remaining.len() {
            return Err(invalid("truncated artifact section"));
        }
        let (bytes, remaining) = self.remaining.split_at(length);
        self.remaining = remaining;
        Ok(bytes)
    }
    pub(super) fn u32(&mut self) -> io::Result<u32> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }
    pub(super) fn u64(&mut self) -> io::Result<u64> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }
    pub(super) fn count(&mut self, minimum_size: usize) -> io::Result<usize> {
        let count = self.u32()? as usize;
        if count > MAX_ITEMS || count > self.remaining.len() / minimum_size.max(1) {
            return Err(invalid(
                "artifact count exceeds section data or resource limit",
            ));
        }
        Ok(count)
    }
    pub(super) fn presence(&mut self, context: &str) -> io::Result<bool> {
        match self.take(1)?[0] {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid(format!("invalid {context} presence tag"))),
        }
    }
    pub(super) fn abi_count(&mut self, minimum: usize, budget: &mut usize) -> io::Result<usize> {
        let count = self.count(minimum)?;
        *budget = budget
            .checked_sub(count)
            .ok_or_else(|| invalid("function ABI cumulative item limit exceeded"))?;
        Ok(count)
    }
    pub(super) fn blob(&mut self) -> io::Result<&'a [u8]> {
        let count = self.u32()? as usize;
        self.take(count)
    }
    pub(super) fn string(&mut self) -> io::Result<String> {
        std::str::from_utf8(self.blob()?)
            .map(str::to_owned)
            .map_err(|_| invalid("invalid UTF-8 in artifact name"))
    }
    pub(super) fn finish(&self) -> io::Result<()> {
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
    let writer = encode_core_sections_with_pool_bytes(
        &artifact.codegen_output,
        &pool_bytes,
        artifact.entry,
        artifact.builtin_abi_version,
        &artifact.builtins,
        &artifact.wasm_modules,
    )?;
    let mut bytes = Vec::new();
    writer.write_to(&mut bytes)?;
    Ok(bytes)
}

pub(super) fn encode_core_sections(
    codegen_output: &CodegenOutput,
    type_pool: &type_pool::TypePool,
    entry: Option<FuncId>,
    builtin_abi_version: u32,
    builtins: &[BuiltinImport],
    wasm_modules: &[WasmModuleDef],
) -> io::Result<ArchiveWriter> {
    let pool_bytes = type_metadata::encode_pool(type_pool)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    encode_core_sections_with_pool_bytes(
        codegen_output,
        &pool_bytes,
        entry,
        builtin_abi_version,
        builtins,
        wasm_modules,
    )
}

fn encode_core_sections_with_pool_bytes(
    codegen_output: &CodegenOutput,
    pool_bytes: &[u8],
    entry: Option<FuncId>,
    builtin_abi_version: u32,
    builtins: &[BuiltinImport],
    wasm_modules: &[WasmModuleDef],
) -> io::Result<ArchiveWriter> {
    let relocated = relocation::encode(codegen_output)?;
    let mut metadata = Writer(Vec::new());
    metadata.data(METADATA_MAGIC)?;
    metadata.u32(METADATA_REVISION)?;
    metadata.u32(TAGGED_ROOT_SCAN)?;
    metadata.u32(builtin_abi_version)?;
    metadata.u32(entry.map_or(u32::MAX, |entry| entry.0))?;
    metadata.blob(pool_bytes)?;
    metadata.count(codegen_output.globals.len())?;
    for global in &codegen_output.globals {
        metadata.u32(global.type_index.as_u32())?;
        metadata.data(&[u8::from(global.is_mutable)])?;
    }
    metadata.count(codegen_output.functions.len())?;
    for function in &codegen_output.functions {
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
    metadata.count(builtins.len())?;
    for builtin in builtins {
        metadata.u32(builtin.id)?;
        metadata.string(&builtin.name)?;
    }

    let scopes = &codegen_output.method_call_scopes;
    metadata.count(scopes.len())?;
    for callsite in scopes {
        metadata.u32(callsite.func_id.0)?;
        metadata.u32(callsite.pc)?;
        metadata.u32(callsite.scope)?;
    }

    let mut items = codegen_output.functions.len();
    for function in &codegen_output.functions {
        let abi = &function.abi;
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
    metadata.count(codegen_output.functions.len())?;
    for function in &codegen_output.functions {
        metadata.u32(function.func_id.0)?;
        let abi = &function.abi;
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

    metadata.count(codegen_output.functions.len())?;
    for function in &codegen_output.functions {
        metadata.u32(function.func_id.0)?;
        metadata.data(&[u8::from(function.display_owner.is_some())])?;
        if let Some(owner) = function.display_owner {
            metadata.u32(owner.as_u32())?;
        }
    }

    let mut code = Writer(Vec::new());
    code.count(codegen_output.functions.len())?;
    let mut offset = 4usize
        .checked_add(
            codegen_output
                .functions
                .len()
                .checked_mul(16)
                .ok_or_else(|| invalid("CODE table overflow"))?,
        )
        .ok_or_else(|| invalid("CODE table overflow"))?;
    for (function, words) in codegen_output.functions.iter().zip(&relocated.instructions) {
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
    stack_maps.count(codegen_output.functions.len())?;
    for function in &codegen_output.functions {
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
    if !wasm_modules.is_empty() {
        writer.add_raw_section(SectionKind::Wasm, encode_wasm_section(wasm_modules)?);
    }
    Ok(writer)
}

fn encode_wasm_section(modules: &[WasmModuleDef]) -> io::Result<Vec<u8>> {
    // Intern unique section strings so `WasmModuleEntry` & `WasmBinding` store
    // compact `u32` string-pool indices per `docs/dev/nessa-bytecode/wasm-in-archive.md`.
    let mut string_pool: Vec<&str> = Vec::new();
    let mut string_map: BTreeMap<&str, u32> = BTreeMap::new();
    for m in modules {
        if !string_map.contains_key(m.name.as_str()) {
            let idx = u32::try_from(string_pool.len())
                .map_err(|_| invalid("WASM section string pool overflow"))?;
            string_pool.push(m.name.as_str());
            string_map.insert(m.name.as_str(), idx);
        }
        for b in &m.bindings {
            for s in [b.nessa_name.as_str(), b.wasm_export.as_str()] {
                if !string_map.contains_key(s) {
                    let idx = u32::try_from(string_pool.len())
                        .map_err(|_| invalid("WASM section string pool overflow"))?;
                    string_pool.push(s);
                    string_map.insert(s, idx);
                }
            }
        }
    }

    // Compute exact header + string-table byte size before writing `wasm_offset`s:
    //   module_count: u32 (4B)
    //   each module: name(4) + wasm_offset(8) + wasm_size(8) + binding_count(4) + bindings(12 * B)
    //   string_count: u32 (4B) + each string: len(4) + bytes
    let mut header_size = 4usize;
    for m in modules {
        let bbytes = m
            .bindings
            .len()
            .checked_mul(12)
            .ok_or_else(|| invalid("WASM binding table overflow"))?;
        header_size = header_size
            .checked_add(24)
            .and_then(|n| n.checked_add(bbytes))
            .ok_or_else(|| invalid("WASM header overflow"))?;
    }
    header_size = header_size
        .checked_add(4)
        .ok_or_else(|| invalid("WASM string table overflow"))?;
    for s in &string_pool {
        header_size = header_size
            .checked_add(4)
            .and_then(|n| n.checked_add(s.len()))
            .ok_or_else(|| invalid("WASM string table overflow"))?;
    }

    let mut out = Writer(Vec::new());
    out.count(modules.len())?;
    let mut payload_offset = header_size;
    for m in modules {
        let next = payload_offset
            .checked_add(m.wasm_bytes.len())
            .filter(|&end| end <= MAX_ARCHIVE_SIZE)
            .ok_or_else(|| invalid("WASM section exceeds MAX_ARCHIVE_SIZE"))?;
        out.u32(string_map[m.name.as_str()])?;
        out.u64(payload_offset as u64)?;
        out.u64(m.wasm_bytes.len() as u64)?;
        out.count(m.bindings.len())?;
        for b in &m.bindings {
            out.u32(string_map[b.nessa_name.as_str()])?;
            out.u32(string_map[b.wasm_export.as_str()])?;
            out.u32(b.signature.as_u32())?;
        }
        payload_offset = next;
    }

    out.count(string_pool.len())?;
    for s in &string_pool {
        out.string(s)?;
    }
    debug_assert_eq!(out.0.len(), header_size);
    for m in modules {
        out.data(&m.wasm_bytes)?;
    }
    Ok(out.0)
}

fn decode_wasm_section(bytes: &[u8]) -> io::Result<Vec<WasmModuleDef>> {
    let mut reader = Reader { remaining: bytes };
    let module_count = reader.count(24)?;
    if module_count == 0 {
        return Err(invalid("empty WASM section is not permitted"));
    }
    struct RawEntry {
        name_idx: u32,
        offset: usize,
        end: usize,
        bindings: Vec<(u32, u32, TypeIndex)>,
    }
    let mut raw_entries = Vec::with_capacity(module_count);
    let mut total_bindings = 0usize;
    for _ in 0..module_count {
        let name_idx = reader.u32()?;
        let offset =
            usize::try_from(reader.u64()?).map_err(|_| invalid("WASM payload offset overflow"))?;
        let size =
            usize::try_from(reader.u64()?).map_err(|_| invalid("WASM payload size overflow"))?;
        let end = offset
            .checked_add(size)
            .filter(|&end| end <= bytes.len())
            .ok_or_else(|| invalid("WASM module payload is outside section data"))?;
        let bcount = reader.count(12)?;
        total_bindings = total_bindings
            .checked_add(bcount)
            .filter(|&n| n <= MAX_ITEMS)
            .ok_or_else(|| invalid("WASM binding count limit exceeded"))?;
        let mut bindings = Vec::with_capacity(bcount);
        for _ in 0..bcount {
            bindings.push((
                reader.u32()?,
                reader.u32()?,
                TypeIndex::from_raw(reader.u32()?),
            ));
        }
        raw_entries.push(RawEntry {
            name_idx,
            offset,
            end,
            bindings,
        });
    }

    let str_count = reader.count(4)?;
    let mut strings = Vec::with_capacity(str_count);
    for _ in 0..str_count {
        strings.push(reader.string()?);
    }

    let data_start = bytes.len() - reader.remaining.len();
    let mut ranges: Vec<(usize, usize)> = raw_entries.iter().map(|e| (e.offset, e.end)).collect();
    ranges.sort_unstable();
    let mut previous = data_start;
    for (start, end) in ranges {
        if start != previous {
            return Err(invalid(
                "WASM module payloads overlap or leave unclaimed bytes",
            ));
        }
        previous = end;
    }
    if previous != bytes.len() {
        return Err(invalid("trailing unclaimed bytes in WASM section"));
    }

    let get_str = |idx: u32| -> io::Result<String> {
        strings
            .get(idx as usize)
            .cloned()
            .ok_or_else(|| invalid("WASM section string index out of bounds"))
    };

    let mut modules = Vec::with_capacity(module_count);
    for raw in raw_entries {
        let name = get_str(raw.name_idx)?;
        let mut bindings = Vec::with_capacity(raw.bindings.len());
        for (n_idx, e_idx, sig) in raw.bindings {
            bindings.push(WasmBindingDef {
                nessa_name: get_str(n_idx)?,
                wasm_export: get_str(e_idx)?,
                signature: sig,
            });
        }
        modules.push(WasmModuleDef {
            name,
            wasm_bytes: bytes[raw.offset..raw.end].to_vec(),
            bindings,
        });
    }
    Ok(modules)
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
    method_call_scopes: Vec<MethodCallScope>,
}

fn decode_metadata(bytes: &[u8]) -> io::Result<Metadata> {
    let mut reader = Reader { remaining: bytes };
    if reader.take(4)? != METADATA_MAGIC {
        return Err(invalid("missing complete artifact metadata magic"));
    }
    let revision = reader.u32()?;
    if revision != METADATA_REVISION {
        return Err(invalid(format!(
            "unsupported artifact metadata revision {revision}; expected {METADATA_REVISION}"
        )));
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

    let count = reader.count(12)?;
    let mut method_call_scopes = Vec::with_capacity(count);
    for _ in 0..count {
        method_call_scopes.push(MethodCallScope {
            func_id: FuncId(reader.u32()?),
            pc: reader.u32()?,
            scope: reader.u32()?,
        });
    }

    let mut budget = MAX_ITEMS;
    let count = reader.abi_count(12, &mut budget)?;
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
                2 => ParameterAbi::TraitSelf {
                    view: TypeIndex::from_raw(reader.u32()?),
                },
                _ => return Err(invalid("invalid parameter ABI tag")),
            });
        }
        function.abi = Some(FunctionAbi {
            captures,
            parameters,
        });
    }

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
        let abi = function
            .abi
            .ok_or_else(|| invalid("missing function ABI metadata"))?;
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
            abi,
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
pub fn read_artifact(bytes: &[u8]) -> io::Result<CompiledArtifact> {
    let mut input = bytes;
    let archive = Archive::read_from(&mut input)?;
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
                    | SectionKind::Wasm
            )
        ) {
            return Err(invalid("unsupported section in complete artifact"));
        }
    }
    let artifact = decode_core_sections(&archive)?;
    validate_artifact(&artifact).map_err(|error| invalid(error.to_string()))?;
    Ok(artifact)
}

pub(super) fn decode_core_sections(archive: &Archive) -> io::Result<CompiledArtifact> {
    let wasm_section_count = archive
        .sections
        .iter()
        .filter(|(e, _)| e.kind == SectionKind::Wasm as u8)
        .count();
    if wasm_section_count > 1 {
        return Err(invalid("duplicate Wasm section in complete artifact"));
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
    let wasm_modules = match archive.find_section(SectionKind::Wasm) {
        Some(wasm_bytes) => decode_wasm_section(wasm_bytes)?,
        None => Vec::new(),
    };
    let mut output = CodegenOutput {
        method_call_scopes: metadata.method_call_scopes,
        functions,
        constants: archive.constants()?,
        globals: metadata.globals,
    };
    relocation::restore(&mut output, &metadata.methods)?;
    Ok(CompiledArtifact {
        codegen_output: output,
        type_pool: metadata.type_pool,
        entry: metadata.entry,
        builtin_abi_version: metadata.builtin_abi_version,
        builtins: metadata.builtins,
        wasm_modules,
    })
}

#[cfg(test)]
#[path = "artifact_tests.rs"]
mod tests;
