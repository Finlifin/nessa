//! Raw lexical payloads and normalized diagnostic coordinates are separate contracts.
use std::collections::BTreeMap;

use ast::{Ast, NodeKind};
use diagnostic::{Diagnostic, DiagnosticContext, Level, NessaError};
use rustc_span::source_map::FilePathMapping;
use rustc_span::{BytePos, FileName, SourceMap, Span};
use type_pool::{IdentityPathSegment, PackageTypeContext, TypeId, TypeIndex, TypeKind, TypePool};

fn map() -> SourceMap {
    SourceMap::new(FilePathMapping::empty())
}
fn parse(source: &str, map: &SourceMap, base: BytePos) -> (Ast, Vec<Diagnostic>) {
    let (tokens, errors) = lexer::tokenize(source);
    assert!(errors.is_empty(), "{errors:?}");
    let context = DiagnosticContext::new(map);
    let ast = parser::Parser::new(&tokens, source, &context, base).parse();
    let diagnostics = context.diagnostics().to_vec();
    (ast, diagnostics)
}
fn primary(d: &Diagnostic) -> Span {
    d.primary_span
        .expect("source diagnostic has a primary span")
}
fn location(map: &SourceMap, span: Span, line: usize, column: usize, snippet: &str) {
    let position = map.lookup_char_pos(span.lo());
    assert_eq!((position.line, position.col.0), (line, column));
    assert_eq!(map.span_to_snippet(span).unwrap(), snippet);
}
fn undefined(diagnostics: &[Diagnostic], name: &str) -> Diagnostic {
    let found: Vec<_> = diagnostics
        .iter()
        .filter(|d| {
            d.level == Level::Error
                && d.message.contains("undefined name")
                && d.message.contains(name)
        })
        .collect();
    assert_eq!(found.len(), 1, "{diagnostics:?}");
    found[0].clone()
}
fn in_owner(span: Span, start: BytePos, end: BytePos) {
    if !span.is_dummy() {
        assert!(
            span.lo() >= start && span.hi() <= end && span.lo() <= span.hi(),
            "{span:?} outside {start:?}..{end:?}"
        );
    }
}
fn successful(source: &str) -> driver::CompileResult {
    let result = driver::Driver::new().compile(source);
    assert!(!result.has_errors, "{:?}", result.diagnostics);
    result.type_pool.validate().unwrap();
    result
}
fn named_id(pool: &TypePool, name: &str) -> TypeId {
    let found: Vec<_> = (0..pool.len()).map(|i| TypeIndex::from_raw(i as u32)).filter(|&i| matches!(pool.get(i).kind,TypeKind::Struct{name:n,..}if str_interner::get(n)==name)).collect();
    assert_eq!(found.len(), 1);
    let id = pool.get(found[0]).type_id;
    assert_ne!(id, TypeId::ZERO);
    id
}
fn standard_ids(pool: &TypePool) -> BTreeMap<String, TypeId> {
    let mut ids = BTreeMap::from([
        (
            "bootstrap.Display".into(),
            pool.get(pool.well_known.display).type_id,
        ),
        ("bootstrap.Eq".into(), pool.get(pool.well_known.eq).type_id),
    ]);
    let mut modules = 0;
    let mut nominals = 0;
    for d in &pool.identity_input().unwrap().declarations {
        if matches!(d.path.first(),Some(IdentityPathSegment::Named(n))if n=="std") {
            if matches!(pool.get(d.type_index).kind, TypeKind::Module { .. }) {
                modules += 1;
            } else {
                nominals += 1;
            }
            assert!(
                ids.insert(format!("{:?}", d.path), pool.get(d.type_index).type_id)
                    .is_none()
            );
        }
    }
    assert!(modules > 0 && nominals > 0);
    assert_ne!(ids["bootstrap.Display"], TypeId::ZERO);
    ids
}

#[test]
fn lf_and_crlf_undefined_name_have_identical_exact_primary_and_label_coverage() {
    let mut spans = Vec::new();
    for newline in ["\n", "\r\n"] {
        let source = format!("fn main(){{{newline}    missing_name{newline}}}");
        let map = map();
        let file = map.new_source_file(FileName::Custom("input".into()), source.clone());
        let result = driver::Driver::new().compile(&source);
        assert!(result.has_errors && result.codegen_output.functions.is_empty());
        let diagnostic = undefined(&result.diagnostics, "missing_name");
        location(&map, primary(&diagnostic), 2, 4, "missing_name");
        assert!(!diagnostic.labels.is_empty());
        for label in &diagnostic.labels {
            location(&map, label.span, 2, 4, "missing_name");
        }
        spans.push((
            primary(&diagnostic).lo().0 - file.start_pos.0,
            primary(&diagnostic).hi().0 - file.start_pos.0,
        ));
        assert!(result.into_artifact().is_err());
    }
    assert_eq!(spans[0], spans[1]);
}

#[test]
fn unicode_lex_error_is_mapped_once_after_two_crlf_lines_at_nonzero_base() {
    let source = "-- 文\r\n-- é\r\n    ©";
    let map = map();
    map.new_source_file(FileName::Custom("prefix".into()), "prior\r\nfile".into());
    let file = map.new_source_file(FileName::Custom("owner".into()), source.into());
    let neighbor = map.new_source_file(FileName::Custom("neighbor".into()), "WRONG".into());
    assert!(file.start_pos.0 > 0 && neighbor.start_pos > file.end_pos);
    let (_, errors) = lexer::tokenize(source);
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].from as usize, source.find('©').unwrap());
    let context = DiagnosticContext::new(&map);
    errors[0].emit(&context, file.start_pos);
    let diagnostics = context.diagnostics();
    assert_eq!(diagnostics.len(), 1);
    location(&map, primary(&diagnostics[0]), 3, 4, "©");
    for label in &diagnostics[0].labels {
        location(&map, label.span, 3, 4, "©");
    }
    assert_eq!(
        map.lookup_char_pos(primary(&diagnostics[0]).lo()).file.name,
        file.name
    );
}

#[test]
fn unicode_parser_error_and_labels_stay_on_the_correct_token_and_file() {
    let source = "fn main(){\r\n    let 文=\"é\";\r\n    40 + )\r\n}";
    let map = map();
    map.new_source_file(FileName::Custom("prefix".into()), "before".into());
    let file = map.new_source_file(FileName::Custom("syntax".into()), source.into());
    map.new_source_file(FileName::Custom("neighbor".into()), "not this file".into());
    let (ast, diagnostics) = parse(source, &map, file.start_pos);
    assert_eq!(ast.source.as_deref(), Some(source));
    assert!(diagnostics.iter().any(|d| d.level == Level::Error));
    let on_token: Vec<_> = diagnostics
        .iter()
        .filter(|d| {
            d.primary_span
                .is_some_and(|s| map.span_to_snippet(s).ok().as_deref() == Some(")"))
        })
        .collect();
    assert!(!on_token.is_empty(), "{diagnostics:?}");
    for diagnostic in on_token {
        location(&map, primary(diagnostic), 3, 9, ")");
        for label in &diagnostic.labels {
            location(&map, label.span, 3, 9, ")");
        }
    }
    for diagnostic in &diagnostics {
        in_owner(primary(diagnostic), file.start_pos, file.end_pos);
        for label in &diagnostic.labels {
            in_owner(label.span, file.start_pos, file.end_pos);
        }
    }
}

#[test]
fn raw_resolution_uses_owning_normalized_unicode_span_with_adjacent_files() {
    let source = "fn main(){\r\n    let 文:i64=40;\r\n    未定义\r\n}";
    let map = map();
    map.new_source_file(FileName::Custom("prefix".into()), "prefix bytes".into());
    let file = map.new_source_file(FileName::Custom("owner".into()), source.into());
    map.new_source_file(FileName::Custom("next".into()), "unrelated".into());
    let (ast, syntax) = parse(source, &map, file.start_pos);
    assert!(syntax.is_empty());
    let context = DiagnosticContext::new(&map);
    let resolved = resolution::resolve(ast, &context);
    let mut diagnostics = context.diagnostics().to_vec();
    diagnostics.extend(resolved.diagnostics);
    let diagnostic = undefined(&diagnostics, "未定义");
    location(&map, primary(&diagnostic), 3, 4, "未定义");
    for label in &diagnostic.labels {
        location(&map, label.span, 3, 4, "未定义");
    }
    assert_eq!(
        map.lookup_char_pos(primary(&diagnostic).lo()).file.name,
        file.name
    );
}

#[test]
fn lowering_capture_diagnostic_covers_the_actual_unicode_capture_without_second_mapping() {
    let source =
        "fn main(){\r\n    let 文:i64=40;\r\n    fn inner()->i64{文+2};\r\n    inner()\r\n}";
    let map = map();
    map.new_source_file(FileName::Custom("user".into()), source.into());
    let result = driver::Driver::new().compile(source);
    assert!(result.has_errors);
    let errors: Vec<_> = result
        .diagnostics
        .iter()
        .filter(|d| d.level == Level::Error && d.message.contains("cannot capture"))
        .collect();
    assert_eq!(errors.len(), 1, "{:?}", result.diagnostics);
    location(&map, primary(errors[0]), 3, 20, "文");
    assert!(result.codegen_output.functions.is_empty());
    assert!(result.into_artifact().is_err());
}

#[test]
fn eof_empty_and_comment_only_inputs_never_claim_the_adjacent_source() {
    for source in ["", "-- 文\r\n", "fn main(){\r\n"] {
        let map = map();
        map.new_source_file(FileName::Custom("prefix".into()), "prefix".into());
        let file = map.new_source_file(FileName::Custom("empty-or-eof".into()), source.into());
        map.new_source_file(FileName::Custom("next".into()), "FOREIGN".into());
        let (ast, diagnostics) = parse(source, &map, file.start_pos);
        assert_eq!(ast.source.as_deref(), Some(source));
        for node in ast.nodes.iter().skip(1) {
            in_owner(node.span, file.start_pos, file.end_pos);
        }
        for diagnostic in diagnostics {
            in_owner(primary(&diagnostic), file.start_pos, file.end_pos);
            for label in diagnostic.labels {
                in_owner(label.span, file.start_pos, file.end_pos);
            }
        }
        let dump = ast::dump_ast_to_string(&ast, ast.root);
        assert!(!dump.contains("FOREIGN"));
    }
}

#[test]
fn stripped_bom_remains_a_lex_error_with_zero_coverage_not_the_first_token() {
    for prefix in [None, Some("earlier source")] {
        let source = "\u{feff}fn main(){42}";
        let map = map();
        if let Some(prefix) = prefix {
            map.new_source_file(FileName::Custom("prefix".into()), prefix.into());
        }
        let file = map.new_source_file(FileName::Custom("bom".into()), source.into());
        let (tokens, errors) = lexer::tokenize(source);
        assert_eq!(errors.len(), 1);
        assert_eq!((errors[0].from, errors[0].to), (0, 3));
        assert!(tokens.iter().any(|t| t.text(source) == "fn"));
        let context = DiagnosticContext::new(&map);
        errors[0].emit(&context, file.start_pos);
        let diagnostics = context.diagnostics();
        assert_eq!(diagnostics.len(), 1);
        let span = primary(&diagnostics[0]);
        assert_eq!((span.lo(), span.hi()), (file.start_pos, file.start_pos));
        for label in &diagnostics[0].labels {
            assert_eq!(
                (label.span.lo(), label.span.hi()),
                (file.start_pos, file.start_pos)
            );
        }
        assert!(driver::Driver::new().compile(source).has_errors);
    }
}

#[test]
fn lexer_literal_and_macro_payloads_are_raw_including_crlf_and_unicode() {
    let source = "\"α\rβ\"\r\n'{ a\r\n文 }";
    let (tokens, errors) = lexer::tokenize(source);
    assert!(errors.is_empty(), "{errors:?}");
    let strings: Vec<_> = tokens
        .iter()
        .filter(|t| t.kind == lexer::TokenKind::String)
        .map(|t| t.text(source))
        .collect();
    let macros: Vec<_> = tokens
        .iter()
        .filter(|t| t.kind == lexer::TokenKind::MacroContent)
        .map(|t| t.text(source))
        .collect();
    assert_eq!(strings, ["\"α\rβ\""]);
    assert_eq!(macros, ["'{ a\r\n文 }"]);
}

#[test]
fn parser_preserves_raw_source_and_interned_literal_while_node_spans_are_normalized() {
    let source = "-- é\r\nfn main(){\r\n    let text=\"α\rβ\";\r\n    42\r\n}";
    let map = map();
    map.new_source_file(FileName::Custom("prefix".into()), "before".into());
    let file = map.new_source_file(FileName::Custom("raw".into()), source.into());
    let (ast, diagnostics) = parse(source, &map, file.start_pos);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert_eq!(ast.source.as_deref(), Some(source));
    let literals: Vec<_> = ast
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Str)
        .collect();
    assert_eq!(literals.len(), 1);
    assert_eq!(str_interner::get(literals[0].str_id), "\"α\rβ\"");
    assert_eq!(map.span_to_snippet(literals[0].span).unwrap(), "\"α\rβ\"");
    let integers: Vec<_> = ast
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Int)
        .collect();
    assert_eq!(integers.len(), 1);
    location(&map, integers[0].span, 4, 4, "42");
    for node in ast.nodes.iter().skip(1) {
        in_owner(node.span, file.start_pos, file.end_pos);
    }
}

#[test]
fn ast_dump_reconstructs_original_literal_bytes_from_normalized_nonzero_spans() {
    let source = "-- é\r\nfn main(){\r\n    \"α\rβ\"\r\n}";
    let map = map();
    map.new_source_file(FileName::Custom("prefix".into()), "longer prefix".into());
    let file = map.new_source_file(FileName::Custom("own".into()), source.into());
    let (ast, diagnostics) = parse(source, &map, file.start_pos);
    assert!(diagnostics.is_empty());
    let dump = ast::dump_ast_to_string(&ast, ast.root);
    assert!(dump.contains("\"α\rβ\""), "{dump}");
    assert!(dump.contains("main"), "{dump}");
    assert_eq!(ast.source.as_deref(), Some(source));
}

#[test]
fn appended_neighbor_ast_cannot_read_own_raw_bytes_even_when_absolute_offsets_fit() {
    let own = format!("{}fn first(){{42}}", "-- padding\r\n".repeat(128));
    let other = "fn other(){987}";
    let map = map();
    map.new_source_file(FileName::Custom("prefix".into()), "before".into());
    // Native SourceMap reserves raw length + 1 between files. Put the short
    // foreign file first so its absolute coordinates genuinely fit within the
    // later primary AST's long raw text without belonging to that source.
    let neighbor = map.new_source_file(FileName::Custom("neighbor".into()), other.into());
    let file = map.new_source_file(FileName::Custom("own".into()), own.clone());
    let (mut ast, errors) = parse(&own, &map, file.start_pos);
    assert!(errors.is_empty());
    let own_root = ast.root;
    let own_before = ast::dump_ast_to_string(&ast, own_root);
    let (foreign, errors) = parse(other, &map, neighbor.start_pos);
    assert!(errors.is_empty());
    let foreign_leaf = foreign
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Int)
        .unwrap();
    assert!(
        (foreign_leaf.span.hi().0 as usize) < own.len(),
        "fixture must expose actual raw-offset overlap"
    );
    let relocated = ast.append_ast(foreign);
    let dump = ast::dump_ast_to_string(&ast, relocated);
    assert!(dump.contains("FunctionDef"), "{dump}");
    assert!(dump.contains("(Id \"other\")"), "{dump}");
    assert!(dump.contains("(Int \"987\")"), "{dump}");
    assert!(
        !dump.contains("padding") && !dump.contains("42"),
        "appended nodes must read their own source, never primary source: {dump}"
    );
    assert_eq!(ast.root, own_root);
    let own_dump = ast::dump_ast_to_string(&ast, ast.root);
    assert_eq!(own_dump, own_before);
    assert!(own_dump.contains("(Int \"42\")") && own_dump.contains("(Id \"first\")"));
    assert_eq!(ast.source.as_deref(), Some(own.as_str()));
}

#[test]
fn unregistered_context_retains_raw_fallback_without_panicking_or_remapping() {
    let source = "-- é\r\n    ©";
    let map = map();
    let context = DiagnosticContext::new(&map);
    let (_, errors) = lexer::tokenize(source);
    assert_eq!(errors.len(), 1);
    let base = BytePos(100);
    errors[0].emit(&context, base);
    let diagnostics = context.diagnostics();
    assert_eq!(
        (
            primary(&diagnostics[0]).lo().0,
            primary(&diagnostics[0]).hi().0
        ),
        (100 + errors[0].from, 100 + errors[0].to)
    );
    drop(diagnostics);
    let valid = "fn main(){\r\n42\r\n}";
    let (ast, errors) = parse(valid, &map, base);
    assert!(errors.is_empty());
    let literal = ast.nodes.iter().find(|n| n.kind == NodeKind::Int).unwrap();
    assert_eq!(literal.span.lo().0, 100 + valid.find("42").unwrap() as u32);
    assert_eq!(ast.source.as_deref(), Some(valid));
}

#[test]
fn manual_ast_dump_keeps_documented_raw_and_missing_source_fallbacks() {
    for source in [None, Some("")] {
        let mut ast = Ast::new();
        ast.source = source.map(str::to_owned);
        let leaf = ast
            .builder(NodeKind::Int, Span::new(BytePos(1), BytePos(3)))
            .build();
        assert_eq!(ast::dump_ast_to_string(&ast, leaf), "(Int)");
    }
    let mut ast = Ast::new().with_source("xé".into());
    let leaf = ast
        .builder(NodeKind::Id, Span::new(BytePos(1), BytePos(3)))
        .build();
    assert_eq!(ast::dump_ast_to_string(&ast, leaf), "(Id \"é\")");
    let dummy = ast.builder(NodeKind::Int, rustc_span::DUMMY_SP).build();
    assert_eq!(ast::dump_ast_to_string(&ast, dummy), "(Int)");
}

#[test]
fn scratch_identity_retains_literal_and_macro_significant_bytes_but_ignores_line_separators() {
    let identity = |source| {
        resolution::scratch_package_identity(source)
            .unwrap()
            .identity
    };
    assert_eq!(
        identity("struct P{n:i64}\nfn main(){42}\n"),
        identity("struct P{n:i64}\r\nfn main(){42}\r\n")
    );
    assert_ne!(identity("\"ab\""), identity("\"a\rb\""));
    assert_ne!(identity("'{ a\nb }"), identity("'{ a\r\nb }"));
    let a = successful("struct Payload{n:String};fn main(){Payload{n:\"ab\"}.n}");
    let b = successful("struct Payload{n:String};fn main(){Payload{n:\"a\rb\"}.n}");
    assert_ne!(
        named_id(&a.type_pool, "Payload"),
        named_id(&b.type_pool, "Payload")
    );
    assert_eq!(standard_ids(&a.type_pool), standard_ids(&b.type_pool));
}

#[test]
fn explicit_package_and_all_standard_original_identities_ignore_newline_coordinate_changes() {
    let context = || driver::CompilationIdentityContext {
        package: PackageTypeContext {
            identity_schema: 1,
            identity: [0x29; 16],
            qualified_name: "example.org/span-tests".into(),
            version: "1.2.3".into(),
        },
        last_stable_version: None,
        type_versions: vec![],
    };
    let source = "struct Payload{n:i64}\nfn main(){42}\n";
    let a = driver::Driver::new().compile_with_identity(source, context());
    let b = driver::Driver::new().compile_with_identity(&source.replace('\n', "\r\n"), context());
    assert!(
        !a.has_errors && !b.has_errors,
        "{:?} {:?}",
        a.diagnostics,
        b.diagnostics
    );
    assert_eq!(
        named_id(&a.type_pool, "Payload"),
        named_id(&b.type_pool, "Payload")
    );
    assert_eq!(standard_ids(&a.type_pool), standard_ids(&b.type_pool));
    let scratch = successful(source);
    let crlf = successful(&source.replace('\n', "\r\n"));
    assert_eq!(
        named_id(&scratch.type_pool, "Payload"),
        named_id(&crlf.type_pool, "Payload")
    );
    assert_eq!(standard_ids(&scratch.type_pool), standard_ids(&a.type_pool));
}

#[test]
fn reused_driver_keeps_each_submission_distinct_and_diagnostic_token_width_exact() {
    let driver = driver::Driver::new();
    let mut previous = BytePos(0);
    for name in ["first_missing", "second_missing", "第三个缺失"] {
        let source = format!("fn main(){{\r\n    {name}\r\n}}");
        let result = driver.compile(&source);
        let d = undefined(&result.diagnostics, name);
        let span = primary(&d);
        assert!(span.lo() > previous);
        assert_eq!(span.hi().0 - span.lo().0, name.len() as u32);
        previous = span.hi();
        assert!(result.has_errors && result.codegen_output.functions.is_empty());
    }
}

#[test]
fn actual_lf_and_crlf_inside_ordinary_strings_remain_lexical_errors_without_artifacts() {
    for newline in ["\n", "\r\n"] {
        let source = format!("fn main(){{\"a{newline}b\"}}");
        let (_, errors) = lexer::tokenize(&source);
        assert!(
            errors
                .iter()
                .any(|e| e.kind == lexer::LexErrorKind::InvalidStrLiteral),
            "{errors:?}"
        );
        let result = driver::Driver::new().compile(&source);
        assert!(result.has_errors);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.level == Level::Error && d.message.contains("newline in string"))
        );
        assert!(result.codegen_output.functions.is_empty());
        assert!(result.into_artifact().is_err());
    }
}
