//! Import paths keep scope prefixes and selection structure in the AST.

use ast::NodeKind;
use lexer::token::TokenKind;

use crate::basic::{self, Rule, try_multi_with_bracket};
use crate::error::{ParseErrorKind, ParseResult};
use crate::parser::{NULL, Parser};

pub(crate) fn try_use_path(p: &mut Parser) -> ParseResult {
    let _guard = p.enter();
    let prefix = if p.eat_token(TokenKind::Dot) {
        Some(NodeKind::SuperPath)
    } else if p.eat_token(TokenKind::At) {
        Some(NodeKind::PackagePath)
    } else {
        None
    };
    let mut path = if p.eat_token(TokenKind::KwSelfLower) {
        let span = p.current_span();
        p.ast()
            .builder(NodeKind::Id, span)
            .set_str_id(str_interner::intern("self"))
            .build()
    } else {
        basic::try_id(p)?
    };
    if path.is_null() {
        if prefix.is_some() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected identifier after use path prefix",
            );
        }
        return Ok(NULL);
    }
    let mut terminal_projection = false;
    while p.eat_token(TokenKind::Dot) {
        if p.eat_token(TokenKind::Star) {
            p.mark_import_glob_end();
            let span = p.current_span();
            path = p
                .ast()
                .builder(NodeKind::PathProjectionAll, span)
                .add_child(path)
                .build();
            terminal_projection = true;
            break;
        }
        if p.peek(&[TokenKind::LBrace]) {
            let items = try_multi_with_bracket(
                p,
                &[Rule::comma("use path", try_use_path)],
                TokenKind::LBrace,
                TokenKind::RBrace,
            )?;
            let span = p.current_span();
            path = p
                .ast()
                .builder(NodeKind::PathProjectionMulti, span)
                .add_child(path)
                .add_multi_children(&items)
                .build();
            terminal_projection = true;
            break;
        }
        let member = basic::try_id(p)?;
        if member.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected identifier, wildcard or selection after `.` in use path",
            );
            break;
        }
        let span = p.current_span();
        path = p
            .ast()
            .builder(NodeKind::PathProjection, span)
            .add_child(path)
            .add_child(member)
            .build();
    }
    if let Some(prefix) = prefix {
        let span = p.current_span();
        path = p.ast().builder(prefix, span).add_child(path).build();
    }
    if terminal_projection && p.peek(&[TokenKind::Dot]) {
        let _ = p.err(
            ParseErrorKind::InvalidSyntax,
            p.next_token_span(),
            "Wildcard and selection imports must end their path",
        );
    }
    if p.eat_token(TokenKind::KwAs) {
        if terminal_projection {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.current_span(),
                "Wildcard and selection imports cannot be renamed",
            );
        }
        let alias = basic::try_id(p)?;
        if alias.is_null() {
            let _ = p.err(
                ParseErrorKind::InvalidSyntax,
                p.next_token_span(),
                "Expected alias name after `as`",
            );
        }
        let span = p.current_span();
        path = p
            .ast()
            .builder(NodeKind::PathAsBind, span)
            .add_child(path)
            .add_child(alias)
            .build();
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use ast::{Ast, NodeIndex, NodeKind};
    use diagnostic::{Diagnostic, DiagnosticContext};
    use rustc_span::{
        FileName,
        source_map::{FilePathMapping, SourceMap},
    };

    fn parse(source: &str) -> (Ast, Vec<Diagnostic>) {
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("use.ns".into()), source.to_owned());
        let diagnostics = DiagnosticContext::new(&map);
        let ast = crate::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        (ast, diagnostics.diagnostics().to_vec())
    }

    fn name(ast: &Ast, node: NodeIndex) -> String {
        assert_eq!(ast.node(node).kind, NodeKind::Id);
        str_interner::get(ast.node(node).str_id).to_string()
    }

    #[test]
    fn scoped_paths_and_tail_aliases_preserve_the_complete_target() {
        for (source, prefix) in [
            ("use .net.Client as HttpClient", NodeKind::SuperPath),
            ("use @net.Client as HttpClient", NodeKind::PackagePath),
        ] {
            let (ast, diagnostics) = parse(source);
            assert!(diagnostics.is_empty(), "{diagnostics:?}");
            let statement = ast.multi_children(ast.root)[0];
            let alias = ast.fixed_children(statement)[0];
            assert_eq!(ast.node(alias).kind, NodeKind::PathAsBind);
            let alias_children = ast.fixed_children(alias);
            assert_eq!(name(&ast, alias_children[1]), "HttpClient");
            let scoped = alias_children[0];
            assert_eq!(ast.node(scoped).kind, prefix);
            let projection = ast.fixed_children(scoped)[0];
            assert_eq!(ast.node(projection).kind, NodeKind::PathProjection);
            let children = ast.fixed_children(projection);
            assert_eq!(name(&ast, children[0]), "net");
            assert_eq!(name(&ast, children[1]), "Client");
        }
    }

    #[test]
    fn nested_selection_paths_and_globs_preserve_each_binding() {
        let (ast, diagnostics) =
            parse("pub use net.{http.Client as Client, io.{read, write as put}, errors.*}");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let public = ast.multi_children(ast.root)[0];
        assert_eq!(ast.node(public).kind, NodeKind::PubDef);
        let statement = ast.fixed_children(public)[0];
        assert_eq!(ast.node(statement).kind, NodeKind::UseStatement);
        let selection = ast.fixed_children(statement)[0];
        assert_eq!(ast.node(selection).kind, NodeKind::PathProjectionMulti);
        assert_eq!(name(&ast, ast.fixed_children(selection)[0]), "net");
        let items = ast.multi_children(selection);
        assert_eq!(items.len(), 3);
        assert_eq!(ast.node(items[0]).kind, NodeKind::PathAsBind);
        let target = ast.fixed_children(items[0])[0];
        assert_eq!(ast.node(target).kind, NodeKind::PathProjection);
        assert_eq!(name(&ast, ast.fixed_children(target)[1]), "Client");
        assert_eq!(ast.node(items[1]).kind, NodeKind::PathProjectionMulti);
        let nested = ast.multi_children(items[1]);
        assert_eq!(name(&ast, nested[0]), "read");
        assert_eq!(ast.node(nested[1]).kind, NodeKind::PathAsBind);
        assert_eq!(name(&ast, ast.fixed_children(nested[1])[1]), "put");
        assert_eq!(ast.node(items[2]).kind, NodeKind::PathProjectionAll);
        assert_eq!(name(&ast, ast.fixed_children(items[2])[0]), "errors");
    }

    #[test]
    fn glob_imports_end_at_newlines_without_changing_multiplication_continuation() {
        let (ast, diagnostics) = parse(
            "use api.*\nmod api {}\npub use api.*\npub use @other.*\nuse .sibling.*\nfn main() {}\n",
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let statements = ast.multi_children(ast.root);
        assert_eq!(statements.len(), 6);
        assert_eq!(ast.node(statements[0]).kind, NodeKind::UseStatement);
        assert_eq!(ast.node(statements[1]).kind, NodeKind::ModuleDef);
        for &statement in &statements[2..4] {
            assert_eq!(ast.node(statement).kind, NodeKind::PubDef);
            let statement = ast.fixed_children(statement)[0];
            assert_eq!(ast.node(statement).kind, NodeKind::UseStatement);
        }
        assert_eq!(ast.node(statements[4]).kind, NodeKind::UseStatement);
        assert_eq!(ast.node(statements[5]).kind, NodeKind::FunctionDef);
        let (ast, diagnostics) = parse("let x = 2 *\n3\nlet y = 4");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let declarations = ast.multi_children(ast.root);
        assert_eq!(declarations.len(), 2);
        let value = ast.fixed_children(declarations[0])[2];
        assert_eq!(ast.node(value).kind, NodeKind::Mul);
    }

    #[test]
    fn malformed_scoped_paths_aliases_and_nonterminal_projections_are_rejected() {
        for source in [
            "use .",
            "use @",
            "use net.",
            "use net as",
            "use net.* as renamed",
            "use net.{Client} as renamed",
            "use net.*.Client",
            "use net.{io.}",
        ] {
            let (_, diagnostics) = parse(source);
            assert!(
                diagnostics
                    .iter()
                    .any(|diag| diag.level == diagnostic::Level::Error),
                "accepted {source}"
            );
        }
    }
}
