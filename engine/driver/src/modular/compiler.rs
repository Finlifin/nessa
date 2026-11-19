//! Separate per-package `.nsbc` compilation, interface-hash DAG caching, and parallel DAG builds.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use ast::{Ast, NodeIndex, NodeKind};
use diagnostic::{Diagnostic, DiagnosticContext, Level};
use nsbc::{
    ArtifactExports, ArtifactImports, BuiltinImport, ExportTarget, ImportedModuleRef,
    ImportedPackageRef, InterfaceModuleStub, ModularArtifact, ModuleDescriptor, SymbolLinkSelector,
};
use pkg_manager::{ManifestDocument, PackageResolver, PackageSources, PackageType};
use resolution::{ResolveOptions, SourcePackageIdentity};
use rustc_span::DUMMY_SP;
use sha2::{Digest, Sha256};

use crate::modular::interface::{
    build_exported_symbols, build_imported_descriptors, compute_interface_hash,
    is_preserved_compile_time_rhs, synthesize_interface_modules,
};
use crate::package::source_units;
use crate::{CompilationIdentityContext, Driver, OptLevel};

static CACHE_TMP_SEQ: AtomicU64 = AtomicU64::new(1);

static STD_MODULAR_MEM_CACHE: OnceLock<Mutex<HashMap<OptLevel, Arc<ModularArtifact>>>> =
    OnceLock::new();

static PKG_MODULAR_MEM_CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<ModularArtifact>>>> =
    OnceLock::new();
const PKG_MODULAR_MEM_LIMIT: usize = 512;

/// Output of compiling a package DAG into modular `.nsbc` archives.
#[derive(Debug)]
pub struct ModularCompilationOutput {
    /// Topological-order modular artifacts (including `std` at index `0`, dependencies, and `root` last).
    pub artifacts: Vec<Arc<ModularArtifact>>,
    /// Content-addressed `.nsbc` cache path corresponding to each entry in `artifacts` (when caching is enabled).
    pub cache_paths: Vec<Option<PathBuf>>,
    /// Qualified name (`domain/name`) of the root package.
    pub root_qualified_name: String,
    /// Qualified names of packages compiled from `.ns` sources during this invocation.
    pub compiled_packages: Vec<String>,
    /// Qualified names of packages loaded from the `.nsbc` cache without recompilation.
    pub cached_packages: Vec<String>,
    /// Non-error warnings collected during compilation.
    pub diagnostics: Vec<Diagnostic>,
}

fn unwrap_vis(ast: &Ast, mut node: NodeIndex) -> NodeIndex {
    while !node.is_null()
        && matches!(
            ast.node(node).kind,
            NodeKind::PubDef | NodeKind::PrivateDef | NodeKind::GlobalDecl
        )
    {
        node = ast.fixed_children(node)[0];
    }
    node
}

fn binding_name(ast: &Ast, node: NodeIndex) -> Option<str_interner::StrId> {
    let inner = unwrap_vis(ast, node);
    if inner.is_null() {
        return None;
    }
    match ast.node(inner).kind {
        NodeKind::ModuleDef
        | NodeKind::FunctionDef
        | NodeKind::StructDef
        | NodeKind::EnumDef
        | NodeKind::Newtype
        | NodeKind::TraitDef
        | NodeKind::EffectDef
        | NodeKind::AsyncEffectDef
        | NodeKind::Typealias => {
            let child = ast.fixed_children(inner)[0];
            (!child.is_null()).then(|| ast.node(child).str_id)
        }
        _ => None,
    }
}

/// Mark bodyless function stubs and stubbed `const`/`let`/`var` declarations in `external_stub_nodes`.
///
/// Template `FunctionDef`s (`impl Ord`, `impl Json`, `impl Iterator`, `impl IntoIterator` with non-empty bodies),
/// `TraitDeriveFn` nodes, and compile-time `'builtin` / `#effect` / boolean `const`s are intentionally NOT marked
/// so `resolution` specializes and evaluates them identically to source compilation.
fn mark_stub_declarations(ast: &Ast, root_node: NodeIndex, stubs: &mut HashSet<NodeIndex>) {
    let mut stack = vec![root_node];
    while let Some(node) = stack.pop() {
        if node.is_null() {
            continue;
        }
        match ast.node(node).kind {
            NodeKind::TraitDeriveFn => continue,
            NodeKind::FunctionDef => {
                let body = ast.fixed_children(node)[2];
                let is_empty_block = !body.is_null()
                    && ast.node(body).kind == NodeKind::Block
                    && ast.multi_children(body).is_empty();
                if is_empty_block {
                    stubs.insert(node);
                }
                // Never descend into preserved template function bodies (`let`/`var`/`const`
                // locals inside preserved trait/iterator/json default bodies are real locals, not module stubs).
                continue;
            }
            NodeKind::ConstDecl | NodeKind::LetDecl | NodeKind::VarDecl => {
                let rhs = ast.fixed_children(node)[2];
                if !is_preserved_compile_time_rhs(ast, ast.node(node).kind, rhs) {
                    stubs.insert(node);
                }
                continue;
            }
            _ => {}
        }
        stack.extend_from_slice(ast.fixed_children(node));
        stack.extend_from_slice(ast.multi_children(node));
    }
}

fn collect_builtin_imports(output: &nsbc::CodegenOutput) -> Result<Vec<BuiltinImport>, String> {
    let refs = nsbc::builtin_references(output).map_err(|e| e.to_string())?;
    refs.into_iter()
        .map(|id| {
            let meta = runtime::lookup_builtin_fn_meta(id)
                .ok_or_else(|| format!("unknown compiled builtin ID {id}"))?;
            Ok(BuiltinImport {
                id,
                name: meta.name.to_owned(),
            })
        })
        .collect()
}

fn attach_single_package_ffi_wasm(
    pkg: &PackageSources,
    codegen_output: &mut nsbc::CodegenOutput,
    type_pool: &mut type_pool::TypePool,
) -> Result<Vec<nsbc::WasmModuleDef>, String> {
    use nsbc::{
        AddrMode, Constant, Instruction, InstructionData, Opcode, Reg, WasmBindingDef,
        WasmModuleDef,
    };
    use type_pool::{Intrinsic, TypeIndex, TypeKind};

    if pkg.manifest.ffi().wasm.is_empty() {
        return Ok(Vec::new());
    }

    let resolve_type_name = |pool: &type_pool::TypePool, name: &str| -> Result<TypeIndex, String> {
        match name {
            "Unit" | "()" => Ok(Intrinsic::Unit.type_index()),
            "bool" => Ok(Intrinsic::Bool.type_index()),
            "char" => Ok(Intrinsic::Char.type_index()),
            "i8" => Ok(Intrinsic::I8.type_index()),
            "u8" => Ok(Intrinsic::U8.type_index()),
            "i16" => Ok(Intrinsic::I16.type_index()),
            "u16" => Ok(Intrinsic::U16.type_index()),
            "i32" => Ok(Intrinsic::I32.type_index()),
            "u32" => Ok(Intrinsic::U32.type_index()),
            "i64" => Ok(Intrinsic::I64.type_index()),
            "u64" => Ok(Intrinsic::U64.type_index()),
            "isize" => Ok(Intrinsic::Isize.type_index()),
            "usize" => Ok(Intrinsic::Usize.type_index()),
            "f32" => Ok(Intrinsic::F32.type_index()),
            "f64" => Ok(Intrinsic::F64.type_index()),
            "String" => Ok(Intrinsic::Str.type_index()),
            "Bytes" => pool
                .bytes_type()
                .ok_or_else(|| "missing standard library `Bytes` role in TypePool".to_owned()),
            other => Err(format!("unsupported [[ffi.wasm]] type `{other}`")),
        }
    };

    let qpkg = pkg.manifest.manifest().qualified_name();
    let mut marker_to_binding: HashMap<String, (u32, u8, bool, String)> = HashMap::new();
    let mut wasm_modules = Vec::new();
    let mut next_binding_id: u32 = 0;

    for spec in &pkg.manifest.ffi().wasm {
        let asset = pkg
            .wasm_assets
            .iter()
            .find(|a| a.name == spec.name)
            .ok_or_else(|| {
                format!(
                    "missing loaded .wasm asset for [[ffi.wasm]] `{}` (`{}`)",
                    spec.name, spec.file
                )
            })?;
        let mut bindings = Vec::with_capacity(spec.exports.len());
        for exp in &spec.exports {
            let mut param_tys = Vec::with_capacity(exp.params.len());
            for p_name in &exp.params {
                param_tys.push(resolve_type_name(type_pool, p_name)?);
            }
            let ret_ty = resolve_type_name(type_pool, &exp.ret)?;
            let is_unit_ret = ret_ty == Intrinsic::Unit.type_index();
            let sig_idx = type_pool.intern_structural(TypeKind::Function {
                params: param_tys,
                ret: ret_ty,
            });
            let marker = format!(
                "__nessa_wasm_bind:{qpkg}::{}::{}",
                spec.name, exp.nessa_name
            );
            marker_to_binding.insert(
                marker,
                (
                    next_binding_id,
                    exp.params.len() as u8,
                    is_unit_ret,
                    exp.nessa_name.clone(),
                ),
            );
            next_binding_id = next_binding_id
                .checked_add(1)
                .ok_or_else(|| "[[ffi.wasm]] binding count overflow".to_owned())?;
            bindings.push(WasmBindingDef {
                nessa_name: exp.nessa_name.clone(),
                wasm_export: exp.wasm_export.clone(),
                signature: sig_idx,
            });
        }
        wasm_modules.push(WasmModuleDef {
            name: format!("{qpkg}::{}", spec.name),
            wasm_bytes: asset.bytes.clone(),
            bindings,
        });
    }

    if let Some(id_input) = type_pool.identity_input().cloned() {
        type_pool.finalize_type_identities(id_input).map_err(|e| {
            format!("failed to re-finalize TypePool after WASM signature intern: {e}")
        })?;
    }

    let mut const_idx_to_binding: HashMap<u32, (u32, u8, bool, String)> = HashMap::new();
    for (cidx, c) in codegen_output.constants.iter().enumerate() {
        if let Constant::Str(s) = c
            && let Some(info) = marker_to_binding.get(s)
        {
            const_idx_to_binding.insert(cidx as u32, info.clone());
        }
    }

    let mut rewritten_funcs = HashSet::new();
    for func in &mut codegen_output.functions {
        let fn_name = str_interner::get(func.name);
        let mut matched: Option<(u32, u8, bool)> = None;
        for &word in &func.instructions {
            let Some(instr) = Instruction::decode(word) else {
                continue;
            };
            match (instr.opcode, instr.amode, instr.data) {
                (Opcode::Load, AddrMode::Const, InstructionData::A { imm12, .. }) => {
                    if let Some((bid, argc, is_unit, expected_name)) =
                        const_idx_to_binding.get(&u32::from(imm12))
                        && fn_name == *expected_name
                    {
                        matched = Some((*bid, *argc, *is_unit));
                        break;
                    }
                }
                (Opcode::LoadConstWide, _, InstructionData::A { base, imm12, .. }) => {
                    let idx17 = (u32::from(base.0) << 12) | u32::from(imm12);
                    if let Some((bid, argc, is_unit, expected_name)) =
                        const_idx_to_binding.get(&idx17)
                        && fn_name == *expected_name
                    {
                        matched = Some((*bid, *argc, *is_unit));
                        break;
                    }
                }
                _ => {}
            }
        }
        if let Some((binding_id, argc, is_unit_ret)) = matched {
            func.instructions = vec![
                Instruction::call_wasm(binding_id, argc).encode(),
                if is_unit_ret {
                    Instruction::return_unit().encode()
                } else {
                    Instruction::ret(Reg(0)).encode()
                },
            ];
            func.register_count = func.register_count.max(argc.max(1));
            func.safepoint_pcs.clear();
            rewritten_funcs.insert(func.func_id);
        }
    }
    if !rewritten_funcs.is_empty() {
        codegen_output
            .method_call_scopes
            .retain(|s| !rewritten_funcs.contains(&s.func_id));
    }

    Ok(wasm_modules)
}

fn modular_std_cache_path(cache_dir: &Path, opt_level: OptLevel) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(b"nessa.modular.std.v2\0");
    hasher.update(nsbc::VERSION.to_le_bytes());
    hasher.update(runtime::BUILTIN_ABI_VERSION.to_le_bytes());
    hasher.update([opt_level.as_u8()]);
    hasher.update(crate::standard_library::std_library_sha256());
    let digest: [u8; 32] = hasher.finalize().into();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    cache_dir.join("modular").join(format!("std-{hex}.nsbc"))
}

fn modular_pkg_cache_path(
    cache_dir: &Path,
    pkg: &PackageSources,
    deps: &[Arc<ModularArtifact>],
    std_artifact: &ModularArtifact,
    is_privileged: bool,
    opt_level: OptLevel,
) -> Result<PathBuf, String> {
    let mut hasher = Sha256::new();
    hasher.update(b"nessa.modular.pkg.v2\0");
    hasher.update(nsbc::VERSION.to_le_bytes());
    hasher.update(runtime::BUILTIN_ABI_VERSION.to_le_bytes());
    hasher.update([opt_level.as_u8(), u8::from(is_privileged)]);
    hasher.update(std_artifact.exports.interface_hash);

    let m = pkg.manifest.manifest();
    hasher.update((m.qualified_name().len() as u64).to_le_bytes());
    hasher.update(m.qualified_name().as_bytes());
    let ver = m.version.to_string();
    hasher.update((ver.len() as u64).to_le_bytes());
    hasher.update(ver.as_bytes());
    let raw_toml = pkg.manifest.to_toml_string();
    hasher.update((raw_toml.len() as u64).to_le_bytes());
    hasher.update(raw_toml.as_bytes());

    let units = source_units(pkg)?;
    hasher.update((units.len() as u64).to_le_bytes());
    for u in units {
        hasher.update((u.path.len() as u64).to_le_bytes());
        for seg in u.path {
            hasher.update((seg.len() as u64).to_le_bytes());
            hasher.update(seg.as_bytes());
        }
        hasher.update((u.file.source.len() as u64).to_le_bytes());
        hasher.update(u.file.source.as_bytes());
    }
    hasher.update((pkg.wasm_assets.len() as u64).to_le_bytes());
    for wa in &pkg.wasm_assets {
        hasher.update((wa.name.len() as u64).to_le_bytes());
        hasher.update(wa.name.as_bytes());
        hasher.update((wa.bytes.len() as u64).to_le_bytes());
        hasher.update(&wa.bytes);
    }

    let mut sorted_deps: Vec<&ModularArtifact> = deps.iter().map(Arc::as_ref).collect();
    sorted_deps.sort_by(|a, b| {
        (&a.exports.qualified_name, &a.exports.version)
            .cmp(&(&b.exports.qualified_name, &b.exports.version))
    });
    hasher.update((sorted_deps.len() as u64).to_le_bytes());
    for d in sorted_deps {
        hasher.update((d.exports.qualified_name.len() as u64).to_le_bytes());
        hasher.update(d.exports.qualified_name.as_bytes());
        hasher.update((d.exports.version.len() as u64).to_le_bytes());
        hasher.update(d.exports.version.as_bytes());
        hasher.update(d.exports.identity);
        hasher.update(d.exports.interface_hash);
    }

    let digest: [u8; 32] = hasher.finalize().into();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let safe_name = m.qualified_name().replace('/', "__");
    Ok(cache_dir
        .join("modular")
        .join(format!("{safe_name}-{hex}.nsbc")))
}

/// Load a validated [`ModularArtifact`] from a content-addressed `.nsbc` path using the
/// process-wide in-memory cache when available.
pub fn load_cached_modular_artifact(path: &Path) -> Option<Arc<ModularArtifact>> {
    load_cached_modular(path)
}

fn load_cached_modular(path: &Path) -> Option<Arc<ModularArtifact>> {
    if !path.is_file() {
        return None;
    }
    let mem_map = PKG_MODULAR_MEM_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = mem_map.lock()
        && let Some(hit) = guard.get(path)
    {
        return Some(Arc::clone(hit));
    }
    let bytes = std::fs::read(path).ok()?;
    match nsbc_io::read_modular_artifact(&bytes) {
        Ok(artifact) => {
            let arc = Arc::new(artifact);
            if let Ok(mut guard) = mem_map.lock() {
                if guard.len() >= PKG_MODULAR_MEM_LIMIT {
                    guard.clear();
                }
                guard.insert(path.to_path_buf(), Arc::clone(&arc));
            }
            Some(arc)
        }
        Err(_) => {
            let _ = std::fs::remove_file(path);
            None
        }
    }
}

fn store_cached_modular(path: &Path, artifact: &Arc<ModularArtifact>) {
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(bytes) = nsbc_io::write_modular_artifact(artifact) else {
        return;
    };
    let seq = CACHE_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        ".{}.tmp.{}.{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("mod.nsbc"),
        std::process::id(),
        seq
    );
    let tmp_path = parent.join(tmp_name);
    if std::fs::write(&tmp_path, &bytes).is_ok() {
        if std::fs::rename(&tmp_path, path).is_ok() {
            let mem_map = PKG_MODULAR_MEM_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
            if let Ok(mut guard) = mem_map.lock() {
                if guard.len() >= PKG_MODULAR_MEM_LIMIT {
                    guard.clear();
                }
                guard.insert(path.to_path_buf(), Arc::clone(artifact));
            }
        } else {
            let _ = std::fs::remove_file(&tmp_path);
        }
    }
}

impl Driver {
    /// Compile (or load from memory/disk cache) the standard library as a [`ModularArtifact`].
    pub fn compile_std_modular(&self) -> Result<Arc<ModularArtifact>, Vec<Diagnostic>> {
        let opt_level = self.effective_opt_level(None);
        let mem_map = STD_MODULAR_MEM_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        if let Ok(guard) = mem_map.lock()
            && let Some(found) = guard.get(&opt_level)
        {
            return Ok(Arc::clone(found));
        }

        let cache_dir = self.resolved_cache_dir();
        if let Some(dir) = cache_dir.as_deref() {
            let path = modular_std_cache_path(dir, opt_level);
            if let Some(cached) = load_cached_modular(&path) {
                if let Ok(mut guard) = mem_map.lock() {
                    guard.insert(opt_level, Arc::clone(&cached));
                }
                return Ok(cached);
            }
        }

        let (empty_ast, mut diagnostics) = self.parse_source_named("", "std-host-wrapper")?;
        let (ast, options) = self.load_standard_library(empty_ast, &mut diagnostics)?;
        let std_root = options
            .package_roots
            .iter()
            .copied()
            .next()
            .ok_or_else(|| self.package_errors("missing std package root".into()))?;
        let std_identity =
            crate::identity::standard_library_identity().map_err(|e| self.package_errors(e))?;
        let manifest_toml = include_str!("../../../../library/std/package.toml").to_owned();

        let (artifact, _warnings) = self.finish_modular_pipeline(
            ast,
            diagnostics,
            options,
            std_root,
            nir::StartupMode::InitializationOnly,
            opt_level,
            &std_identity.package.qualified_name,
            &std_identity.package.version,
            std_identity.package.identity,
            manifest_toml,
            true,
            None,
            &HashMap::new(),
        )?;

        let arc = Arc::new(artifact);
        if let Some(dir) = cache_dir.as_deref() {
            let path = modular_std_cache_path(dir, opt_level);
            store_cached_modular(&path, &arc);
        }
        if let Ok(mut guard) = mem_map.lock() {
            guard.insert(opt_level, Arc::clone(&arc));
        }
        Ok(arc)
    }

    fn reconstruct_stub_tree(
        &self,
        ast: &mut Ast,
        label_prefix: &str,
        stubs: &[InterfaceModuleStub],
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<Vec<NodeIndex>, Vec<Diagnostic>> {
        if stubs.is_empty() {
            return Ok(Vec::new());
        }
        let mut sorted: Vec<&InterfaceModuleStub> = stubs.iter().collect();
        sorted.sort_by(|a, b| {
            a.module_path
                .len()
                .cmp(&b.module_path.len())
                .then_with(|| a.module_path.cmp(&b.module_path))
        });
        if !sorted[0].module_path.is_empty() {
            return Err(self.package_errors(format!(
                "corrupt interface stubs in `{label_prefix}`: missing root module `[]`"
            )));
        }

        let mut path_to_idx: HashMap<Vec<String>, usize> = HashMap::with_capacity(sorted.len());
        let mut parsed_asts = Vec::with_capacity(sorted.len());
        for (idx, stub) in sorted.iter().enumerate() {
            path_to_idx.insert(stub.module_path.clone(), idx);
            let label = if stub.module_path.is_empty() {
                format!("{label_prefix}/<stub-root>.ns")
            } else {
                format!("{label_prefix}/{}.stub.ns", stub.module_path.join("/"))
            };
            let (p_ast, warnings) = self.parse_source_named(&stub.stub_source, &label)?;
            diagnostics.extend(warnings);
            parsed_asts.push(p_ast);
        }

        let roots = self.append_source_units(ast, parsed_asts)?;
        let mut members: Vec<Vec<NodeIndex>> = roots
            .iter()
            .map(|&r| ast.multi_children(r).to_vec())
            .collect();
        let mut generated: Vec<Vec<NodeIndex>> = vec![Vec::new(); sorted.len()];

        for idx in (1..sorted.len()).rev() {
            members[idx].extend(generated[idx].iter().rev().copied());
            let mpath = &sorted[idx].module_path;
            let parent_slice = &mpath[..mpath.len() - 1];
            let Some(&parent_idx) = path_to_idx.get(parent_slice) else {
                return Err(self.package_errors(format!(
                    "orphan interface module `{}` in `{label_prefix}`",
                    mpath.join(".")
                )));
            };
            let name = &mpath[mpath.len() - 1];
            let matching: Vec<NodeIndex> = members[parent_idx]
                .iter()
                .copied()
                .filter(|&it| {
                    binding_name(ast, it).is_some_and(|id| str_interner::get(id) == *name)
                })
                .collect();
            let body = &members[idx];
            if let Some(&item) = matching.first() {
                let decl = unwrap_vis(ast, item);
                let start = ast.extra_children.len() as u32;
                ast.extra_children.extend_from_slice(body);
                ast.nodes[decl.0 as usize].multi_start = start;
                ast.nodes[decl.0 as usize].multi_len = body.len() as u32;
            } else {
                let name_node = self.package_node(
                    ast,
                    NodeKind::Id,
                    DUMMY_SP,
                    str_interner::intern(name),
                    NodeIndex::NULL,
                    &[],
                )?;
                let mod_node = self.package_node(
                    ast,
                    NodeKind::ModuleDef,
                    ast.node(roots[idx]).span,
                    str_interner::StrId::from_raw(0),
                    name_node,
                    body,
                )?;
                generated[parent_idx].push(mod_node);
            }
        }
        members[0].extend(generated[0].iter().rev().copied());
        Ok(std::mem::take(&mut members[0]))
    }

    fn mount_std_from_artifact(
        &self,
        mut ast: Ast,
        std_artifact: &ModularArtifact,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<(Ast, ResolveOptions, NodeIndex), Vec<Diagnostic>> {
        let user_items = ast.multi_children(ast.root).to_vec();
        let mut options = ResolveOptions {
            expose_root_builtins: false,
            expose_root_types: false,
            ..ResolveOptions::default()
        };
        let std_members = self.reconstruct_stub_tree(
            &mut ast,
            "std",
            &std_artifact.exports.interface_modules,
            diagnostics,
        )?;
        let name_node = self.package_node(
            &mut ast,
            NodeKind::Id,
            DUMMY_SP,
            str_interner::intern("std"),
            NodeIndex::NULL,
            &[],
        )?;
        let std_pkg_node = self.package_node(
            &mut ast,
            NodeKind::ModuleDef,
            DUMMY_SP,
            str_interner::StrId::from_raw(0),
            name_node,
            &std_members,
        )?;
        crate::standard_library::mark_trusted(&ast, std_pkg_node, &mut options.privileged_nodes);
        mark_stub_declarations(&ast, std_pkg_node, &mut options.external_stub_nodes);
        options.package_roots.insert(std_pkg_node);
        options.external_package_roots.insert(std_pkg_node);

        let std_identity =
            crate::identity::standard_library_identity().map_err(|e| self.package_errors(e))?;
        options
            .package_identities
            .insert(std_pkg_node, std_identity);

        let pub_std = self.package_node(
            &mut ast,
            NodeKind::PubDef,
            DUMMY_SP,
            str_interner::StrId::from_raw(0),
            std_pkg_node,
            &[],
        )?;
        let (prelude, warnings) =
            self.parse_source_named("use std.prelude.*", "implicit-prelude")?;
        diagnostics.extend(warnings);
        let prelude_root = self.append_source_units(&mut ast, vec![prelude])?[0];
        let imports = ast.multi_children(prelude_root).to_vec();
        options.implicit_imports.extend(imports.iter().copied());

        let mut root_items = Vec::with_capacity(1 + imports.len() + user_items.len());
        root_items.push(pub_std);
        root_items.extend(imports);
        root_items.extend(user_items);
        let root_span = ast.node(ast.root).span;
        ast.root = self.package_node(
            &mut ast,
            NodeKind::FileScope,
            root_span,
            str_interner::StrId::from_raw(0),
            NodeIndex::NULL,
            &root_items,
        )?;
        Ok((ast, options, std_pkg_node))
    }

    /// Compile a single package directly against precompiled dependency [`ModularArtifact`]s
    /// without reading or parsing any dependency `.ns` implementation files!
    pub fn compile_package_modular(
        &self,
        pkg: &PackageSources,
        deps: &[Arc<ModularArtifact>],
        privileged_packages: &BTreeSet<String>,
    ) -> Result<(ModularArtifact, Vec<Diagnostic>), Vec<Diagnostic>> {
        let std_artifact = self.compile_std_modular()?;
        self.compile_package_modular_with_std(pkg, deps, &std_artifact, privileged_packages)
    }

    pub(crate) fn compile_package_modular_with_std(
        &self,
        pkg: &PackageSources,
        deps: &[Arc<ModularArtifact>],
        std_artifact: &Arc<ModularArtifact>,
        privileged_packages: &BTreeSet<String>,
    ) -> Result<(ModularArtifact, Vec<Diagnostic>), Vec<Diagnostic>> {
        // Resolve manifest dependency graph purely from `pkg.manifest` + `deps`' embedded `manifest_toml`!
        let mut resolver = PackageResolver::new();
        let mut dep_by_key: BTreeMap<(String, String), &Arc<ModularArtifact>> = BTreeMap::new();
        let mut dep_docs: HashMap<String, ManifestDocument> = HashMap::new();
        for dep in deps {
            if dep.exports.is_privileged_std {
                continue;
            }
            let doc = ManifestDocument::parse(&dep.exports.manifest_toml)
                .map_err(|e| self.package_errors(e.to_string()))?;
            resolver
                .register_document(doc.clone())
                .map_err(|e| self.package_errors(e.to_string()))?;
            dep_docs.insert(dep.exports.qualified_name.clone(), doc);
            dep_by_key.insert(
                (
                    dep.exports.qualified_name.clone(),
                    dep.exports.version.clone(),
                ),
                dep,
            );
        }
        // Also allow bundled packages if compiled in-memory if present in `deps`.
        let graph = resolver
            .resolve_document(&pkg.manifest)
            .map_err(|e| self.package_errors(e.to_string()))?;
        let root_identity =
            CompilationIdentityContext::from_graph(&graph).map_err(|e| self.package_errors(e))?;

        let (raw_ast, mut diagnostics) = self.package_ast(pkg)?;
        let (mut ast, mut options, std_node) =
            self.mount_std_from_artifact(raw_ast, std_artifact, &mut diagnostics)?;

        let root_qname = pkg.manifest.manifest().qualified_name();
        if privileged_packages.contains(&root_qname) {
            // Mark only the user's own items (not external deps yet to be appended).
            for &item in ast.multi_children(ast.root) {
                if item != std_node && ast.fixed_children(item).first().copied() != Some(std_node) {
                    crate::standard_library::mark_trusted(
                        &ast,
                        item,
                        &mut options.privileged_nodes,
                    );
                }
            }
        }
        crate::standard_library::mark_ffi_permissions(
            &ast,
            ast.root,
            pkg.manifest.ffi(),
            &mut options,
        );

        let mut pkg_nodes: BTreeMap<String, NodeIndex> = BTreeMap::new();
        let mut ext_root_to_artifact: HashMap<NodeIndex, Arc<ModularArtifact>> = HashMap::new();
        ext_root_to_artifact.insert(std_node, Arc::clone(std_artifact));

        let mut root_members = ast.multi_children(ast.root).to_vec();
        for manifest in graph.packages() {
            let qname = manifest.qualified_name();
            if qname == root_qname {
                continue;
            }
            let key = (qname.clone(), manifest.version.to_string());
            let dep_art = dep_by_key.get(&key).copied().ok_or_else(|| {
                self.package_errors(format!(
                    "missing precompiled modular dependency `{}`@{}",
                    key.0, key.1
                ))
            })?;
            let stub_members = self.reconstruct_stub_tree(
                &mut ast,
                &qname,
                &dep_art.exports.interface_modules,
                &mut diagnostics,
            )?;
            let (prelude, warnings) =
                self.parse_source_named("use std.prelude.*", "implicit-dep-prelude")?;
            diagnostics.extend(warnings);
            let prelude_root = self.append_source_units(&mut ast, vec![prelude])?[0];
            let imports = ast.multi_children(prelude_root).to_vec();
            options.implicit_imports.extend(imports.iter().copied());

            let mut items = imports;
            items.extend(stub_members);
            let name_node = self.package_node(
                &mut ast,
                NodeKind::Id,
                DUMMY_SP,
                str_interner::intern(&manifest.name),
                NodeIndex::NULL,
                &[],
            )?;
            let dep_mod_node = self.package_node(
                &mut ast,
                NodeKind::ModuleDef,
                DUMMY_SP,
                str_interner::StrId::from_raw(0),
                name_node,
                &items,
            )?;
            mark_stub_declarations(&ast, dep_mod_node, &mut options.external_stub_nodes);
            options.package_roots.insert(dep_mod_node);
            options.detached_package_roots.insert(dep_mod_node);
            options.external_package_roots.insert(dep_mod_node);
            if privileged_packages.contains(&qname) {
                crate::standard_library::mark_trusted(
                    &ast,
                    dep_mod_node,
                    &mut options.privileged_nodes,
                );
            }
            if let Some(doc) = dep_docs.get(&qname) {
                crate::standard_library::mark_ffi_permissions(
                    &ast,
                    dep_mod_node,
                    doc.ffi(),
                    &mut options,
                );
            }
            options.package_identities.insert(
                dep_mod_node,
                SourcePackageIdentity {
                    package: type_pool::PackageTypeContext {
                        identity_schema: 1,
                        identity: dep_art.exports.identity,
                        qualified_name: dep_art.exports.qualified_name.clone(),
                        version: dep_art.exports.version.clone(),
                    },
                    last_stable_version: None,
                    type_versions: Vec::new(),
                },
            );
            pkg_nodes.insert(qname, dep_mod_node);
            ext_root_to_artifact.insert(dep_mod_node, Arc::clone(dep_art));
            root_members.push(dep_mod_node);
        }

        let root_span = ast.node(ast.root).span;
        ast.root = self.package_node(
            &mut ast,
            NodeKind::FileScope,
            root_span,
            str_interner::StrId::from_raw(0),
            NodeIndex::NULL,
            &root_members,
        )?;
        if privileged_packages.contains(&root_qname) {
            options.privileged_nodes.insert(ast.root);
        }
        pkg_nodes.insert(root_qname.clone(), ast.root);
        options
            .package_identities
            .insert(ast.root, root_identity.clone().into_source());

        for manifest in graph.packages() {
            let mut imports_map: HashMap<str_interner::StrId, Vec<NodeIndex>> = HashMap::new();
            imports_map
                .entry(str_interner::intern("std"))
                .or_default()
                .push(std_node);
            for dep in &manifest.dependencies {
                let dname = format!("{}/{}", dep.domain, dep.name);
                let Some(&target_node) = pkg_nodes.get(&dname) else {
                    return Err(self.package_errors(format!(
                        "selected dependency `{dname}` has no mounted interface root"
                    )));
                };
                imports_map
                    .entry(str_interner::intern(&dep.name))
                    .or_default()
                    .push(target_node);
            }
            options
                .package_dependencies
                .insert(pkg_nodes[&manifest.qualified_name()], imports_map);
        }

        let startup = match pkg.manifest.manifest().package_type {
            PackageType::Exe => nir::StartupMode::RequiredMain,
            PackageType::Lib | PackageType::Tmp => nir::StartupMode::InitializationOnly,
        };
        let opt_level = self.effective_opt_level(pkg.manifest.build_opt_level());
        let target_root = ast.root;

        self.finish_modular_pipeline(
            ast,
            diagnostics,
            options,
            target_root,
            startup,
            opt_level,
            &root_identity.package.qualified_name,
            &root_identity.package.version,
            root_identity.package.identity,
            pkg.manifest.to_toml_string(),
            false,
            Some(pkg),
            &ext_root_to_artifact,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_modular_pipeline(
        &self,
        ast: Ast,
        mut diagnostics: Vec<Diagnostic>,
        options: ResolveOptions,
        target_root: NodeIndex,
        startup: nir::StartupMode,
        opt_level: OptLevel,
        qualified_name: &str,
        version: &str,
        identity: [u8; 16],
        manifest_toml: String,
        is_privileged_std: bool,
        pkg_sources: Option<&PackageSources>,
        ext_root_to_artifact: &HashMap<NodeIndex, Arc<ModularArtifact>>,
    ) -> Result<(ModularArtifact, Vec<Diagnostic>), Vec<Diagnostic>> {
        let diag_ctx = DiagnosticContext::new(&self.source_map);
        let mut resolved = resolution::resolve_with_options(ast, &diag_ctx, options);
        diagnostics.extend(diag_ctx.diagnostics().iter().cloned());
        diagnostics.extend(resolved.diagnostics.iter().cloned());
        if diagnostics.iter().any(|d| d.level == Level::Error) {
            return Err(diagnostics);
        }

        let mut mod_nir = match nir::try_lower_modular(&resolved, target_root, startup) {
            Ok(out) => out,
            Err(errors) => {
                let ctx = DiagnosticContext::new(&self.source_map);
                for error in errors {
                    let mut builder = ctx
                        .error(error.message)
                        .with_code(error.code.unwrap_or(diagnostic::codes::NIR_LOWERING_ERROR))
                        .with_primary_span(resolved.ast.node(error.node).span);
                    if let Some((note_node, note_msg)) = error.note_node
                        && !note_node.is_null()
                    {
                        builder = builder.with_label(
                            resolved.ast.node(note_node).span,
                            note_msg,
                            Level::Note,
                        );
                    }
                    if let Some(help) = error.help {
                        builder = builder.with_help(help);
                    }
                    builder.emit(&ctx);
                }
                diagnostics.extend(ctx.diagnostics().iter().cloned());
                return Err(diagnostics);
            }
        };

        if let Err(msg) =
            nir::finalize_modular_nir(&mut mod_nir, &mut resolved.type_pool, opt_level)
        {
            diagnostics.extend(self.package_errors(msg));
            return Err(diagnostics);
        }

        let interface_modules = synthesize_interface_modules(
            &resolved,
            &self.source_map,
            target_root,
            &mod_nir.modules,
        )
        .map_err(|msg| self.package_errors(msg))?;

        let mut codegen_output = codegen::try_codegen_with_imported_globals(
            &mod_nir.nir_module,
            mod_nir.imported_globals.len(),
            opt_level,
        )
        .map_err(|errs| {
            let ctx = DiagnosticContext::new(&self.source_map);
            for err in errs {
                ctx.error(err.message).emit(&ctx);
            }
            ctx.diagnostics().to_vec()
        })?;

        let wasm_modules = if let Some(pkg) = pkg_sources {
            attach_single_package_ffi_wasm(pkg, &mut codegen_output, &mut resolved.type_pool)
                .map_err(|msg| self.package_errors(msg))?
        } else {
            Vec::new()
        };

        let builtins =
            collect_builtin_imports(&codegen_output).map_err(|msg| self.package_errors(msg))?;

        // Assign dense `package_index` values ONLY for external packages actually referenced
        // by imported functions, imported globals, or external module dependencies.
        let mut used_ext_roots: Vec<NodeIndex> = Vec::new();
        let mut seen_roots = HashSet::new();
        for root_node in mod_nir
            .imported_functions
            .iter()
            .map(|f| f.package_root)
            .chain(mod_nir.imported_globals.iter().map(|g| g.package_root))
            .chain(
                mod_nir
                    .modules
                    .iter()
                    .flat_map(|m| m.external_module_deps.iter().map(|(r, _)| *r))
                    .chain(
                        mod_nir
                            .modules
                            .iter()
                            .flat_map(|m| m.external_init_prereqs.iter().map(|(r, _)| *r)),
                    ),
            )
        {
            if seen_roots.insert(root_node) {
                used_ext_roots.push(root_node);
            }
        }
        used_ext_roots.sort_by(|a, b| {
            let aa = &ext_root_to_artifact[a].exports;
            let ba = &ext_root_to_artifact[b].exports;
            (&aa.qualified_name, &aa.version).cmp(&(&ba.qualified_name, &ba.version))
        });

        let mut pkg_index_by_root: HashMap<NodeIndex, u32> = HashMap::new();
        let mut imported_packages: Vec<ImportedPackageRef> =
            Vec::with_capacity(used_ext_roots.len());
        let mut dep_exports_by_pkg_index: HashMap<u32, HashMap<SymbolLinkSelector, &ExportTarget>> =
            HashMap::new();

        for (idx, root_node) in used_ext_roots.iter().enumerate() {
            let dep_art = &ext_root_to_artifact[root_node];
            let pidx = idx as u32;
            pkg_index_by_root.insert(*root_node, pidx);
            imported_packages.push(ImportedPackageRef {
                qualified_name: dep_art.exports.qualified_name.clone(),
                version: dep_art.exports.version.clone(),
                identity: dep_art.exports.identity,
                interface_hash: dep_art.exports.interface_hash,
            });
            let sym_map: HashMap<SymbolLinkSelector, &ExportTarget> = dep_art
                .exports
                .symbols
                .iter()
                .map(|s| (s.selector.clone(), &s.target))
                .collect();
            dep_exports_by_pkg_index.insert(pidx, sym_map);
        }

        let (imp_functions, imp_globals) = build_imported_descriptors(
            &resolved,
            &resolved.type_pool,
            &pkg_index_by_root,
            &dep_exports_by_pkg_index,
            &mod_nir.imported_functions,
            &mod_nir.imported_globals,
        )
        .map_err(|msg| self.package_errors(msg))?;

        let mut ext_mod_set: BTreeSet<ImportedModuleRef> = BTreeSet::new();
        for m in &mod_nir.modules {
            for (r, mp) in m
                .external_module_deps
                .iter()
                .chain(&m.external_init_prereqs)
            {
                ext_mod_set.insert(ImportedModuleRef {
                    package_index: pkg_index_by_root[r],
                    module_path: mp.clone(),
                });
            }
        }
        let module_dependencies: Vec<ImportedModuleRef> = ext_mod_set.into_iter().collect();
        let ext_mod_index: HashMap<ImportedModuleRef, u32> = module_dependencies
            .iter()
            .enumerate()
            .map(|(i, m)| (m.clone(), i as u32))
            .collect();

        let exported_symbols = build_exported_symbols(
            &resolved,
            &resolved.type_pool,
            &codegen_output,
            &mod_nir.modules,
            &mod_nir.local_exports,
        )
        .map_err(|msg| self.package_errors(msg))?;

        let mut module_descriptors = Vec::with_capacity(mod_nir.modules.len());
        for m in &mod_nir.modules {
            let module_type_id = match m.module_type_index {
                Some(ty) => resolved.type_pool.stable_type_id(ty).ok(),
                None => None,
            };
            let external_module_deps = m
                .external_module_deps
                .iter()
                .map(|(r, mp)| {
                    ext_mod_index[&ImportedModuleRef {
                        package_index: pkg_index_by_root[r],
                        module_path: mp.clone(),
                    }]
                })
                .collect();
            let external_init_prereqs = m
                .external_init_prereqs
                .iter()
                .map(|(r, mp)| {
                    ext_mod_index[&ImportedModuleRef {
                        package_index: pkg_index_by_root[r],
                        module_path: mp.clone(),
                    }]
                })
                .collect();
            module_descriptors.push(ModuleDescriptor {
                module_path: m.module_path.clone(),
                module_type_id,
                owned_functions: m.owned_functions.clone(),
                owned_globals: m.owned_globals.clone(),
                scope_initializers: m.scope_initializers.clone(),
                local_module_deps: m.local_module_deps.clone(),
                local_init_prereqs: m.local_init_prereqs.clone(),
                external_module_deps,
                external_init_prereqs,
                main_func: m.main_func,
            });
        }

        let interface_hash = compute_interface_hash(
            qualified_name,
            version,
            &identity,
            &manifest_toml,
            &interface_modules,
        );

        let artifact = ModularArtifact {
            codegen_output,
            type_pool: resolved.type_pool,
            builtin_abi_version: runtime::BUILTIN_ABI_VERSION,
            builtins,
            wasm_modules,
            imports: ArtifactImports {
                packages: imported_packages,
                functions: imp_functions,
                globals: imp_globals,
                module_dependencies,
            },
            exports: ArtifactExports {
                qualified_name: qualified_name.to_owned(),
                version: version.to_owned(),
                identity,
                interface_hash,
                manifest_toml,
                is_privileged_std,
                modules: module_descriptors,
                symbols: exported_symbols,
                interface_modules,
            },
        };

        nsbc::validate_modular_artifact(&artifact)
            .map_err(|e| self.package_errors(format!("modular artifact validation failed: {e}")))?;

        Ok((artifact, diagnostics))
    }

    /// Resolve and compile a package DAG into cached per-package [`ModularArtifact`]s.
    ///
    /// - Each package's cache key depends ONLY on its own `.ns` sources and its dependencies'
    ///   `interface_hash`es (not their implementation bodies!).
    /// - Independent packages within the same DAG topological level are compiled in parallel.
    pub fn compile_modular_graph_cached(
        &self,
        root: &PackageSources,
        catalog: &[PackageSources],
        privileged_packages: &BTreeSet<String>,
    ) -> Result<ModularCompilationOutput, Vec<Diagnostic>> {
        let (graph, selected) = crate::package_graph::select_sources(root, catalog)
            .map_err(|e| self.package_errors(e))?;
        let std_artifact = self.compile_std_modular()?;
        let cache_dir = self.resolved_cache_dir();

        let by_qname: HashMap<String, &PackageSources> = selected
            .iter()
            .map(|&s| (s.manifest.manifest().qualified_name(), s))
            .collect();

        // Compute transitive dependency closure per package in `graph.packages()` (which is already topological!).
        let ordered_qnames: Vec<String> = graph
            .packages()
            .iter()
            .map(|m| m.qualified_name())
            .collect();
        let mut direct_deps: HashMap<String, Vec<String>> = HashMap::new();
        for m in graph.packages() {
            let deps: Vec<String> = m
                .dependencies
                .iter()
                .map(|d| format!("{}/{}", d.domain, d.name))
                .collect();
            direct_deps.insert(m.qualified_name(), deps);
        }

        // Partition into topological levels for parallel per-level compilation.
        let mut depth_of: HashMap<String, usize> = HashMap::new();
        let mut max_depth = 0usize;
        for qname in &ordered_qnames {
            let d = direct_deps[qname]
                .iter()
                .map(|dep| depth_of[dep] + 1)
                .max()
                .unwrap_or(0);
            depth_of.insert(qname.clone(), d);
            max_depth = max_depth.max(d);
        }
        let mut levels: Vec<Vec<String>> = vec![Vec::new(); max_depth + 1];
        for qname in &ordered_qnames {
            levels[depth_of[qname]].push(qname.clone());
        }

        let mut built_by_qname: HashMap<String, Arc<ModularArtifact>> = HashMap::new();
        let mut cache_path_by_qname: HashMap<String, Option<PathBuf>> = HashMap::new();
        let mut compiled_packages = Vec::new();
        let mut cached_packages = Vec::new();
        let mut all_diagnostics = Vec::new();

        let collect_transitive_deps = |qname: &str,
                                       built: &HashMap<String, Arc<ModularArtifact>>|
         -> Vec<Arc<ModularArtifact>> {
            let mut visited = BTreeSet::new();
            let mut stack = direct_deps[qname].clone();
            let mut out = Vec::new();
            while let Some(cur) = stack.pop() {
                if !visited.insert(cur.clone()) {
                    continue;
                }
                out.push(Arc::clone(&built[&cur]));
                stack.extend(direct_deps[&cur].iter().cloned());
            }
            out
        };

        let worker_limit = std::thread::available_parallelism()
            .map(|n| n.get().clamp(1, 8))
            .unwrap_or(4);

        for level in levels {
            let mut planned = Vec::with_capacity(level.len());
            for qname in level {
                let pkg_src = by_qname[&qname];
                let dep_arts = collect_transitive_deps(&qname, &built_by_qname);
                let is_priv = privileged_packages.contains(&qname);
                let opt_level = self.effective_opt_level(pkg_src.manifest.build_opt_level());
                let cache_path = match cache_dir.as_deref() {
                    Some(dir) => Some(
                        modular_pkg_cache_path(
                            dir,
                            pkg_src,
                            &dep_arts,
                            &std_artifact,
                            is_priv,
                            opt_level,
                        )
                        .map_err(|e| self.package_errors(e))?,
                    ),
                    None => None,
                };
                planned.push((qname, pkg_src, dep_arts, cache_path));
            }

            let cache_probe_results: Vec<Option<Arc<ModularArtifact>>> = if planned.len() <= 1 {
                planned
                    .iter()
                    .map(|(_, _, _, cp)| cp.as_deref().and_then(load_cached_modular))
                    .collect()
            } else {
                let mut probes = Vec::with_capacity(planned.len());
                for chunk in planned.chunks(worker_limit) {
                    let chunk_res: Vec<Option<Arc<ModularArtifact>>> = std::thread::scope(|s| {
                        let handles: Vec<_> = chunk
                            .iter()
                            .map(|(_, _, _, cp)| {
                                let cp_ref = cp.as_deref();
                                s.spawn(move || cp_ref.and_then(load_cached_modular))
                            })
                            .collect();
                        handles
                            .into_iter()
                            .map(|h| h.join().expect("cache loader worker panicked"))
                            .collect()
                    });
                    probes.extend(chunk_res);
                }
                probes
            };

            type PendingPkgCompile<'a> = (
                String,
                &'a PackageSources,
                Vec<Arc<ModularArtifact>>,
                Option<PathBuf>,
            );
            type PkgCompileOutcome = Result<
                (String, ModularArtifact, Vec<Diagnostic>, Option<PathBuf>),
                Vec<Diagnostic>,
            >;

            let mut to_compile: Vec<PendingPkgCompile<'_>> = Vec::new();
            for ((qname, pkg_src, dep_arts, cache_path), hit_opt) in
                planned.into_iter().zip(cache_probe_results)
            {
                if let Some(hit) = hit_opt {
                    cache_path_by_qname.insert(qname.clone(), cache_path);
                    built_by_qname.insert(qname.clone(), hit);
                    cached_packages.push(qname);
                } else {
                    to_compile.push((qname, pkg_src, dep_arts, cache_path));
                }
            }

            if to_compile.is_empty() {
                continue;
            }

            if to_compile.len() == 1 {
                let (qname, pkg_src, dep_arts, cache_path) = to_compile.pop().unwrap();
                let (artifact, warnings) = self.compile_package_modular_with_std(
                    pkg_src,
                    &dep_arts,
                    &std_artifact,
                    privileged_packages,
                )?;
                let arc = Arc::new(artifact);
                if let Some(path) = cache_path.as_deref() {
                    store_cached_modular(path, &arc);
                }
                cache_path_by_qname.insert(qname.clone(), cache_path);
                all_diagnostics.extend(warnings);
                built_by_qname.insert(qname.clone(), arc);
                compiled_packages.push(qname);
            } else {
                let explicit_opt = self.opt_level;
                let cache_override = self.cache_override.clone();
                for chunk in to_compile.chunks(worker_limit) {
                    let results: Vec<PkgCompileOutcome> = std::thread::scope(|s| {
                        let handles: Vec<_> = chunk
                            .iter()
                            .map(|(qname, pkg_src, dep_arts, cache_path)| {
                                let std_ref = &std_artifact;
                                let priv_ref = privileged_packages;
                                let cache_ov = cache_override.clone();
                                s.spawn(move || {
                                    let mut worker_driver = Driver::new();
                                    worker_driver.opt_level = explicit_opt;
                                    worker_driver.cache_override = cache_ov;
                                    let (art, warns) = worker_driver
                                        .compile_package_modular_with_std(
                                            pkg_src, dep_arts, std_ref, priv_ref,
                                        )?;
                                    Ok((qname.clone(), art, warns, cache_path.clone()))
                                })
                            })
                            .collect();
                        handles
                            .into_iter()
                            .map(|h| h.join().expect("modular compiler worker thread panicked"))
                            .collect()
                    });

                    for res in results {
                        let (qname, artifact, warnings, cache_path) = res?;
                        let arc = Arc::new(artifact);
                        if let Some(path) = cache_path.as_deref() {
                            store_cached_modular(path, &arc);
                        }
                        cache_path_by_qname.insert(qname.clone(), cache_path);
                        all_diagnostics.extend(warnings);
                        built_by_qname.insert(qname.clone(), arc);
                        compiled_packages.push(qname);
                    }
                }
            }
        }

        let std_opt = self.effective_opt_level(None);
        let std_cache_path = cache_dir
            .as_deref()
            .map(|dir| modular_std_cache_path(dir, std_opt));
        if let Some(std_cp) = std_cache_path.as_deref()
            && !std_cp.is_file()
        {
            store_cached_modular(std_cp, &std_artifact);
        }

        let mut artifacts = Vec::with_capacity(1 + ordered_qnames.len());
        let mut cache_paths = Vec::with_capacity(1 + ordered_qnames.len());
        artifacts.push(std_artifact);
        cache_paths.push(std_cache_path);
        for qname in &ordered_qnames {
            artifacts.push(Arc::clone(&built_by_qname[qname]));
            cache_paths.push(cache_path_by_qname.remove(qname).flatten());
        }

        Ok(ModularCompilationOutput {
            artifacts,
            cache_paths,
            root_qualified_name: root.manifest.manifest().qualified_name(),
            compiled_packages,
            cached_packages,
            diagnostics: all_diagnostics,
        })
    }
}
