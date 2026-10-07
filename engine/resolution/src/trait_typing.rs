//! Trait obligations run after implementations have complete method metadata.

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{TypeIndex, TypeKind};

use crate::ScopeId;
use crate::resolver::Resolver;

pub(crate) struct TraitObligation {
    actual: TypeIndex,
    expected: TypeIndex,
    node: NodeIndex,
    scope: ScopeId,
}

fn contains_trait(r: &Resolver<'_>, ty: TypeIndex, remaining: usize) -> bool {
    if remaining == 0 {
        return false;
    }
    let Some(ty) = r.type_pool.canonical_type(ty) else {
        return false;
    };
    match &r.type_pool.get(ty).kind {
        TypeKind::Trait { .. } => true,
        TypeKind::Tuple { elements } => elements
            .iter()
            .any(|&ty| contains_trait(r, ty, remaining - 1)),
        TypeKind::IterationStepTemplate { item: inner } | TypeKind::Optional { inner } => {
            contains_trait(r, *inner, remaining - 1)
        }
        TypeKind::ErrorQualified { inner, errors } => {
            contains_trait(r, *inner, remaining - 1)
                || errors
                    .iter()
                    .any(|&error| contains_trait(r, error, remaining - 1))
        }
        _ => false,
    }
}

pub(crate) fn is_subtype(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    actual: TypeIndex,
    expected: TypeIndex,
) -> bool {
    let actual = crate::default_methods::checked_self_value_type(r, ast, node, actual);
    if crate::associated_types::contains_dynamic_view(r, expected, 0) {
        r.diag_ctx.error("dynamic trait views with associated types are not supported; use a concrete implementation".into())
            .with_primary_span(ast.node(node).span).emit(r.diag_ctx);
        return false;
    }
    if contains_trait(r, expected, 256) {
        let Some(&scope) = r.node_scopes.get(&node) else {
            let diagnostic = r
                .diag_ctx
                .error("trait requirement is missing its lexical scope".into());
            if node.is_null() {
                diagnostic.emit(r.diag_ctx);
            } else {
                diagnostic
                    .with_primary_span(ast.node(node).span)
                    .emit(r.diag_ctx);
            }
            return false;
        };
        r.trait_obligations.push(TraitObligation {
            actual,
            expected,
            node,
            scope,
        });
        true
    } else {
        r.type_pool.is_subtype(actual, expected)
    }
}

pub(crate) fn validate(r: &Resolver<'_>, ast: &Ast) {
    for index in 1..ast.nodes.len() {
        let node = NodeIndex(index as u32);
        let traits = match ast.node(node).kind {
            NodeKind::BoolEq | NodeKind::BoolNotEq => {
                [r.type_pool.well_known.eq, r.type_pool.well_known.partial_eq]
            }
            NodeKind::BoolLt | NodeKind::BoolLtEq | NodeKind::BoolGt | NodeKind::BoolGtEq => [
                r.type_pool.well_known.ord,
                r.type_pool.well_known.partial_ord,
            ],
            _ => continue,
        };
        let lhs = ast.fixed_children(node)[0];
        let Some(&ty) = r.node_types.get(&lhs) else {
            continue;
        };
        let Some(scope) = r.node_scopes.get(&node).map(|scope| scope.0) else {
            r.diag_ctx
                .error("comparison query is missing its lexical scope".into())
                .with_primary_span(ast.node(node).span)
                .emit(r.diag_ctx);
            continue;
        };
        for trait_type in traits {
            match r.type_pool.find_trait_impl_scoped(ty, trait_type, scope) {
                Ok(Some(record)) => {
                    let name = if traits[0] == r.type_pool.well_known.eq {
                        "eq"
                    } else if trait_type == r.type_pool.well_known.partial_ord {
                        "partial_cmp"
                    } else {
                        "cmp"
                    };
                    if let Some(method) = record
                        .methods
                        .iter()
                        .find(|method| method.name == str_interner::intern(name))
                    {
                        match r.type_pool.method_accessible(method, scope) {
                            Ok(true) => {}
                            result => {
                                let message = match result {
                                    Ok(false) => format!(
                                        "comparison method `{name}` is not visible from this scope"
                                    ),
                                    Err(error) => {
                                        format!("invalid comparison method access: {error}")
                                    }
                                    Ok(true) => continue,
                                };
                                r.diag_ctx
                                    .error(message)
                                    .with_primary_span(ast.node(node).span)
                                    .emit(r.diag_ctx);
                            }
                        }
                    }
                    break;
                }
                Ok(None) => {}
                Err(error) => {
                    r.diag_ctx
                        .error(format!(
                            "ambiguous comparison trait implementation: {error}"
                        ))
                        .with_primary_span(ast.node(node).span)
                        .emit(r.diag_ctx);
                    break;
                }
            }
        }
    }
    for obligation in &r.trait_obligations {
        let result = r.type_pool.is_subtype_scoped(
            obligation.actual,
            obligation.expected,
            obligation.scope.0,
        );
        let message = match result {
            Ok(true) => continue,
            Ok(false)
                if r.type_pool
                    .is_gradually_consistent(obligation.actual, obligation.expected) =>
            {
                continue;
            }
            Ok(false) => format!(
                "type mismatch: `{}` does not implement expected trait type `{}` in this scope",
                r.type_pool
                    .display_name(obligation.actual)
                    .unwrap_or_else(|| "<invalid>".into()),
                r.type_pool
                    .display_name(obligation.expected)
                    .unwrap_or_else(|| "<invalid>".into()),
            ),
            Err(error) => format!("ambiguous or invalid trait requirement: {error}"),
        };
        let diagnostic = r.diag_ctx.error(message);
        if obligation.node.is_null() {
            diagnostic.emit(r.diag_ctx);
        } else {
            diagnostic
                .with_primary_span(ast.node(obligation.node).span)
                .emit(r.diag_ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};

    fn resolve(source: &str) -> (crate::ResolvedAst, Vec<String>) {
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("scoped-trait.ns".into()), source.into());
        let diagnostics = DiagnosticContext::new(&map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve(ast, &diagnostics);
        let errors = diagnostics
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect();
        (resolved, errors)
    }

    #[test]
    fn independent_empty_implementations_keep_exact_scope_vtables() {
        let (resolved, errors) = resolve(
            "struct P{};trait Empty{};mod a{extend Empty for P{}};mod b{extend Empty for P{}};fn main(){42}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let records = resolved.type_pool.trait_impls_snapshot();
        assert_eq!(records.len(), 2);
        assert!(
            records
                .iter()
                .all(|record| record.methods.is_empty() && record.visible_scope.is_some())
        );
        assert_ne!(records[0].visible_scope, records[1].visible_scope);
        assert!(
            !resolved
                .type_pool
                .has_trait_impl(records[0].implementor, records[0].trait_type)
        );
        let tables = resolved.type_pool.vtables_snapshot();
        assert_eq!(tables.len(), 2);
        for record in records {
            let table = tables
                .iter()
                .find(|table| table.visible_scope == record.visible_scope)
                .unwrap();
            assert!(table.entries.is_empty());
            assert_eq!(table.trait_type, record.trait_type);
            assert_eq!(table.implementor, record.implementor);
        }
        resolved.type_pool.validate().unwrap();
    }

    #[test]
    fn obligations_check_parameters_returns_and_optional_lifts_after_registration() {
        let (_, errors) = resolve(
            "struct P{};trait Empty{};mod a{extend Empty for P{};fn take(x:?Empty)->Empty{P{}};pub fn answer(){let p:Empty=take(P{});42}};fn main(){a.answer()}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        for source in [
            "struct P{};trait Empty{};mod a{extend Empty for P{}};fn f(x:?Empty){42};fn main(){f(P{})}",
            "struct P{};trait Empty{};mod a{extend Empty for P{}};fn f()->Empty{P{}};fn main(){42}",
            "struct P{};trait Empty{};mod a{extend Empty for P{}};fn main(){let p:Empty=P{};42}",
        ] {
            let (_, errors) = resolve(source);
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains("does not implement")),
                "{source}: {errors:?}"
            );
        }
    }
}
