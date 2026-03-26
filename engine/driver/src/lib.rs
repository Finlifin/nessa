//! Compilation driver — orchestrates the full pipeline:
//!   Source → Lexer → Parser → Resolution → NIR → Codegen → Archive / Run

use ast::dump::dump_ast_to_string_pretty;
use diagnostic::DiagnosticContext;
use initialization::Engine;
use nsbc::CodegenOutput;
use runtime::FunctionCode;
use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};
use std::path::Path;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// CompileResult
// ---------------------------------------------------------------------------

/// Result of a full compilation.
pub struct CompileResult {
    pub codegen_output: CodegenOutput,
    pub diagnostics: Vec<diagnostic::Diagnostic>,
    pub has_errors: bool,
}

// ---------------------------------------------------------------------------
// Driver
// ---------------------------------------------------------------------------

/// The main compilation driver.
pub struct Driver {
    source_map: Arc<SourceMap>,
}

impl Driver {
    pub fn new() -> Self {
        Self {
            source_map: Arc::new(SourceMap::new(FilePathMapping::empty())),
        }
    }

    /// Compile source code through the full pipeline.
    pub fn compile(&self, source: &str) -> CompileResult {
        let sf = self
            .source_map
            .new_source_file(FileName::Custom("input".into()), source.to_string());
        let diag_ctx = DiagnosticContext::new(&self.source_map);

        // Phase 1: Lex
        let (tokens, _lex_errors) = lexer::tokenize(source);

        // Phase 2: Parse
        let parser = parser::Parser::new(&tokens, source, &diag_ctx, sf.start_pos);
        let ast = parser.parse();

        // Phase 3: Resolution
        let resolved = resolution::resolve(ast, &diag_ctx);

        // Phase 4: NIR lowering
        let nir_module = nir::lower(&resolved);

        // Phase 5: Code generation
        let codegen_output = codegen::codegen(&nir_module);

        let has_errors = diag_ctx.has_errors()
            || resolved
                .diagnostics
                .iter()
                .any(|d| d.level == diagnostic::Level::Error);

        CompileResult {
            codegen_output,
            diagnostics: resolved.diagnostics,
            has_errors,
        }
    }

    /// Compile and write an `.nsbc` archive to a file.
    pub fn compile_to_archive(&self, source: &str, output_path: &Path) -> std::io::Result<()> {
        let result = self.compile(source);
        let bytes = nsbc_io::write_archive(&result.codegen_output)?;
        std::fs::write(output_path, bytes)
    }

    /// Parse source and write a pretty-printed AST S-expression dump to a file.
    pub fn emit_ast_dump(&self, source: &str, output_path: &Path) -> std::io::Result<()> {
        let sf = self
            .source_map
            .new_source_file(FileName::Custom("input".into()), source.to_string());
        let diag_ctx = DiagnosticContext::new(&self.source_map);
        let (tokens, _) = lexer::tokenize(source);
        let parser = parser::Parser::new(&tokens, source, &diag_ctx, sf.start_pos);
        let mut ast = parser.parse();
        ast.source = source.to_string();
        let dump = dump_ast_to_string_pretty(&ast, ast.root);
        std::fs::write(output_path, dump)
    }

    /// Compile source and run it immediately.
    pub fn run(&self, source: &str) -> RunResult {
        let result = self.compile(source);
        if result.has_errors {
            return RunResult::CompileError(result.diagnostics);
        }

        // Initialize the engine.
        let mut engine = Engine::with_defaults();

        // Load compiled functions into the VM.
        let main_str_id = str_interner::intern("main");
        let mut entry_func_id = None;
        for func in &result.codegen_output.functions {
            let code = FunctionCode {
                func_id: func.func_id,
                instructions: func.instructions.clone(),
                register_count: func.register_count,
                param_count: func.param_count,
                is_closure: func.is_closure,
            };
            let fid = engine.vm_mut().add_function(code);
            if func.name == main_str_id {
                entry_func_id = Some(fid);
            }
        }

        // If there are no functions to run, return unit.
        let Some(entry) = entry_func_id else {
            return RunResult::Ok;
        };

        // Spawn and execute.
        engine.vm_mut().spawn_root(entry);
        match engine.vm_mut().run() {
            interpreter::VmResult::Finished => RunResult::Ok,
            interpreter::VmResult::Error(e) => RunResult::RuntimeError(format!("{:?}", e)),
        }
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
        // An empty source should compile without panicking.
        let _ = result;
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
}
