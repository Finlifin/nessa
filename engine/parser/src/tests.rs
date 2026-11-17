#[cfg(test)]
mod syntax {
    use ast::NodeKind;
    use ast::dump::dump_ast_to_string;
    use diagnostic::DiagnosticContext;
    use lexer::tokenize;
    use rustc_span::source_map::FilePathMapping;
    use rustc_span::{FileName, SourceMap};

    use crate::parser::Parser;

    #[test]
    fn parse_and_finish_retain_original_source_bytes_without_source_map_entries() {
        for source in ["", "let value=42\r\n", "let text=\"你好\"\r\n"] {
            let (tokens, errors) = tokenize(source);
            assert!(errors.is_empty(), "{errors:?}");
            let map = SourceMap::new(FilePathMapping::empty());
            let diagnostics = DiagnosticContext::new(&map);
            let parsed = Parser::new(&tokens, source, &diagnostics, rustc_span::BytePos(0)).parse();
            assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
            assert_eq!(parsed.source.as_deref(), Some(source));
            let (partial, errors) =
                Parser::new(&tokens, source, &diagnostics, rustc_span::BytePos(0)).finish();
            assert!(errors.is_empty());
            assert_eq!(partial.source.as_deref(), Some(source));
        }
    }

    /// Helper: parse source code and return the AST S-expression dump.
    fn parse_dump(src: &str) -> String {
        let (tokens, lex_errors) = tokenize(src);
        assert!(lex_errors.is_empty(), "Lexer errors: {lex_errors:?}");

        let sm = SourceMap::new(FilePathMapping::empty());
        let sf = sm.new_source_file(FileName::Custom("test".into()), src.to_string());
        let diag = DiagnosticContext::new(&sm);
        let ast = Parser::new(&tokens, src, &diag, sf.start_pos).parse();
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
    fn named_call_arguments_are_keys_with_values_not_assignments() {
        for source in ["f(value = 42)", "f(.value = 42)"] {
            let dump = parse_dump(source);
            assert!(dump.contains("NamedArg"), "{dump}");
            assert!(!dump.contains("Assign"), "{dump}");
        }
    }

    #[test]
    fn named_arguments_and_optional_parameters_require_values() {
        for source in ["f(value = )", "fn f(.value: i64 = ) {}"] {
            let (tokens, errors) = tokenize(source);
            assert!(errors.is_empty());
            let map = SourceMap::new(FilePathMapping::empty());
            let file = map.new_source_file(FileName::Custom("arguments".into()), source.to_owned());
            let diagnostics = DiagnosticContext::new(&map);
            Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
            assert!(
                diagnostics
                    .diagnostics()
                    .iter()
                    .any(|diagnostic| diagnostic.level == diagnostic::Level::Error),
                "{source}"
            );
        }
    }

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
    fn match_arms_accept_commas_semicolons_and_newlines_with_expression_guards() {
        for separator in [",", ";", "\n"] {
            let source =
                format!("fn main() {{ 42 match {{ n if n > 40 => n{separator}_ => 0 }} }}");
            let (tokens, errors) = tokenize(&source);
            assert!(errors.is_empty());
            let map = SourceMap::new(FilePathMapping::empty());
            let file = map.new_source_file(FileName::Custom("match".into()), source.clone());
            let diagnostics = DiagnosticContext::new(&map);
            let ast = Parser::new(&tokens, &source, &diagnostics, file.start_pos).parse();
            assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
            let matcher = ast
                .nodes
                .iter()
                .enumerate()
                .find_map(|(index, node)| {
                    (node.kind == NodeKind::PostMatch).then_some(ast::NodeIndex(index as u32))
                })
                .unwrap();
            assert_eq!(ast.multi_children(matcher).len(), 2);
            let first = ast.multi_children(matcher)[0];
            let guard = ast.fixed_children(first)[0];
            assert_eq!(ast.node(guard).kind, NodeKind::PatternIfGuard);
            assert_eq!(
                ast.node(ast.fixed_children(guard)[1]).kind,
                NodeKind::BoolGt
            );
        }
    }

    #[test]
    fn tuple_numeric_projection_preserves_the_index_node() {
        let source = "pair.0";
        let (tokens, errors) = tokenize(source);
        assert!(errors.is_empty());
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("projection".into()), source.into());
        let diagnostics = DiagnosticContext::new(&map);
        let ast = Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        assert!(!diagnostics.has_errors());
        let projection = ast
            .nodes
            .iter()
            .enumerate()
            .find_map(|(index, node)| {
                (node.kind == NodeKind::Projection).then_some(ast::NodeIndex(index as u32))
            })
            .unwrap();
        let member = ast.fixed_children(projection)[1];
        assert_eq!(ast.node(member).kind, NodeKind::Int);
        assert_eq!(str_interner::get(ast.node(member).str_id), "0");
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
        let dump = parse_dump("enum Color {\n  red,\n  green,\n  blue\n}");
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
        let (ast, errors) = parse_checked("a zip b");
        assert!(errors.is_empty(), "{errors:?}");
        let call = ast.multi_children(ast.root)[0];
        assert_eq!(ast.node(call).kind, NodeKind::InfixFnCall);
        assert_eq!(str_interner::get(ast.node(call).str_id), "zip");
        let children = ast.fixed_children(call);
        assert_eq!(id_name(&ast, children[0]), "a");
        assert_eq!(id_name(&ast, children[1]), "b");

        let (ast_arb, errors_arb) = parse_checked("a `my-op` b");
        assert!(errors_arb.is_empty(), "{errors_arb:?}");
        let arb_call = ast_arb.multi_children(ast_arb.root)[0];
        assert_eq!(ast_arb.node(arb_call).kind, NodeKind::InfixFnCall);
        assert_eq!(str_interner::get(ast_arb.node(arb_call).str_id), "my-op");
    }

    fn parse_checked(src: &str) -> (ast::Ast, Vec<String>) {
        let (tokens, lex_errors) = tokenize(src);
        assert!(lex_errors.is_empty(), "Lexer errors: {lex_errors:?}");
        let sm = SourceMap::new(FilePathMapping::empty());
        let sf = sm.new_source_file(FileName::Custom("test.ns".into()), src.to_string());
        let diag = DiagnosticContext::new(&sm);
        let ast = Parser::new(&tokens, src, &diag, sf.start_pos).parse();
        let errs = diag
            .diagnostics()
            .iter()
            .filter(|d| d.level == diagnostic::Level::Error)
            .map(|d| d.message.clone())
            .collect();
        (ast, errs)
    }

    fn id_name(ast: &ast::Ast, idx: ast::NodeIndex) -> String {
        assert_eq!(ast.node(idx).kind, NodeKind::Id);
        str_interner::get(ast.node(idx).str_id).to_string()
    }

    #[test]
    fn arbitrary_identifiers_work_across_declarations_expressions_patterns_and_paths() {
        let src = "\
use @`my-pkg`.`my-mod` as `mod-alias`
struct `my-struct` {
    pub `in`: i64,
    private `else`: i64 = 1
}
enum `my-enum` {
    pub `match`(`val`: i64)
}
trait `my-trait` {
    assoc `my-item`: Type = i64
    fn `do-work`(self, .`opt-key`: i64 = 0, ...`rest-args`: List) -> i64
}
fn `run`(`arg-1`: i64) -> i64 {
    let `let` = `arg-1`
    let obj = { `field-a`: `let` }
    let proj = obj.`field-a`
    let vw = obj'`type-view`
    let sym = .`tag-name`
    let called = `callee`(`in` = proj, .`else` = vw)
    for : `outer-loop` `item`: i64 in items {
        continue `outer-loop` if `item` == 0
        break `outer-loop`
    }
    called match {
        { `field-a`: `bound`, `shorthand` } => `bound`,
        [...`tail`] => 0,
        _ as `fallback` => `fallback`
    }
}
";
        let (ast, errors) = parse_checked(src);
        assert!(errors.is_empty(), "{errors:?}");
        let top = ast.multi_children(ast.root);
        assert_eq!(top.len(), 5);

        // Verify unquoted interning on representative nodes:
        // 0: UseStatement -> PathAsBind -> alias = "mod-alias"
        let path_as = ast.fixed_children(top[0])[0];
        assert_eq!(ast.node(path_as).kind, NodeKind::PathAsBind);
        assert_eq!(id_name(&ast, ast.fixed_children(path_as)[1]), "mod-alias");

        // 1: StructDef "my-struct", fields "in" and "else"
        assert_eq!(ast.node(top[1]).kind, NodeKind::StructDef);
        assert_eq!(id_name(&ast, ast.fixed_children(top[1])[0]), "my-struct");
        let pub_field = ast.multi_children(top[1])[0];
        assert_eq!(ast.node(pub_field).kind, NodeKind::PubDef);
        let field_inner = ast.fixed_children(pub_field)[0];
        assert_eq!(id_name(&ast, ast.fixed_children(field_inner)[0]), "in");

        // 2: EnumDef "my-enum", variant "match"
        assert_eq!(ast.node(top[2]).kind, NodeKind::EnumDef);
        assert_eq!(id_name(&ast, ast.fixed_children(top[2])[0]), "my-enum");
        let pub_var = ast.multi_children(top[2])[0];
        assert_eq!(ast.node(pub_var).kind, NodeKind::PubDef);
        let var_inner = ast.fixed_children(pub_var)[0];
        assert_eq!(id_name(&ast, ast.fixed_children(var_inner)[0]), "match");

        // 3: TraitDef "my-trait", AssocBinding "my-item"
        assert_eq!(ast.node(top[3]).kind, NodeKind::TraitDef);
        assert_eq!(id_name(&ast, ast.fixed_children(top[3])[0]), "my-trait");
        let assoc_decl = ast.multi_children(top[3])[0];
        let assoc_bind = ast.fixed_children(assoc_decl)[0];
        assert_eq!(id_name(&ast, ast.fixed_children(assoc_bind)[0]), "my-item");

        // 4: FunctionDef "run" -> verify NamedArg keys inside body
        assert_eq!(ast.node(top[4]).kind, NodeKind::FunctionDef);
        assert_eq!(id_name(&ast, ast.fixed_children(top[4])[0]), "run");
        let fn_body = ast.fixed_children(top[4])[2];
        let stmts = ast.multi_children(fn_body);
        // stmts[5]: let called = `callee`(`in` = proj, .`else` = vw)
        let call_expr = ast.fixed_children(stmts[5])[2];
        assert_eq!(ast.node(call_expr).kind, NodeKind::Call);
        assert_eq!(id_name(&ast, ast.fixed_children(call_expr)[0]), "callee");
        let call_args = ast.multi_children(call_expr);
        assert_eq!(call_args.len(), 2);
        assert_eq!(ast.node(call_args[0]).kind, NodeKind::NamedArg);
        assert_eq!(id_name(&ast, ast.fixed_children(call_args[0])[0]), "in");
        assert_eq!(ast.node(call_args[1]).kind, NodeKind::NamedArg);
        assert_eq!(id_name(&ast, ast.fixed_children(call_args[1])[0]), "else");
    }

    #[test]
    fn var_decl_supports_optional_else_branch_with_four_fixed_children() {
        let (ast_plain, errs_plain) = parse_checked("var count: i64 = 0");
        assert!(errs_plain.is_empty(), "{errs_plain:?}");
        let plain = ast_plain.multi_children(ast_plain.root)[0];
        assert_eq!(ast_plain.node(plain).kind, NodeKind::VarDecl);
        let plain_ch = ast_plain.fixed_children(plain);
        assert_eq!(plain_ch.len(), 4);
        assert_eq!(id_name(&ast_plain, plain_ch[0]), "count");
        assert_eq!(id_name(&ast_plain, plain_ch[1]), "i64");
        assert_eq!(ast_plain.node(plain_ch[2]).kind, NodeKind::Int);
        assert!(plain_ch[3].is_null());

        let (ast_else, errs_else) =
            parse_checked("fn f() { var Some(x): ?i64 = next_val() else { return 0 } }");
        assert!(errs_else.is_empty(), "{errs_else:?}");
        let fn_node = ast_else.multi_children(ast_else.root)[0];
        let block = ast_else.fixed_children(fn_node)[2];
        let var_node = ast_else.multi_children(block)[0];
        assert_eq!(ast_else.node(var_node).kind, NodeKind::VarDecl);
        let ch = ast_else.fixed_children(var_node);
        assert_eq!(ch.len(), 4);
        assert_eq!(ast_else.node(ch[0]).kind, NodeKind::PatternCall);
        assert_eq!(ast_else.node(ch[1]).kind, NodeKind::OptionalType);
        assert_eq!(ast_else.node(ch[2]).kind, NodeKind::Call);
        assert_eq!(ast_else.node(ch[3]).kind, NodeKind::Block);

        let (_, errs_bad) = parse_checked("var x = 1 else");
        assert!(!errs_bad.is_empty(), "expected error for missing else body");
    }

    #[test]
    fn postfix_await_parses_and_chains_with_postfix_operators() {
        let (ast, errors) = parse_checked("fut.await");
        assert!(errors.is_empty(), "{errors:?}");
        let await_node = ast.multi_children(ast.root)[0];
        assert_eq!(ast.node(await_node).kind, NodeKind::Await);
        assert_eq!(id_name(&ast, ast.fixed_children(await_node)[0]), "fut");

        let (ast_chain, errs_chain) = parse_checked("http.get(url)#.await!.body");
        assert!(errs_chain.is_empty(), "{errs_chain:?}");
        let proj = ast_chain.multi_children(ast_chain.root)[0];
        assert_eq!(ast_chain.node(proj).kind, NodeKind::Projection);
        assert_eq!(
            id_name(&ast_chain, ast_chain.fixed_children(proj)[1]),
            "body"
        );
        let err_prop = ast_chain.fixed_children(proj)[0];
        assert_eq!(ast_chain.node(err_prop).kind, NodeKind::ErrorPropagation);
        let aw = ast_chain.fixed_children(err_prop)[0];
        assert_eq!(ast_chain.node(aw).kind, NodeKind::Await);
        let eff_prop = ast_chain.fixed_children(aw)[0];
        assert_eq!(ast_chain.node(eff_prop).kind, NodeKind::EffectPropagation);

        // `.await()` with parens is rejected unless calling an arbitrary id ``.`await`()``
        let (_, errs_call) = parse_checked("fut.await()");
        assert!(!errs_call.is_empty(), "expected fut.await() to be rejected");

        let (ast_arb, errs_arb) = parse_checked("fut.`await`()");
        assert!(errs_arb.is_empty(), "{errs_arb:?}");
        let call = ast_arb.multi_children(ast_arb.root)[0];
        assert_eq!(ast_arb.node(call).kind, NodeKind::Call);
        assert_eq!(
            ast_arb.node(ast_arb.fixed_children(call)[0]).kind,
            NodeKind::Projection
        );
    }

    #[test]
    fn call_spread_arguments_preserve_spread_node_and_coexist_with_symbols_and_ranges() {
        let (ast, errors) = parse_checked("f(1, ...xs, ...make(_0), .json, ..10, key = 2)");
        assert!(errors.is_empty(), "{errors:?}");
        let call = ast.multi_children(ast.root)[0];
        assert_eq!(ast.node(call).kind, NodeKind::Call);
        let args = ast.multi_children(call);
        assert_eq!(args.len(), 6);
        assert_eq!(ast.node(args[0]).kind, NodeKind::Int);
        assert_eq!(ast.node(args[1]).kind, NodeKind::SpreadArg);
        assert_eq!(id_name(&ast, ast.fixed_children(args[1])[0]), "xs");
        assert_eq!(ast.node(args[2]).kind, NodeKind::SpreadArg);
        assert_eq!(
            ast.node(ast.fixed_children(args[2])[0]).kind,
            NodeKind::Lambda
        );
        assert_eq!(ast.node(args[3]).kind, NodeKind::Symbol);
        assert_eq!(ast.node(args[4]).kind, NodeKind::RangeTo);
        assert_eq!(ast.node(args[5]).kind, NodeKind::NamedArg);

        for invalid in ["f(...)", "f(1, ...)"] {
            let (_, errs) = parse_checked(invalid);
            assert!(!errs.is_empty(), "accepted invalid spread call: {invalid}");
        }
    }

    #[test]
    fn all_three_handles_forms_parse_in_blocks_and_top_level() {
        let src = "\
fn main() {
    handles(log) let logger: Logger = Logger()
    handles(read_line) handles(print_line)
        let term = Terminal { encoding: \"utf-8\" }
    handles(pkg.log)(msg: String) -> Unit { println(msg) }
    handles(fast_log)(msg) = println(msg)
    handles log(msg, level) => println(msg)
    handles pkg.log(msg: String) -> Unit => {
        println(msg)
    }
}
";
        let (ast, errors) = parse_checked(src);
        assert!(errors.is_empty(), "{errors:?}");
        let fn_node = ast.multi_children(ast.root)[0];
        let body = ast.fixed_children(fn_node)[2];
        let stmts = ast.multi_children(body);
        assert_eq!(stmts.len(), 6);

        // 0: Form A single effect LetHandler
        assert_eq!(ast.node(stmts[0]).kind, NodeKind::LetHandler);
        let lh0 = ast.fixed_children(stmts[0]);
        assert_eq!(id_name(&ast, lh0[0]), "log");
        assert_eq!(id_name(&ast, lh0[1]), "logger");
        assert_eq!(id_name(&ast, lh0[2]), "Logger");
        assert_eq!(ast.node(lh0[3]).kind, NodeKind::Call);

        // 1: Form A chained LetHandler across newline
        assert_eq!(ast.node(stmts[1]).kind, NodeKind::LetHandler);
        let lh1 = ast.fixed_children(stmts[1]);
        assert_eq!(ast.node(lh1[0]).kind, NodeKind::ListOf);
        let eff_list = ast.multi_children(lh1[0]);
        assert_eq!(eff_list.len(), 2);
        assert_eq!(id_name(&ast, eff_list[0]), "read_line");
        assert_eq!(id_name(&ast, eff_list[1]), "print_line");
        assert_eq!(id_name(&ast, lh1[1]), "term");
        assert!(lh1[2].is_null());
        assert_eq!(ast.node(lh1[3]).kind, NodeKind::ExtendedCall);

        // 2: Form B parenthesized block
        assert_eq!(ast.node(stmts[2]).kind, NodeKind::HandlesStatement);
        let hb0 = ast.fixed_children(stmts[2]);
        assert_eq!(ast.node(hb0[0]).kind, NodeKind::Projection);
        assert_eq!(id_name(&ast, hb0[1]), "Unit");
        assert_eq!(ast.node(hb0[2]).kind, NodeKind::Block);
        assert_eq!(ast.multi_children(stmts[2]).len(), 1);

        // 3: Form B parenthesized `= expr`
        assert_eq!(ast.node(stmts[3]).kind, NodeKind::HandlesStatement);
        let hb1 = ast.fixed_children(stmts[3]);
        assert_eq!(id_name(&ast, hb1[0]), "fast_log");
        assert!(hb1[1].is_null());
        assert_eq!(ast.node(hb1[2]).kind, NodeKind::Call);
        assert_eq!(ast.multi_children(stmts[3]).len(), 1);

        // 4: Form C pattern-arrow with simple id and statement body
        assert_eq!(ast.node(stmts[4]).kind, NodeKind::HandlesStatement);
        let hc0 = ast.fixed_children(stmts[4]);
        assert_eq!(id_name(&ast, hc0[0]), "log");
        assert!(hc0[1].is_null());
        assert_eq!(ast.node(hc0[2]).kind, NodeKind::Call);
        assert_eq!(ast.multi_children(stmts[4]).len(), 2);

        // 5: Form C pattern-arrow with projection, return type, and block body
        assert_eq!(ast.node(stmts[5]).kind, NodeKind::HandlesStatement);
        let hc1 = ast.fixed_children(stmts[5]);
        assert_eq!(ast.node(hc1[0]).kind, NodeKind::Projection);
        assert_eq!(id_name(&ast, hc1[1]), "Unit");
        assert_eq!(ast.node(hc1[2]).kind, NodeKind::Block);
        assert_eq!(ast.multi_children(stmts[5]).len(), 1);
    }

    #[test]
    fn handles_forms_work_with_indented_blocks_and_reject_malformed_inputs() {
        let indented = "\
fn main():
    handles(e1)
    handles(e2)
    let h = make_h()
    handles log(msg) =>
        println(msg)
";
        let (ast, errors) = parse_checked(indented);
        assert!(errors.is_empty(), "{errors:?}");
        let fn_node = ast.multi_children(ast.root)[0];
        let body = ast.fixed_children(fn_node)[2];
        let stmts = ast.multi_children(body);
        assert_eq!(stmts.len(), 2);
        assert_eq!(ast.node(stmts[0]).kind, NodeKind::LetHandler);
        assert_eq!(ast.node(stmts[1]).kind, NodeKind::HandlesStatement);
        assert_eq!(
            ast.node(ast.fixed_children(stmts[1])[2]).kind,
            NodeKind::Block
        );

        for invalid in [
            "handles",
            "handles()",
            "handles(log)",
            "handles(log); let x = 1",
            "handles(log) let = 1",
            "handles(log) let x =",
            "handles(log)(x) =",
            "handles log => 1",
            "handles log(x)",
            "handles log(x) =>",
            "handles pkg.(x) => 1",
        ] {
            let (_, errs) = parse_checked(invalid);
            assert!(!errs.is_empty(), "accepted malformed handles: {invalid}");
        }
    }
    #[test]
    fn error_sets_nested_qualifiers_and_branch_patterns_have_distinct_ast() {
        let dump = parse_dump(
            "enum E{Bad,};fn f(v:![E] ++ [E] ![] i64){v match{ok! =>ok,error E.* as e=>1,#e=>0}}",
        );
        assert_eq!(dump.matches("ErrorQualifiedType").count(), 2, "{dump}");
        assert!(dump.contains("Concat"), "{dump}");
        assert!(dump.contains("PatternErrorOk"), "{dump}");
        assert!(dump.contains("PatternTypeFamily"), "{dump}");
        assert!(dump.contains("PatternEffect"), "{dump}");
        assert!(dump.contains("PatternError"), "{dump}");
        assert!(
            !dump.contains("ErrorPropagation"),
            "nested qualifier consumed as propagation: {dump}"
        );
    }
}
