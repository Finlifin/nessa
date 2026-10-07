use ast::{Ast, NodeKind};
use diagnostic::DiagnosticContext;
use lexer::tokenize;
use rustc_span::source_map::FilePathMapping;
use rustc_span::{FileName, SourceMap};

use crate::{ParseError, Parser};

fn parse(source: &str) -> (Ast, Vec<ParseError>) {
    let (tokens, errors) = tokenize(source);
    assert!(errors.is_empty(), "{errors:?}");
    let sources = SourceMap::new(FilePathMapping::empty());
    let file = sources.new_source_file(FileName::Custom("assoc-test".into()), source.to_owned());
    let diagnostics = DiagnosticContext::new(&sources);
    let mut parser = Parser::new(&tokens, source, &diagnostics, file.start_pos);
    if let Ok(root) = crate::basic::try_file_scope(&mut parser) {
        parser.ast().root = root;
    }
    parser.finish()
}

#[test]
fn documented_associated_declarations_preserve_name_type_and_value() {
    for container in ["trait Iter", "impl Iter for Range", "extend Iter for Range"] {
        let source = format!("{container} {{ assoc Item: Type = i32; assoc LIMIT: i64 = 42 }}");
        let (ast, errors) = parse(&source);
        assert!(errors.is_empty(), "{source}: {errors:?}");
        let definition = ast.multi_children(ast.root)[0];
        let declarations = ast.multi_children(definition);
        assert_eq!(declarations.len(), 2);
        for (node, name, ty, value_kind) in [
            (declarations[0], "Item", "Type", NodeKind::Id),
            (declarations[1], "LIMIT", "i64", NodeKind::Int),
        ] {
            assert_eq!(ast.node(node).kind, NodeKind::AssocDecl);
            let inner = ast.fixed_children(node)[0];
            assert_eq!(ast.node(inner).kind, NodeKind::AssocBinding);
            let children = ast.fixed_children(inner);
            assert_eq!(str_interner::get(ast.node(children[0]).str_id), name);
            assert_eq!(str_interner::get(ast.node(children[1]).str_id), ty);
            assert_eq!(ast.node(children[2]).kind, value_kind);
            assert_eq!(children.len(), 3);
        }
        let first = ast.fixed_children(ast.fixed_children(declarations[0])[0]);
        assert_eq!(str_interner::get(ast.node(first[2]).str_id), "i32");
    }
}

#[test]
fn existing_associated_definition_wrappers_keep_their_ast() {
    let (ast, errors) =
        parse("trait Iter { assoc typealias Item = i64; assoc const LIMIT: i64 = 42 }");
    assert!(errors.is_empty(), "{errors:?}");
    let definition = ast.multi_children(ast.root)[0];
    for (&node, expected) in ast
        .multi_children(definition)
        .iter()
        .zip([NodeKind::Typealias, NodeKind::ConstDecl])
    {
        assert_eq!(ast.node(node).kind, NodeKind::AssocDecl);
        assert_eq!(ast.node(ast.fixed_children(node)[0]).kind, expected);
    }
}

#[test]
fn named_associated_declarations_require_both_type_and_value() {
    for declaration in [
        "assoc Item = i64",
        "assoc Item: = i64",
        "assoc Item: Type",
        "assoc Item: Type =",
        "assoc : Type = i64",
    ] {
        let source = format!("trait Iter {{ {declaration} }}");
        let (_, errors) = parse(&source);
        assert!(!errors.is_empty(), "accepted {source}");
    }
}
