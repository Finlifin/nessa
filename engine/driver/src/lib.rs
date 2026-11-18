//! Compilation driver — orchestrates the full pipeline:
//!   Source → Lexer → Parser → Resolution → NIR → Codegen → Archive / Run

mod artifact_cache;
mod execution;
mod ide;
mod identity;
pub use ide::{CheckedSourceUnit, IdeCheckResult, standard_library_sources};
pub use identity::CompilationIdentityContext;
pub use nsbc_io::{disassemble_artifact, disassemble_bytes};
pub use pkg_manager::ManifestDocument;
pub use pkg_manager::{
    LockedPackage, Lockfile, ModuleSources, PackageLoadError, PackageSources, PackageWorkspace,
    SourceFile, load_package_sources, load_package_workspace,
};
mod package;
mod package_graph;
mod source_units;
mod standard_library;

pub use execution::{ArtifactLoadError, install_artifact};
pub use initialization::{EngineConfig, JitConfig, JitMode, JitStats};
pub use nir::OptLevel;
pub use wasm::{
    WasmCompileError, WasmCompileOptions, WasmTarget, inspect_wasm_bytes, validate_wasm_bytes,
};

use ast::dump::dump_ast_to_string_pretty;
use diagnostic::{Diagnostic, DiagnosticContext, Level, NessaError};
use initialization::Engine;
use nsbc::CodegenOutput;
use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// ---------------------------------------------------------------------------
// CompileResult
// ---------------------------------------------------------------------------

/// Result of a full compilation.
pub struct CompileResult {
    /// Empty when compilation fails; check `has_errors` before using artifacts.
    pub codegen_output: CodegenOutput,
    pub diagnostics: Vec<diagnostic::Diagnostic>,
    pub has_errors: bool,
    pub type_pool: type_pool::TypePool,
    /// Startup entry, which initializes modules before invoking user main.
    pub entry_func_id: Option<nsbc::FuncId>,
    /// Embedded WebAssembly modules bound via `[[ffi.wasm]]`.
    pub wasm_modules: Vec<nsbc::WasmModuleDef>,
}

impl CompileResult {
    /// Package successful compilation with its startup entry and native imports.
    pub fn into_artifact(self) -> std::io::Result<nsbc::CompiledArtifact> {
        if self.has_errors {
            return Err(compilation_error(&self.diagnostics));
        }
        let references = nsbc::builtin_references(&self.codegen_output)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let builtins = references
            .into_iter()
            .map(|id| {
                let metadata = runtime::lookup_builtin_fn_meta(id).ok_or_else(|| {
                    std::io::Error::other(format!("unknown compiled builtin ID {id}"))
                })?;
                Ok(nsbc::BuiltinImport {
                    id,
                    name: metadata.name.to_owned(),
                })
            })
            .collect::<std::io::Result<Vec<_>>>()?;
        Ok(nsbc::CompiledArtifact {
            codegen_output: self.codegen_output,
            type_pool: self.type_pool,
            entry: self.entry_func_id,
            builtin_abi_version: runtime::BUILTIN_ABI_VERSION,
            builtins,
            wasm_modules: self.wasm_modules,
        })
    }

    fn failed(diagnostics: Vec<Diagnostic>, type_pool: type_pool::TypePool) -> Self {
        Self {
            codegen_output: CodegenOutput {
                method_call_scopes: Vec::new(),
                functions: Vec::new(),
                constants: Vec::new(),
                globals: Vec::new(),
            },
            diagnostics,
            has_errors: true,
            entry_func_id: None,
            type_pool,
            wasm_modules: Vec::new(),
        }
    }
}

fn compilation_error(diagnostics: &[Diagnostic]) -> std::io::Error {
    let messages = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.level == Level::Error)
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("compilation failed: {messages}"),
    )
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// The main compilation driver.
pub struct Driver {
    source_map: SourceMap,
    next_source: AtomicU64,
    script_args: Vec<String>,
    cache_override: Option<Option<PathBuf>>,
    jit_config: Option<JitConfig>,
    opt_level: Option<OptLevel>,
}

impl Driver {
    pub fn new() -> Self {
        Self {
            source_map: SourceMap::new(FilePathMapping::empty()),
            next_source: AtomicU64::new(0),
            script_args: Vec::new(),
            cache_override: None,
            jit_config: None,
            opt_level: None,
        }
    }

    /// Override the AOT optimization level (`O0`, `O1`, `O2`, `O3`) for compilations on this driver.
    pub fn with_opt_level(mut self, level: OptLevel) -> Self {
        self.opt_level = Some(level);
        self
    }

    /// Set or clear the explicit [`OptLevel`] override (`None` falls back to `package.toml` / `NESSA_OPT_LEVEL` / `O0`).
    pub fn set_opt_level(&mut self, level: Option<OptLevel>) {
        self.opt_level = level;
    }

    /// Return the explicit [`OptLevel`] configured on this driver, if any.
    pub fn opt_level(&self) -> Option<OptLevel> {
        self.opt_level
    }

    /// Resolve effective optimization level: explicit `Driver` override > optional manifest `[build] opt_level` > `NESSA_OPT_LEVEL` > `O0`.
    pub(crate) fn effective_opt_level(&self, manifest_opt_level: Option<u8>) -> OptLevel {
        self.opt_level
            .or_else(|| manifest_opt_level.and_then(OptLevel::from_u8))
            .unwrap_or_else(OptLevel::from_env)
    }

    /// Override the JIT mode used when executing artifacts on this driver.
    pub fn with_jit_mode(mut self, mode: JitMode) -> Self {
        self.jit_config = Some(JitConfig::with_mode(mode));
        self
    }

    /// Override the full [`JitConfig`] (mode + code-cache budget) used on this driver.
    pub fn with_jit_config(mut self, config: JitConfig) -> Self {
        self.jit_config = Some(config);
        self
    }

    /// Set or clear the explicit [`JitMode`] override (`None` falls back to `NESSA_JIT` / `Auto`).
    pub fn set_jit_mode(&mut self, mode: Option<JitMode>) {
        self.jit_config = mode.map(JitConfig::with_mode);
    }

    /// Return the explicit [`JitMode`] configured on this driver, if any.
    pub fn jit_mode(&self) -> Option<JitMode> {
        self.jit_config.map(|cfg| cfg.mode)
    }

    /// Override the content-addressed `.nsbc` compilation cache directory for this driver.
    pub fn with_cache_dir(mut self, dir: PathBuf) -> Self {
        self.cache_override = Some(Some(dir));
        self
    }

    /// Disable content-addressed `.nsbc` compilation caching on this driver.
    pub fn without_cache(mut self) -> Self {
        self.cache_override = Some(None);
        self
    }

    pub(crate) fn resolved_cache_dir(&self) -> Option<PathBuf> {
        artifact_cache::active_cache_dir(self.cache_override.as_ref())
    }

    /// Configure command-line arguments visible to the executed Nessa program.
    ///
    /// When invoked from the `nessa` CLI (`nessa run <target> [-- <args...>]`),
    /// index `0` is the target path display string followed by trailing arguments.
    /// Defaults to an empty `Vec` in [`Driver::new`].
    pub fn with_script_args(mut self, args: Vec<String>) -> Self {
        self.script_args = args;
        self
    }

    /// Replace the command-line arguments visible to subsequent runs on this driver.
    pub fn set_script_args(&mut self, args: Vec<String>) {
        self.script_args = args;
    }

    /// Return the configured command-line arguments for script execution.
    pub fn script_args(&self) -> &[String] {
        &self.script_args
    }

    /// Parse only valid token streams, retaining lexical and syntax diagnostics.
    fn parse_source(&self, source: &str) -> Result<(ast::Ast, Vec<Diagnostic>), Vec<Diagnostic>> {
        self.parse_source_named(source, "input")
    }

    fn parse_source_named(
        &self,
        source: &str,
        name: &str,
    ) -> Result<(ast::Ast, Vec<Diagnostic>), Vec<Diagnostic>> {
        // SourceMap caches by filename. Distinct submissions need distinct names,
        // otherwise a reused driver would retain the first submission's contents.
        let source_id = self.next_source.fetch_add(1, Ordering::Relaxed);
        let sf = self.source_map.new_source_file(
            FileName::Custom(format!("{name}-{source_id}")),
            source.to_string(),
        );
        let diag_ctx = DiagnosticContext::new(&self.source_map);

        // Phase 1: Lex
        let (tokens, lex_errors) = lexer::tokenize(source);
        for error in &lex_errors {
            error.emit(&diag_ctx, sf.start_pos);
        }
        if diag_ctx.has_errors() {
            return Err(diag_ctx.diagnostics().to_vec());
        }

        // Phase 2: Parse
        let parser = parser::Parser::new(&tokens, source, &diag_ctx, sf.start_pos);
        let ast = parser.parse();
        if diag_ctx.has_errors() {
            return Err(diag_ctx.diagnostics().to_vec());
        }
        Ok((ast, diag_ctx.diagnostics().to_vec()))
    }

    /// Compile source through each stage, stopping before invalid artifacts are produced.
    pub fn compile(&self, source: &str) -> CompileResult {
        self.compile_with_context(source, None)
    }

    /// Compile with real package provenance and checked type-version overrides.
    pub fn compile_with_identity(
        &self,
        source: &str,
        context: CompilationIdentityContext,
    ) -> CompileResult {
        self.compile_with_context(source, Some(context))
    }

    fn compile_with_context(
        &self,
        source: &str,
        identity: Option<CompilationIdentityContext>,
    ) -> CompileResult {
        let (ast, diagnostics) = match self.parse_source(source) {
            Ok(parsed) => parsed,
            Err(diagnostics) => {
                return CompileResult::failed(diagnostics, type_pool::TypePool::with_intrinsics());
            }
        };
        self.compile_ast(ast, diagnostics, identity, nir::StartupMode::Main)
    }

    fn compile_ast(
        &self,
        ast: ast::Ast,
        diagnostics: Vec<Diagnostic>,
        identity: Option<CompilationIdentityContext>,
        startup: nir::StartupMode,
    ) -> CompileResult {
        self.compile_ast_with_options(
            ast,
            diagnostics,
            identity,
            startup,
            None,
            self.effective_opt_level(None),
        )
    }

    fn compile_ast_with_options(
        &self,
        ast: ast::Ast,
        mut diagnostics: Vec<Diagnostic>,
        identity: Option<CompilationIdentityContext>,
        startup: nir::StartupMode,
        package_options: Option<resolution::ResolveOptions>,
        opt_level: OptLevel,
    ) -> CompileResult {
        let original_root = ast.root;
        let needs_bundled_script_packages = package_options.is_none() && {
            let bundled_names: std::collections::HashSet<str_interner::StrId> =
                pkg_manager::bundled_packages()
                    .iter()
                    .map(|pkg| str_interner::intern(&pkg.manifest.manifest().name))
                    .collect();
            ast.nodes
                .iter()
                .any(|node| node.kind == ast::NodeKind::Id && bundled_names.contains(&node.str_id))
        };
        let (mut ast, mut options) = match self.load_standard_library(ast, &mut diagnostics) {
            Ok(loaded) => loaded,
            Err(errors) => {
                diagnostics.extend(errors);
                return CompileResult::failed(diagnostics, type_pool::TypePool::with_intrinsics());
            }
        };
        let standard = options.package_roots.iter().copied().next();
        if needs_bundled_script_packages
            && let Err(errors) = self.attach_bundled_script_packages(
                &mut ast,
                &mut options,
                standard,
                &mut diagnostics,
            )
        {
            diagnostics.extend(errors);
            return CompileResult::failed(diagnostics, type_pool::TypePool::with_intrinsics());
        }
        if let Some(mut packages) = package_options {
            if let Some(dependencies) = packages.package_dependencies.remove(&original_root) {
                packages.package_dependencies.insert(ast.root, dependencies);
            }
            // std is provided by the engine, independently of manifest edges.
            if let Some(standard) = standard {
                for dependencies in packages.package_dependencies.values_mut() {
                    dependencies
                        .entry(str_interner::intern("std"))
                        .or_default()
                        .push(standard);
                }
            }
            options.package_roots.extend(packages.package_roots);
            options.detached_package_roots = packages.detached_package_roots;
            options.package_dependencies = packages.package_dependencies;
            options
                .package_identities
                .extend(packages.package_identities);
            options.implicit_imports.extend(packages.implicit_imports);
            options.privileged_nodes.extend(packages.privileged_nodes);
            options.ffi_nodes.extend(packages.ffi_nodes);
            options
                .ffi_library_allowlists
                .extend(packages.ffi_library_allowlists);
        }
        if let Some(identity) = identity {
            options
                .package_identities
                .insert(ast.root, identity.into_source());
        }
        let diag_ctx = DiagnosticContext::new(&self.source_map);

        // Phase 3: Resolution
        let mut resolved = resolution::resolve_with_options(ast, &diag_ctx, options);
        diagnostics.extend(diag_ctx.diagnostics().iter().cloned());
        diagnostics.extend(resolved.diagnostics.iter().cloned());
        let has_errors = diagnostics
            .iter()
            .any(|diagnostic| diagnostic.level == Level::Error);
        if has_errors {
            return CompileResult::failed(diagnostics, resolved.type_pool);
        }

        // Phase 4: NIR lowering
        let mut nir_module = match nir::try_lower_with_startup(&resolved, startup) {
            Ok(module) => module,
            Err(errors) => {
                let context = DiagnosticContext::new(&self.source_map);
                for error in errors {
                    let mut builder = context
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
                    builder.emit(&context);
                }
                diagnostics.extend(context.diagnostics().iter().cloned());
                return CompileResult::failed(diagnostics, resolved.type_pool);
            }
        };

        // Resolution records source symbols in method tables. Publish only
        // generated function identities, consistently across traits and vtables.
        if let Err(symbol) = resolved.type_pool.remap_function_ids(|symbol| {
            nir_module
                .function_symbols
                .get(&resolution::SymbolId(symbol))
                .map(|function| function.0)
        }) {
            let context = DiagnosticContext::new(&self.source_map);
            let node = resolved
                .symbols
                .get(symbol as usize)
                .map(|symbol| symbol.def_node)
                .unwrap_or(resolved.ast.root);
            context
                .error(format!(
                    "method symbol {symbol} has no generated function body"
                ))
                .with_primary_span(resolved.ast.node(node).span)
                .emit(&context);
            diagnostics.extend(context.diagnostics().iter().cloned());
            return CompileResult::failed(diagnostics, resolved.type_pool);
        }

        if let Err(error) =
            resolved
                .type_pool
                .resolve_derived_methods(nir_module.derived_methods.iter().map(|method| {
                    (
                        method.implementor,
                        method.trait_type,
                        method.method_name,
                        method.func_id.0,
                    )
                }))
        {
            let context = DiagnosticContext::new(&self.source_map);
            context
                .error(format!("cannot publish generated derived method: {error}"))
                .with_primary_span(resolved.ast.node(resolved.ast.root).span)
                .emit(&context);
            diagnostics.extend(context.diagnostics().iter().cloned());
            return CompileResult::failed(diagnostics, resolved.type_pool);
        }

        if let Err(error) = resolved.type_pool.validate_type_identities() {
            let context = DiagnosticContext::new(&self.source_map);
            context
                .error(format!("invalid finalized source type identities: {error}"))
                .with_primary_span(resolved.ast.node(resolved.ast.root).span)
                .emit(&context);
            diagnostics.extend(context.diagnostics().iter().cloned());
            return CompileResult::failed(diagnostics, resolved.type_pool);
        }

        // Phase 4b: Mid-end NIR optimization passes (`O1` / `O2` / `O3`)
        nir::optimize_module(&mut nir_module, &mut resolved.type_pool, opt_level);

        // Phase 5: Code generation (with constant deduplication & bytecode peepholes at `O1+`)
        let codegen_output = match codegen::try_codegen_with_opt(&nir_module, opt_level) {
            Ok(output) => output,
            Err(errors) => {
                let encoding_diagnostics = DiagnosticContext::new(&self.source_map);
                for error in errors {
                    let name = nir_module
                        .functions
                        .iter()
                        .find(|function| function.func_id == error.function)
                        .map(|function| function.name);
                    let node = resolved
                        .symbols
                        .iter()
                        .find(|symbol| Some(symbol.name) == name)
                        .map(|symbol| symbol.def_node)
                        .unwrap_or(resolved.ast.root);
                    encoding_diagnostics
                        .error(error.message)
                        .with_code(diagnostic::codes::CODEGEN_LIMIT_OR_ENCODING)
                        .with_primary_span(resolved.ast.node(node).span)
                        .emit(&encoding_diagnostics);
                }
                diagnostics.extend(encoding_diagnostics.diagnostics().iter().cloned());
                return CompileResult::failed(diagnostics, resolved.type_pool);
            }
        };

        CompileResult {
            codegen_output,
            type_pool: resolved.type_pool,
            entry_func_id: nir_module.entry,
            diagnostics,
            has_errors,
            wasm_modules: Vec::new(),
        }
    }

    /// Compile source or load a validated content-addressed `.nsbc` artifact from cache.
    pub fn compile_cached_artifact(
        &self,
        source: &str,
    ) -> Result<nsbc::CompiledArtifact, Vec<Diagnostic>> {
        let cache_dir = self.resolved_cache_dir();
        let opt_level = self.effective_opt_level(None);
        let key = artifact_cache::single_source_key(source, None, opt_level);
        if let Some(cached) = artifact_cache::load_cached_artifact(cache_dir.as_deref(), &key) {
            return Ok(cached);
        }
        let compiled = self.compile(source);
        if compiled.has_errors {
            return Err(compiled.diagnostics);
        }
        let artifact = compiled
            .into_artifact()
            .map_err(|error| self.package_errors(error.to_string()))?;
        artifact_cache::store_cached_artifact(cache_dir.as_deref(), &key, &artifact);
        Ok(artifact)
    }

    /// Compile source and write an `.nsbc` archive to a file, reusing/populating the `.nsbc` cache.
    pub fn compile_to_archive(&self, source: &str, output_path: &Path) -> std::io::Result<()> {
        let artifact = self
            .compile_cached_artifact(source)
            .map_err(|diagnostics| compilation_error(&diagnostics))?;
        let bytes = nsbc_io::write_artifact(&artifact)?;
        std::fs::write(output_path, bytes)
    }

    /// Compile Nessa source directly to standalone `.wasm` binary bytes (`wasm32-wasi` or `wasm32-freestanding`).
    pub fn compile_to_wasm_bytes(
        &self,
        source: &str,
        options: WasmCompileOptions,
    ) -> std::io::Result<Vec<u8>> {
        let artifact = self
            .compile_cached_artifact(source)
            .map_err(|diagnostics| compilation_error(&diagnostics))?;
        wasm::compile_artifact_to_wasm(&artifact, &options).map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("wasm compilation failed: {err}"),
            )
        })
    }

    /// Compile Nessa source to a standalone `.wasm` binary file.
    pub fn compile_to_wasm(
        &self,
        source: &str,
        output_path: &Path,
        options: WasmCompileOptions,
    ) -> std::io::Result<()> {
        let bytes = self.compile_to_wasm_bytes(source, options)?;
        std::fs::write(output_path, bytes)
    }

    /// Read a `.wasm` binary file with a bounded allocation and render its structural summary.
    pub fn disassemble_wasm_file(&self, path: &Path) -> std::io::Result<String> {
        use std::io::Read;
        const MAX_WASM_FILE_BYTES: u64 = 64 * 1024 * 1024;
        let file = std::fs::File::open(path)?;
        let mut bytes = Vec::new();
        file.take(MAX_WASM_FILE_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_WASM_FILE_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "wasm file exceeds 64 MiB limit",
            ));
        }
        wasm::inspect_wasm_bytes(&bytes)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err.to_string()))
    }

    /// Execute a standalone `.wasm` WASI command binary with live stdio forwarding.
    pub fn run_wasm_file(&self, path: &Path) -> Result<i32, String> {
        use std::io::Read;
        const MAX_WASM_FILE_BYTES: u64 = 64 * 1024 * 1024;
        let file = std::fs::File::open(path)
            .map_err(|err| format!("cannot read wasm file {}: {err}", path.display()))?;
        let mut bytes = Vec::new();
        file.take(MAX_WASM_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|err| format!("cannot read wasm file {}: {err}", path.display()))?;
        if bytes.len() as u64 > MAX_WASM_FILE_BYTES {
            return Err("wasm file exceeds 64 MiB limit".into());
        }
        let cfg = wasm::WasiConfig {
            args: self.script_args.clone(),
            env: std::env::vars().collect(),
            stdin: Vec::new(),
            inherit_stdio: true,
            limits: wasm::WasmConfig::default(),
        };
        wasm::run_wasi_bytes(&bytes, cfg)
            .map(|out| out.exit_code)
            .map_err(|err| err.to_string())
    }

    /// Parse source and write a pretty-printed AST S-expression dump to a file.
    pub fn emit_ast_dump(&self, source: &str, output_path: &Path) -> std::io::Result<()> {
        let (ast, _) = self
            .parse_source(source)
            .map_err(|diagnostics| compilation_error(&diagnostics))?;
        let dump = dump_ast_to_string_pretty(&ast, ast.root);
        std::fs::write(output_path, dump)
    }

    /// Compile source and render its human-readable NSBC disassembly.
    pub fn disassemble_source(&self, source: &str) -> std::io::Result<String> {
        let compiled = self.compile(source);
        if compiled.has_errors {
            return Err(compilation_error(&compiled.diagnostics));
        }
        let artifact = compiled.into_artifact()?;
        Ok(nsbc_io::disassemble_artifact(&artifact))
    }

    /// Read an `.nsbc` archive file with a bounded allocation and render its disassembly.
    pub fn disassemble_archive_file(&self, path: &Path) -> std::io::Result<String> {
        use std::io::Read;
        let file = std::fs::File::open(path)?;
        let mut bytes = Vec::new();
        file.take(nsbc_io::MAX_ARCHIVE_SIZE as u64 + 1)
            .read_to_end(&mut bytes)?;
        nsbc_io::disassemble_bytes(&bytes)
    }

    /// Compile source and write its human-readable NSBC disassembly to a file.
    pub fn emit_bc_dump(&self, source: &str, output_path: &Path) -> std::io::Result<()> {
        let dump = self.disassemble_source(source)?;
        std::fs::write(output_path, dump)
    }

    /// Read an `.nsbc` archive file and write its human-readable NSBC disassembly to a file.
    pub fn emit_archive_bc_dump(
        &self,
        archive_path: &Path,
        output_path: &Path,
    ) -> std::io::Result<()> {
        let dump = self.disassemble_archive_file(archive_path)?;
        std::fs::write(output_path, dump)
    }

    /// Compile source and run it immediately.
    pub fn run(&self, source: &str) -> RunResult {
        let result = self.compile(source);
        if result.has_errors {
            return RunResult::CompileError(result.diagnostics);
        }

        match result.into_artifact() {
            Ok(artifact) => self.run_artifact(artifact),
            Err(error) => RunResult::RuntimeError(error.to_string()),
        }
    }

    /// Compile source (reusing the content-addressed `.nsbc` cache) and run it immediately.
    pub fn run_cached(&self, source: &str) -> RunResult {
        match self.compile_cached_artifact(source) {
            Ok(artifact) => self.run_artifact(artifact),
            Err(diagnostics) => RunResult::CompileError(diagnostics),
        }
    }

    /// Load and execute a self-contained archive without reading source files.
    pub fn run_archive(&self, bytes: &[u8]) -> RunResult {
        match nsbc_io::read_artifact(bytes) {
            Ok(artifact) => self.run_artifact(artifact),
            Err(error) => RunResult::RuntimeError(format!("archive load failed: {error}")),
        }
    }

    /// Read an archive with a bounded allocation, then load and execute it.
    pub fn run_archive_file(&self, path: &Path) -> RunResult {
        use std::io::Read;
        let bytes = (|| -> std::io::Result<Vec<u8>> {
            let file = std::fs::File::open(path)?;
            let mut bytes = Vec::new();
            file.take(nsbc_io::MAX_ARCHIVE_SIZE as u64 + 1)
                .read_to_end(&mut bytes)?;
            Ok(bytes)
        })();
        match bytes {
            Ok(bytes) => self.run_archive(&bytes),
            Err(error) => {
                RunResult::RuntimeError(format!("cannot read archive {}: {error}", path.display()))
            }
        }
    }

    /// Execute an artifact through the same installation path as source runs.
    pub fn run_artifact(&self, artifact: nsbc::CompiledArtifact) -> RunResult {
        let mut engine_cfg = EngineConfig::default();
        if let Some(jit_cfg) = self.jit_config {
            engine_cfg.jit = jit_cfg;
        }
        let mut engine = Engine::new(engine_cfg);
        engine.vm_mut().set_script_args(self.script_args.clone());
        let entry = match install_artifact(engine.vm_mut(), artifact) {
            Ok(entry) => entry,
            Err(error) => return RunResult::RuntimeError(error.to_string()),
        };
        if let Some(entry) = entry {
            engine.vm_mut().spawn_root(entry);
            if let interpreter::VmResult::Error(error) = engine.vm_mut().run() {
                return RunResult::RuntimeError(format!("{error:?}"));
            }
        }
        RunResult::Ok
    }
}

impl Default for Driver {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of running compiled code.
#[derive(Debug)]
pub enum RunResult {
    Ok,
    CompileError(Vec<diagnostic::Diagnostic>),
    RuntimeError(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn driver_compiles_empty() {
        let driver = Driver::new();
        let result = driver.compile("");
        assert!(!result.has_errors);
        assert!(result.diagnostics.is_empty());
    }

    #[test]
    fn native_import_mismatches_are_rejected_before_entry_execution() {
        let driver = Driver::new();
        let source = "fn main() { panic(\"entry must not run\"); }";
        for change_revision in [false, true] {
            let mut artifact = driver.compile(source).into_artifact().unwrap();
            if change_revision {
                artifact.builtin_abi_version += 1;
            } else {
                artifact.builtins[0].name = "different_builtin".into();
            }
            let result = driver.run_artifact(artifact);
            assert!(
                matches!(result, RunResult::RuntimeError(ref message) if message.contains("incompatible builtin ABI") && !message.contains("entry must not run")),
                "{result:?}"
            );
        }
    }

    #[test]
    fn driver_roundtrip_archive() {
        let driver = Driver::new();
        let artifact = driver.compile("").into_artifact().unwrap();
        let bytes = nsbc_io::write_artifact(&artifact).unwrap();
        let archive = nsbc_io::Archive::read_from(&mut &bytes[..]).unwrap();
        // Should have 4 sections: code, constants, stack_maps, metadata.
        assert_eq!(archive.sections.len(), 4);
        let restored = nsbc_io::read_artifact(&bytes).unwrap();
        assert_eq!(restored.builtin_abi_version, runtime::BUILTIN_ABI_VERSION);
    }

    #[test]
    fn rejects_lexical_syntax_and_resolution_errors_before_codegen() {
        let cases = [
            ("\"unterminated", "string", Some(1001)),
            ("fn main() {", "", None),
            (
                "fn main() { missing_name }",
                "undefined name `missing_name`",
                None,
            ),
            ("fn main() { let x: bool = 42 }", "type mismatch", None),
        ];
        let driver = Driver::new();
        for (source, expected_message, code) in cases {
            let result = driver.compile(source);
            assert!(result.has_errors, "accepted invalid source: {source}");
            assert!(
                result.codegen_output.functions.is_empty(),
                "emitted code for {source}"
            );
            assert!(result.codegen_output.constants.is_empty());
            assert!(
                result.diagnostics.iter().any(|diagnostic| {
                    diagnostic.level == Level::Error
                        && diagnostic.primary_span.is_some()
                        && diagnostic.message.to_lowercase().contains(expected_message)
                        && (code.is_none() || diagnostic.code == code)
                }),
                "missing diagnostic for {source}: {:?}",
                result.diagnostics
            );
            assert!(
                matches!(driver.run(source), RunResult::CompileError(diagnostics) if !diagnostics.is_empty())
            );
        }
    }

    #[test]
    fn diagnostic_spans_follow_each_source_file_in_a_reused_driver() {
        let driver = Driver::new();
        let first = driver.compile("fn main() { first_missing }");
        let second = driver.compile("fn main() { second_missing }");
        let first_span = first
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.level == Level::Error)
            .unwrap()
            .primary_span
            .unwrap();
        let second_span = second
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.level == Level::Error)
            .unwrap()
            .primary_span
            .unwrap();
        assert!(second_span.lo() > first_span.hi());
        assert_eq!(
            driver.source_map.span_to_snippet(second_span).unwrap(),
            "second_missing"
        );
        assert_eq!(
            driver.source_map.span_to_snippet(first_span).unwrap(),
            "first_missing"
        );
    }

    struct TestDirectory(std::path::PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            loop {
                let path = std::env::temp_dir().join(format!(
                    "nessa-driver-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed),
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("cannot create test directory: {error}"),
                }
            }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn failed_compilation_preserves_existing_artifacts_and_creates_no_new_files() {
        let directory = TestDirectory::new();
        let driver = Driver::new();
        for (source, extension, ast_dump) in [
            ("fn main() { missing_name }", "nsbc", false),
            ("fn main() {", "lisp", true),
        ] {
            let existing = directory.0.join(format!("existing.{extension}"));
            let missing = directory.0.join(format!("missing.{extension}"));
            std::fs::write(&existing, b"previous successful artifact").unwrap();
            for destination in [&existing, &missing] {
                let error = if ast_dump {
                    driver.emit_ast_dump(source, destination)
                } else {
                    driver.compile_to_archive(source, destination)
                }
                .unwrap_err();
                assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
                assert!(error.to_string().contains("compilation failed"));
            }
            assert_eq!(
                std::fs::read(existing).unwrap(),
                b"previous successful artifact"
            );
            assert!(!missing.exists());
        }
    }

    #[test]
    fn driver_script_args_hooks_default_empty_and_preserve_arguments() {
        let default_driver = Driver::new();
        assert!(default_driver.script_args().is_empty());

        let mut configured = Driver::new().with_script_args(vec![
            "script.ns".to_owned(),
            "--some-flag".to_owned(),
            "value".to_owned(),
        ]);
        assert_eq!(
            configured.script_args(),
            &["script.ns", "--some-flag", "value"]
        );

        configured.set_script_args(vec!["pkg_dir".to_owned(), "pos1".to_owned()]);
        assert_eq!(configured.script_args(), &["pkg_dir", "pos1"]);
    }

    #[test]
    fn driver_disassembles_compiled_source_and_saved_nsbc_archives() {
        let directory = TestDirectory::new();
        let driver = Driver::new();
        let source = r#"
fn add_bias(x: i64) -> i64 {
    let f = |n: i64| n + 7
    f(x)
}

fn main() {
    let total = add_bias(35)
    if total == 42 {
        println("ok")
    }
}
"#;
        let source_dump = driver.disassemble_source(source).unwrap();
        assert!(
            source_dump.contains("; NSBC Disassembly\n"),
            "{source_dump}"
        );
        assert!(
            source_dump.contains("builtin_abi_version:"),
            "{source_dump}"
        );
        assert!(source_dump.contains("add_bias"), "{source_dump}");
        assert!(source_dump.contains("NewClosure"), "{source_dump}");
        assert!(source_dump.contains("CallIndirect"), "{source_dump}");
        assert!(source_dump.contains("Str(\"ok\")"), "{source_dump}");

        let archive_path = directory.0.join("sample.nsbc");
        driver.compile_to_archive(source, &archive_path).unwrap();
        let archive_dump = driver.disassemble_archive_file(&archive_path).unwrap();
        assert!(archive_dump.contains("entry: fn #"), "{archive_dump}");
        assert!(archive_dump.contains("add_bias"), "{archive_dump}");

        let bad_archive = directory.0.join("corrupt.nsbc");
        std::fs::write(&bad_archive, b"corrupt-bytes").unwrap();
        assert!(driver.disassemble_archive_file(&bad_archive).is_err());
    }

    #[test]
    fn driver_compiles_multi_package_local_workspace_and_verifies_lockfile() {
        let directory = TestDirectory::new();
        let math_dir = directory.0.join("math");
        std::fs::create_dir_all(math_dir.join("src")).unwrap();
        std::fs::write(
            math_dir.join("package.toml"),
            "[package]\nname = \"math\"\ndomain = \"org.test\"\nversion = \"0.2.0\"\ntype = \"lib\"\n",
        )
        .unwrap();
        std::fs::write(
            math_dir.join("src/lib.ns"),
            "pub fn double(x: i64) -> i64 { x * 2 }\n",
        )
        .unwrap();

        let app_dir = directory.0.join("app");
        std::fs::create_dir_all(app_dir.join("src")).unwrap();
        std::fs::write(
            app_dir.join("package.toml"),
            "[package]\nname = \"app\"\ndomain = \"org.test\"\nversion = \"0.1.0\"\ntype = \"exe\"\n\n[dependencies]\nmath = { path = \"../math\", version = \"^0.2.0\" }\n",
        )
        .unwrap();
        std::fs::write(
            app_dir.join("src/main.ns"),
            "use @math.double\nfn main() { let _ = double(21) }\n",
        )
        .unwrap();

        let driver = Driver::new();
        let compiled = driver.compile_package(&app_dir);
        assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
        assert!(!app_dir.join("package.lock").exists());

        let lock = driver.sync_package_lockfile(&app_dir).unwrap();
        assert_eq!(lock.packages.len(), 1);
        assert_eq!(lock.packages[0].name, "math");
        assert_eq!(lock.packages[0].path, "../math");
        driver.verify_package_lockfile(&app_dir).unwrap();
    }

    #[test]
    fn cached_single_source_hits_invalidates_recovers_from_corruption_and_honors_disable() {
        let directory = TestDirectory::new();
        let cache_dir = directory.0.join("nsbc-cache");
        let marker = directory.0.join("out.txt");
        let source_v1 = format!(
            "use std.fs\nfn main() {{ let _ = fs.write_string({:?}, \"v1\") }}\n",
            marker.to_string_lossy()
        );
        let source_v2 = format!(
            "use std.fs\nfn main() {{ let _ = fs.write_string({:?}, \"v2\") }}\n",
            marker.to_string_lossy()
        );

        let no_cache_driver = Driver::new().without_cache();
        assert!(matches!(
            no_cache_driver.run_cached(&source_v1),
            RunResult::Ok
        ));
        assert!(!cache_dir.exists());
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "v1");

        let cached_driver = Driver::new().with_cache_dir(cache_dir.clone());
        assert!(matches!(
            cached_driver.run_cached(&source_v1),
            RunResult::Ok
        ));
        let entries_v1: Vec<_> = std::fs::read_dir(&cache_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(entries_v1.len(), 1);

        // Corrupt the cached .nsbc file: driver must evict it, recompile cleanly, and succeed.
        std::fs::write(&entries_v1[0], b"corrupted-nsbc-cache-entry").unwrap();
        std::fs::write(&marker, "stale").unwrap();
        assert!(matches!(
            cached_driver.run_cached(&source_v1),
            RunResult::Ok
        ));
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "v1");
        let valid_bytes = std::fs::read(&entries_v1[0]).unwrap();
        assert!(nsbc_io::read_artifact(&valid_bytes).is_ok());

        // Editing source must produce a distinct cache entry and run the updated program.
        assert!(matches!(
            cached_driver.run_cached(&source_v2),
            RunResult::Ok
        ));
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "v2");
        let count_after_v2 = std::fs::read_dir(&cache_dir).unwrap().count();
        assert_eq!(count_after_v2, 2);
    }

    #[test]
    fn cached_package_compilation_tracks_dependency_edits_and_preserves_locked_checks() {
        let directory = TestDirectory::new();
        let cache_dir = directory.0.join("pkg-cache");
        let marker = directory.0.join("pkg-out.txt");

        let dep_dir = directory.0.join("util");
        std::fs::create_dir_all(dep_dir.join("src")).unwrap();
        std::fs::write(
            dep_dir.join("package.toml"),
            "[package]\nname = \"util\"\ndomain = \"org.test\"\nversion = \"0.1.0\"\ntype = \"lib\"\n",
        )
        .unwrap();
        std::fs::write(
            dep_dir.join("src/lib.ns"),
            "pub fn greeting() -> String { \"hello-v1\" }\n",
        )
        .unwrap();

        let app_dir = directory.0.join("app");
        std::fs::create_dir_all(app_dir.join("src")).unwrap();
        std::fs::write(
            app_dir.join("package.toml"),
            "[package]\nname = \"app\"\ndomain = \"org.test\"\nversion = \"0.1.0\"\ntype = \"exe\"\n\n[dependencies]\nutil = { path = \"../util\" }\n",
        )
        .unwrap();
        std::fs::write(
            app_dir.join("src/main.ns"),
            format!(
                "use std.fs\nuse @util.greeting\nfn main() {{ let _ = fs.write_string({:?}, greeting()) }}\n",
                marker.to_string_lossy()
            ),
        )
        .unwrap();

        let driver = Driver::new().with_cache_dir(cache_dir.clone());
        assert!(matches!(driver.run_package_cached(&app_dir), RunResult::Ok));
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "hello-v1");
        assert_eq!(std::fs::read_dir(&cache_dir).unwrap().count(), 1);

        // Even with a warm bytecode cache, `--locked` must reject missing `package.lock`.
        assert!(matches!(
            driver.run_package_locked_cached(&app_dir),
            RunResult::CompileError(_)
        ));

        // Sync lockfile, then `--locked` succeeds using the warm cache entry.
        driver.sync_package_lockfile(&app_dir).unwrap();
        std::fs::write(&marker, "cleared").unwrap();
        assert!(matches!(
            driver.run_package_locked_cached(&app_dir),
            RunResult::Ok
        ));
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "hello-v1");
        assert_eq!(std::fs::read_dir(&cache_dir).unwrap().count(), 1);

        // Editing dependency invalidates both `--locked` and the bytecode cache key.
        std::fs::write(
            dep_dir.join("src/lib.ns"),
            "pub fn greeting() -> String { \"hello-v2\" }\n",
        )
        .unwrap();
        assert!(matches!(
            driver.run_package_locked_cached(&app_dir),
            RunResult::CompileError(_)
        ));
        driver.sync_package_lockfile(&app_dir).unwrap();
        assert!(matches!(
            driver.run_package_locked_cached(&app_dir),
            RunResult::Ok
        ));
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "hello-v2");
        assert_eq!(std::fs::read_dir(&cache_dir).unwrap().count(), 2);
    }
}
