//! Pipeline integration tests — verify the full compilation pipeline:
//!   Source → Lex → Parse → Resolve → NIR → Codegen → Archive → VM
//!
//! These tests focus on correctness at each pipeline stage, using the
//! convenience wrappers in the `driver` crate where appropriate.

mod common;

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

#[test]
fn constant_operands_keep_distinct_values_until_the_operation() {
    for (expression, expected) in [
        ("1 + 2", 3),
        ("8 - 3", 5),
        ("6 * 7", 42),
        ("9 / 3", 3),
        ("10 % 3", 1),
        ("if 1 < 2 { 42 } else { 0 }", 42),
    ] {
        let source = format!("fn main() {{ {expression} }}");
        assert_eq!(common::run_value(&source), Ok(expected), "{expression}");
    }
}

#[test]
fn many_local_values_do_not_alias_registers() {
    let declarations = (0..40)
        .map(|index| format!("let x{index} = {index}\n"))
        .collect::<String>();
    assert_eq!(
        common::run_value(&format!("fn main() {{ {declarations} x0 + x39 }}")),
        Ok(39)
    );
}

#[test]
fn caller_locals_survive_a_function_with_many_locals() {
    let caller = (0..40)
        .map(|index| format!("let x{index} = {index}\n"))
        .collect::<String>();
    let callee = (0..40)
        .map(|index| format!("let x{index} = {}\n", index + 100))
        .collect::<String>();
    let source = format!(
        "fn clobber() {{ {callee} x12 }}\nfn main() {{ {caller} let result = clobber()\nx12 + result }}"
    );
    assert_eq!(common::run_value(&source), Ok(124));
}

#[test]
fn many_function_parameters_keep_their_argument_values() {
    let parameters = (0..25)
        .map(|index| format!("x{index}: i64"))
        .collect::<Vec<_>>()
        .join(", ");
    let arguments = (0..25)
        .map(|index| index.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let source =
        format!("fn sum({parameters}) {{ x0 + x19 + x24 }}\nfn main() {{ sum({arguments}) }}");
    assert_eq!(common::run_value(&source), Ok(43));
}

#[test]
fn recursive_calls_restore_each_functions_local_slots() {
    assert_eq!(
        common::run_value(
            "fn factorial(n: i64) -> i64 { if n == 0 { 1 } else { n * factorial(n - 1) } }\nfn main() { factorial(5) }"
        ),
        Ok(120)
    );
}

#[test]
fn local_slots_preserve_reassignment_in_loops() {
    assert_eq!(
        common::run_value(
            "fn main() { var value = 0\nwhile value < 40 { value = value + 1 }\nvalue + 2 }"
        ),
        Ok(42)
    );
}

#[test]
fn excessive_function_arguments_report_errors_before_emission() {
    let parameters = (0..33)
        .map(|index| format!("x{index}: i64"))
        .collect::<Vec<_>>()
        .join(", ");
    let result = compile(&format!("fn wide({parameters}) {{ x0 }}"));
    assert!(result.has_errors);
    assert!(result.codegen_output.functions.is_empty());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("at most 32 parameters"))
    );
}

#[test]
fn objects_allocate_all_declared_fields() {
    let fields = (0..12)
        .map(|index| format!("f{index}: i64"))
        .collect::<Vec<_>>()
        .join(", ");
    let values = (0..12)
        .map(|index| format!("f{index}: {index}"))
        .collect::<Vec<_>>()
        .join(", ");
    assert_eq!(
        common::run_value(&format!(
            "struct Large {{ {fields} }}\nfn main() {{ let p = Large {{ {values} }}\np.f11 }}"
        )),
        Ok(11)
    );
}

#[test]
fn wide_object_type_indices_report_a_diagnostic_instead_of_panicking() {
    let definitions = (0..4100)
        .map(|index| format!("struct T{index} {{ x: i64 }}\n"))
        .collect::<String>();
    let result = compile(&format!(
        "{definitions}fn main() {{ let p = T4099 {{ x: 42 }}\np.x }}"
    ));
    assert!(result.has_errors);
    assert!(result.codegen_output.functions.is_empty());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("12-bit type indices"))
    );
}

#[test]
fn closure_capture_parameters_survive_a_full_register_window() {
    let declarations = (0..31)
        .map(|index| format!("let x{index} = {index}\n"))
        .collect::<String>();
    let sum = (0..31)
        .map(|index| format!("x{index}"))
        .collect::<Vec<_>>()
        .join(" + ");
    assert_eq!(
        common::run_value(&format!(
            "fn main() {{ {declarations} let f = |value| {sum} + value\nf(1) }}"
        )),
        Ok(466)
    );
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
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "nessa_test_archive_{}_{}",
        std::process::id(),
        nonce
    ));
    std::fs::create_dir(&dir).expect("create isolated archive directory");
    let path = dir.join("test.nsbc");

    let driver = Driver::new();
    driver
        .compile_to_archive("fn main() { 1 }", &path)
        .expect("compile_to_archive failed");

    // Verify file exists and starts with magic.
    let bytes = std::fs::read(&path).expect("Failed to read archive file");
    assert_eq!(&bytes[..4], b"NSBC");

    // Clean up.
    std::fs::remove_file(&path).expect("remove archive");
    std::fs::remove_dir(&dir).expect("remove isolated archive directory");
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
    let v = runtime::TaggedValue::from_f64(1.25);
    assert_eq!(v.as_f64().unwrap().to_bits(), 1.25_f64.to_bits());
    assert!(runtime::TaggedValue::try_from_f64(std::f64::consts::PI).is_none());
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

#[test]
fn gradual_boundaries_reject_incompatible_values() {
    for source in [
        "fn main() { let x: Any = true; let y: i64 = x; y }",
        "fn main() { let x: Any = true; let y: i64 = 1; y = x; y }",
        "fn consume(x: i64) { 42 }\nfn main() { let x: Any = true; consume(x) }",
        "fn value() -> i64 { let x: Any = true; x }\nfn main() { value() }",
        "fn value() -> i64 { let x: Any = true; if true { return x; }; 42 }\nfn main() { value() }",
        "fn main() { let x: Any = 42; if x { 1 } else { 0 } }",
        "fn main() { let x: Any = 42; while x { return 1; }; 0 }",
        "fn main() { let x: Any = 42; if true and x { 1 } else { 0 } }",
        "fn main() { let x: Any = 42; if not x { 1 } else { 0 } }",
        "fn main() { let x: Any = 1.5; let y: i64 = x; y }",
        "fn main() { let f: Any = |x: i64| 42; f(true) }",
    ] {
        let error = common::run_value(source).expect_err(source);
        assert!(error.contains("TypeError"), "{source}: {error}");
    }
}

#[test]
fn gradual_numeric_widening_establishes_float_representation() {
    for source in [
        "fn main() { let x: Any = 2; let y: f64 = x; y / 4.0 }",
        "fn main() { let x: i64 = 2; let y: f64 = x; y / 4.0 }",
        "fn half(x: f64) -> f64 { x / 4.0 }\nfn main() { let x: Any = 2; half(x) }",
    ] {
        assert_eq!(
            common::run_number(source).unwrap(),
            runtime::Number::F64(0.5),
            "{source}"
        );
    }
}

#[test]
fn inferred_returns_check_any_and_materialize_numeric_joins() {
    for source in [
        "fn identity(x: Any) { x }\nfn main() { let y: i64 = identity(true); y }",
        "fn main() { let y: i64 = identity(true); y }\nfn identity(x: Any) { x }",
        "fn mixed(b: bool) { if b { return true; }; 42 }\nfn main() { let y: i64 = mixed(true); y }",
    ] {
        let error = common::run_value(source).expect_err(source);
        assert!(error.contains("TypeError"), "{source}: {error}");
    }
    assert_eq!(
        common::run_number(
            "fn mixed(b: bool) { if b { return 2; }; 0.5 }\nfn main() { mixed(true) / 4.0 }"
        )
        .unwrap(),
        runtime::Number::F64(0.5),
    );
}

#[test]
fn typed_literals_and_arithmetic_preserve_narrow_identity() {
    for source in [
        "fn main() { let x: i8 = 42; let y: Any = x; let z: i8 = y; z }",
        "fn main() { let x: i8 = 20; let y: i8 = x + x; let z: Any = y; let result: i8 = z; result + 2 }",
        "fn main() { let x: f32 = 0.5; let y: Any = x; let z: f32 = y; if z == 0.5 { 42 } else { 0 } }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn ordinary_branch_and_loop_values_do_not_return_from_the_function() {
    for source in [
        "fn main() { if true { 1 }; 42 }",
        "fn main() { let i = 0; while i < 3 { i = i + 1; 100 }; i + 39 }",
        "fn main() { while true { return 42; }; 0 }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
    assert_eq!(
        common::run_value("fn done() { while true { return; }; 1 }\nfn main() { done(); 42 }")
            .unwrap(),
        42
    );
}

#[test]
fn type_values_reflect_dynamic_values_and_preserve_type_identity() {
    for condition in [
        "type_of(42) == i64",
        "42'type == i64",
        "type_of(i64) == Type",
        "i64'type == Type",
        "type_of(true) == bool",
        "null'type == ?NoReturn",
        "null'type != Unit",
        "type_of(0.1) == f64",
        "type_of(42.as(u128)) == u128",
    ] {
        let source = format!("fn main() {{ if {condition} {{ 42 }} else {{ 0 }} }}");
        assert_eq!(common::run_value(&source).unwrap(), 42, "{condition}");
    }
    for source in [
        "fn identity(t: Type) -> Type { t }\nfn main() { let t: Type = i64; if identity(t) == i64 { 42 } else { 0 } }",
        "typealias Tiny = i8\nfn main() { let x: Tiny = 42; if Tiny == i8 and x'type == Tiny { 42 } else { 0 } }",
        "struct Left {}\nstruct Right {}\nfn main() { if Left != Right { 42 } else { 0 } }",
        "fn main() { let x: Any = true; if x'type == bool { 42 } else { 0 } }",
        "fn main() { let t = i64; let f = || t; if f() == i64 { 42 } else { 0 } }",
        "fn main() { if to_string(i64) == \"i64\" { 42 } else { 0 } }",
    ] {
        assert_eq!(common::run_value(source).unwrap(), 42, "{source}");
    }
}

#[test]
fn type_values_cannot_be_fabricated_or_called() {
    for source in [
        "fn main() { Type(42) }",
        "fn main() { let t: Type = 42; t }",
        "fn main() { let t = i64; t(42) }",
    ] {
        let result = compile(source);
        assert!(result.has_errors, "{source}");
        assert!(result.codegen_output.functions.is_empty());
    }
    assert!(
        common::run_value("fn main() { let t: Any = 42; let x: Type = t; 42 }")
            .unwrap_err()
            .contains("TypeError")
    );
}

#[test]
fn strings_compare_contents_from_distinct_allocations() {
    for condition in [
        "to_string(42) == \"42\"",
        "to_string(42) != \"43\"",
        "\"aa\" < \"ab\"",
        "\"ab\" >= \"aa\"",
    ] {
        let source = format!("fn main() {{ if {condition} {{ 42 }} else {{ 0 }} }}");
        assert_eq!(common::run_value(&source).unwrap(), 42, "{condition}");
    }
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

// ===========================================================================
// Builtin / CallBuiltin
// ===========================================================================

#[test]
fn print_compiles_via_root_builtin() {
    let src = "fn main() { print(42); }";
    let result = compile(src);
    assert!(
        !result.has_errors,
        "print should compile via root builtins: {:?}",
        result.diagnostics
    );
}

#[test]
fn privileged_builtin_view_compiles() {
    let src = "\
const print2 = .print'builtin
fn main() { print2(1); }
";
    let (tokens, lex_errors) = tokenize(src);
    assert!(lex_errors.is_empty());
    let sm = SourceMap::new(FilePathMapping::empty());
    let sf = sm.new_source_file(FileName::Custom("test".into()), src.to_string());
    let diag = DiagnosticContext::new(&sm);
    let parser = Parser::new(&tokens, src, &diag, sf.start_pos);
    let ast = parser.parse();
    let resolved = resolution::resolve_with_options(
        ast,
        &diag,
        resolution::ResolveOptions::for_builtin_package(),
    );
    assert!(
        resolved
            .diagnostics
            .iter()
            .all(|d| d.level != diagnostic::Level::Error),
        "privileged 'builtin should resolve: {:?}",
        resolved.diagnostics
    );
    assert!(!resolved.builtin_fns.is_empty());
}

#[test]
fn function_returns_reject_implicit_integer_narrowing_and_sign_loss() {
    for source in [
        "fn narrow(x: u64) -> i8 { x }",
        "fn narrow(x: u64) -> i64 { x }",
        "fn unsigned(x: i8) -> u64 { x }",
        "fn narrow(x: i128) -> i64 { x }",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "accepted unsafe widening: {source}");
        assert!(result.codegen_output.functions.is_empty());
    }
    for source in [
        "fn widen(x: u8) -> i16 { x }",
        "fn widen(x: u64) -> i128 { x }",
        "fn widen(x: i8) -> i64 { x }",
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(!result.has_errors, "{source}: {:?}", result.diagnostics);
    }
}

#[test]
fn annotated_declarations_supply_expected_literal_types() {
    for (source, expected) in [
        ("fn main() -> i64 { let x: i8 = 42; x }", 42),
        ("fn main() -> i64 { const x: u8 = 255; x }", 255),
        ("fn main() -> i64 { var x: i16 = -128; x }", -128),
        ("fn main() -> i64 { let x: i8 = -128; x }", -128),
    ] {
        assert_eq!(common::run_value(source), Ok(expected), "{source}");
    }
    let source = "fn main() { let x: f32 = 1.25; x }";
    let result = compile(source);
    assert!(!result.has_errors, "{:?}", result.diagnostics);
}

#[test]
fn integer_literal_bounds_are_checked_in_declarations_returns_and_arguments() {
    for source in [
        "fn main() { let x: i8 = 128; }",
        "fn main() { let x: i8 = -129; }",
        "fn main() { let x: u8 = 256; }",
        "fn main() { let x: u8 = -1; }",
        "fn main() { let x: i8 = 0x80; }",
        "fn main() { let x: i8 = 0b10000000; }",
        "fn main() { let x: i8 = 0o200; }",
        "fn main() { let x = 9223372036854775808; }",
        "fn main() { let x: i128 = 170141183460469231731687303715884105728; }",
        "fn main() { let x: i128 = -170141183460469231731687303715884105729; }",
        "fn main() { let x: u128 = 340282366920938463463374607431768211456; }",
        "fn main() -> i8 { 128 }",
        "fn main() -> i8 { -129 }",
        "fn take(x: u8) {}\nfn main() { take(256); }",
        "fn main() { take(256); }\nfn take(x: u8) {}",
        "typealias Tiny = i8\nfn main() { let x: Tiny = 128; }",
    ] {
        let result = compile(source);
        assert!(result.has_errors, "accepted out of range literal: {source}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.message.contains("out of range")),
            "{source}: {:?}",
            result.diagnostics
        );
        assert!(result.codegen_output.functions.is_empty());
    }
    // These assert static range checking only; wide integer runtime storage is separate.
    for source in [
        "fn main() -> i8 { -128 }",
        "fn main() { let x: u8 = 0xff; }",
        "fn main() { let x: i64 = -9223372036854775808; }",
        "fn main() { let x: u64 = 18446744073709551615; }",
        "fn main() { let x: i128 = -170141183460469231731687303715884105728; }",
        "fn main() { let x: u128 = 340282366920938463463374607431768211455; }",
        "fn take(x: i8) {}\nfn main() { take(-128); }",
        "typealias Tiny = i8\nfn main() { let x: Tiny = -128; }",
    ] {
        let result = compile(source);
        assert!(!result.has_errors, "{source}: {:?}", result.diagnostics);
    }
}

#[test]
fn unannotated_returns_do_not_disable_fixed_parameter_checks() {
    for source in [
        "fn take(x: i64) {}\nfn main() { take(true); }",
        "fn main() { take(true); }\nfn take(x: i64) {}",
        "fn take(x: i64) {}\nfn main() { take(); }",
        "fn take(x: i64) {}\nfn main() { take(1, 2); }",
    ] {
        let result = compile(source);
        assert!(result.has_errors, "accepted invalid call: {source}");
        assert!(result.codegen_output.functions.is_empty());
    }
}

#[test]
fn numeric_values_survive_literal_lowering_and_heap_promotion() {
    use runtime::Number;
    for (source, expected) in [
        ("fn main() { 0x2a + 0b101010 + 0o52 }", Number::I64(126)),
        (
            "fn main() -> i64 { 9223372036854775807 }",
            Number::I64(i64::MAX),
        ),
        (
            "fn main() -> i64 { -9223372036854775808 }",
            Number::I64(i64::MIN),
        ),
        (
            "fn main() -> u64 { 18446744073709551615 }",
            Number::U64(u64::MAX),
        ),
        (
            "fn main() -> i128 { -170141183460469231731687303715884105728 }",
            Number::I128(i128::MIN),
        ),
        (
            "fn main() -> u128 { 340282366920938463463374607431768211455 }",
            Number::U128(u128::MAX),
        ),
        ("fn main() -> i128 { 42 }", Number::I128(42)),
        ("fn main() -> u128 { 42 }", Number::U128(42)),
        (
            "fn main() { 72057594037927935 + 1 }",
            Number::I64(1_i64 << 56),
        ),
        (
            "fn main() { 9223372036854775807 + 1 }",
            Number::I128(1_i128 << 63),
        ),
        (
            "fn main() { let x: u64 = 18446744073709551615; x + x }",
            Number::U128(2 * u64::MAX as u128),
        ),
        (
            "fn main() { let x: u64 = 18446744073709551615; x / 3 }",
            Number::I128(u64::MAX as i128 / 3),
        ),
        (
            "fn main() { let x: u64 = 18446744073709551615; x % 3 }",
            Number::I128(0),
        ),
        (
            "fn main() { to_i64(9223372036854775807) }",
            Number::I64(i64::MAX),
        ),
        (
            "fn main() { abs(-9223372036854775808) }",
            Number::I128(1_i128 << 63),
        ),
        ("fn main() { pow(2, 63) }", Number::I128(1_i128 << 63)),
        (
            "fn main() -> i128 { let x: u64 = 18446744073709551615; -x }",
            Number::I128(-(u64::MAX as i128)),
        ),
    ] {
        assert_eq!(common::run_number(source), Ok(expected), "{source}");
    }
    for (source, expected) in [
        ("fn main() { 0.1 }", 0.1_f64),
        ("fn main() { 0.1 + 0.2 }", 0.1_f64 + 0.2_f64),
        ("fn main() { -0.1 * 0.3 }", -0.1_f64 * 0.3_f64),
        ("fn main() { to_f64(9223372036854775807) }", i64::MAX as f64),
        ("fn main() { sin(0.1) }", 0.1_f64.sin()),
    ] {
        let Number::F64(actual) = common::run_number(source).unwrap() else {
            panic!("{source}");
        };
        assert_eq!(actual.to_bits(), expected.to_bits(), "{source}");
    }
}

#[test]
fn numeric_comparison_uses_values_instead_of_heap_addresses() {
    for source in [
        "fn main() { if 9223372036854775807 == 9223372036854775807 { 42 } else { 0 } }",
        "fn main() { if 9223372036854775806 < 9223372036854775807 { 42 } else { 0 } }",
        "fn main() { let x: u128 = 340282366920938463463374607431768211455; if x > 0 { 42 } else { 0 } }",
        "fn main() { if 0.1 + 0.2 > 0.3 { 42 } else { 0 } }",
        "fn main() { let x: u64 = 18446744073709551615; x match {\n18446744073709551615 => 42\n_ => 0\n} }",
        "fn main() { 42 match {\n0x2a => 42\n_ => 0\n} }",
    ] {
        assert_eq!(common::run_value(source), Ok(42), "{source}");
    }
}

#[test]
fn derived_methods_read_numeric_values_and_validate_other_layouts() {
    for (source, expected) in [
        (
            "struct Boxed { value: i64 }\nderive Eq for Boxed\nfn main() { let a = Boxed { value: 9223372036854775807 }; let b = Boxed { value: 9223372036854775807 }; if a.eq(b) { 42 } else { 0 } }",
            Ok(42),
        ),
        (
            "struct Boxed { value: i64 }\nderive Eq, Ord for Boxed\nfn main() { let a = Boxed { value: 9223372036854775807 }; let b = Boxed { value: 4611686018427387904 }; if a.cmp(b) == Ordering.greater { 1 } else { 0 } }",
            Ok(1),
        ),
        (
            "struct Boxed { value: i64 }\nderive Eq for Boxed\nfn main() { let a = Boxed { value: 9223372036854775807 }; a.eq(42) }",
            Err("TypeError".to_owned()),
        ),
    ] {
        assert_eq!(common::run_value(source), expected, "{source}");
    }
}

#[test]
fn explicit_numeric_casts_convert_values_and_reject_overflow() {
    use runtime::Number;
    for (source, expected) in [
        ("fn main() { 42.as(f64) }", Number::F64(42.0)),
        ("fn main() { 42.as(u128) }", Number::U128(42)),
        ("fn main() { 42.as(i128) }", Number::I128(42)),
        ("fn main() { (-128).as(i8) }", Number::I64(-128)),
        ("fn main() { 255.as(u8) }", Number::U64(255)),
        ("fn main() { 127.9.as(i8) }", Number::I64(127)),
        ("fn main() { (-0.5).as(u8) }", Number::U64(0)),
        ("fn main() { 0.1.as(f32) }", Number::F64(0.1_f32 as f64)),
        (
            "typealias Tiny = i8\nfn main() { 42.as(Tiny) }",
            Number::I64(42),
        ),
    ] {
        assert_eq!(common::run_number(source), Ok(expected), "{source}");
    }
    for (source, expected) in [
        ("fn main() { 300.as(i8) }", "NumericOverflow"),
        ("fn main() { (-1).as(u64) }", "NumericOverflow"),
        ("fn main() { true.as(i64) }", "TypeError"),
        ("fn main() { 1.as(bool) }", "TypeError"),
        (
            "fn main() { (9223372036854775807 + 1).as(i64) }",
            "NumericOverflow",
        ),
    ] {
        assert_eq!(
            common::run_number(source),
            Err(expected.to_owned()),
            "{source}"
        );
    }
}

#[test]
fn invalid_numeric_operations_report_errors() {
    for source in [
        "fn main() { true + false }",
        "fn main() { 1 + \"a\" }",
        "fn main() { -true }",
        "fn main() -> u64 { let x: u64 = 3; -x }",
    ] {
        let result = compile(source);
        assert!(result.has_errors, "{source}");
        assert!(result.codegen_output.functions.is_empty());
    }
    for (source, error) in [
        ("fn main() { 1 / 0 }", "DivisionByZero"),
        ("fn main() { 1 % 0 }", "DivisionByZero"),
        (
            "fn main() { to_i64(9223372036854775807 + 1) }",
            "NumericOverflow",
        ),
        (
            "fn main() { let x: u128 = 340282366920938463463374607431768211455; x + x }",
            "NumericOverflow",
        ),
    ] {
        assert_eq!(
            common::run_number(source),
            Err(error.to_owned()),
            "{source}"
        );
    }
    let result = compile(
        "fn main() { 42 match {\n340282366920938463463374607431768211456 => 1\n_ => 0\n} }",
    );
    assert!(result.has_errors);
    assert!(result.codegen_output.functions.is_empty());
}

#[test]
fn boolean_operators_preserve_values_and_skip_unneeded_operands() {
    for source in [
        "fn main() { if not false { 42 } else { 0 } }",
        "fn main() { if not (not true) { 42 } else { 0 } }",
        "fn main() { if true and true { 42 } else { 0 } }",
        "fn main() { if false or true { 42 } else { 0 } }",
        "fn main() { if not (true and false) { 42 } else { 0 } }",
        "fn main() { if not (false or false) { 42 } else { 0 } }",
        "fn fail() -> bool { 1 / 0; true }\nfn main() { if false and fail() { 0 } else { 42 } }",
        "fn fail() -> bool { 1 / 0; false }\nfn main() { if true or fail() { 42 } else { 0 } }",
        "fn main() { if (false or true) and (true or false) { 42 } else { 0 } }",
    ] {
        assert_eq!(common::run_value(source), Ok(42), "{source}");
    }
    for source in [
        "fn fail() -> bool { 1 / 0; true }\nfn main() { true and fail() }",
        "fn fail() -> bool { 1 / 0; true }\nfn main() { false or fail() }",
    ] {
        assert_eq!(
            common::run_number(source),
            Err("DivisionByZero".into()),
            "{source}"
        );
    }
    for source in [
        "fn main() { 1 and true }",
        "fn main() { false or 2 }",
        "fn main() { not 1 }",
    ] {
        let result = compile(source);
        assert!(result.has_errors, "{source}");
        assert!(result.codegen_output.functions.is_empty());
    }
}
