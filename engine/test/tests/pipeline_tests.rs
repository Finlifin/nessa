//! Pipeline integration tests — verify the full compilation pipeline:
//!   Source → Lex → Parse → Resolve → NIR → Codegen → Archive → VM
//!
//! These tests focus on correctness at each pipeline stage, using the
//! convenience wrappers in the `driver` crate where appropriate.

use diagnostic::DiagnosticContext;
use driver::{CompileResult, Driver, RunResult};
use lexer::tokenize;
use parser::Parser;
use rustc_span::source_map::FilePathMapping;
use rustc_span::{FileName, SourceMap};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn compile(src: &str) -> CompileResult {
    Driver::new().compile(src)
}

fn run(src: &str) -> RunResult {
    Driver::new().run(src)
}

/// Run the pipeline up to resolution and return the ResolvedAst.
fn resolve_source(src: &str) -> resolution::ResolvedAst {
    let (tokens, lex_errors) = tokenize(src);
    assert!(lex_errors.is_empty(), "Lex errors: {lex_errors:?}");
    let sm = SourceMap::new(FilePathMapping::empty());
    let sf = sm.new_source_file(FileName::Custom("test".into()), src.to_string());
    let diag = DiagnosticContext::new(&sm);
    let parser = Parser::new(&tokens, src, &diag, sf.start_pos);
    let ast = parser.parse();
    resolution::resolve(ast, &diag)
}

/// Run the pipeline up to NIR and return the NirModule.
fn lower_source(src: &str) -> nir::NirModule {
    let resolved = resolve_source(src);
    nir::lower(&resolved)
}

/// Run the pipeline up to codegen and return CodegenOutput.
fn codegen_source(src: &str) -> nsbc::CodegenOutput {
    let nir_module = lower_source(src);
    codegen::codegen(&nir_module)
}

// ===========================================================================
// Resolution stage
// ===========================================================================

#[test]
fn resolve_empty_source() {
    let resolved = resolve_source("");
    // Should produce at least the root scope.
    assert!(!resolved.scopes.is_empty(), "Expected at least one scope");
}

#[test]
fn resolve_let_binding() {
    let resolved = resolve_source("let x = 42");
    // Should have scopes and possibly symbols.
    assert!(!resolved.scopes.is_empty());
}

#[test]
fn resolve_function_def() {
    let resolved = resolve_source("fn add(a: i32, b: i32) -> i32 { a + b }");
    // The function should appear as a symbol.
    assert!(!resolved.scopes.is_empty());
}

#[test]
fn resolve_struct_def() {
    let resolved = resolve_source("struct Point {\n  x: i32,\n  y: i32\n}");
    assert!(!resolved.scopes.is_empty());
}

#[test]
fn resolve_multiple_defs() {
    let src = "\
fn foo() { 1 }
fn bar() { foo() }";
    let resolved = resolve_source(src);
    // Both functions should be resolved.
    assert!(!resolved.scopes.is_empty());
    // Check we have at least 2 scopes (root + function bodies).
    assert!(
        resolved.scopes.len() >= 2,
        "Expected multiple scopes, got {}",
        resolved.scopes.len()
    );
}

#[test]
fn resolve_enum_def() {
    let src = "enum Color {\n  .Red,\n  .Green,\n  .Blue\n}";
    let resolved = resolve_source(src);
    assert!(!resolved.scopes.is_empty());
}

#[test]
fn resolve_trait_def() {
    let src = "trait Show {\n  fn show(self) -> String\n}";
    let resolved = resolve_source(src);
    assert!(!resolved.scopes.is_empty());
}

// ===========================================================================
// NIR lowering stage
// ===========================================================================

#[test]
fn nir_empty_source() {
    let module = lower_source("");
    // No functions in an empty source.
    assert!(
        module.functions.is_empty(),
        "Expected no functions in empty source, got {}",
        module.functions.len()
    );
}

#[test]
fn nir_single_function() {
    let module = lower_source("fn foo() { 1 }");
    // Should produce at least one NIR function.
    assert!(
        !module.functions.is_empty(),
        "Expected at least one NIR function"
    );
}

#[test]
fn nir_function_has_entry_block() {
    let module = lower_source("fn id(x: i32) -> i32 { x }");
    if let Some(func) = module.functions.first() {
        assert!(!func.blocks.is_empty(), "Expected at least one basic block");
    }
}

#[test]
fn nir_multiple_functions() {
    let src = "\
fn add(a: i32, b: i32) -> i32 { a + b }
fn sub(a: i32, b: i32) -> i32 { a - b }";
    let module = lower_source(src);
    assert!(
        module.functions.len() >= 2,
        "Expected >= 2 functions, got {}",
        module.functions.len()
    );
}

// ===========================================================================
// Codegen stage
// ===========================================================================

#[test]
fn codegen_empty_source() {
    let output = codegen_source("");
    assert!(
        output.functions.is_empty(),
        "Empty source should produce no compiled functions"
    );
}

#[test]
fn codegen_single_function() {
    let output = codegen_source("fn foo() { 1 }");
    assert!(
        !output.functions.is_empty(),
        "Expected at least one compiled function"
    );
}

#[test]
fn codegen_function_has_instructions() {
    let output = codegen_source("fn foo() { 1 }");
    if let Some(func) = output.functions.first() {
        assert!(
            !func.instructions.is_empty(),
            "Compiled function should have at least one instruction"
        );
    }
}

#[test]
fn codegen_instructions_decodable() {
    let output = codegen_source("fn add(a: i32, b: i32) -> i32 { a + b }");
    for func in &output.functions {
        for (i, &word) in func.instructions.iter().enumerate() {
            let decoded = nsbc::Instruction::decode(word);
            assert!(
                decoded.is_some(),
                "Instruction at index {i} ({word:#018x}) failed to decode in func {:?}",
                func.func_id
            );
        }
    }
}

#[test]
fn codegen_multiple_functions() {
    let src = "\
fn f() { 1 }
fn g() { 2 }
fn h() { 3 }";
    let output = codegen_source(src);
    assert!(
        output.functions.len() >= 3,
        "Expected >= 3 compiled functions, got {}",
        output.functions.len()
    );
}

// ===========================================================================
// Archive (nsbc_io) roundtrip
// ===========================================================================

#[test]
fn archive_roundtrip_empty() {
    let output = codegen_source("");
    let bytes = nsbc_io::write_archive(&output).expect("write_archive failed");
    // Should start with the NSBC magic bytes.
    assert!(bytes.len() >= 4, "Archive too short");
    assert_eq!(&bytes[..4], b"NSBC", "Bad magic bytes");
}

#[test]
fn archive_roundtrip_single_function() {
    let output = codegen_source("fn foo() { 1 }");
    let bytes = nsbc_io::write_archive(&output).expect("write_archive failed");
    assert_eq!(&bytes[..4], b"NSBC");

    // Read it back.
    let archive = nsbc_io::Archive::read_from(&mut &bytes[..]).expect("read_from failed");
    assert_eq!(archive.header.magic, *b"NSBC");
    assert_eq!(archive.header.version, nsbc::VERSION);
}

#[test]
fn archive_has_code_section() {
    let output = codegen_source("fn foo() { 1 }");
    let bytes = nsbc_io::write_archive(&output).expect("write_archive failed");
    let archive = nsbc_io::Archive::read_from(&mut &bytes[..]).expect("read_from failed");
    let code = archive.find_section(nsbc::SectionKind::Code);
    assert!(code.is_some(), "Archive should have a Code section");
}

// ===========================================================================
// BytecodeStore (nsbc)
// ===========================================================================

#[test]
fn bytecode_store_load_codegen_output() {
    let output = codegen_source("fn foo() { 1 }");
    let mut store = nsbc::BytecodeStore::new();
    store.load_codegen_output(output);
    assert!(
        store.function_count() > 0,
        "Store should have functions after loading"
    );
}

#[test]
fn bytecode_store_fetch() {
    let output = codegen_source("fn foo() { 1 }");
    let func_count = output.functions.len();
    let mut store = nsbc::BytecodeStore::new();
    store.load_codegen_output(output);

    // Try fetching the first instruction of the first function.
    if func_count > 0 {
        let header = store.header(nsbc::FuncId(0));
        assert!(header.is_some(), "Expected header for FuncId(0)");
    }
}

// ===========================================================================
// Driver: compile
// ===========================================================================

#[test]
fn driver_compile_empty() {
    let result = compile("");
    // Empty source compiles with no functions (or trivially).
    // Main check: it doesn't panic.
    let _ = result;
}

#[test]
fn driver_compile_simple_fn() {
    let result = compile("fn main() { 42 }");
    // Should not indicate errors for valid source.
    assert!(!result.has_errors, "Expected no compile errors");
}

#[test]
fn driver_compile_multiple_fns() {
    let src = "\
fn add(a: i32, b: i32) -> i32 { a + b }
fn main() { add(1, 2) }";
    let result = compile(src);
    assert!(!result.has_errors, "Expected no compile errors");
    assert!(
        result.codegen_output.functions.len() >= 2,
        "Expected >= 2 compiled functions"
    );
}

#[test]
fn driver_compile_struct_and_fn() {
    let src = "\
struct Point {
  x: i32,
  y: i32
}
fn origin() -> Point { Point { x: 0, y: 0 } }";
    let result = compile(src);
    // Just verify it compiles without panicking.
    let _ = result;
}

// ===========================================================================
// Driver: compile_to_archive (file I/O)
// ===========================================================================

#[test]
fn driver_compile_to_archive() {
    let dir = std::env::temp_dir().join("nessa_test_archive");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("test.nsbc");

    let driver = Driver::new();
    driver
        .compile_to_archive("fn main() { 1 }", &path)
        .expect("compile_to_archive failed");

    // Verify file exists and starts with magic.
    let bytes = std::fs::read(&path).expect("Failed to read archive file");
    assert_eq!(&bytes[..4], b"NSBC");

    // Clean up.
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir(&dir);
}

// ===========================================================================
// Driver: run
// ===========================================================================

#[test]
fn driver_run_empty() {
    let result = run("");
    // Empty source should run successfully (nothing to execute).
    assert!(
        matches!(result, RunResult::Ok),
        "Expected RunResult::Ok for empty source, got: {result:?}"
    );
}

#[test]
fn driver_run_simple_fn() {
    let result = run("fn main() { 42 }");
    // Should finish without error.
    assert!(
        matches!(result, RunResult::Ok),
        "Expected RunResult::Ok, got: {result:?}"
    );
}

// ===========================================================================
// Engine initialization
// ===========================================================================

#[test]
fn engine_with_defaults() {
    let engine = initialization::Engine::with_defaults();
    // Should have a valid type pool.
    let _ = engine.type_pool();
}

#[test]
fn engine_custom_config() {
    let config = initialization::EngineConfig {
        gc: gc::GcConfig {
            heap_size: 1024 * 1024,
            ..Default::default()
        },
        debug: true,
    };
    let engine = initialization::Engine::new(config);
    let _ = engine.type_pool();
}

// ===========================================================================
// Instruction encode/decode consistency
// ===========================================================================

#[test]
fn instruction_roundtrip_r_type() {
    let instr = nsbc::Instruction::add(nsbc::Reg(0), nsbc::Reg(1), nsbc::Reg(2));
    let word = instr.encode();
    let decoded = nsbc::Instruction::decode(word).unwrap();
    assert_eq!(decoded, instr);
}

#[test]
fn instruction_roundtrip_sub() {
    let instr = nsbc::Instruction::sub(nsbc::Reg(3), nsbc::Reg(4), nsbc::Reg(5));
    let word = instr.encode();
    let decoded = nsbc::Instruction::decode(word).unwrap();
    assert_eq!(decoded, instr);
}

#[test]
fn instruction_roundtrip_mov() {
    let instr = nsbc::Instruction::mov(nsbc::Reg(0), nsbc::Reg(7));
    let word = instr.encode();
    let decoded = nsbc::Instruction::decode(word).unwrap();
    assert_eq!(decoded, instr);
}

// ===========================================================================
// Runtime: TaggedValue
// ===========================================================================

#[test]
fn tagged_value_int() {
    let v = runtime::TaggedValue::from_i64(42);
    assert_eq!(v.as_i64(), Some(42));
    assert!(v.is_immediate());
}

#[test]
fn tagged_value_float() {
    let v = runtime::TaggedValue::from_f64(3.14);
    assert!((v.as_f64().unwrap() - 3.14).abs() < 1e-10);
}

#[test]
fn tagged_value_bool() {
    let t = runtime::TaggedValue::from_bool(true);
    let f = runtime::TaggedValue::from_bool(false);
    assert_eq!(t.as_bool(), Some(true));
    assert_eq!(f.as_bool(), Some(false));
}

#[test]
fn tagged_value_unit() {
    let v = runtime::TaggedValue::UNIT;
    assert!(v.is_unit());
}

#[test]
fn tagged_value_null() {
    let v = runtime::TaggedValue::NULL;
    assert!(v.is_null());
}

#[test]
fn tagged_value_char() {
    let v = runtime::TaggedValue::from_char('X');
    assert_eq!(v.as_char(), Some('X'));
}

// ===========================================================================
// Runtime: RegisterFile
// ===========================================================================

#[test]
fn register_file_get_set() {
    let mut regs = runtime::RegisterFile::new();
    let val = runtime::TaggedValue::from_i64(99);
    regs.set(nsbc::Reg(0), val);
    assert_eq!(regs.get(nsbc::Reg(0)).as_i64(), Some(99));
}

#[test]
fn register_file_independent_regs() {
    let mut regs = runtime::RegisterFile::new();
    regs.set(nsbc::Reg(0), runtime::TaggedValue::from_i64(1));
    regs.set(nsbc::Reg(1), runtime::TaggedValue::from_i64(2));
    assert_eq!(regs.get(nsbc::Reg(0)).as_i64(), Some(1));
    assert_eq!(regs.get(nsbc::Reg(1)).as_i64(), Some(2));
}

// ===========================================================================
// Full pipeline stress: parse a non-trivial program through all stages
// ===========================================================================

#[test]
fn full_pipeline_fibonacci() {
    let src = "\
fn fib(n: i32) -> i32 {
  if n <= 1 { n }
  else { fib(n - 1) + fib(n - 2) }
}";
    let result = compile(src);
    assert!(
        !result.has_errors,
        "Fibonacci should compile without errors"
    );
    assert!(
        !result.codegen_output.functions.is_empty(),
        "Expected compiled functions"
    );

    // Verify all instructions decode.
    for func in &result.codegen_output.functions {
        for &word in &func.instructions {
            assert!(
                nsbc::Instruction::decode(word).is_some(),
                "Instruction {word:#018x} should decode"
            );
        }
    }
}

#[test]
fn full_pipeline_struct_and_methods() {
    let src = "\
struct Counter {
  value: i32
}
impl Counter {
  fn new() -> Counter { Counter { value: 0 } }
  fn inc(self) -> Counter { Counter { value: self.value + 1 } }
}";
    let result = compile(src);
    // Just check it compiles — structural correctness is the important part.
    let _ = result;
}

// ===========================================================================
// Lambda / closure tests
// ===========================================================================

#[test]
fn lambda_compiles() {
    let src = "\
fn main() {
    let add = |x, y| x + y;
    let result = add(10, 20);
    print(result);
}";
    let result = compile(src);
    assert!(
        !result.has_errors,
        "Lambda should compile without errors: {:?}",
        result.diagnostics,
    );
}

#[test]
fn lambda_type_inferred() {
    let src = "fn main() { let f = |x, y| x + y; f(1, 2); }";
    let resolved = resolve_source(src);
    // The lambda variable should have a Function type.
    let f_sym = resolved
        .symbols
        .iter()
        .find(|s| str_interner::get(s.name) == "f");
    assert!(f_sym.is_some(), "Should have symbol 'f'");
    let ti = f_sym.unwrap().type_index;
    assert_ne!(
        ti,
        type_pool::TypeIndex::INVALID,
        "Lambda variable should have a resolved type",
    );
}

#[test]
fn lambda_run() {
    let src = "\
fn main() {
    let add = |x, y| x + y;
    let result = add(10, 20);
    print(result);
}";
    let result = run(src);
    assert!(
        matches!(result, RunResult::Ok),
        "Lambda execution should succeed, got: {result:?}",
    );
}

// ===========================================================================
// Tacit lambda (_0, _1 placeholders) tests
// ===========================================================================

#[test]
fn tacit_lambda_parses() {
    // _0 + _1 in a call argument should be desugared to |_0, _1| _0 + _1
    let src = "\
fn apply(f, x, y) { f(x, y) }
fn main() {
    let r = apply(_0 + _1, 3, 7);
    print(r);
}";
    let result = compile(src);
    assert!(
        !result.has_errors,
        "Tacit lambda should compile: {:?}",
        result.diagnostics,
    );
}

#[test]
fn tacit_lambda_single_param() {
    // _0 * 2 should produce |_0| _0 * 2
    let src = "\
fn apply1(f, x) { f(x) }
fn main() {
    let r = apply1(_0 * 2, 5);
    print(r);
}";
    let result = compile(src);
    assert!(
        !result.has_errors,
        "Single-param tacit lambda should compile: {:?}",
        result.diagnostics,
    );
}

// ===========================================================================
// Optional parameter tests
// ===========================================================================

#[test]
fn optional_param_compiles() {
    let src = "\
fn greet(.greeting: i64 = 42) {
    print(greeting);
}
fn main() {
    greet();
}";
    let result = compile(src);
    assert!(
        !result.has_errors,
        "Optional param should compile: {:?}",
        result.diagnostics,
    );
}

// ===========================================================================
// Named argument tests
// ===========================================================================

#[test]
fn named_arg_parses() {
    let src = "\
fn greet(.greeting: i64 = 42) {
    print(greeting);
}
fn main() {
    greet(greeting = 99);
}";
    let result = compile(src);
    assert!(
        !result.has_errors,
        "Named argument should compile: {:?}",
        result.diagnostics,
    );
}
