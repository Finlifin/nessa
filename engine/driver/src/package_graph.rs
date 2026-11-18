//! Compile a selected manifest DAG from caller-owned package source trees.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use ast::{NodeIndex, NodeKind};
use pkg_manager::{PackageResolver, PackageSources, PackageType, SourceLoadLimits};
use resolution::ResolveOptions;
use rustc_span::DUMMY_SP;

use crate::package::source_units;
use crate::{CompilationIdentityContext, CompileResult, Driver};

type SourceKey = (String, String);

impl Driver {
    /// Resolve and compile a root and an in-memory source catalog in one pipeline.
    /// Only selected, reachable versions are parsed. Paths are diagnostic labels;
    /// compilation never reopens them. Conflicting duplicate source trees fail.
    pub fn compile_package_sources_with_dependencies(
        &self,
        root: &PackageSources,
        catalog: &[PackageSources],
    ) -> CompileResult {
        self.compile_package_sources_with_privileged(root, catalog, &BTreeSet::new())
    }

    /// Resolve and compile a root and an in-memory source catalog, reusing or populating
    /// the content-addressed `.nsbc` compilation cache when compilation succeeds.
    pub fn compile_package_sources_with_privileged_cached(
        &self,
        root: &PackageSources,
        catalog: &[PackageSources],
        privileged_packages: &BTreeSet<String>,
    ) -> Result<nsbc::CompiledArtifact, Vec<diagnostic::Diagnostic>> {
        let cache_dir = self.resolved_cache_dir();
        let opt_level = self.effective_opt_level(root.manifest.build_opt_level());
        let key =
            crate::artifact_cache::package_graph_key(root, catalog, privileged_packages, opt_level);
        if let Some(cached) =
            crate::artifact_cache::load_cached_artifact(cache_dir.as_deref(), &key)
        {
            return Ok(cached);
        }
        let compiled =
            self.compile_package_sources_with_privileged(root, catalog, privileged_packages);
        if compiled.has_errors {
            return Err(compiled.diagnostics);
        }
        let artifact = compiled
            .into_artifact()
            .map_err(|error| self.package_errors(error.to_string()))?;
        crate::artifact_cache::store_cached_artifact(cache_dir.as_deref(), &key, &artifact);
        Ok(artifact)
    }

    /// Resolve and compile a root and an in-memory source catalog, granting `'builtin`
    /// view access to explicitly listed host-bridge qualified package names (`domain/name`).
    pub fn compile_package_sources_with_privileged(
        &self,
        root: &PackageSources,
        catalog: &[PackageSources],
        privileged_packages: &BTreeSet<String>,
    ) -> CompileResult {
        let selected = match select_sources(root, catalog) {
            Ok(selected) => selected,
            Err(error) => return self.package_failure(error),
        };
        let (graph, sources) = selected;
        let identity = match CompilationIdentityContext::from_graph(&graph) {
            Ok(identity) => identity,
            Err(error) => return self.package_failure(error),
        };
        let (mut ast, mut diagnostics) = match self.package_ast(root) {
            Ok(parsed) => parsed,
            Err(errors) => {
                return CompileResult::failed(errors, type_pool::TypePool::with_intrinsics());
            }
        };
        let mut options = ResolveOptions::default();
        if privileged_packages.contains(&root.manifest.manifest().qualified_name()) {
            crate::standard_library::mark_trusted(&ast, ast.root, &mut options.privileged_nodes);
        }
        crate::standard_library::mark_ffi_permissions(
            &ast,
            ast.root,
            root.manifest.ffi(),
            &mut options,
        );
        let mut nodes = BTreeMap::from([(root.manifest.manifest().qualified_name(), ast.root)]);
        let mut dependencies = Vec::new();
        for source in &sources {
            if std::ptr::eq(*source, root) {
                continue;
            }
            let (dependency, warnings) = match self.package_ast(source) {
                Ok(parsed) => parsed,
                Err(errors) => {
                    return CompileResult::failed(errors, type_pool::TypePool::with_intrinsics());
                }
            };
            diagnostics.extend(warnings);
            dependencies.push(dependency);
        }
        let bodies = match self.append_source_units(&mut ast, dependencies) {
            Ok(bodies) => bodies,
            Err(errors) => {
                return CompileResult::failed(errors, type_pool::TypePool::with_intrinsics());
            }
        };
        let mut members = ast.multi_children(ast.root).to_vec();
        for (source, body) in sources
            .iter()
            .filter(|source| !std::ptr::eq(**source, root))
            .zip(bodies)
        {
            let manifest = source.manifest.manifest();
            let result = (|| {
                let (prelude, warnings) =
                    self.parse_source_named("use std.prelude.*", "implicit-package-prelude")?;
                diagnostics.extend(warnings);
                let prelude = self.append_source_units(&mut ast, vec![prelude])?[0];
                let imports = ast.multi_children(prelude).to_vec();
                options.implicit_imports.extend(imports.iter().copied());
                let mut items = imports;
                items.extend_from_slice(ast.multi_children(body));
                let name = self.package_node(
                    &mut ast,
                    NodeKind::Id,
                    DUMMY_SP,
                    str_interner::intern(&manifest.name),
                    NodeIndex::NULL,
                    &[],
                )?;
                let span = ast.node(body).span;
                self.package_node(
                    &mut ast,
                    NodeKind::ModuleDef,
                    span,
                    str_interner::StrId::from_raw(0),
                    name,
                    &items,
                )
            })();
            let node = match result {
                Ok(node) => node,
                Err(errors) => {
                    return CompileResult::failed(errors, type_pool::TypePool::with_intrinsics());
                }
            };
            nodes.insert(manifest.qualified_name(), node);
            options.package_roots.insert(node);
            options.detached_package_roots.insert(node);
            if privileged_packages.contains(&manifest.qualified_name()) {
                crate::standard_library::mark_trusted(&ast, node, &mut options.privileged_nodes);
            }
            crate::standard_library::mark_ffi_permissions(
                &ast,
                node,
                source.manifest.ffi(),
                &mut options,
            );
            let context =
                match CompilationIdentityContext::for_package(&graph, &manifest.qualified_name()) {
                    Ok(context) => context,
                    Err(error) => return self.package_failure(error),
                };
            options
                .package_identities
                .insert(node, context.into_source());
            members.push(node);
        }
        let original_root = ast.root;
        let span = ast.node(original_root).span;
        ast.root = match self.package_node(
            &mut ast,
            NodeKind::FileScope,
            span,
            str_interner::StrId::from_raw(0),
            NodeIndex::NULL,
            &members,
        ) {
            Ok(node) => node,
            Err(errors) => {
                return CompileResult::failed(errors, type_pool::TypePool::with_intrinsics());
            }
        };
        nodes.insert(root.manifest.manifest().qualified_name(), ast.root);
        for manifest in graph.packages() {
            let mut imports = HashMap::new();
            for dependency in &manifest.dependencies {
                let name = format!("{}/{}", dependency.domain, dependency.name);
                let Some(&target) = nodes.get(&name) else {
                    return self.package_failure(format!(
                        "selected dependency `{name}` has no source root"
                    ));
                };
                imports
                    .entry(str_interner::intern(&dependency.name))
                    .or_insert_with(Vec::new)
                    .push(target);
            }
            options
                .package_dependencies
                .insert(nodes[&manifest.qualified_name()], imports);
        }
        let startup = match root.manifest.manifest().package_type {
            PackageType::Exe => nir::StartupMode::RequiredMain,
            PackageType::Lib | PackageType::Tmp => nir::StartupMode::InitializationOnly,
        };
        let opt_level = self.effective_opt_level(root.manifest.build_opt_level());
        let mut compiled = self.compile_ast_with_options(
            ast,
            diagnostics,
            Some(identity),
            startup,
            Some(options),
            opt_level,
        );
        if !compiled.has_errors
            && let Err(message) = attach_ffi_wasm_modules(root, &sources, &mut compiled)
        {
            let mut errs = compiled.diagnostics;
            errs.extend(self.package_errors(message));
            return CompileResult::failed(errs, compiled.type_pool);
        }
        compiled
    }
}

fn attach_ffi_wasm_modules(
    root: &PackageSources,
    selected_sources: &[&PackageSources],
    compiled: &mut CompileResult,
) -> Result<(), String> {
    use nsbc::{
        AddrMode, Constant, Instruction, InstructionData, Opcode, Reg, WasmBindingDef,
        WasmModuleDef,
    };
    use type_pool::{Intrinsic, TypeIndex, TypeKind};

    let any_wasm = !root.manifest.ffi().wasm.is_empty()
        || selected_sources
            .iter()
            .any(|s| !s.manifest.ffi().wasm.is_empty());
    if !any_wasm {
        return Ok(());
    }

    let mut ordered_pkgs: Vec<&PackageSources> = Vec::new();
    for &src in selected_sources {
        if !std::ptr::eq(src, root) {
            ordered_pkgs.push(src);
        }
    }
    ordered_pkgs.push(root);

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

    // Map `"__nessa_wasm_bind:<qpkg>::<mod_name>::<nessa_name>"` -> `(binding_id, argc, is_unit_ret, nessa_name)`.
    let mut marker_to_binding: HashMap<String, (u32, u8, bool, String)> = HashMap::new();
    let mut wasm_modules = Vec::new();
    let mut next_binding_id: u32 = 0;

    for pkg in ordered_pkgs {
        let qpkg = pkg.manifest.manifest().qualified_name();
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

            let archive_mod_name = if std::ptr::eq(pkg, root) {
                spec.name.clone()
            } else {
                format!("{qpkg}::{}", spec.name)
            };

            let mut bindings = Vec::with_capacity(spec.exports.len());
            for exp in &spec.exports {
                let mut param_tys = Vec::with_capacity(exp.params.len());
                for p_name in &exp.params {
                    param_tys.push(resolve_type_name(&compiled.type_pool, p_name)?);
                }
                let ret_ty = resolve_type_name(&compiled.type_pool, &exp.ret)?;
                let is_unit_ret = ret_ty == Intrinsic::Unit.type_index();
                let sig_idx = compiled.type_pool.intern_structural(TypeKind::Function {
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
                name: archive_mod_name,
                wasm_bytes: asset.bytes.clone(),
                bindings,
            });
        }
    }

    // Identify constant pool indices holding `"__nessa_wasm_bind:..."` markers.
    let mut const_idx_to_binding: HashMap<u32, (u32, u8, bool, String)> = HashMap::new();
    for (cidx, c) in compiled.codegen_output.constants.iter().enumerate() {
        if let Constant::Str(s) = c
            && let Some(info) = marker_to_binding.get(s)
        {
            const_idx_to_binding.insert(cidx as u32, info.clone());
        }
    }

    let mut rewritten_funcs = std::collections::HashSet::new();
    for func in &mut compiled.codegen_output.functions {
        if Some(func.func_id) == compiled.entry_func_id {
            continue;
        }
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
                    let idx19 = (u32::from(base.0) << 12) | u32::from(imm12);
                    if let Some((bid, argc, is_unit, expected_name)) =
                        const_idx_to_binding.get(&idx19)
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
        compiled
            .codegen_output
            .method_call_scopes
            .retain(|s| !rewritten_funcs.contains(&s.func_id));
    }

    compiled.wasm_modules = wasm_modules;

    // Validate the resulting embedded WebAssembly modules, exports, and signatures immediately
    // so `nessa check`, `nessa build`, and `nessa run` catch invalid `.wasm` / export mismatches at compile time.
    let references =
        nsbc::builtin_references(&compiled.codegen_output).map_err(|e| e.to_string())?;
    let builtins = references
        .into_iter()
        .filter_map(|id| {
            runtime::lookup_builtin_fn_meta(id).map(|m| nsbc::BuiltinImport {
                id,
                name: m.name.to_owned(),
            })
        })
        .collect();
    let empty_codegen = nsbc::CodegenOutput {
        method_call_scopes: Vec::new(),
        functions: Vec::new(),
        constants: Vec::new(),
        globals: Vec::new(),
    };
    let check_artifact = nsbc::CompiledArtifact {
        codegen_output: std::mem::replace(&mut compiled.codegen_output, empty_codegen),
        type_pool: std::mem::replace(
            &mut compiled.type_pool,
            type_pool::TypePool::with_intrinsics(),
        ),
        entry: compiled.entry_func_id,
        builtin_abi_version: runtime::BUILTIN_ABI_VERSION,
        builtins,
        wasm_modules: std::mem::take(&mut compiled.wasm_modules),
    };
    let validation_res = nsbc::validate_artifact(&check_artifact);
    compiled.codegen_output = check_artifact.codegen_output;
    compiled.type_pool = check_artifact.type_pool;
    compiled.wasm_modules = check_artifact.wasm_modules;
    validation_res.map_err(|e| e.to_string())?;

    Ok(())
}

fn key(sources: &PackageSources) -> SourceKey {
    let manifest = sources.manifest.manifest();
    (manifest.qualified_name(), manifest.version.to_string())
}

fn same_sources(left: &PackageSources, right: &PackageSources) -> Result<bool, String> {
    let left_u = source_units(left)?;
    let right_u = source_units(right)?;
    Ok(left_u.len() == right_u.len()
        && left.wasm_assets == right.wasm_assets
        && left_u
            .iter()
            .zip(right_u)
            .all(|(l, r)| l.path == r.path && l.file.source == r.file.source))
}

pub(crate) fn select_sources<'a>(
    root: &'a PackageSources,
    catalog: &'a [PackageSources],
) -> Result<(pkg_manager::ResolvedPackageGraph, Vec<&'a PackageSources>), String> {
    if catalog.len() > 65_536 {
        return Err("source catalog exceeds package count limit (65536)".into());
    }
    let mut resolver = PackageResolver::new();
    let mut registered = BTreeMap::from([(key(root), root)]);
    for source in catalog {
        resolver
            .register_document(source.manifest.clone())
            .map_err(|error| error.to_string())?;
        let key = key(source);
        if let Some(previous) = registered.get(&key) {
            if !same_sources(previous, source)? {
                return Err(format!(
                    "conflicting duplicate sources for {}@{}",
                    key.0, key.1
                ));
            }
        } else {
            registered.insert(key, source);
        }
    }
    for bundled in pkg_manager::bundled_packages() {
        let bkey = key(bundled);
        if let std::collections::btree_map::Entry::Vacant(entry) = registered.entry(bkey) {
            resolver
                .register_document(bundled.manifest.clone())
                .map_err(|error| error.to_string())?;
            entry.insert(bundled);
        }
    }
    let graph = resolver
        .resolve_document(&root.manifest)
        .map_err(|error| error.to_string())?;
    let limits = SourceLoadLimits::default();
    let mut units = 0usize;
    let mut bytes = 0usize;
    let mut selected = Vec::new();
    for manifest in graph.packages() {
        let key = (manifest.qualified_name(), manifest.version.to_string());
        let source = registered
            .get(&key)
            .copied()
            .ok_or_else(|| format!("missing selected sources for {}@{}", key.0, key.1))?;
        for unit in source_units(source)? {
            units = units
                .checked_add(1)
                .filter(|&n| n <= limits.max_modules)
                .ok_or("selected packages exceed combined module count limit")?;
            bytes = bytes
                .checked_add(unit.file.source.len())
                .filter(|&n| n <= limits.max_source_bytes)
                .ok_or("selected packages exceed combined source byte limit")?;
        }
        selected.push(source);
    }
    Ok((graph, selected))
}
