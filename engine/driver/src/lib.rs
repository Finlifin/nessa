//! Compilation driver — orchestrates the full pipeline:
//!   Source → Lexer → Parser → Resolution → NIR → Codegen → Archive / Run

mod execution;
mod identity;
pub use identity::CompilationIdentityContext;
pub use pkg_manager::ManifestDocument;
pub use pkg_manager::{ModuleSources, PackageSources, SourceFile, load_package_sources};
mod package;
mod package_graph;
mod source_units;
mod standard_library;

pub use execution::{ArtifactLoadError, install_artifact};

use ast::dump::dump_ast_to_string_pretty;
use diagnostic::{Diagnostic, DiagnosticContext, Level, NessaError};
use initialization::Engine;
use nsbc::CodegenOutput;
use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};
use std::path::Path;
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
        })
    }

    fn failed(diagnostics: Vec<Diagnostic>, type_pool: type_pool::TypePool) -> Self {
        Self {
            codegen_output: CodegenOutput {
                scope_coverage: nsbc::ScopeCoverage::Calls,
                method_call_scopes: None,
                functions: Vec::new(),
                constants: Vec::new(),
                globals: Vec::new(),
            },
            diagnostics,
            has_errors: true,
            entry_func_id: None,
            type_pool,
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
}

impl Driver {
    pub fn new() -> Self {
        Self {
            source_map: SourceMap::new(FilePathMapping::empty()),
            next_source: AtomicU64::new(0),
        }
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
        self.compile_ast_with_options(ast, diagnostics, identity, startup, None)
    }

    fn compile_ast_with_options(
        &self,
        ast: ast::Ast,
        mut diagnostics: Vec<Diagnostic>,
        identity: Option<CompilationIdentityContext>,
        startup: nir::StartupMode,
        package_options: Option<resolution::ResolveOptions>,
    ) -> CompileResult {
        let original_root = ast.root;
        let (ast, mut options) = match self.load_standard_library(ast, &mut diagnostics) {
            Ok(loaded) => loaded,
            Err(errors) => {
                diagnostics.extend(errors);
                return CompileResult::failed(diagnostics, type_pool::TypePool::with_intrinsics());
            }
        };
        if let Some(mut packages) = package_options {
            if let Some(dependencies) = packages.package_dependencies.remove(&original_root) {
                packages.package_dependencies.insert(ast.root, dependencies);
            }
            // std is provided by the engine, independently of manifest edges.
            let standard = options.package_roots.iter().copied().next();
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
        let nir_module = match nir::try_lower_with_startup(&resolved, startup) {
            Ok(module) => module,
            Err(errors) => {
                let context = DiagnosticContext::new(&self.source_map);
                for error in errors {
                    context
                        .error(error.message)
                        .with_primary_span(resolved.ast.node(error.node).span)
                        .emit(&context);
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

        // Phase 5: Code generation
        let codegen_output = match codegen::try_codegen(&nir_module) {
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
        }
    }

    /// Compile and write an `.nsbc` archive to a file.
    pub fn compile_to_archive(&self, source: &str, output_path: &Path) -> std::io::Result<()> {
        let artifact = self.compile(source).into_artifact()?;
        let bytes = nsbc_io::write_artifact(&artifact)?;
        std::fs::write(output_path, bytes)
    }

    /// Parse source and write a pretty-printed AST S-expression dump to a file.
    pub fn emit_ast_dump(&self, source: &str, output_path: &Path) -> std::io::Result<()> {
        let (ast, _) = self
            .parse_source(source)
            .map_err(|diagnostics| compilation_error(&diagnostics))?;
        let dump = dump_ast_to_string_pretty(&ast, ast.root);
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
        let mut engine = Engine::with_defaults();
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
        let result = driver.compile("");
        let bytes = nsbc_io::write_archive(&result.codegen_output).unwrap();
        let archive = nsbc_io::Archive::read_from(&mut &bytes[..]).unwrap();
        // Should have 3 sections: code, constants, stack_maps.
        assert_eq!(archive.sections.len(), 3);
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
}
