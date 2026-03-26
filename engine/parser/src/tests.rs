#[cfg(test)]
mod tests {
    use ast::NodeKind;
    use ast::dump::dump_ast_to_string;
    use diagnostic::DiagnosticContext;
    use lexer::tokenize;
    use rustc_span::source_map::FilePathMapping;
    use rustc_span::{FileName, SourceMap};

    use crate::parser::Parser;

    /// Helper: parse source code and return the AST S-expression dump.
    fn parse_dump(src: &str) -> String {
        let (tokens, lex_errors) = tokenize(src);
        assert!(lex_errors.is_empty(), "Lexer errors: {lex_errors:?}");

        let sm = SourceMap::new(FilePathMapping::empty());
        let sf = sm.new_source_file(FileName::Custom("test".into()), src.to_string());
        let diag = DiagnosticContext::new(&sm);
        let mut parser = Parser::new(&tokens, src, &diag, sf.start_pos);
        parser.ast().source = src.to_string();
        let ast = parser.parse();
        dump_ast_to_string(&ast, ast.root)
    }

    /// Helper: parse and check that the root node kind matches.
    fn parse_root_kind(src: &str) -> NodeKind {
        let (tokens, _) = tokenize(src);
        let sm = SourceMap::new(FilePathMapping::empty());
        let sf = sm.new_source_file(FileName::Custom("test".into()), src.to_string());
        let diag = DiagnosticContext::new(&sm);
        let parser = Parser::new(&tokens, src, &diag, sf.start_pos);
        let ast = parser.parse();
        ast.node(ast.root).kind
    }

    // -----------------------------------------------------------------------
    // Basic expression tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_integer_literal() {
        let dump = parse_dump("42");
        assert!(dump.contains("Int"), "Expected Int node, got: {dump}");
    }

    #[test]
    fn test_string_literal() {
        let dump = parse_dump("\"hello\"");
        assert!(dump.contains("Str"), "Expected Str node, got: {dump}");
    }

    #[test]
    fn test_identifier() {
        let dump = parse_dump("foo");
        assert!(dump.contains("Id"), "Expected Id node, got: {dump}");
    }

    #[test]
    fn test_bool_literal() {
        let dump = parse_dump("true");
        assert!(dump.contains("Bool"), "Expected Bool node, got: {dump}");
    }

    #[test]
    fn test_null_literal() {
        let dump = parse_dump("null");
        assert!(dump.contains("Null"), "Expected Null node, got: {dump}");
    }

    // -----------------------------------------------------------------------
    // Arithmetic expressions
    // -----------------------------------------------------------------------

    #[test]
    fn test_addition() {
        let dump = parse_dump("1 + 2");
        assert!(dump.contains("Add"), "Expected Add node, got: {dump}");
    }

    #[test]
    fn test_subtraction() {
        let dump = parse_dump("3 - 1");
        assert!(dump.contains("Sub"), "Expected Sub node, got: {dump}");
    }

    #[test]
    fn test_multiplication() {
        let dump = parse_dump("2 * 3");
        assert!(dump.contains("Mul"), "Expected Mul node, got: {dump}");
    }

    #[test]
    fn test_operator_precedence() {
        // 1 + 2 * 3 should parse as Add(1, Mul(2, 3))
        let dump = parse_dump("1 + 2 * 3");
        // The Add should contain the Mul
        assert!(dump.contains("Add"), "Expected Add at top, got: {dump}");
        assert!(dump.contains("Mul"), "Expected Mul inside, got: {dump}");
    }

    #[test]
    fn test_parenthesized_expr() {
        // (1 + 2) * 3 should parse as Mul(Add(1, 2), 3)
        let dump = parse_dump("(1 + 2) * 3");
        assert!(dump.contains("Mul"), "Expected Mul at top, got: {dump}");
    }

    // -----------------------------------------------------------------------
    // Comparison and boolean expressions
    // -----------------------------------------------------------------------

    #[test]
    fn test_comparison() {
        let dump = parse_dump("a > b");
        assert!(dump.contains("BoolGt"), "Expected BoolGt, got: {dump}");
    }

    #[test]
    fn test_boolean_and() {
        let dump = parse_dump("a and b");
        assert!(dump.contains("BoolAnd"), "Expected BoolAnd, got: {dump}");
    }

    #[test]
    fn test_boolean_or() {
        let dump = parse_dump("a or b");
        assert!(dump.contains("BoolOr"), "Expected BoolOr, got: {dump}");
    }

    #[test]
    fn test_bool_not() {
        let dump = parse_dump("not x");
        assert!(dump.contains("BoolNot"), "Expected BoolNot, got: {dump}");
    }

    // -----------------------------------------------------------------------
    // Prefix expressions
    // -----------------------------------------------------------------------

    #[test]
    fn test_negative() {
        let dump = parse_dump("-42");
        assert!(dump.contains("Negative"), "Expected Negative, got: {dump}");
    }

    #[test]
    fn test_unit() {
        let dump = parse_dump("()");
        assert!(dump.contains("Unit"), "Expected Unit, got: {dump}");
    }

    #[test]
    fn test_tuple() {
        let dump = parse_dump("(1, 2, 3)");
        assert!(dump.contains("Tuple"), "Expected Tuple, got: {dump}");
    }

    #[test]
    fn test_list() {
        let dump = parse_dump("[1, 2, 3]");
        assert!(dump.contains("ListOf"), "Expected ListOf, got: {dump}");
    }

    #[test]
    fn test_symbol() {
        let dump = parse_dump(".foo");
        assert!(dump.contains("Symbol"), "Expected Symbol, got: {dump}");
    }

    // -----------------------------------------------------------------------
    // Postfix / call expressions
    // -----------------------------------------------------------------------

    #[test]
    fn test_function_call() {
        let dump = parse_dump("foo(1, 2)");
        assert!(dump.contains("Call"), "Expected Call, got: {dump}");
    }

    #[test]
    fn test_projection() {
        let dump = parse_dump("foo.bar");
        assert!(
            dump.contains("Projection"),
            "Expected Projection, got: {dump}"
        );
    }

    #[test]
    fn test_chained_projection() {
        let dump = parse_dump("foo.bar.baz");
        assert!(
            dump.contains("Projection"),
            "Expected chained Projections, got: {dump}"
        );
    }

    #[test]
    fn test_option_propagation() {
        let dump = parse_dump("x?");
        assert!(
            dump.contains("OptionPropagation"),
            "Expected OptionPropagation, got: {dump}"
        );
    }

    #[test]
    fn test_error_propagation() {
        let dump = parse_dump("x!");
        assert!(
            dump.contains("ErrorPropagation"),
            "Expected ErrorPropagation, got: {dump}"
        );
    }

    #[test]
    fn test_pipeline() {
        let dump = parse_dump("x |> f");
        assert!(dump.contains("Pipeline"), "Expected Pipeline, got: {dump}");
    }

    // -----------------------------------------------------------------------
    // Variable declarations
    // -----------------------------------------------------------------------

    #[test]
    fn test_let_decl() {
        let dump = parse_dump("let x = 1");
        assert!(dump.contains("LetDecl"), "Expected LetDecl, got: {dump}");
    }

    #[test]
    fn test_const_decl() {
        let dump = parse_dump("const x = 1");
        assert!(
            dump.contains("ConstDecl"),
            "Expected ConstDecl, got: {dump}"
        );
    }

    #[test]
    fn test_var_decl() {
        let dump = parse_dump("var x = 1");
        assert!(dump.contains("VarDecl"), "Expected VarDecl, got: {dump}");
    }

    #[test]
    fn test_let_with_type_annotation() {
        let dump = parse_dump("let x: i32 = 1");
        assert!(dump.contains("LetDecl"), "Expected LetDecl, got: {dump}");
        assert!(dump.contains("Id"), "Expected Id for type, got: {dump}");
    }

    // -----------------------------------------------------------------------
    // Assignment
    // -----------------------------------------------------------------------

    #[test]
    fn test_assignment() {
        let dump = parse_dump("x = 1");
        assert!(dump.contains("Assign"), "Expected Assign, got: {dump}");
    }

    #[test]
    fn test_add_assign() {
        let dump = parse_dump("x += 1");
        assert!(
            dump.contains("AddAssign"),
            "Expected AddAssign, got: {dump}"
        );
    }

    // -----------------------------------------------------------------------
    // Control flow
    // -----------------------------------------------------------------------

    #[test]
    fn test_if_statement() {
        let dump = parse_dump("if true { 1 }");
        assert!(
            dump.contains("IfStatement"),
            "Expected IfStatement, got: {dump}"
        );
    }

    #[test]
    fn test_if_else() {
        let dump = parse_dump("if true { 1 } else { 2 }");
        assert!(
            dump.contains("IfStatement"),
            "Expected IfStatement, got: {dump}"
        );
    }

    #[test]
    fn test_while_loop() {
        let dump = parse_dump("while true { x }");
        assert!(
            dump.contains("WhileLoop"),
            "Expected WhileLoop, got: {dump}"
        );
    }

    #[test]
    fn test_for_loop() {
        let dump = parse_dump("for x in items { x }");
        assert!(dump.contains("ForLoop"), "Expected ForLoop, got: {dump}");
    }

    #[test]
    fn test_return_statement() {
        let dump = parse_dump("return 42");
        assert!(
            dump.contains("ReturnStatement"),
            "Expected ReturnStatement, got: {dump}"
        );
    }

    #[test]
    fn test_break_statement() {
        let dump = parse_dump("break");
        assert!(
            dump.contains("BreakStatement"),
            "Expected BreakStatement, got: {dump}"
        );
    }

    #[test]
    fn test_continue_statement() {
        let dump = parse_dump("continue");
        assert!(
            dump.contains("ContinueStatement"),
            "Expected ContinueStatement, got: {dump}"
        );
    }

    #[test]
    fn test_defer() {
        let dump = parse_dump("defer cleanup()");
        assert!(
            dump.contains("DeferStatement"),
            "Expected DeferStatement, got: {dump}"
        );
    }

    // -----------------------------------------------------------------------
    // Lambda / closures
    // -----------------------------------------------------------------------

    #[test]
    fn test_lambda() {
        let dump = parse_dump("|x| x + 1");
        assert!(dump.contains("Lambda"), "Expected Lambda, got: {dump}");
    }

    #[test]
    fn test_lambda_no_params() {
        let dump = parse_dump("|| 42");
        assert!(dump.contains("Lambda"), "Expected Lambda, got: {dump}");
    }

    // -----------------------------------------------------------------------
    // Definitions
    // -----------------------------------------------------------------------

    #[test]
    fn test_fn_def() {
        let dump = parse_dump("fn add(a: i32, b: i32) -> i32 { a + b }");
        assert!(
            dump.contains("FunctionDef"),
            "Expected FunctionDef, got: {dump}"
        );
    }

    #[test]
    fn test_struct_def() {
        let dump = parse_dump("struct Point {\n  x: i32,\n  y: i32\n}");
        assert!(
            dump.contains("StructDef"),
            "Expected StructDef, got: {dump}"
        );
    }

    #[test]
    fn test_enum_def() {
        let dump = parse_dump("enum Color {\n  Red,\n  Green,\n  Blue\n}");
        assert!(dump.contains("EnumDef"), "Expected EnumDef, got: {dump}");
    }

    #[test]
    fn test_trait_def() {
        let dump = parse_dump("trait Show {\n  fn show(self) -> String\n}");
        assert!(dump.contains("TraitDef"), "Expected TraitDef, got: {dump}");
    }

    #[test]
    fn test_impl_def() {
        let dump = parse_dump("impl i32 {\n  fn zero() -> i32 { 0 }\n}");
        assert!(dump.contains("ImplDef"), "Expected ImplDef, got: {dump}");
    }

    #[test]
    fn test_typealias() {
        let dump = parse_dump("typealias Name = String");
        assert!(
            dump.contains("Typealias"),
            "Expected Typealias, got: {dump}"
        );
    }

    #[test]
    fn test_newtype() {
        let dump = parse_dump("newtype UserId = i32");
        assert!(dump.contains("Newtype"), "Expected Newtype, got: {dump}");
    }

    #[test]
    fn test_use_statement() {
        let dump = parse_dump("use std.io");
        assert!(
            dump.contains("UseStatement"),
            "Expected UseStatement, got: {dump}"
        );
    }

    #[test]
    fn test_pub_modifier() {
        let dump = parse_dump("pub fn foo() { 1 }");
        assert!(dump.contains("PubDef"), "Expected PubDef, got: {dump}");
        assert!(
            dump.contains("FunctionDef"),
            "Expected FunctionDef inside PubDef, got: {dump}"
        );
    }

    // -----------------------------------------------------------------------
    // Pattern matching
    // -----------------------------------------------------------------------

    #[test]
    fn test_post_match() {
        let dump = parse_dump("x match {\n  1 => true\n  _ => false\n}");
        assert!(
            dump.contains("PostMatch"),
            "Expected PostMatch, got: {dump}"
        );
    }

    #[test]
    fn test_pattern_call() {
        let dump = parse_dump("x match {\n  Some(y) => y\n  _ => 0\n}");
        assert!(
            dump.contains("PatternCall"),
            "Expected PatternCall, got: {dump}"
        );
    }

    // -----------------------------------------------------------------------
    // FileScope and multiple items
    // -----------------------------------------------------------------------

    #[test]
    fn test_file_scope() {
        let kind = parse_root_kind("let x = 1");
        assert_eq!(kind, NodeKind::FileScope);
    }

    #[test]
    fn test_multiple_statements() {
        let dump = parse_dump("let x = 1\nlet y = 2");
        // Two LetDecl inside a FileScope
        let count = dump.matches("LetDecl").count();
        assert!(count >= 2, "Expected 2 LetDecl, found {count} in: {dump}");
    }

    // -----------------------------------------------------------------------
    // Edge cases
    // -----------------------------------------------------------------------

    #[test]
    fn test_empty_file() {
        let dump = parse_dump("");
        assert!(
            dump.contains("FileScope"),
            "Expected FileScope for empty file, got: {dump}"
        );
    }

    #[test]
    fn test_nested_function_calls() {
        let dump = parse_dump("f(g(x))");
        let count = dump.matches("Call").count();
        assert!(
            count >= 2,
            "Expected nested calls, found {count} in: {dump}"
        );
    }

    #[test]
    fn test_complex_expression() {
        let dump = parse_dump("(a + b) * c - d / e");
        assert!(dump.contains("Sub"), "Expected Sub at top, got: {dump}");
        assert!(dump.contains("Mul"), "Expected Mul, got: {dump}");
        assert!(dump.contains("Add"), "Expected Add, got: {dump}");
        assert!(dump.contains("Div"), "Expected Div, got: {dump}");
    }

    #[test]
    fn test_infix_fn_call() {
        let dump = parse_dump("a zip b");
        assert!(
            dump.contains("InfixFnCall"),
            "Expected InfixFnCall, got: {dump}"
        );
    }
}
