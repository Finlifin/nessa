//! Error typing and explicit conversion facts. Lowering never infers a branch from payload type.
use crate::{
    ErrorConstructionPlan, ErrorConversionKind, ErrorConversionPlan, ErrorPropagationPlan,
    ErrorTagSource, resolver::Resolver, typing,
};
use ast::{Ast, NodeIndex, NodeKind};
use std::collections::HashSet;
use type_pool::{ErrorDomain, ErrorShape, Intrinsic, TypeIndex, TypeKind};

pub(crate) fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: impl Into<String>) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}
pub(crate) fn normalize(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    errors: Vec<TypeIndex>,
    inner: TypeIndex,
) -> Option<TypeIndex> {
    match r.type_pool.normalize_error_type(errors, inner) {
        Ok(ty) => Some(ty),
        Err(error) => {
            report(r, ast, node, error.to_string());
            None
        }
    }
}
pub(crate) fn domain_members(r: &Resolver<'_>, domain: &ErrorDomain) -> Vec<TypeIndex> {
    match domain {
        ErrorDomain::Closed(errors) => errors.clone(),
        ErrorDomain::Open => vec![r.type_pool.intrinsic(Intrinsic::Any)],
    }
}
/// Restricted static language: no function calls, mutable bindings or runtime concat dispatch.
pub(crate) fn set(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) -> Option<Vec<TypeIndex>> {
    fn walk(
        r: &mut Resolver<'_>,
        ast: &Ast,
        node: NodeIndex,
        active: &mut HashSet<NodeIndex>,
    ) -> Option<Vec<TypeIndex>> {
        if active.len() >= 256 {
            report(
                r,
                ast,
                node,
                "Error set expression exceeds 256 nested expressions",
            );
            return None;
        }
        if node.is_null() || !active.insert(node) {
            report(r, ast, node, "cyclic or missing Error set expression");
            return None;
        }
        let result = match ast.node(node).kind {
            NodeKind::ListOf => {
                let mut types = Vec::new();
                for &child in ast.multi_children(node) {
                    let members = walk(r, ast, child, active)?;
                    if members.len() > 262144usize.saturating_sub(types.len()) {
                        report(r, ast, node, "Error set exceeds 262144 members");
                        return None;
                    }
                    types.extend(members);
                }
                Some(types)
            }
            NodeKind::Concat => {
                let children = ast.fixed_children(node);
                let mut types = walk(r, ast, children[0], active)?;
                let members = walk(r, ast, children[1], active)?;
                if members.len() > 262144usize.saturating_sub(types.len()) {
                    report(r, ast, node, "Error set exceeds 262144 members");
                    return None;
                }
                types.extend(members);
                Some(types)
            }
            NodeKind::Id | NodeKind::Projection | NodeKind::SelfUpper | NodeKind::View => {
                let definition = r
                    .node_symbols
                    .get(&node)
                    .map(|symbol| r.symbols[symbol.0 as usize].def_node);
                if let Some(definition) = definition.filter(|definition| {
                    !definition.is_null() && ast.node(*definition).kind == NodeKind::ConstDecl
                }) {
                    walk(r, ast, ast.fixed_children(definition)[2], active)
                } else if let Some(ty) = typing::resolve_type_expr_inner(r, ast, node) {
                    let ty = r.type_pool.canonical_type(ty)?;
                    let eligible = !matches!(
                        r.type_pool.get(ty).kind,
                        TypeKind::Intrinsic(Intrinsic::Any | Intrinsic::NoReturn)
                            | TypeKind::Module { .. }
                            | TypeKind::Effect { .. }
                            | TypeKind::Trait { .. }
                            | TypeKind::EffectQualified { .. }
                    );
                    let symbolic = ast.node(node).kind == NodeKind::SelfUpper
                        || r.type_pool.contains_associated_type(ty);
                    if eligible || symbolic {
                        r.node_type_values.insert(node, ty);
                        Some(vec![ty])
                    } else {
                        report(
                            r,
                            ast,
                            node,
                            "Error set members must be concrete payload types",
                        );
                        None
                    }
                } else {
                    report(
                        r,
                        ast,
                        node,
                        "Error set requires a type path or eligible const reference",
                    );
                    None
                }
            }
            _ => {
                report(
                    r,
                    ast,
                    node,
                    "Error set supports only type paths, lists, const references and concat",
                );
                None
            }
        };
        active.remove(&node);
        result
    }
    walk(r, ast, node, &mut HashSet::new())
}
pub(crate) fn construction(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    expected: Option<TypeIndex>,
) -> Option<TypeIndex> {
    let operand = ast.fixed_children(node)[0];
    typing::resolve_types(r, ast, operand);
    let payload = *r.node_types.get(&operand)?;
    let payload = crate::default_methods::checked_self_value_type(r, ast, operand, payload);
    let payload = r.type_pool.canonical_type(payload)?;
    let dynamic = r.type_pool.as_intrinsic(payload) == Some(Intrinsic::Any);
    if matches!(
        r.type_pool.get(payload).kind,
        TypeKind::Intrinsic(Intrinsic::NoReturn)
            | TypeKind::Module { .. }
            | TypeKind::Effect { .. }
            | TypeKind::EffectQualified { .. }
    ) {
        report(
            r,
            ast,
            node,
            "error construction requires an instantiable payload type",
        );
        return None;
    }
    if !dynamic && matches!(r.type_pool.get(payload).kind, TypeKind::Trait { .. }) {
        report(
            r,
            ast,
            node,
            "error construction from a bare trait requires a concrete payload carrier",
        );
        return None;
    }
    let result = normalize(
        r,
        ast,
        node,
        vec![payload],
        Intrinsic::NoReturn.type_index(),
    )?;
    let target = expected.and_then(|target| r.type_pool.error_shape(target).ok().flatten());
    let result = if dynamic && let Some(target) = target {
        normalize(
            r,
            ast,
            node,
            domain_members(r, &target.domain),
            Intrinsic::NoReturn.type_index(),
        )?
    } else {
        result
    };
    r.error_constructions.insert(
        node,
        ErrorConstructionPlan {
            operand,
            result,
            tag: if dynamic {
                ErrorTagSource::DynamicPayload
            } else {
                ErrorTagSource::Static(payload)
            },
        },
    );
    Some(result)
}
pub(crate) fn conversion(
    r: &Resolver<'_>,
    source: TypeIndex,
    target: TypeIndex,
) -> Option<ErrorConversionPlan> {
    let source = r.type_pool.canonical_type(source)?;
    let target = r.type_pool.canonical_type(target)?;
    if source == target || r.type_pool.as_intrinsic(source) == Some(Intrinsic::NoReturn) {
        return None;
    }
    let target_shape = r.type_pool.error_shape(target).ok().flatten()?;
    let kind = match r.type_pool.error_shape(source).ok().flatten() {
        None => ErrorConversionKind::LiftOk,
        Some(ErrorShape {
            domain: ErrorDomain::Open,
            ..
        }) if matches!(target_shape.domain, ErrorDomain::Closed(_)) => {
            ErrorConversionKind::CheckQualified
        }
        Some(_) => ErrorConversionKind::MapQualified,
    };
    Some(ErrorConversionPlan {
        source,
        target,
        kind,
    })
}
pub(crate) fn record_conversion(
    r: &mut Resolver<'_>,
    node: NodeIndex,
    source: TypeIndex,
    target: TypeIndex,
) -> bool {
    let Some(plan) = conversion(r, source, target) else {
        return false;
    };
    let plans = r.error_conversions.entry(node).or_default();
    if !plans.last().is_some_and(|previous| {
        previous.source == plan.source
            && previous.target == plan.target
            && previous.kind == plan.kind
    }) {
        plans.push(plan);
    }
    true
}
pub(crate) fn join(r: &mut Resolver<'_>, left: TypeIndex, right: TypeIndex) -> TypeIndex {
    let a = r.type_pool.error_shape(left).ok().flatten();
    let b = r.type_pool.error_shape(right).ok().flatten();
    if a.is_none() && b.is_none() {
        return crate::optional::join(r, left, right);
    }
    let mut errors = Vec::new();
    let a_inner = a
        .as_ref()
        .map(|shape| {
            errors.extend(domain_members(r, &shape.domain));
            shape.inner
        })
        .unwrap_or(left);
    let b_inner = b
        .as_ref()
        .map(|shape| {
            errors.extend(domain_members(r, &shape.domain));
            shape.inner
        })
        .unwrap_or(right);
    let inner = crate::optional::join(r, a_inner, b_inner);
    // Shapes originate from checked source types; errors cannot contain invalid indices.
    match r.type_pool.normalize_error_type(errors, inner) {
        Ok(ty) => ty,
        Err(error) => {
            r.diag_ctx.error(error.to_string()).emit(r.diag_ctx);
            Intrinsic::NoReturn.type_index()
        }
    }
}
pub(crate) fn callable(r: &Resolver<'_>, ast: &Ast, node: NodeIndex) -> Option<NodeIndex> {
    let mut scope = r.node_scopes.get(&node).copied();
    while let Some(id) = scope {
        let entry = &r.scopes[id.0 as usize];
        if !entry.node.is_null()
            && (matches!(
                ast.node(entry.node).kind,
                NodeKind::FunctionDef | NodeKind::TraitDeriveFn | NodeKind::Lambda
            ) || r.handler_arms.contains(&entry.node))
        {
            return Some(entry.node);
        }
        scope = entry.parent;
    }
    None
}
pub(crate) fn propagate(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) -> Option<TypeIndex> {
    let operand = ast.fixed_children(node)[0];
    typing::resolve_types(r, ast, operand);
    let input = *r.node_types.get(&operand)?;
    let Some(shape) = r.type_pool.error_shape(input).ok().flatten() else {
        report(
            r,
            ast,
            node,
            "Error propagation requires an Error-qualified operand",
        );
        return None;
    };
    let Some(callable) = callable(r, ast, node) else {
        report(r, ast, node, "Error propagation requires a callable body");
        return None;
    };
    let errors = domain_members(r, &shape.domain);
    let error_only = normalize(r, ast, node, errors, Intrinsic::NoReturn.type_index())?;
    let target = r
        .return_types
        .last()
        .copied()
        .flatten()
        .unwrap_or(error_only);
    if let Some(returns) = r.inferred_returns.last_mut() {
        returns.push(error_only);
    }
    if !crate::trait_typing::is_subtype(r, ast, node, error_only, target)
        && !r.type_pool.is_gradually_consistent(error_only, target)
    {
        report(
            r,
            ast,
            node,
            "propagated error set is not allowed by the callable result",
        );
    }
    let error_conversion = conversion(r, input, target).unwrap_or(ErrorConversionPlan {
        source: input,
        target,
        kind: ErrorConversionKind::MapQualified,
    });
    r.error_propagations.insert(
        node,
        ErrorPropagationPlan {
            operand,
            input,
            success: shape.inner,
            callable,
            return_target: target,
            error_conversion,
        },
    );
    Some(shape.inner)
}
pub(crate) fn finish_callable(r: &mut Resolver<'_>, callable: NodeIndex, target: TypeIndex) {
    let updates: Vec<_> = r
        .error_propagations
        .iter()
        .filter(|(_, plan)| plan.callable == callable)
        .map(|(node, plan)| (*node, plan.input))
        .collect();
    for (node, input) in updates {
        let conversion = conversion(r, input, target).unwrap_or(ErrorConversionPlan {
            source: input,
            target,
            kind: ErrorConversionKind::MapQualified,
        });
        if let Some(plan) = r.error_propagations.get_mut(&node) {
            plan.return_target = target;
            plan.error_conversion = conversion;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};

    fn resolve(source: &str) -> (crate::ResolvedAst, Vec<String>) {
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("error-static.ns".into()), source.into());
        let diagnostics = diagnostic::DiagnosticContext::new(&map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve(ast, &diagnostics);
        let errors = diagnostics
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.message.clone())
            .collect();
        (resolved, errors)
    }
    fn result(resolved: &crate::ResolvedAst, name: &str) -> TypeIndex {
        let signature = resolved
            .symbols
            .iter()
            .find(|symbol| {
                str_interner::get(symbol.name) == name && symbol.kind == crate::SymbolKind::Function
            })
            .unwrap()
            .type_index;
        match resolved.type_pool.get(signature).kind {
            TypeKind::Function { ret, .. } => ret,
            _ => panic!("not a function"),
        }
    }
    #[test]
    fn source_sets_lists_const_concat_flatten_empty_and_aliases() {
        let (resolved, errors) = resolve(
            "enum E{bad,};enum F{bad,};const ES=[E,F,E];typealias X=E;fn a()->!ES ++ [X] !F i64{42};fn b()->![] i64{42};fn c()->![F,E] i64{42}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let a = result(&resolved, "a");
        let c = result(&resolved, "c");
        assert_eq!(a, c);
        assert_eq!(result(&resolved, "b"), Intrinsic::I64.type_index());
        assert_eq!(
            (
                resolved.type_pool.get(a).size,
                resolved.type_pool.get(a).align
            ),
            (24, 8)
        );
        assert!(
            matches!(resolved.type_pool.error_shape(a).unwrap().unwrap().domain,ErrorDomain::Closed(ref members) if members.len()==2)
        );
        assert_eq!(
            resolved
                .error_conversions
                .values()
                .flatten()
                .filter(|plan| plan.kind == ErrorConversionKind::LiftOk)
                .count(),
            2
        );
    }
    #[test]
    fn constructor_is_error_only_and_inference_joins_success() {
        let (resolved, errors) = resolve(
            "enum E{bad,};fn bad(){error E.bad};fn mixed(flag:bool){if flag {error E.bad}else{42}}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let bad = resolved
            .type_pool
            .error_shape(result(&resolved, "bad"))
            .unwrap()
            .unwrap();
        assert_eq!(bad.inner, Intrinsic::NoReturn.type_index());
        let mixed = resolved
            .type_pool
            .error_shape(result(&resolved, "mixed"))
            .unwrap()
            .unwrap();
        assert_eq!(mixed.inner, Intrinsic::I64.type_index());
        assert_eq!(resolved.error_constructions.len(), 2);
    }
    #[test]
    fn propagation_yields_success_and_contributes_callable_errors() {
        let (resolved, errors) = resolve("enum E{bad,};fn get()->!E i64{42};fn next(){get()! + 1}");
        assert!(errors.is_empty(), "{errors:?}");
        let shape = resolved
            .type_pool
            .error_shape(result(&resolved, "next"))
            .unwrap()
            .unwrap();
        assert_eq!(shape.inner, Intrinsic::I64.type_index());
        let plan = resolved.error_propagations.values().next().unwrap();
        assert_eq!(plan.success, Intrinsic::I64.type_index());
        assert_eq!(plan.return_target, result(&resolved, "next"));
        assert_eq!(resolved.ast.node(plan.callable).kind, NodeKind::FunctionDef);
    }
    #[test]
    fn elimination_partial_enum_and_guard_preserve_residual_and_success() {
        for arm in ["E.a=>1", "E.* if false=>1", "E.a=>1,E.b if false=>2"] {
            let (resolved, errors) = resolve(&format!(
                "enum E{{a,b,}};fn get()->!E i64{{42}};fn next(){{get()!{{{arm}}}}}"
            ));
            assert!(errors.is_empty(), "{arm}: {errors:?}");
            let plan = resolved.error_eliminations.values().next().unwrap();
            assert!(
                resolved
                    .type_pool
                    .error_shape(plan.result)
                    .unwrap()
                    .is_some()
            );
            assert!(plan.residual_conversion.is_some());
            assert!(plan.implicit_success_conversion.is_some());
        }
    }
    #[test]
    fn whole_enum_and_family_remove_only_error_branch() {
        for arm in ["E.a=>1,E.b=>2", "E.* as e=>1", "catch e=>1"] {
            let (resolved, errors) = resolve(&format!(
                "enum E{{a,b,}};fn get()->!E i64{{42}};fn next(){{get()!{{{arm}}}}}"
            ));
            assert!(errors.is_empty(), "{arm}: {errors:?}");
            let plan = resolved.error_eliminations.values().next().unwrap();
            assert_eq!(plan.result, Intrinsic::I64.type_index());
            assert_eq!(plan.residual, Intrinsic::NoReturn.type_index());
            assert!(plan.residual_conversion.is_none());
            assert!(plan.implicit_success_conversion.is_some());
        }
    }
    #[test]
    fn ordinary_ok_binder_and_error_binder_have_payload_types() {
        let (resolved, errors) = resolve(
            "enum E{bad,};fn get()->!E i64{42};fn next(){get()!{ok! =>ok+1,catch e=>error e}}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let ok = resolved
            .symbols
            .iter()
            .find(|symbol| str_interner::get(symbol.name) == "ok")
            .unwrap();
        assert_eq!(ok.type_index, Intrinsic::I64.type_index());
        let e = resolved
            .symbols
            .iter()
            .find(|symbol| str_interner::get(symbol.name) == "e")
            .unwrap();
        assert_eq!(e.type_index, Intrinsic::Any.type_index());
        assert!(
            resolved
                .error_constructions
                .values()
                .any(|plan| matches!(plan.tag, ErrorTagSource::DynamicPayload))
        );
        assert!(matches!(
            resolved
                .type_pool
                .error_shape(result(&resolved, "next"))
                .unwrap()
                .unwrap()
                .domain,
            ErrorDomain::Open
        ));
        assert!(
            resolved
                .error_eliminations
                .values()
                .next()
                .unwrap()
                .implicit_success_conversion
                .is_none()
        );
    }
    #[test]
    fn open_finite_family_catch_does_not_exhaust_dynamic_domain() {
        let (resolved, errors) = resolve("enum E{bad,};fn next(e:Any){(error e)!{E.* =>1}}");
        assert!(errors.is_empty(), "{errors:?}");
        let plan = resolved.error_eliminations.values().next().unwrap();
        assert!(matches!(
            resolved
                .type_pool
                .error_shape(plan.residual)
                .unwrap()
                .unwrap()
                .domain,
            ErrorDomain::Open
        ));
    }
    #[test]
    fn direct_match_wrappers_preserve_equal_success_error_payloads() {
        let (resolved, errors) =
            resolve("enum E{bad,};fn next(v:!E E){v match{ok! =>ok;error E.bad=>E.bad}}");
        assert!(errors.is_empty(), "{errors:?}");
        assert!(
            resolved
                .error_patterns
                .values()
                .any(|plan| plan.branch == crate::ErrorPatternBranch::Ok)
        );
        assert!(
            resolved
                .error_patterns
                .values()
                .any(|plan| plan.branch == crate::ErrorPatternBranch::Error)
        );
    }
    #[test]
    fn rejects_invalid_sets_propagation_and_qualified_any_subset() {
        for source in [
            "enum E{bad,};enum F{bad,};fn bad(v:!F Any)->!E Any{v}",
            "fn bad()->!Any i64{42}",
            "fn bad()->![42] i64{42}",
            "enum E{bad,};let ES=[E];fn bad()->!ES i64{42}",
            "enum E{bad,};fn make(){E};fn bad()->!make() i64{42}",
            "fn bad(){42!}",
            "enum E{bad,};fn get()->!E i64{42};get()!",
            "enum E{bad,};fn get()->!E i64{42};fn bad(.x:i64=get()!){x}",
            "enum E{bad,};fn get()->!E i64{42};fn bad()->i64{get()!}",
            "enum E{bad,};fn get()->!E i64{42};fn bad(){get() match{#e=>1}}",
        ] {
            let (_, errors) = resolve(source);
            assert!(!errors.is_empty(), "unexpected acceptance: {source}");
        }
    }
    #[test]
    fn normalized_trait_error_source_paths_preserve_self_leaf_and_empty() {
        let (resolved, errors) = resolve("struct E{};trait T{fn get(self)->![E,Self,E] ![] Self}");
        assert!(errors.is_empty(), "{errors:?}");
        let signature = resolved
            .type_pool
            .trait_schemas_snapshot()
            .iter()
            .flat_map(|schema| &schema.slots)
            .find(|key| str_interner::get(key.name) == "get")
            .unwrap()
            .signature
            .as_ref()
            .unwrap();
        assert!(signature.self_paths.iter().any(|path| matches!(
            path.as_slice(),
            [
                type_pool::TraitTypeStep::Return,
                type_pool::TraitTypeStep::ErrorMember(_)
            ]
        )));
        assert!(signature.self_paths.contains(&vec![
            type_pool::TraitTypeStep::Return,
            type_pool::TraitTypeStep::ErrorInner
        ]));
    }
    #[test]
    fn generic_error_pattern_binding_is_any_and_dynamic_rethrow() {
        let (resolved, errors) =
            resolve("enum E{bad,};fn next(v:!E i64){v match{ok! =>ok,error e=>error e}}");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            resolved
                .symbols
                .iter()
                .find(|symbol| str_interner::get(symbol.name) == "e")
                .unwrap()
                .type_index,
            Intrinsic::Any.type_index()
        );
        assert!(
            resolved
                .error_constructions
                .values()
                .any(|plan| matches!(plan.tag, ErrorTagSource::DynamicPayload))
        );
    }
    #[test]
    fn two_default_replays_keep_concrete_error_and_associated_facts_isolated() {
        let (resolved, errors) = resolve(
            "enum Fail{bad,};struct P{};struct Q{};trait Read{assoc Item:Type=i64;fn next(self)->Item;derive fn wrap(self)->!Fail Item{self.next()};derive fn pass(self,v:!Fail Self)->!Fail Self{v!}};impl Read for P{fn next(self)->i64{42}};impl Read for Q{assoc Item:Type=String;fn next(self)->String{\"q\"}}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let wrappers: Vec<_> = resolved
            .default_methods
            .iter()
            .filter(|plan| str_interner::get(plan.method_name) == "wrap")
            .collect();
        assert_eq!(wrappers.len(), 2);
        let mut inners = Vec::new();
        for plan in wrappers {
            let TypeKind::Function { ret, .. } = resolved.type_pool.get(plan.signature).kind else {
                panic!("function")
            };
            inners.push(resolved.type_pool.error_shape(ret).unwrap().unwrap().inner);
            assert!(
                plan.body_facts
                    .as_ref()
                    .unwrap()
                    .error_conversions
                    .values()
                    .flatten()
                    .any(|conversion| conversion.kind == ErrorConversionKind::LiftOk)
            );
        }
        inners.sort_by_key(|ty| ty.as_u32());
        let mut expected = vec![Intrinsic::I64.type_index(), Intrinsic::Str.type_index()];
        expected.sort_by_key(|ty| ty.as_u32());
        assert_eq!(inners, expected);
        let passes: Vec<_> = resolved
            .default_methods
            .iter()
            .filter(|plan| str_interner::get(plan.method_name) == "pass")
            .collect();
        assert_eq!(passes.len(), 2);
        for plan in passes {
            let facts = plan.body_facts.as_ref().unwrap();
            let propagation = facts.error_propagations.values().next().unwrap();
            assert_eq!(propagation.success, plan.implementor);
            let TypeKind::Function { ret, .. } = resolved.type_pool.get(plan.signature).kind else {
                panic!("function")
            };
            assert_eq!(propagation.return_target, ret);
        }
    }
    #[test]
    fn catch_and_case_local_nominals_have_distinct_persisted_identity_paths() {
        let (resolved, errors) = resolve(
            "enum E{bad,};fn get()->!E i64{42};fn next(){get()!{E.bad=>{struct Local{};42},catch e=>{struct Local{};42}}}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let local: Vec<_> = resolved
            .type_pool
            .snapshot()
            .types
            .iter()
            .filter_map(|info| match info.kind {
                TypeKind::Struct { name, .. } if str_interner::get(name) == "Local" => {
                    Some(info.type_id)
                }
                _ => None,
            })
            .collect();
        assert_eq!(local.len(), 2);
        assert_ne!(local[0], local[1]);
        assert!(local.iter().all(|id| *id != type_pool::TypeId::ZERO));
        let restored = type_pool::TypePool::restore(resolved.type_pool.snapshot()).unwrap();
        for id in local {
            assert_eq!(
                restored
                    .stable_type_id(restored.lookup_by_id(id).unwrap())
                    .unwrap(),
                id
            );
        }
    }
    #[test]
    fn dynamic_context_checks_closed_set_but_construction_stays_error_only() {
        let (resolved, errors) = resolve("enum E{bad,};fn next(e:Any)->!E i64{error e}");
        assert!(errors.is_empty(), "{errors:?}");
        let plan = resolved.error_constructions.values().next().unwrap();
        let shape = resolved
            .type_pool
            .error_shape(plan.result)
            .unwrap()
            .unwrap();
        assert_eq!(shape.inner, Intrinsic::NoReturn.type_index());
        assert!(matches!(shape.domain,ErrorDomain::Closed(ref members) if members.len()==1));
        assert!(matches!(plan.tag, ErrorTagSource::DynamicPayload));
    }
    #[test]
    fn propagation_in_capture_handler_targets_that_handler_callable() {
        let (resolved, errors) = resolve(
            "enum E{bad,};effect ask(catch k)->i64;fn get()->!E i64{42};fn next(){ask()#{ask(k)=>get()!}}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let plan = resolved.error_propagations.values().next().unwrap();
        assert_eq!(resolved.ast.node(plan.callable).kind, NodeKind::CaseArm);
        assert_eq!(plan.return_target, result(&resolved, "next"));
        assert_eq!(plan.success, Intrinsic::I64.type_index());
    }
    #[test]
    fn direct_error_matches_require_both_branch_coverage() {
        for arms in [
            "error E.bad=>1",
            "ok! =>ok",
            "ok! if false=>ok,error E.bad=>1",
        ] {
            let (_, errors) = resolve(&format!(
                "enum E{{bad,}};fn next(v:!E i64){{v match{{{arms}}}}}"
            ));
            assert!(
                errors.iter().any(|error| error.contains("must exhaust")),
                "{arms}: {errors:?}"
            );
        }
        for arms in [
            "_=>1",
            "v=>1",
            "ok! =>ok,error E.bad=>1",
            "ok! =>ok,error e=>1",
        ] {
            let (_, errors) = resolve(&format!(
                "enum E{{bad,}};fn next(v:!E i64){{v match{{{arms}}}}}"
            ));
            assert!(errors.is_empty(), "{arms}: {errors:?}");
        }
    }
    #[test]
    fn dynamic_propagation_checks_restricted_callable_set_explicitly() {
        let (resolved, errors) =
            resolve("enum E{bad,};fn make(e:Any){error e};fn next(e:Any)->!E i64{make(e)!}");
        assert!(errors.is_empty(), "{errors:?}");
        let plan = resolved.error_propagations.values().next().unwrap();
        assert_eq!(
            plan.error_conversion.kind,
            ErrorConversionKind::CheckQualified
        );
    }
    #[test]
    fn finite_nested_payload_constructor_matrix_proves_total_family_coverage() {
        let (resolved, errors) = resolve(
            "enum Inner{a,b,};enum E{some(x:Inner),none,};fn next(v:!E i64){v!{E.some(Inner.a)=>1,E.some(Inner.b)=>2,E.none=>3}}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let plan = resolved.error_eliminations.values().next().unwrap();
        assert_eq!(plan.residual, Intrinsic::NoReturn.type_index());
        assert_eq!(plan.result, Intrinsic::I64.type_index());
        let (resolved, errors) = resolve(
            "enum Inner{a,b,};enum E{some(x:Inner),none,};fn next(v:!E i64){v!{E.some(Inner.a)=>1,E.none=>3}}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert!(
            resolved
                .error_eliminations
                .values()
                .next()
                .unwrap()
                .residual_conversion
                .is_some()
        );
    }
    #[test]
    fn mixed_composite_elimination_keeps_each_wrapper_entry_owned_once() {
        let (resolved, errors) = resolve("enum E{bad,};fn next(v:!E i64){v!{E.bad or _! =>1}}");
        assert!(errors.is_empty(), "{errors:?}");
        let plan = resolved.error_eliminations.values().next().unwrap();
        assert!(plan.implicit_success_conversion.is_none());
        assert!(plan.residual_conversion.is_none());
        let root = resolved.ast.fixed_children(plan.arms[0])[0];
        assert_eq!(resolved.ast.node(root).kind, NodeKind::PatternOr);
        assert!(!resolved.error_patterns.contains_key(&root));
        let entries = resolved.ast.fixed_children(root);
        assert!(resolved.error_patterns.contains_key(&entries[0]));
        assert!(resolved.error_patterns.contains_key(&entries[1]));
    }
    #[test]
    fn qualified_error_payload_can_be_matched_as_an_explicit_nested_wrapper() {
        let (resolved, errors) = resolve(
            "enum E{bad,};typealias Wrapped=!E i64;fn next(v:!Wrapped i64){v match{_! =>1,error (error e)=>1,error (_!)=>1}}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            resolved
                .error_patterns
                .values()
                .filter(|plan| plan.branch == crate::ErrorPatternBranch::Error)
                .count(),
            3
        );
    }
    #[test]
    fn qualified_result_wraps_arithmetic_once_after_raw_propagated_operands() {
        let (resolved, errors) =
            resolve("enum E{bad,};fn get()->!E i64{40};fn next()->!E i64{get()!+2}");
        assert!(errors.is_empty(), "{errors:?}");
        let (&propagation, _) = resolved.error_propagations.iter().next().unwrap();
        assert!(!resolved.error_conversions.contains_key(&propagation));
        let add = resolved
            .ast
            .nodes
            .iter()
            .enumerate()
            .find(|(_, node)| node.kind == NodeKind::Add)
            .map(|(index, _)| NodeIndex(index as u32))
            .unwrap();
        let plan = resolved.error_conversions.get(&add).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].kind, ErrorConversionKind::LiftOk);
        for child in resolved.ast.fixed_children(add) {
            assert!(!resolved.error_conversions.contains_key(child));
        }
    }
    #[test]
    fn qualified_tuple_success_payload_widens_recursively() {
        let (resolved, errors) =
            resolve("enum E{bad,};fn small()->!E (i8,i8){(1,2)};fn wide()->!E (i64,i64){small()}");
        assert!(errors.is_empty(), "{errors:?}");
        let shape = resolved
            .type_pool
            .error_shape(result(&resolved, "wide"))
            .unwrap()
            .unwrap();
        assert!(
            matches!(&resolved.type_pool.get(shape.inner).kind, TypeKind::Tuple { elements } if elements == &vec![Intrinsic::I64.type_index(); 2])
        );
        assert!(
            resolved
                .error_conversions
                .values()
                .flatten()
                .any(|plan| plan.kind == ErrorConversionKind::MapQualified
                    && plan.source != plan.target)
        );
    }
    #[test]
    fn capturing_handler_checks_its_contextual_continuation_answer() {
        let (resolved, errors) = resolve(
            "enum E{bad,};effect choose(catch k)->!E i64;fn work()->!E i64{choose()#!};fn main(){let value:!E i64=work()#{choose(k)=>{let result:!E i64=error E.bad;result!;k(42)}};value match{error E.bad=>42,_! =>0}}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let (&call, coercion) = resolved
            .node_coercions
            .iter()
            .find(|(node, _)| {
                resolved.ast.node(**node).kind == NodeKind::Call
                    && resolved
                        .type_pool
                        .error_shape(resolved.node_types[node])
                        .ok()
                        .flatten()
                        .is_some()
            })
            .unwrap();
        assert_eq!(coercion.kind, crate::CoercionKind::Assert);
        assert!(!resolved.error_conversions.contains_key(&call));
        let propagation = resolved
            .error_propagations
            .values()
            .find(|plan| resolved.ast.node(plan.callable).kind == NodeKind::CaseArm)
            .unwrap();
        assert_eq!(propagation.return_target, coercion.target);
    }
    #[test]
    fn runtime_type_values_are_eligible_error_families() {
        let (resolved, errors) =
            resolve("fn f()->!Type i64{error i64};fn main(){f()!{Type.* =>42}}");
        assert!(errors.is_empty(), "{errors:?}");
        assert!(
            resolved
                .error_patterns
                .values()
                .any(|plan| plan.type_test == Some(Intrinsic::Type.type_index()))
        );
    }
    #[test]
    fn tuple_payload_conversion_rejects_narrowing_and_wrong_arity() {
        for target in ["(i8,i8)", "(i64,i64,i64)"] {
            let (_, errors) = resolve(&format!(
                "enum E{{bad,}};fn a()->!E (i64,i64){{(1,2)}};fn b()->!E {target}{{a()}}"
            ));
            assert!(
                errors.iter().any(|error| error.contains("type mismatch")),
                "{target}: {errors:?}"
            );
        }
    }
    #[test]
    fn contextual_handler_still_checks_continuation_input() {
        let (_, errors) = resolve(
            r#"enum E{bad,};effect choose(catch k)->!E i64;fn work()->!E i64{choose()#!};fn main(){let value:!E i64=work()#{choose(k)=>k("wrong")}}"#,
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("continuation input")),
            "{errors:?}"
        );
    }
    #[test]
    fn concrete_error_family_membership_never_widens_or_retags() {
        let (rejected, errors) =
            resolve("fn a()->!i8 i64{let n:i8=1;error n};fn b()->!i64 i64{a()}");
        assert!(
            errors.iter().any(|error| error.contains("type mismatch")),
            "{errors:?}"
        );
        let source = result(&rejected, "a");
        let target = result(&rejected, "b");
        assert!(!rejected.type_pool.is_subtype(source, target));
        assert!(
            !rejected
                .type_pool
                .is_subtype_scoped(source, target, 0)
                .unwrap()
        );
        let (resolved, errors) = resolve("enum E{bad,};fn a()->!E i8{1};fn b()->!E i64{a()}");
        assert!(errors.is_empty(), "{errors:?}");
        assert!(
            resolved
                .error_conversions
                .values()
                .flatten()
                .any(|plan| plan.kind == ErrorConversionKind::MapQualified
                    && plan.source != plan.target)
        );
        let source = result(&resolved, "a");
        let target = result(&resolved, "b");
        assert!(resolved.type_pool.is_subtype(source, target));
        assert!(
            resolved
                .type_pool
                .is_subtype_scoped(source, target, 0)
                .unwrap()
        );
    }
    #[test]
    fn reachable_error_branch_rejects_refutable_declaration_and_parameter() {
        for source in [
            "enum E{bad,};fn main(){let value:!E i64=42;let n! = value;42}",
            "enum E{bad,};fn take(n! : !E i64)->i64{n};fn main(){42}",
        ] {
            let (_, errors) = resolve(source);
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains("Error binding pattern must be irrefutable")),
                "{errors:?}"
            );
        }
        let (_, errors) = resolve(
            "enum E{bad,};fn take(value:!E i64)->!E i64{value};fn main(){let value:!E i64=42;let whole=value;take(whole)}",
        );
        assert!(errors.is_empty(), "{errors:?}");
    }
}
