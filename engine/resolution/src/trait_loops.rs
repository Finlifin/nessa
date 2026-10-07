//! Source-only iterator contracts and loop plans frozen before NIR lowering.

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{TraitImplRecord, TypeIndex, TypeKind, TypePool};

use crate::{ScopeId, SymbolId, resolver::Resolver};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IteratorCall {
    pub implementor: TypeIndex,
    pub implementation_trait: TypeIndex,
    pub visible_scope: Option<u32>,
    /// Filled from this exact record after trait completion, never guessed by NIR.
    pub function: Option<SymbolId>,
}

#[derive(Debug, Clone)]
pub struct ForLoopPlan {
    pub query_scope: ScopeId,
    pub iterator_type: TypeIndex,
    pub item_type: TypeIndex,
    pub step_type: TypeIndex,
    pub into_iter: Option<IteratorCall>,
    pub next: IteratorCall,
}

/// Fresh source pools gain new declarations. Persisted pools are never upgraded.
pub(crate) fn prepare_bootstrap(pool: &mut TypePool) {
    for (owner, name) in [
        (pool.well_known.iterator, "Item"),
        (pool.well_known.into_iterator, "Iter"),
    ] {
        let name = str_interner::intern(name);
        let marker = pool.register(type_pool::TypeInfo {
            kind: TypeKind::AssociatedType {
                trait_owner: owner,
                name,
            },
            type_id: type_pool::TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let TypeKind::Trait { assoc_types, .. } = &mut pool.get_mut(owner).kind else {
            unreachable!("bootstrap role is a trait")
        };
        assoc_types.push((name, marker));
        pool.register_associated_default(type_pool::AssociatedTypeDefault {
            trait_owner: owner,
            name,
            expression: type_pool::AssociatedTypeExpr::Required,
        })
        .expect("fresh bootstrap required declaration matches its marker");
    }
}

pub(crate) fn bootstrap_signature(
    r: &mut Resolver<'_>,
    owner: TypeIndex,
) -> Option<type_pool::TraitMethodSignature> {
    let (name, path) = if owner == r.type_pool.well_known.iterator {
        (
            "Item",
            vec![
                type_pool::TraitTypeStep::Return,
                type_pool::TraitTypeStep::IterationItem,
            ],
        )
    } else if owner == r.type_pool.well_known.into_iterator {
        ("Iter", vec![type_pool::TraitTypeStep::Return])
    } else {
        return None;
    };
    let TypeKind::Trait { assoc_types, .. } = &r.type_pool.get(owner).kind else {
        return None;
    };
    let marker = assoc_types
        .iter()
        .find(|(candidate, _)| *candidate == str_interner::intern(name))?
        .1;
    let ret = if name == "Item" {
        r.type_pool
            .intern_iteration_step_template(marker)
            .expect("fresh bootstrap marker is abstract")
    } else {
        marker
    };
    let declaration = r.type_pool.intern_structural(TypeKind::Function {
        params: vec![owner],
        ret,
    });
    Some(type_pool::TraitMethodSignature {
        declaration,
        self_paths: vec![vec![type_pool::TraitTypeStep::Parameter(0)]],
        associated_paths: vec![type_pool::TraitAssociatedPath {
            trait_owner: owner,
            name: str_interner::intern(name),
            path,
        }],
        parameter_kinds: vec![type_pool::TraitParameterKind::Receiver],
    })
}

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: impl Into<String>) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}

fn selection(
    r: &Resolver<'_>,
    ty: TypeIndex,
    view: TypeIndex,
    scope: ScopeId,
) -> Result<Option<IteratorCall>, String> {
    r.type_pool
        .find_trait_impl_scoped(ty, view, scope.0)
        .map(|record| {
            record.map(|record| IteratorCall {
                implementor: record.implementor,
                implementation_trait: record.trait_type,
                visible_scope: record.visible_scope,
                function: None,
            })
        })
        .map_err(|error| format!("ambiguous iterator trait implementation: {error}"))
}

fn binding(
    r: &Resolver<'_>,
    call: &IteratorCall,
    owner: TypeIndex,
    name: &str,
) -> Result<TypeIndex, String> {
    let mut matches = r
        .type_pool
        .associated_bindings_snapshot()
        .iter()
        .filter(|binding| {
            binding.implementor == call.implementor
                && binding.trait_type == call.implementation_trait
                && binding.visible_scope == call.visible_scope
                && binding.trait_owner == owner
                && binding.name == str_interner::intern(name)
        });
    let ty = matches
        .next()
        .ok_or_else(|| {
            format!("iterator implementation requires exact associated `{name}` binding")
        })?
        .value;
    if matches.next().is_some() || !r.type_pool.is_static_associated_type(ty) {
        return Err(format!(
            "iterator associated `{name}` must be one concrete type"
        ));
    }
    r.type_pool
        .canonical_type(ty)
        .ok_or_else(|| format!("invalid iterator associated `{name}`"))
}

fn reject_interface(r: &Resolver<'_>, ty: TypeIndex) -> Result<(), String> {
    if r.type_pool
        .canonical_type(ty)
        .is_some_and(|ty| matches!(r.type_pool.get(ty).kind, TypeKind::Trait { .. }))
    {
        return Err("iteration over a trait interface requires an associated iterator return proof contract, which is not supported yet".into());
    }
    Ok(())
}

fn plan(
    r: &mut Resolver<'_>,
    node: NodeIndex,
    iterable: NodeIndex,
    ty: TypeIndex,
) -> Result<ForLoopPlan, String> {
    reject_interface(r, ty)?;
    let mut scope = *r
        .node_scopes
        .get(&node)
        .ok_or("iterator query is missing its lexical scope")?;
    // A copied source receiver uses the selected adapter's implementation scope.
    // Independent values keep the declaration's lexical scope.
    if let Some(context) = &r.default_body_context
        && context
            .self_value_paths
            .get(&iterable)
            .is_some_and(|paths| paths.iter().any(Vec::is_empty))
        && let Some(visible) = context.scope
    {
        scope = ScopeId(visible);
    }
    let into_iter = selection(r, ty, r.type_pool.well_known.into_iterator, scope)?;
    let iterator_type = if let Some(call) = &into_iter {
        binding(r, call, r.type_pool.well_known.into_iterator, "Iter")?
    } else {
        ty
    };
    reject_interface(r, iterator_type)?;
    let next = selection(r, iterator_type, r.type_pool.well_known.iterator, scope)?
        .ok_or("for input must implement Iterator or IntoIterator with an Iterator Iter type")?;
    let item_type = binding(r, &next, r.type_pool.well_known.iterator, "Item")?;
    let step_type = r
        .type_pool
        .intern_iteration_step(item_type)
        .map_err(|error| error.to_string())?;
    Ok(ForLoopPlan {
        query_scope: scope,
        iterator_type,
        item_type,
        step_type,
        into_iter,
        next,
    })
}

pub(crate) fn type_loop(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) {
    let children = ast.fixed_children(node);
    crate::typing::resolve_types(r, ast, children[2]);
    if let Some(&ty) = r.node_types.get(&children[2]) {
        let ty = crate::default_methods::checked_self_value_type(r, ast, children[2], ty);
        match plan(r, node, children[2], ty) {
            Ok(plan) => {
                crate::enums::pattern(r, ast, children[1], Some(plan.item_type));
                r.for_loops.insert(node, plan);
            }
            Err(error) => report(r, ast, node, error),
        }
    }
    crate::typing::resolve_types(r, ast, children[3]);
}

fn exact_record<'a>(r: &'a Resolver<'_>, call: &IteratorCall) -> Option<&'a TraitImplRecord> {
    r.type_pool.trait_impls_snapshot().iter().find(|record| {
        record.implementor == call.implementor
            && record.trait_type == call.implementation_trait
            && record.visible_scope == call.visible_scope
    })
}

fn freeze_call(
    r: &Resolver<'_>,
    call: &mut IteratorCall,
    name: &str,
    expected: TypeIndex,
    scope: ScopeId,
) -> Result<(), String> {
    let record =
        exact_record(r, call).ok_or("iterator implementation disappeared during completion")?;
    let method = record
        .methods
        .iter()
        .find(|method| method.name == str_interner::intern(name))
        .ok_or_else(|| format!("iterator implementation is missing required method `{name}`"))?;
    if !r
        .type_pool
        .method_accessible(method, scope.0)
        .map_err(|error| error.to_string())?
    {
        return Err(format!(
            "iterator method `{name}` is not visible from this scope"
        ));
    }
    let function = SymbolId(method.func_id);
    let ty = r
        .symbols
        .get(function.0 as usize)
        .and_then(|symbol| r.type_pool.canonical_type(symbol.type_index))
        .ok_or_else(|| format!("iterator method `{name}` has no checked function signature"))?;
    let TypeKind::Function { params, ret } = &r.type_pool.get(ty).kind else {
        return Err(format!("iterator method `{name}` must be a function"));
    };
    if params.len() != 1
        || r.type_pool.canonical_type(params[0]) != r.type_pool.canonical_type(call.implementor)
        || r.type_pool.canonical_type(*ret) != Some(expected)
    {
        return Err(format!(
            "iterator method `{name}` must accept exactly its receiver and return `{}`; old has_next/next source must migrate to tagged next",
            r.type_pool.display_name(expected).unwrap_or_default()
        ));
    }
    call.function = Some(function);
    Ok(())
}

fn freeze_plan(r: &Resolver<'_>, plan: &mut ForLoopPlan) -> Result<(), String> {
    if let Some(call) = &mut plan.into_iter {
        freeze_call(r, call, "into_iter", plan.iterator_type, plan.query_scope)?;
    }
    freeze_call(r, &mut plan.next, "next", plan.step_type, plan.query_scope)
}

pub(crate) fn validate(r: &mut Resolver<'_>, ast: &Ast) {
    let mut plans = std::mem::take(&mut r.for_loops);
    for (&node, plan) in &mut plans {
        if let Err(error) = freeze_plan(r, plan) {
            report(r, ast, node, error);
        }
    }
    r.for_loops = plans;
    // Adapter ASTs share nodes but must retain their own exact calls.
    for index in 0..r.default_methods.len() {
        let Some(mut facts) = r.default_methods[index].body_facts.take() else {
            continue;
        };
        for (&node, plan) in &mut facts.for_loops {
            if let Err(error) = freeze_plan(r, plan) {
                report(r, ast, node, error);
            }
        }
        r.default_methods[index].body_facts = Some(facts);
    }
    // IntoIterator.Iter's obligation holds even for an unused implementation.
    for record in r.type_pool.trait_impls_snapshot() {
        if !r
            .type_pool
            .is_subtype(record.trait_type, r.type_pool.well_known.into_iterator)
        {
            continue;
        }
        let call = IteratorCall {
            implementor: record.implementor,
            implementation_trait: record.trait_type,
            visible_scope: record.visible_scope,
            function: None,
        };
        let definition = r.scopes.iter().find(|candidate| {
            candidate.assoc_type == Some(record.implementor)
                && !candidate.node.is_null()
                && matches!(
                    ast.node(candidate.node).kind,
                    NodeKind::ImplTraitDef | NodeKind::ExtendTraitDef
                )
                && r.node_symbols
                    .get(&ast.fixed_children(candidate.node)[0])
                    .is_some_and(|symbol| {
                        r.type_pool
                            .canonical_type(r.symbols[symbol.0 as usize].type_index)
                            == Some(record.trait_type)
                    })
                && r.imports
                    .extension_scopes
                    .get(&candidate.id)
                    .map(|scope| scope.0)
                    == record.visible_scope
        });
        let Some(definition) = definition else {
            continue;
        };
        let scope = ScopeId(record.visible_scope.unwrap_or(definition.id.0));
        let obligation =
            binding(r, &call, r.type_pool.well_known.into_iterator, "Iter").and_then(|iter| {
                selection(r, iter, r.type_pool.well_known.iterator, scope)?.ok_or(
                    "IntoIterator.Iter must implement Iterator in the implementation scope".into(),
                )
            });
        if let Err(error) = obligation {
            report(r, ast, definition.node, error);
        }
    }
}

#[cfg(test)]
mod tests {
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};

    #[test]
    fn trait_iterator_interfaces_are_rejected_before_lowering() {
        for ty in ["Iterator", "IntoIterator"] {
            let source = format!(
                "typealias {ty} = .{ty}'builtin; fn consume(xs:{ty}) {{ for x in xs {{ x }} }}"
            );
            let (tokens, errors) = lexer::tokenize(&source);
            assert!(errors.is_empty(), "{errors:?}");
            let map = SourceMap::new(FilePathMapping::empty());
            let file =
                map.new_source_file(FileName::Custom("trait_iterator.ns".into()), source.clone());
            let diagnostics = diagnostic::DiagnosticContext::new(&map);
            let ast = parser::Parser::new(&tokens, &source, &diagnostics, file.start_pos).parse();
            crate::resolve_with_options(
                ast,
                &diagnostics,
                crate::ResolveOptions::for_builtin_package(),
            );
            assert!(
                diagnostics.diagnostics().iter().any(|diagnostic| diagnostic
                    .message
                    .contains("associated iterator return proof contract")),
                "{:?}",
                diagnostics.diagnostics()
            );
        }
    }
}
