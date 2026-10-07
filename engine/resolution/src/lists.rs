//! Non-generic List values contain Any elements; indexing uses apply/update.

use ast::{Ast, NodeIndex};
use type_pool::{Intrinsic, TypeIndex};

use crate::resolver::Resolver;
use crate::typing;

pub(crate) fn is_list(r: &Resolver<'_>, ty: TypeIndex) -> bool {
    r.type_pool
        .list_type()
        .is_some_and(|list| r.type_pool.canonical_type(ty) == Some(list))
}

pub(crate) fn literal(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) -> Option<TypeIndex> {
    for &element in ast.multi_children(node) {
        typing::resolve_types_expected(r, ast, element, Some(Intrinsic::Any.type_index()));
    }
    let ty = r.type_pool.list_type();
    if ty.is_none() {
        r.diag_ctx
            .error("List construction requires List type metadata".into())
            .with_primary_span(ast.node(node).span)
            .emit(r.diag_ctx);
    }
    ty
}

#[cfg(test)]
mod tests {
    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};

    use super::*;
    use ast::NodeKind;
    use type_pool::TypeKind;

    #[test]
    fn builtin_list_views_use_the_current_pool_role_after_index_relocation() {
        let map = SourceMap::new(FilePathMapping::empty());
        let diagnostics = DiagnosticContext::new(&map);
        let mut resolver =
            Resolver::new(&diagnostics, crate::ResolveOptions::for_builtin_package());
        // This relocation fixture tests legacy collection descriptors, without source-only binders.
        resolver.type_pool = type_pool::TypePool::with_intrinsics();
        let original = resolver.type_pool.list_type().unwrap();
        let mut snapshot = resolver.type_pool.snapshot();
        let offset = original.as_u32() as usize;
        snapshot.types.insert(
            offset,
            type_pool::TypeInfo {
                kind: TypeKind::Struct {
                    name: str_interner::intern("InsertedBeforeCollectionRoles"),
                    fields: Vec::new(),
                },
                type_id: type_pool::TypeId::ZERO,
                size: 0,
                align: 8,
            },
        );
        snapshot.methods.insert(offset, Vec::new());
        resolver.type_pool = type_pool::TypePool::restore(snapshot).unwrap();
        let expected = resolver.type_pool.list_type().unwrap();
        assert_eq!(expected.as_u32(), original.as_u32() + 1);
        let symbol = resolver
            .resolve_builtin_view(NodeIndex::NULL, "List")
            .unwrap();
        assert_eq!(resolver.symbols[symbol.0 as usize].type_index, expected);
    }

    fn resolve(source: &str) -> (crate::ResolvedAst, Vec<String>) {
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("list.ns".into()), source.to_owned());
        let diagnostics = DiagnosticContext::new(&map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve_with_options(
            ast,
            &diagnostics,
            crate::ResolveOptions::for_builtin_package(),
        );
        let errors = diagnostics
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect();
        (resolved, errors)
    }

    #[test]
    fn empty_heterogeneous_nested_lists_have_one_canonical_type() {
        let (resolved, errors) = resolve(
            "typealias List = .List'builtin\ntypealias Items = List\nfn main() { let empty: Items = []; let nested: List = [empty, [1, true, null], \"text\"]; nested }",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let list = resolved.type_pool.list_type().unwrap();
        let literals: Vec<_> = resolved
            .ast
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                (node.kind == NodeKind::ListOf).then_some(NodeIndex(index as u32))
            })
            .collect();
        assert_eq!(literals.len(), 3);
        assert!(
            literals
                .iter()
                .all(|literal| resolved.node_types[literal] == list)
        );
    }

    #[test]
    fn index_calls_require_one_usize_and_return_any_including_assignment() {
        let (resolved, errors) = resolve("fn main() { let xs = [1]; xs(0) = true; xs(0) }");
        assert!(errors.is_empty(), "{errors:?}");
        for (index, node) in resolved.ast.nodes.iter().enumerate() {
            if node.kind == NodeKind::Call {
                assert_eq!(
                    resolved.node_types[&NodeIndex(index as u32)],
                    Intrinsic::Any.type_index()
                );
                let argument = resolved.ast.multi_children(NodeIndex(index as u32))[0];
                assert_eq!(
                    resolved.node_types[&argument],
                    Intrinsic::Usize.type_index()
                );
            }
        }
        for source in [
            "fn main() { [1](true) }",
            "fn main() { [1](-1) }",
            "fn main() { [1]() }",
            "fn main() { [1](0, 1) }",
        ] {
            let (_, errors) = resolve(source);
            assert!(!errors.is_empty(), "{source}");
        }
    }

    #[test]
    fn known_method_calls_keep_receiver_abi_and_check_explicit_arguments() {
        let prefix = "typealias List = .List'builtin\nimpl List { pub fn len(self) -> i64 { 0 }; pub fn get(self, index: usize) -> Any { null } }\n";
        let (resolved, errors) = resolve(&format!(
            "{prefix}fn main() {{ let xs = [1]; xs.get(0); xs.len() }}"
        ));
        assert!(errors.is_empty(), "{errors:?}");
        for (index, node) in resolved.ast.nodes.iter().enumerate() {
            if node.kind == NodeKind::Projection {
                let projection = NodeIndex(index as u32);
                assert!(!resolved.node_symbols.contains_key(&projection));
                let signature = resolved.node_types[&projection];
                let TypeKind::Function { params, .. } = &resolved.type_pool.get(signature).kind
                else {
                    panic!("method signature");
                };
                assert!(params.len() <= 1, "receiver must be omitted");
            }
        }
        for (tail, message) in [
            ("fn main() { [1].get(true) }", "type mismatch"),
            ("fn main() { [1].get() }", "expects"),
            (
                "fn main() { let f = [1].len; f() }",
                "bound List method values",
            ),
            ("fn main() { [1].capacity }", "unknown List member"),
            ("fn main() { List {} }", "collection storage"),
        ] {
            let (_, errors) = resolve(&format!("{prefix}{tail}"));
            assert!(
                errors.iter().any(|error| error.contains(message)),
                "{tail}: {errors:?}"
            );
        }
    }

    #[test]
    fn collection_derivation_rejects_aliases_without_synthesizing_traits() {
        for target in ["List", "Items"] {
            let (resolved, errors) = resolve(&format!(
                "typealias List = .List'builtin\ntypealias Items = List\ntrait Eq {{ fn eq(self, rhs: Self) -> bool }}\nderive Eq for {target}\nfn main() {{ [] }}"
            ));
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains("collection traits cannot be derived")),
                "{errors:?}"
            );
            let list = resolved.type_pool.list_type().unwrap();
            assert!(
                resolved
                    .type_pool
                    .snapshot()
                    .trait_impls
                    .iter()
                    .all(|implementation| resolved
                        .type_pool
                        .canonical_type(implementation.implementor)
                        != Some(list))
            );
        }
    }

    #[test]
    fn single_variadic_calls_record_empty_or_source_ordered_list_packaging() {
        for (arguments, expected) in [("", vec![]), ("1, true, [2]", vec![0, 1, 2])] {
            let (resolved, errors) = resolve(&format!(
                "typealias List = .List'builtin\nfn values(...xs: List) {{ xs }}\nfn main() {{ values({arguments}) }}"
            ));
            assert!(errors.is_empty(), "{errors:?}");
            let plan = resolved.call_arguments.values().next().unwrap();
            assert_eq!(plan.parameters.len(), 1);
            assert_eq!(
                plan.parameters[0].value,
                crate::CallArgumentValue::Variadic {
                    source_indices: expected
                }
            );
        }
        let (_, errors) = resolve(
            "typealias List = .List'builtin\nfn f(a: i64, ...xs: List, .scale: i64 = 2) { xs }\nfn main() { f(1, true, 3, scale = 4) }",
        );
        assert!(errors.is_empty(), "{errors:?}");
        for source in [
            "typealias List = .List'builtin\nfn f(...xs: List, b: i64) {}\nfn main() { f(1) }",
            "typealias List = .List'builtin\nfn f(...xs: List) {}\nfn main() { f(xs = []) }",
            "fn f(...xs: Any) {}",
        ] {
            let (_, errors) = resolve(source);
            assert!(!errors.is_empty(), "{source}");
        }
    }
}
