//! Register comparison derivations before checking their recursive prerequisites.

use std::collections::HashSet;

use ast::{Ast, NodeIndex, NodeKind};
use str_interner::StrId;
use type_pool::{Intrinsic, MethodSlot, TraitImplRecord, TypeIndex, TypeKind};

use crate::SymbolKind;
use crate::resolver::Resolver;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedComparisonPlan {
    pub implementor: TypeIndex,
    pub trait_type: TypeIndex,
    pub method_name: StrId,
    pub signature: TypeIndex,
    pub node: NodeIndex,
}

fn comparison_name(r: &Resolver<'_>, trait_type: TypeIndex) -> Option<StrId> {
    let known = &r.type_pool.well_known;
    if trait_type == known.eq || trait_type == known.partial_eq {
        Some(str_interner::intern("eq"))
    } else if trait_type == known.ord {
        Some(str_interner::intern("cmp"))
    } else if trait_type == known.partial_ord {
        Some(str_interner::intern("partial_cmp"))
    } else {
        None
    }
}

fn comparison_return(r: &mut Resolver<'_>, trait_type: TypeIndex) -> Option<TypeIndex> {
    let known = &r.type_pool.well_known;
    if trait_type == known.eq || trait_type == known.partial_eq {
        Some(Intrinsic::Bool.type_index())
    } else if trait_type == known.ord {
        crate::ordering::type_index(r)
    } else if trait_type == known.partial_ord {
        crate::ordering::type_index(r).map(|inner| r.register_type(TypeKind::Optional { inner }))
    } else {
        None
    }
}

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: impl Into<String>) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}

fn denoted_type(r: &Resolver<'_>, node: NodeIndex) -> Option<TypeIndex> {
    r.node_type_values
        .get(&node)
        .copied()
        .or_else(|| {
            r.node_symbols
                .get(&node)
                .map(|symbol| r.symbols[symbol.0 as usize].type_index)
        })
        .and_then(|ty| r.type_pool.canonical_type(ty))
}

/// Typing precedes trait registration. Discover the declaration's method shape
/// without registering a slot before aggregate fields and signatures are ready.
pub(crate) fn declared_method_signature(
    r: &mut Resolver<'_>,
    ast: &Ast,
    ty: TypeIndex,
    name: StrId,
) -> Option<TypeIndex> {
    if !["eq", "cmp", "partial_cmp"]
        .into_iter()
        .any(|method| name == str_interner::intern(method))
    {
        return None;
    }
    let ty = r.type_pool.canonical_type(ty)?;
    let declared = (1..ast.nodes.len()).find_map(|index| {
        let node = NodeIndex(index as u32);
        if ast.node(node).kind != NodeKind::DeriveDef
            || denoted_type(r, ast.fixed_children(node)[0]) != Some(ty)
        {
            return None;
        }
        ast.multi_children(node).iter().find_map(|&trait_node| {
            let trait_type = denoted_type(r, trait_node)?;
            (comparison_name(r, trait_type) == Some(name)).then_some(trait_type)
        })
    });
    let trait_type = declared?;
    let ret = comparison_return(r, trait_type)?;
    Some({
        r.register_type(TypeKind::Function {
            params: vec![ty],
            ret,
        })
    })
}

pub(crate) fn register(r: &mut Resolver<'_>, ast: &Ast) {
    let mut registered = HashSet::new();
    for index in 1..ast.nodes.len() {
        let node = NodeIndex(index as u32);
        if ast.node(node).kind != NodeKind::DeriveDef {
            continue;
        }
        let Some(implementor) = denoted_type(r, ast.fixed_children(node)[0]) else {
            report(r, ast, node, "derive requires a statically known type");
            continue;
        };
        if r.type_pool.is_reserved_collection_role(implementor) {
            continue;
        }
        for &trait_node in ast.multi_children(node) {
            let Some(trait_type) = denoted_type(r, trait_node) else {
                report(
                    r,
                    ast,
                    trait_node,
                    "derive requires a statically known trait",
                );
                continue;
            };
            let comparison = comparison_name(r, trait_type).is_some();
            let aggregate = matches!(
                r.type_pool.get(implementor).kind,
                TypeKind::Struct { .. } | TypeKind::Enum { .. } | TypeKind::Tuple { .. }
            );
            if comparison && aggregate {
                if !registered.insert((implementor, trait_type)) {
                    report(r, ast, node, "duplicate comparison derivation");
                    continue;
                }
                if r.type_pool.has_trait_impl(implementor, trait_type) {
                    report(
                        r,
                        ast,
                        node,
                        "cannot derive a comparison trait already implemented for this type",
                    );
                    continue;
                }
                let Some(ret) = comparison_return(r, trait_type) else {
                    report(
                        r,
                        ast,
                        node,
                        "ordering derivation requires the trusted std.ordering.Ordering interface",
                    );
                    continue;
                };
                let method_name = comparison_name(r, trait_type).expect("comparison trait");
                let signature = r.register_type(TypeKind::Function {
                    params: vec![implementor, implementor],
                    ret,
                });
                r.derived_comparisons.push(DerivedComparisonPlan {
                    implementor,
                    trait_type,
                    method_name,
                    signature,
                    node,
                });
                let method = MethodSlot {
                    name: method_name,
                    func_id: type_pool::DERIVE_FUNC_ID,
                    access: type_pool::MethodAccess::Public,
                    trait_impl: Some(trait_type),
                    visible_scope: None,
                };
                r.type_pool.add_method(implementor, method.clone());
                r.type_pool.add_trait_impl(TraitImplRecord {
                    trait_type,
                    implementor,
                    methods: vec![method],
                    visible_scope: None,
                });
            } else {
                // Display preserves legacy Struct formatting and generates Enum/Tuple field calls.
                let display = aggregate && trait_type == r.type_pool.well_known.display;
                if !display {
                    report(
                        r,
                        ast,
                        node,
                        if trait_type == r.type_pool.well_known.hash {
                            "Hash derivation is not implemented"
                        } else {
                            "derivation of this trait for this type is not implemented"
                        },
                    );
                    continue;
                }
                if !registered.insert((implementor, trait_type))
                    || r.type_pool.has_trait_impl(implementor, trait_type)
                {
                    report(
                        r,
                        ast,
                        node,
                        "cannot derive a trait already implemented for this type",
                    );
                    continue;
                }
                crate::display_derivation::register_display(r, node, implementor);
                let methods = crate::traits::generate_derive_methods(r, implementor, trait_type);
                for method in &methods {
                    r.type_pool.add_method(implementor, method.clone());
                }
                r.type_pool.add_trait_impl(TraitImplRecord {
                    trait_type,
                    implementor,
                    methods,
                    visible_scope: None,
                });
            }
        }
    }
    // User Eq/PartialEq implementations also feed compiler-generated field calls.
    let records = r.type_pool.trait_impls_snapshot().to_vec();
    for record in records {
        if record.trait_type != r.type_pool.well_known.eq
            && record.trait_type != r.type_pool.well_known.partial_eq
        {
            continue;
        }
        if !record
            .methods
            .iter()
            .any(|method| method.name == str_interner::intern("eq"))
        {
            let node = r
                .scopes
                .iter()
                .find_map(|scope| {
                    if scope.node.is_null() || ast.node(scope.node).kind != NodeKind::ImplTraitDef {
                        return None;
                    }
                    let children = ast.fixed_children(scope.node);
                    (denoted_type(r, children[0]) == Some(record.trait_type)
                        && denoted_type(r, children[1]) == Some(record.implementor))
                    .then_some(scope.node)
                })
                .unwrap_or(ast.root);
            report(
                r,
                ast,
                node,
                "Eq/PartialEq implementations require eq(self, other: Self) -> bool",
            );
        }
        for method in record.methods.iter().filter(|method| {
            method.name == str_interner::intern("eq") && method.func_id != type_pool::DERIVE_FUNC_ID
        }) {
            if !checked_user_comparison(
                r,
                ast,
                record.implementor,
                method.func_id,
                record.trait_type,
            ) {
                let node = r
                    .symbols
                    .get(method.func_id as usize)
                    .map(|symbol| symbol.def_node)
                    .filter(|node| !node.is_null())
                    .unwrap_or(ast.root);
                report(
                    r,
                    ast,
                    node,
                    "Eq/PartialEq implementations require eq(self, other: Self) -> bool",
                );
            }
        }
    }
    for plan in r.derived_comparisons.clone() {
        let parent = if plan.trait_type == r.type_pool.well_known.ord {
            Some(r.type_pool.well_known.eq)
        } else if plan.trait_type == r.type_pool.well_known.partial_ord {
            Some(r.type_pool.well_known.partial_eq)
        } else {
            None
        };
        if parent.is_some_and(|parent| !r.type_pool.has_trait_impl(plan.implementor, parent)) {
            report(
                r,
                ast,
                plan.node,
                "ordering derivation requires its global equality parent implementation",
            );
        }
        let mut visited = HashSet::new();
        let fields = components(r, plan.implementor);
        let Some(scope) = r.node_scopes.get(&plan.node).map(|scope| scope.0) else {
            report(
                r,
                ast,
                plan.node,
                "comparison derivation is missing its lexical scope",
            );
            continue;
        };
        for field in fields {
            if !comparable(r, ast, field, plan.trait_type, scope, &mut visited) {
                let name = r
                    .type_pool
                    .display_name(field)
                    .unwrap_or_else(|| "<invalid>".into());
                report(
                    r,
                    ast,
                    plan.node,
                    format!(
                        "cannot derive comparison: field type `{name}` has no checked comparison implementation"
                    ),
                );
            }
        }
    }
}

fn components(r: &Resolver<'_>, ty: TypeIndex) -> Vec<TypeIndex> {
    match &r.type_pool.get(ty).kind {
        TypeKind::Struct { fields, .. } => fields.iter().map(|field| field.ty).collect(),
        TypeKind::Enum { variants, .. } => variants
            .iter()
            .flat_map(|variant| variant.fields.iter().map(|field| field.ty))
            .collect(),
        TypeKind::Tuple { elements } => elements.clone(),
        _ => Vec::new(),
    }
}

fn checked_user_comparison(
    r: &Resolver<'_>,
    ast: &Ast,
    ty: TypeIndex,
    function: u32,
    trait_type: TypeIndex,
) -> bool {
    let Some(symbol) = r.symbols.get(function as usize) else {
        return false;
    };
    if symbol.kind != SymbolKind::Function
        || symbol.def_node.is_null()
        || Some(symbol.name) != comparison_name(r, trait_type)
    {
        return false;
    }
    if ast
        .multi_children(symbol.def_node)
        .first()
        .is_none_or(|&parameter| ast.node(parameter).kind != NodeKind::ParamSelf)
    {
        return false;
    }
    let Some(signature) = r.type_pool.canonical_type(symbol.type_index) else {
        return false;
    };
    let TypeKind::Function { params, ret } = &r.type_pool.get(signature).kind else {
        return false;
    };
    params.len() == 2
        && params
            .iter()
            .all(|&parameter| r.type_pool.canonical_type(parameter) == Some(ty))
        && if trait_type == r.type_pool.well_known.eq
            || trait_type == r.type_pool.well_known.partial_eq
        {
            r.type_pool.as_intrinsic(*ret) == Some(Intrinsic::Bool)
        } else if let Some(ordering) = crate::ordering::type_index(r) {
            let ret = r.type_pool.canonical_type(*ret);
            if trait_type == r.type_pool.well_known.ord {
                ret == Some(ordering)
            } else {
                ret.is_some_and(|ret| matches!(r.type_pool.get(ret).kind, TypeKind::Optional { inner } if r.type_pool.canonical_type(inner) == Some(ordering)))
            }
        } else {
            false
        }
}

fn comparable(
    r: &Resolver<'_>,
    ast: &Ast,
    ty: TypeIndex,
    trait_type: TypeIndex,
    scope: u32,
    visited: &mut HashSet<(TypeIndex, TypeIndex)>,
) -> bool {
    let Some(ty) = r.type_pool.canonical_type(ty) else {
        return false;
    };
    if r.type_pool.is_reserved_collection_role(ty) {
        return false;
    }
    if !visited.insert((ty, trait_type)) {
        return true;
    }
    let result = match &r.type_pool.get(ty).kind {
        TypeKind::Intrinsic(_)
            if trait_type == r.type_pool.well_known.ord
                || trait_type == r.type_pool.well_known.partial_ord =>
        {
            checked_trait_comparison(r, ast, ty, trait_type, scope, visited).unwrap_or(false)
        }
        TypeKind::Intrinsic(kind) => {
            kind.is_numeric()
                || matches!(
                    kind,
                    Intrinsic::Bool
                        | Intrinsic::Char
                        | Intrinsic::Str
                        | Intrinsic::Unit
                        | Intrinsic::Type
                )
        }
        TypeKind::Optional { inner } => comparable(r, ast, *inner, trait_type, scope, visited),
        TypeKind::Tuple { elements } => {
            checked_trait_comparison(r, ast, ty, trait_type, scope, visited).unwrap_or_else(|| {
                elements
                    .iter()
                    .all(|&element| comparable(r, ast, element, trait_type, scope, visited))
            })
        }
        TypeKind::Struct { .. } | TypeKind::Enum { .. } => {
            checked_trait_comparison(r, ast, ty, trait_type, scope, visited).unwrap_or(false)
        }
        _ => false,
    };
    visited.remove(&(ty, trait_type));
    result
}

/// A present trait slot takes precedence over compositional tuple equality.
fn checked_trait_comparison(
    r: &Resolver<'_>,
    ast: &Ast,
    ty: TypeIndex,
    trait_type: TypeIndex,
    scope: u32,
    visited: &mut HashSet<(TypeIndex, TypeIndex)>,
) -> Option<bool> {
    let record = match r.type_pool.find_trait_impl_scoped(ty, trait_type, scope) {
        Ok(Some(record)) => record,
        Ok(None)
            if trait_type == r.type_pool.well_known.partial_eq
                || trait_type == r.type_pool.well_known.partial_ord =>
        {
            let fallback = if trait_type == r.type_pool.well_known.partial_eq {
                r.type_pool.well_known.eq
            } else {
                r.type_pool.well_known.ord
            };
            match r.type_pool.find_trait_impl_scoped(ty, fallback, scope) {
                Ok(Some(record)) => record,
                Ok(None) => return None,
                Err(_) => return Some(false),
            }
        }
        Ok(None) => return None,
        Err(_) => return Some(false),
    };
    // Generated comparison methods are globally registered. A local field
    // implementation cannot become global evidence through derivation.
    if record.visible_scope.is_some() {
        return Some(false);
    }
    Some(record.methods.iter().any(|method| {
        Some(method.name) == comparison_name(r, record.trait_type)
            && method.visible_scope.is_none()
            && if method.func_id == type_pool::DERIVE_FUNC_ID {
                r.derived_comparisons
                    .iter()
                    .any(|plan| plan.implementor == ty && plan.trait_type == record.trait_type)
                    && components(r, ty)
                        .into_iter()
                        .all(|field| comparable(r, ast, field, record.trait_type, scope, visited))
            } else {
                checked_user_comparison(r, ast, ty, method.func_id, record.trait_type)
            }
    }))
}

#[cfg(test)]
mod tests {
    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};

    use super::*;

    fn resolve(source: &str) -> (crate::ResolvedAst, Vec<String>) {
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("derive.ns".into()), source.into());
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
    fn recursive_derivations_and_structural_components_are_predeclared() {
        let (resolved, errors) = resolve(
            "enum Tree { leaf(value:i64), node(child:Tree), }\nstruct Box { value:Tree, extra:(bool, ?String) }\nderive Eq for Box\nderive Eq for Tree\ntypealias Pair = (i64, bool)\nderive PartialEq for Pair",
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(resolved.derived_comparisons.len(), 3);
        for plan in &resolved.derived_comparisons {
            let TypeKind::Function { params, ret } = &resolved.type_pool.get(plan.signature).kind
            else {
                panic!("signature");
            };
            assert_eq!(params, &[plan.implementor, plan.implementor]);
            assert_eq!(*ret, Intrinsic::Bool.type_index());
            assert_eq!(resolved.ast.node(plan.node).kind, NodeKind::DeriveDef);
        }
    }

    #[test]
    fn checked_user_eq_satisfies_fields_and_rejects_wrong_signatures() {
        let (resolved, errors) = resolve(
            "struct Inner { x:i64 }\nimpl Eq for Inner { fn eq(self, other:Self)->bool { true } }\nstruct Outer { x:Inner }\nderive Eq for Outer",
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(resolved.derived_comparisons.len(), 1);
        for signature in ["other:Any)->bool { true }", "other:Self)->i64 { 42 }"] {
            let (_, errors) = resolve(&format!(
                "struct Inner {{ x:i64 }}\nimpl Eq for Inner {{ fn eq(self, {signature} }}\nstruct Outer {{ x:Inner }}\nderive Eq for Outer"
            ));
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains("Eq/PartialEq implementations require")),
                "{errors:?}"
            );
        }
    }

    #[test]
    fn unsupported_fields_hash_and_unknown_traits_are_explicit_errors() {
        for field in ["Any", "fn(i64)->i64", "Missing"] {
            let (_, errors) = resolve(&format!(
                "struct Missing {{ x:i64 }}\nstruct Bad {{ value:{field} }}\nderive Eq for Bad"
            ));
            assert!(
                errors.iter().any(|error| error.contains("field type")),
                "{field}: {errors:?}"
            );
        }
        for source in [
            "struct X { x:i64 }\nderive Hash for X",
            "enum E { a, }\nderive Ord for E",
            "trait Unknown {}\nstruct X { x:i64 }\nderive Unknown for X",
        ] {
            let (resolved, errors) = resolve(source);
            assert!(!errors.is_empty(), "{source}");
            assert!(resolved.type_pool.trait_impls_snapshot().is_empty());
        }
    }

    #[test]
    fn trait_specific_implementations_and_forward_aliases_are_independent() {
        let (resolved, errors) = resolve(
            "impl Eq for Alias { fn eq(self, other:Self)->bool { true } }\nimpl PartialEq for Alias { fn eq(self, other:Self)->bool { false } }\ntypealias Alias = Inner\nstruct Inner { x:i64 }\nstruct Outer { x:Inner }\nderive Eq, PartialEq for Outer",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let owner = resolved.derived_comparisons[0].implementor;
        let TypeKind::Struct { fields, .. } = &resolved.type_pool.get(owner).kind else {
            panic!("struct")
        };
        let inner = resolved.type_pool.canonical_type(fields[0].ty).unwrap();
        let name = str_interner::intern("eq");
        let eq = resolved
            .type_pool
            .find_trait_method(inner, resolved.type_pool.well_known.eq, name)
            .unwrap();
        let partial_eq = resolved
            .type_pool
            .find_trait_method(inner, resolved.type_pool.well_known.partial_eq, name)
            .unwrap();
        assert_ne!(eq.func_id, partial_eq.func_id);
        assert_eq!(resolved.derived_comparisons.len(), 2);
    }

    #[test]
    fn partial_eq_may_use_eq_but_eq_requires_eq() {
        let (_, errors) = resolve(
            "struct I { x:i64 }\nimpl Eq for I { fn eq(self, other:Self)->bool { true } }\nstruct O { x:I }\nderive PartialEq for O",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let (_, errors) = resolve(
            "struct I { x:i64 }\nimpl PartialEq for I { fn eq(self, other:Self)->bool { true } }\nstruct O { x:I }\nderive Eq for O",
        );
        assert!(
            errors.iter().any(|error| error.contains("field type")),
            "{errors:?}"
        );
        let (_, errors) = resolve("struct I { x:i64 }\nimpl Eq for I {}");
        assert!(
            errors
                .iter()
                .any(|error| error.contains("implementations require")),
            "{errors:?}"
        );
    }

    #[test]
    fn tuple_user_comparison_precedes_component_prerequisites() {
        let (_, errors) = resolve(
            "typealias P = (Any, bool)\nimpl Eq for P { fn eq(self, other:Self)->bool { true } }\nstruct O { x:P }\nderive Eq for O",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let (_, errors) = resolve("typealias P = (Any, bool)\nstruct O { x:P }\nderive Eq for O");
        assert!(
            errors.iter().any(|error| error.contains("field type")),
            "{errors:?}"
        );
    }

    #[test]
    fn derived_enum_methods_have_checked_signatures_before_registration() {
        let (_, errors) = resolve(
            "enum E { none, some(x:i64), }\nderive Eq for E\nfn main()->bool { E.some(42).eq(E.none) }",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let (_, errors) =
            resolve("enum E { none, }\nderive Eq for E\nfn main()->bool { E.none.eq(true) }");
        assert!(
            errors.iter().any(|error| error.contains("type mismatch")),
            "{errors:?}"
        );
    }
}
