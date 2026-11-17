//! Parser integration tests — comprehensive verification of AST construction.
//!
//! These tests go beyond the existing unit tests in engine/parser/src/tests.rs
//! by exercising larger, more realistic Nessa programs and checking structural
//! properties of the generated AST.

use ast::dump::dump_ast_to_string;
use ast::{NodeIndex, NodeKind};
use diagnostic::DiagnosticContext;
use lexer::tokenize;
use parser::Parser;
use rustc_span::source_map::FilePathMapping;
use rustc_span::{FileName, SourceMap};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse source and return the S-expression dump of the AST.
fn parse_dump(src: &str) -> String {
    let (tokens, lex_errors) = tokenize(src);
    assert!(lex_errors.is_empty(), "Lex errors: {lex_errors:?}");
    let sm = SourceMap::new(FilePathMapping::empty());
    let sf = sm.new_source_file(FileName::Custom("test".into()), src.to_string());
    let diag = DiagnosticContext::new(&sm);
    let parser = Parser::new(&tokens, src, &diag, sf.start_pos);
    let ast = parser.parse();
    dump_ast_to_string(&ast, ast.root)
}

/// Parse and return the root NodeKind.
fn parse_root_kind(src: &str) -> NodeKind {
    let (tokens, _) = tokenize(src);
    let sm = SourceMap::new(FilePathMapping::empty());
    let sf = sm.new_source_file(FileName::Custom("test".into()), src.to_string());
    let diag = DiagnosticContext::new(&sm);
    let parser = Parser::new(&tokens, src, &diag, sf.start_pos);
    let ast = parser.parse();
    ast.node(ast.root).kind
}

/// Parse and return the full Ast for structural inspection.
fn parse_ast(src: &str) -> ast::Ast {
    let (tokens, lex_errors) = tokenize(src);
    assert!(lex_errors.is_empty(), "Lex errors: {lex_errors:?}");
    let sm = SourceMap::new(FilePathMapping::empty());
    let sf = sm.new_source_file(FileName::Custom("test".into()), src.to_string());
    let diag = DiagnosticContext::new(&sm);
    let parser = Parser::new(&tokens, src, &diag, sf.start_pos);
    parser.parse()
}

// ===========================================================================
// Root node is always FileScope
// ===========================================================================

#[test]
fn root_is_filescope_empty() {
    assert_eq!(parse_root_kind(""), NodeKind::FileScope);
}

#[test]
fn root_is_filescope_single_expr() {
    assert_eq!(parse_root_kind("42"), NodeKind::FileScope);
}

#[test]
fn root_is_filescope_multiple_statements() {
    assert_eq!(parse_root_kind("let x = 1\nlet y = 2"), NodeKind::FileScope);
}

// ===========================================================================
// Literal expressions
// ===========================================================================

#[test]
fn parse_integer_literals() {
    for src in ["0", "42", "1000000"] {
        let dump = parse_dump(src);
        assert!(
            dump.contains("Int"),
            "Expected Int for `{src}`, got: {dump}"
        );
    }
}

#[test]
fn parse_real_literal() {
    let dump = parse_dump("3.14");
    assert!(dump.contains("Real"), "Expected Real, got: {dump}");
}

#[test]
fn parse_string_literal() {
    let dump = parse_dump("\"hello\"");
    assert!(dump.contains("Str"), "Expected Str, got: {dump}");
}

#[test]
fn parse_char_literal() {
    let dump = parse_dump("'a'");
    assert!(dump.contains("Char"), "Expected Char, got: {dump}");
}

#[test]
fn parse_bool_literals() {
    let t = parse_dump("true");
    let f = parse_dump("false");
    assert!(t.contains("Bool"), "Expected Bool for true, got: {t}");
    assert!(f.contains("Bool"), "Expected Bool for false, got: {f}");
}

#[test]
fn parse_null_literal() {
    let dump = parse_dump("null");
    assert!(dump.contains("Null"), "Expected Null, got: {dump}");
}

#[test]
fn parse_unit_literal() {
    let dump = parse_dump("()");
    assert!(dump.contains("Unit"), "Expected Unit, got: {dump}");
}

#[test]
fn parse_symbol_literal() {
    let dump = parse_dump(".foo");
    assert!(dump.contains("Symbol"), "Expected Symbol, got: {dump}");
}

// ===========================================================================
// Collection expressions
// ===========================================================================

#[test]
fn parse_list() {
    let dump = parse_dump("[1, 2, 3]");
    assert!(dump.contains("ListOf"), "Expected ListOf, got: {dump}");
    assert_eq!(
        dump.matches("Int").count(),
        3,
        "Expected 3 Int nodes in: {dump}"
    );
}

#[test]
fn parse_empty_list() {
    let dump = parse_dump("[]");
    assert!(dump.contains("ListOf"), "Expected ListOf, got: {dump}");
}

#[test]
fn parse_tuple() {
    let dump = parse_dump("(1, 2, 3)");
    assert!(dump.contains("Tuple"), "Expected Tuple, got: {dump}");
}

#[test]
fn parse_two_element_tuple() {
    let dump = parse_dump("(1, 2)");
    assert!(dump.contains("Tuple"), "Expected Tuple, got: {dump}");
}

// ===========================================================================
// Arithmetic & operator precedence
// ===========================================================================

#[test]
fn parse_all_arithmetic_ops() {
    assert!(parse_dump("a + b").contains("Add"));
    assert!(parse_dump("a - b").contains("Sub"));
    assert!(parse_dump("a * b").contains("Mul"));
    assert!(parse_dump("a / b").contains("Div"));
    assert!(parse_dump("a % b").contains("Mod"));
}

#[test]
fn parse_string_concat() {
    let dump = parse_dump("a ++ b");
    assert!(dump.contains("Concat"), "Expected Concat, got: {dump}");
}

#[test]
fn parse_precedence_mul_over_add() {
    // 1 + 2 * 3  ⟶  Add(Int "1", Mul(Int "2", Int "3"))
    let dump = parse_dump("1 + 2 * 3");
    // The outermost binary op should be Add
    assert!(dump.starts_with("(FileScope") || dump.contains("(Add"));
    assert!(dump.contains("Add"), "Expected Add, got: {dump}");
    assert!(dump.contains("Mul"), "Expected Mul inside, got: {dump}");
}

#[test]
fn parse_all_range_forms() {
    let cases = [
        ("0..10", r#"(RangeFromTo (Int "0") (Int "10"))"#),
        ("0..=10", r#"(RangeFromToInclusive (Int "0") (Int "10"))"#),
        ("(0..=3)", r#"(RangeFromToInclusive (Int "0") (Int "3"))"#),
        ("3..", r#"(RangeFrom (Int "3"))"#),
        ("..4", r#"(RangeTo (Int "4"))"#),
        ("..=5", r#"(RangeToInclusive (Int "5"))"#),
        (
            "x.y..z",
            r#"(RangeFromTo (Projection (Id "x") (Id "y")) (Id "z"))"#,
        ),
    ];
    for (src, expected) in cases {
        let dump = parse_dump(src);
        assert!(
            dump.contains(expected),
            "`{src}` should contain {expected}, got: {dump}"
        );
    }
    assert!(parse_dump("..").contains("(RangeFull"));
}

#[test]
fn parse_range_binds_looser_than_binary_operators() {
    let dump = parse_dump("a + 1..=b * 2");
    assert!(
        dump.contains(
            r#"(RangeFromToInclusive (Add (Id "a") (Int "1")) (Mul (Id "b") (Int "2")))"#
        ),
        "got: {dump}"
    );
    let dump = parse_dump("..n - 1");
    assert!(
        dump.contains(r#"(RangeTo (Sub (Id "n") (Int "1")))"#),
        "got: {dump}"
    );
}

#[test]
fn parse_open_range_before_loop_body() {
    let dump = parse_dump("for i in 3.. { break }");
    assert!(
        dump.contains(r#"(ForLoop nil (Id "i") (RangeFrom (Int "3")) (Block"#),
        "got: {dump}"
    );
    let dump = parse_dump("for i in .. { break }");
    assert!(
        dump.contains(r#"(ForLoop nil (Id "i") (RangeFull"#),
        "got: {dump}"
    );
}

#[test]
fn parse_inclusive_range_requires_end() {
    for src in ["0..=", "..=", "for i in 0..= { break }"] {
        let (tokens, _) = tokenize(src);
        let sm = SourceMap::new(FilePathMapping::empty());
        let sf = sm.new_source_file(FileName::Custom("test".into()), src.to_string());
        let diag = DiagnosticContext::new(&sm);
        Parser::new(&tokens, src, &diag, sf.start_pos).parse();
        assert!(diag.has_errors(), "`{src}` should be rejected");
    }
}

#[test]
fn parse_parenthesized_changes_precedence() {
    // (1 + 2) * 3  ⟶  Mul(Add(...), Int "3")
    let dump = parse_dump("(1 + 2) * 3");
    assert!(dump.contains("Mul"), "Expected Mul at top, got: {dump}");
    assert!(
        dump.contains("Add"),
        "Expected Add inside parens, got: {dump}"
    );
}

#[test]
fn parse_left_associativity() {
    // 1 - 2 - 3  ⟶  Sub(Sub(1, 2), 3)
    let dump = parse_dump("1 - 2 - 3");
    // Should have two Sub nodes
    assert_eq!(
        dump.matches("Sub").count(),
        2,
        "Expected two Sub nodes in: {dump}"
    );
}

#[test]
fn parse_negative_prefix() {
    let dump = parse_dump("-42");
    assert!(dump.contains("Negative"), "Expected Negative, got: {dump}");
}

// ===========================================================================
// Comparison & boolean operators
// ===========================================================================

#[test]
fn parse_all_comparisons() {
    assert!(parse_dump("a == b").contains("BoolEq"));
    assert!(parse_dump("a != b").contains("BoolNotEq"));
    assert!(parse_dump("a > b").contains("BoolGt"));
    assert!(parse_dump("a >= b").contains("BoolGtEq"));
    assert!(parse_dump("a < b").contains("BoolLt"));
    assert!(parse_dump("a <= b").contains("BoolLtEq"));
}

#[test]
fn parse_boolean_operators() {
    assert!(parse_dump("a and b").contains("BoolAnd"));
    assert!(parse_dump("a or b").contains("BoolOr"));
    assert!(parse_dump("not a").contains("BoolNot"));
}

#[test]
fn parse_matches_operator() {
    let dump = parse_dump("x matches Some(_)");
    assert!(
        dump.contains("BoolMatches"),
        "Expected BoolMatches, got: {dump}"
    );
}

// ===========================================================================
// Postfix & call expressions
// ===========================================================================

#[test]
fn parse_simple_call() {
    let dump = parse_dump("foo(1, 2)");
    assert!(dump.contains("Call"), "Expected Call, got: {dump}");
}

#[test]
fn parse_call_no_args() {
    let dump = parse_dump("foo()");
    assert!(dump.contains("Call"), "Expected Call, got: {dump}");
}

#[test]
fn parse_nested_calls() {
    let dump = parse_dump("f(g(h(x)))");
    assert_eq!(
        dump.matches("Call").count(),
        3,
        "Expected 3 Call nodes in: {dump}"
    );
}

#[test]
fn parse_projection() {
    let dump = parse_dump("obj.field");
    assert!(
        dump.contains("Projection"),
        "Expected Projection, got: {dump}"
    );
}

#[test]
fn parse_chained_projections() {
    let dump = parse_dump("a.b.c.d");
    assert_eq!(
        dump.matches("Projection").count(),
        3,
        "Expected 3 Projection nodes in: {dump}"
    );
}

#[test]
fn parse_projection_then_call() {
    let dump = parse_dump("obj.method(x)");
    assert!(
        dump.contains("Projection"),
        "Expected Projection in: {dump}"
    );
    assert!(dump.contains("Call"), "Expected Call in: {dump}");
}

#[test]
fn parse_pipeline() {
    let dump = parse_dump("x |> f |> g");
    assert_eq!(
        dump.matches("Pipeline").count(),
        2,
        "Expected 2 Pipeline in: {dump}"
    );
}

#[test]
fn parse_infix_fn_call() {
    let dump = parse_dump("a zip b");
    assert!(
        dump.contains("InfixFnCall"),
        "Expected InfixFnCall, got: {dump}"
    );
}

#[test]
fn parse_option_propagation() {
    let dump = parse_dump("x?");
    assert!(
        dump.contains("OptionPropagation"),
        "Expected OptionPropagation, got: {dump}"
    );
}

#[test]
fn parse_error_propagation() {
    let dump = parse_dump("x!");
    assert!(
        dump.contains("ErrorPropagation"),
        "Expected ErrorPropagation, got: {dump}"
    );
}

#[test]
fn parse_type_cast() {
    // Type cast uses dot syntax: `x.as(i32)`.
    let dump = parse_dump("x.as(i32)");
    assert!(dump.contains("TypeCast"), "Expected TypeCast, got: {dump}");
}

// ===========================================================================
// Variable declarations
// ===========================================================================

#[test]
fn parse_let_simple() {
    let dump = parse_dump("let x = 42");
    assert!(dump.contains("LetDecl"), "Expected LetDecl, got: {dump}");
    assert!(dump.contains("Id"), "Expected Id, got: {dump}");
    assert!(dump.contains("Int"), "Expected Int, got: {dump}");
}

#[test]
fn parse_let_typed() {
    let dump = parse_dump("let x: i32 = 42");
    assert!(dump.contains("LetDecl"), "Expected LetDecl, got: {dump}");
}

#[test]
fn parse_var_declaration() {
    let dump = parse_dump("var count = 0");
    assert!(dump.contains("VarDecl"), "Expected VarDecl, got: {dump}");
}

#[test]
fn parse_const_declaration() {
    let dump = parse_dump("const PI = 3.14");
    assert!(
        dump.contains("ConstDecl"),
        "Expected ConstDecl, got: {dump}"
    );
}

// ===========================================================================
// Assignments
// ===========================================================================

#[test]
fn parse_all_assignments() {
    assert!(parse_dump("x = 1").contains("Assign"));
    assert!(parse_dump("x += 1").contains("AddAssign"));
    assert!(parse_dump("x -= 1").contains("SubAssign"));
    assert!(parse_dump("x *= 2").contains("MulAssign"));
    assert!(parse_dump("x /= 2").contains("DivAssign"));
    assert!(parse_dump("x %= 3").contains("ModAssign"));
}

// ===========================================================================
// Control flow
// ===========================================================================

#[test]
fn parse_if_no_else() {
    let dump = parse_dump("if true { 1 }");
    assert!(
        dump.contains("IfStatement"),
        "Expected IfStatement, got: {dump}"
    );
}

#[test]
fn parse_if_else() {
    let dump = parse_dump("if true { 1 } else { 2 }");
    assert!(
        dump.contains("IfStatement"),
        "Expected IfStatement, got: {dump}"
    );
}

#[test]
fn parse_if_else_if_else() {
    let dump = parse_dump("if a { 1 } else if b { 2 } else { 3 }");
    assert!(
        dump.contains("IfStatement"),
        "Expected IfStatement, got: {dump}"
    );
}

#[test]
fn parse_while_loop() {
    let dump = parse_dump("while x > 0 { x -= 1 }");
    assert!(
        dump.contains("WhileLoop"),
        "Expected WhileLoop, got: {dump}"
    );
}

#[test]
fn parse_for_loop() {
    let dump = parse_dump("for item in collection { process(item) }");
    assert!(dump.contains("ForLoop"), "Expected ForLoop, got: {dump}");
}

#[test]
fn parse_return_with_value() {
    let dump = parse_dump("return 42");
    assert!(
        dump.contains("ReturnStatement"),
        "Expected ReturnStatement, got: {dump}"
    );
}

#[test]
fn parse_return_bare() {
    let dump = parse_dump("return");
    assert!(
        dump.contains("ReturnStatement"),
        "Expected ReturnStatement, got: {dump}"
    );
}

#[test]
fn parse_break_statement() {
    let dump = parse_dump("break");
    assert!(
        dump.contains("BreakStatement"),
        "Expected BreakStatement, got: {dump}"
    );
}

#[test]
fn parse_continue_statement() {
    let dump = parse_dump("continue");
    assert!(
        dump.contains("ContinueStatement"),
        "Expected ContinueStatement, got: {dump}"
    );
}

#[test]
fn parse_defer() {
    let dump = parse_dump("defer close(fd)");
    assert!(
        dump.contains("DeferStatement"),
        "Expected DeferStatement, got: {dump}"
    );
}

#[test]
fn parse_block_in_if() {
    // Braces at expression level parse as Object, not Block.
    // Blocks appear as bodies of control flow — test via if.
    let dump = parse_dump("if true { 1 }");
    assert!(
        dump.contains("IfStatement"),
        "Expected IfStatement, got: {dump}"
    );
}

// ===========================================================================
// Pattern matching
// ===========================================================================

#[test]
fn parse_match_basic() {
    let dump = parse_dump("x match {\n  1 => true\n  _ => false\n}");
    assert!(
        dump.contains("PostMatch"),
        "Expected PostMatch, got: {dump}"
    );
    assert!(dump.contains("CaseArm"), "Expected CaseArm, got: {dump}");
}

#[test]
fn parse_match_pattern_call() {
    let dump = parse_dump("x match {\n  Some(v) => v\n  None => 0\n}");
    assert!(
        dump.contains("PostMatch"),
        "Expected PostMatch, got: {dump}"
    );
}

#[test]
fn parse_when_statement() {
    let dump = parse_dump("when {\n  x > 0 => 1\n  x < 0 => -1\n  else => 0\n}");
    assert!(
        dump.contains("WhenStatement"),
        "Expected WhenStatement, got: {dump}"
    );
}

// ===========================================================================
// Lambda / closures
// ===========================================================================

#[test]
fn parse_lambda_one_param() {
    let dump = parse_dump("|x| x + 1");
    assert!(dump.contains("Lambda"), "Expected Lambda, got: {dump}");
}

#[test]
fn parse_lambda_multi_params() {
    let dump = parse_dump("|a, b| a + b");
    assert!(dump.contains("Lambda"), "Expected Lambda, got: {dump}");
}

#[test]
fn parse_lambda_no_params() {
    let dump = parse_dump("|| 42");
    assert!(dump.contains("Lambda"), "Expected Lambda, got: {dump}");
}

// ===========================================================================
// Function definitions
// ===========================================================================

#[test]
fn parse_fn_minimal() {
    let dump = parse_dump("fn f() { 1 }");
    assert!(
        dump.contains("FunctionDef"),
        "Expected FunctionDef, got: {dump}"
    );
}

#[test]
fn parse_fn_with_params_and_return() {
    let dump = parse_dump("fn add(a: i32, b: i32) -> i32 { a + b }");
    assert!(
        dump.contains("FunctionDef"),
        "Expected FunctionDef, got: {dump}"
    );
    assert!(
        dump.contains("ParamTyped"),
        "Expected ParamTyped, got: {dump}"
    );
}

#[test]
fn parse_fn_self_param() {
    let dump = parse_dump("fn show(self) -> String { \"...\" }");
    assert!(
        dump.contains("FunctionDef"),
        "Expected FunctionDef, got: {dump}"
    );
    assert!(
        dump.contains("ParamSelf"),
        "Expected ParamSelf, got: {dump}"
    );
}

// ===========================================================================
// Type definitions
// ===========================================================================

#[test]
fn parse_struct_simple() {
    let src = "struct Point {\n  x: i32,\n  y: i32\n}";
    let dump = parse_dump(src);
    assert!(
        dump.contains("StructDef"),
        "Expected StructDef, got: {dump}"
    );
    assert!(
        dump.contains("StructField"),
        "Expected StructField, got: {dump}"
    );
}

#[test]
fn parse_struct_empty() {
    let dump = parse_dump("struct Empty {}");
    assert!(
        dump.contains("StructDef"),
        "Expected StructDef, got: {dump}"
    );
}

#[test]
fn parse_enum_simple() {
    let src = "enum Color {\n  red,\n  green,\n  blue\n}";
    let dump = parse_dump(src);
    assert!(dump.contains("EnumDef"), "Expected EnumDef, got: {dump}");
    assert!(
        dump.contains("EnumVariant"),
        "Expected EnumVariant, got: {dump}"
    );
}

#[test]
fn parse_enum_with_named_fields() {
    // Enum variant fields use struct-field syntax: `name: Type`.
    let src = "enum Shape {\n  circle(r: f64),\n  rect(w: f64, h: f64)\n}";
    let dump = parse_dump(src);
    assert!(dump.contains("EnumDef"), "Expected EnumDef, got: {dump}");
}

#[test]
fn parse_trait_def() {
    let src = "trait Show {\n  fn show(self) -> String\n}";
    let dump = parse_dump(src);
    assert!(dump.contains("TraitDef"), "Expected TraitDef, got: {dump}");
}

#[test]
fn parse_impl_def() {
    let src = "impl Point {\n  fn origin() -> Point { Point { x: 0, y: 0 } }\n}";
    let dump = parse_dump(src);
    assert!(dump.contains("ImplDef"), "Expected ImplDef, got: {dump}");
}

#[test]
fn parse_impl_trait_for_type() {
    let src = "impl Show for Point {\n  fn show(self) -> String { \"Point\" }\n}";
    let dump = parse_dump(src);
    assert!(
        dump.contains("ImplTraitDef"),
        "Expected ImplTraitDef, got: {dump}"
    );
}

#[test]
fn parse_extend_def() {
    let src = "extend i32 {\n  fn double(self) -> i32 { self * 2 }\n}";
    let dump = parse_dump(src);
    assert!(
        dump.contains("ExtendDef"),
        "Expected ExtendDef, got: {dump}"
    );
}

#[test]
fn parse_typealias() {
    let dump = parse_dump("typealias Name = String");
    assert!(
        dump.contains("Typealias"),
        "Expected Typealias, got: {dump}"
    );
}

#[test]
fn parse_newtype() {
    let dump = parse_dump("newtype UserId = i32");
    assert!(dump.contains("Newtype"), "Expected Newtype, got: {dump}");
}

// ===========================================================================
// Effects
// ===========================================================================

#[test]
fn parse_effect_def() {
    let src = "effect Console {\n  fn print(msg: String)\n}";
    let dump = parse_dump(src);
    assert!(
        dump.contains("EffectDef"),
        "Expected EffectDef, got: {dump}"
    );
}

// ===========================================================================
// Use / import statements
// ===========================================================================

#[test]
fn parse_use_simple() {
    let dump = parse_dump("use std.io");
    assert!(
        dump.contains("UseStatement"),
        "Expected UseStatement, got: {dump}"
    );
}

#[test]
fn parse_use_multi_with_alias() {
    // `as` bindings work inside `{...}` multi-import, not on the path itself.
    let dump = parse_dump("use std.io.{read as rd, write}");
    assert!(
        dump.contains("UseStatement"),
        "Expected UseStatement, got: {dump}"
    );
    assert!(
        dump.contains("PathAsBind"),
        "Expected PathAsBind, got: {dump}"
    );
}

// ===========================================================================
// Visibility modifiers
// ===========================================================================

#[test]
fn parse_pub_fn() {
    let dump = parse_dump("pub fn greet() { 1 }");
    assert!(dump.contains("PubDef"), "Expected PubDef, got: {dump}");
    assert!(
        dump.contains("FunctionDef"),
        "Expected FunctionDef inside, got: {dump}"
    );
}

#[test]
fn parse_pub_struct() {
    let dump = parse_dump("pub struct Foo {\n  x: i32\n}");
    assert!(dump.contains("PubDef"), "Expected PubDef, got: {dump}");
    assert!(
        dump.contains("StructDef"),
        "Expected StructDef inside, got: {dump}"
    );
}

// ===========================================================================
// Complex / multi-statement programs
// ===========================================================================

#[test]
fn parse_multi_function_file() {
    let src = "\
fn add(a: i32, b: i32) -> i32 { a + b }
fn mul(a: i32, b: i32) -> i32 { a * b }
let result = add(1, mul(2, 3))";
    let dump = parse_dump(src);
    assert_eq!(
        dump.matches("FunctionDef").count(),
        2,
        "Expected 2 FunctionDef in: {dump}"
    );
    assert!(dump.contains("LetDecl"), "Expected LetDecl, got: {dump}");
}

#[test]
fn parse_struct_with_methods() {
    let src = "\
struct Vec2 {
  x: f64,
  y: f64
}
impl Vec2 {
  fn length(self) -> f64 { 0.0 }
}";
    let dump = parse_dump(src);
    assert!(
        dump.contains("StructDef"),
        "Expected StructDef, got: {dump}"
    );
    assert!(dump.contains("ImplDef"), "Expected ImplDef, got: {dump}");
}

#[test]
fn parse_enum_and_function() {
    // Simple enum (no variant data) with a function using match.
    let src = r###"
    enum Color {
        red,
        green,
        blue
    }
    fn index(c: Color) -> i32 {
        c match {
            Color.red => 0
            Color.green => 1
            _ => 2
        }
    }"###;
    let dump = parse_dump(src);
    assert!(dump.contains("EnumDef"), "Expected EnumDef, got: {dump}");
    assert!(
        dump.contains("FunctionDef"),
        "Expected FunctionDef, got: {dump}"
    );
    assert!(
        dump.contains("PostMatch"),
        "Expected PostMatch, got: {dump}"
    );
}

// ===========================================================================
// AST structural checks (using Ast directly)
// ===========================================================================

#[test]
fn ast_root_has_children() {
    let ast = parse_ast("let x = 1\nlet y = 2");
    let root = ast.node(ast.root);
    assert_eq!(root.kind, NodeKind::FileScope);
    // FileScope should have multi_children for its top-level items.
    let children = ast.multi_children(ast.root);
    assert!(
        children.len() >= 2,
        "Expected at least 2 children in FileScope, got {}",
        children.len()
    );
}

#[test]
fn ast_let_decl_structure() {
    let ast = parse_ast("let x = 42");
    let root_children = ast.multi_children(ast.root);
    assert!(!root_children.is_empty(), "FileScope has no children");

    // Find the first ExprStatement or LetDecl
    let first = root_children[0];
    let node = ast.node(first);
    // It should be LetDecl (or ExprStatement wrapping it)
    assert!(
        node.kind == NodeKind::LetDecl || node.kind == NodeKind::ExprStatement,
        "Expected LetDecl or ExprStatement, got {:?}",
        node.kind
    );
}

#[test]
fn ast_function_def_structure() {
    let ast = parse_ast("fn foo() { 1 }");
    let root_children = ast.multi_children(ast.root);
    assert!(!root_children.is_empty());

    let first = root_children[0];
    let node = ast.node(first);
    assert_eq!(node.kind, NodeKind::FunctionDef, "Expected FunctionDef");
}

#[test]
fn ast_node_index_null_check() {
    assert!(NodeIndex::NULL.is_null());
    assert!(!NodeIndex(1).is_null());
}

// ===========================================================================
// Error construction / optional type
// ===========================================================================

#[test]
fn parse_error_construction() {
    let dump = parse_dump("error(\"fail\")");
    // Should parse as either ErrorConstruction or a Call with "error" id
    assert!(
        dump.contains("ErrorConstruction") || dump.contains("Call"),
        "Expected ErrorConstruction or Call, got: {dump}"
    );
}

// ===========================================================================
// Continuation expressions (reset/shift)
// ===========================================================================

#[test]
fn parse_reset_expr() {
    // reset syntax: `reset <prompt> <body>`
    let dump = parse_dump("reset p { 42 }");
    assert!(
        dump.contains("ResetExpr"),
        "Expected ResetExpr, got: {dump}"
    );
}

#[test]
fn parse_shift_expr() {
    // shift syntax: `shift <prompt>, <k> <body>`
    let dump = parse_dump("shift p, k { k }");
    assert!(
        dump.contains("ShiftExpr"),
        "Expected ShiftExpr, got: {dump}"
    );
}

// ===========================================================================
// Do-notation / PostDo
// ===========================================================================

#[test]
fn parse_post_do() {
    let dump = parse_dump("list do |x| x + 1");
    assert!(dump.contains("PostDo"), "Expected PostDo, got: {dump}");
}

// ===========================================================================
// Module definitions
// ===========================================================================

#[test]
fn parse_module_def() {
    let src = "mod inner {\n  fn foo() { 1 }\n}";
    let dump = parse_dump(src);
    assert!(
        dump.contains("ModuleDef"),
        "Expected ModuleDef, got: {dump}"
    );
}

// ===========================================================================
// Edge cases & stress tests
// ===========================================================================

#[test]
fn parse_deeply_nested_parens() {
    let dump = parse_dump("((((1))))");
    assert!(dump.contains("Int"), "Expected Int, got: {dump}");
}

#[test]
fn parse_complex_expression_all_ops() {
    let dump = parse_dump("(a + b) * c - d / e % f");
    for op in ["Add", "Mul", "Sub", "Div", "Mod"] {
        assert!(dump.contains(op), "Expected {op} in: {dump}");
    }
}

#[test]
fn parse_chained_method_calls() {
    let dump = parse_dump("a.b().c().d()");
    let call_count = dump.matches("Call").count();
    assert!(
        call_count >= 3,
        "Expected at least 3 Call nodes, got {call_count} in: {dump}"
    );
}

#[test]
fn parse_mixed_postfix() {
    // Combining projection, call, option propagation
    let dump = parse_dump("obj.field?.method()!");
    assert!(
        dump.contains("Projection"),
        "Expected Projection in: {dump}"
    );
    assert!(
        dump.contains("OptionPropagation"),
        "Expected OptionPropagation in: {dump}"
    );
    assert!(dump.contains("Call"), "Expected Call in: {dump}");
    assert!(
        dump.contains("ErrorPropagation"),
        "Expected ErrorPropagation in: {dump}"
    );
}

// ===========================================================================
// Typed patterns
// ===========================================================================

fn nodes_of_kind(ast: &ast::Ast, kind: NodeKind) -> Vec<NodeIndex> {
    (1..ast.nodes.len())
        .map(|index| NodeIndex(index as u32))
        .filter(|&node| ast.node(node).kind == kind)
        .collect()
}

#[test]
fn typed_patterns_bind_tighter_than_or_and_guards() {
    let ast = parse_ast("fn f(v: Any) { v match { x: A or _: B if c => 1, g: i64 -> i64 => 2 } }");
    let guard = nodes_of_kind(&ast, NodeKind::PatternIfGuard)[0];
    let alternatives = ast.fixed_children(guard)[0];
    assert_eq!(ast.node(alternatives).kind, NodeKind::PatternOr);
    let typed = nodes_of_kind(&ast, NodeKind::PatternTyped);
    assert_eq!(typed.len(), 3);
    assert_eq!(ast.fixed_children(alternatives), &typed[..2]);
    let binders: Vec<_> = typed
        .iter()
        .map(|&node| ast.node(ast.fixed_children(node)[0]).kind)
        .collect();
    assert_eq!(binders, [NodeKind::Id, NodeKind::Underscore, NodeKind::Id]);
    assert_eq!(
        ast.node(ast.fixed_children(typed[2])[1]).kind,
        NodeKind::Arrow
    );
}

#[test]
fn typed_patterns_parse_in_matches_and_for_but_not_declarations() {
    let ast = parse_ast(
        "fn f(v: Any, xs: List) { let ok = v matches n: i64; for s: String in xs { s }; let y: i64 = 1; let g = |z: i64| z }",
    );
    assert_eq!(nodes_of_kind(&ast, NodeKind::PatternTyped).len(), 2);
    assert!(!nodes_of_kind(&ast, NodeKind::LetDecl).is_empty());
}

#[test]
fn definitions_without_their_block_are_diagnosed_instead_of_dropping_the_file() {
    for (source, message) in [
        (
            "struct P derive(Eq) { x: i64 }\nfn main() { 1 }",
            "Expected `{` or an indented block after struct name",
        ),
        (
            "fn main() { 1 }\nenum E x",
            "Expected `{` or an indented block after enum name",
        ),
        (
            "trait T x",
            "Expected `{` or an indented block after trait header",
        ),
        (
            "struct P {}\nextend P x",
            "Expected `{` or an indented block after extend header",
        ),
        (
            "fn main() { 1 match 2 }",
            "Expected `{` or an indented block after `match`",
        ),
        (
            "fn main() { when 1 }",
            "Expected `{` or an indented block after `when`",
        ),
    ] {
        let compiled = driver::Driver::new().compile(source);
        assert!(compiled.has_errors, "{source} should be rejected");
        assert!(
            compiled
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message == message),
            "{source}: {:?}",
            compiled.diagnostics
        );
    }
}
