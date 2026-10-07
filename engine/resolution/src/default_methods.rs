//! Each default body receives an independent concrete executable identity.

use std::collections::HashMap;

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{TraitMethodKey, TraitTypeStep, TypeIndex};

use crate::{SymbolId, resolver::Resolver};

pub use crate::default_body::DefaultBodyFacts;
use crate::default_body::check_body;
pub(crate) use crate::default_body::{
    DefaultBodyContext, checked_self_value_type, contextual_parameter_type, parameter_body_type,
    specialize_body_annotation,
};

/// Resolve declared defaults before executable adapters are published. This
/// lets concrete callers infer the specialized result during the typing pass.
pub(crate) fn source_member(
    r: &Resolver<'_>,
    ast: &Ast,
    receiver: TypeIndex,
    name: str_interner::StrId,
    from: crate::ScopeId,
) -> Result<Option<SymbolId>, String> {
    let Some(receiver) = r.type_pool.canonical_type(receiver) else {
        return Ok(None);
    };
    let mut selected = None;
    for scope in &r.scopes {
        if scope.assoc_type != Some(receiver)
            || scope.node.is_null()
            || !matches!(
                ast.node(scope.node).kind,
                NodeKind::ImplTraitDef | NodeKind::ExtendTraitDef
            )
        {
            continue;
        }
        if let Some(&visible) = r.imports.extension_scopes.get(&scope.id) {
            let mut caller = Some(from);
            let mut allowed = false;
            while let Some(current) = caller {
                if current == visible {
                    allowed = true;
                    break;
                }
                caller = r.scopes[current.0 as usize].parent;
            }
            if !allowed {
                continue;
            }
        }
        let trait_node = ast.fixed_children(scope.node)[0];
        let Some(owner) = r.node_symbols.get(&trait_node).and_then(|symbol| {
            r.type_pool
                .canonical_type(r.symbols[symbol.0 as usize].type_index)
        }) else {
            continue;
        };
        let mut pending = vec![owner];
        let mut visited = std::collections::HashSet::new();
        while let Some(owner) = pending.pop() {
            if !visited.insert(owner) {
                continue;
            }
            let declared = r.scopes.iter().find(|scope| {
                scope.assoc_type == Some(owner) && scope.bindings.contains_key(&name)
            });
            if declared.is_some_and(|scope| {
                let symbol = scope.bindings[&name];
                let definition = r.symbols[symbol.0 as usize].def_node;
                !definition.is_null()
                    && (ast.node(definition).kind == NodeKind::TraitDeriveFn
                        || crate::ordering::default_template(r, name) == Some(symbol))
            }) {
                let symbol = crate::associated::member_from(r, owner, name, from)?;
                if selected.is_some_and(|previous| previous != symbol) {
                    return Err(format!(
                        "ambiguous associated member `{}`",
                        str_interner::get(name)
                    ));
                }
                selected = Some(symbol);
                break;
            }
            if let type_pool::TypeKind::Trait { parents, .. } = &r.type_pool.get(owner).kind {
                pending.extend(parents.iter().rev().copied());
            }
        }
    }
    Ok(selected)
}

pub(crate) fn bind_calls(r: &mut Resolver<'_>, ast: &Ast) {
    for (node, function) in &mut r.instance_methods {
        let receiver = ast.fixed_children(*node)[0];
        let Some(&ty) = r.node_types.get(&receiver) else {
            continue;
        };
        for plan in &r.default_methods {
            if plan.declaration != *function
                || r.type_pool.canonical_type(ty) != Some(plan.implementor)
            {
                continue;
            }
            let Some(scope) = r.node_scopes.get(node) else {
                continue;
            };
            if let Ok(Some(slot)) =
                r.type_pool
                    .find_trait_method_scoped(ty, plan.trait_type, plan.method_name, scope.0)
                && slot.func_id == plan.function.0
            {
                *function = plan.function;
                break;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct DefaultMethodPlan {
    pub implementor: TypeIndex,
    pub implementation_trait: TypeIndex,
    pub body_facts: Option<DefaultBodyFacts>,
    pub associated_bindings: Vec<type_pool::AssociatedTypeBinding>,
    pub specialized_node_types: HashMap<NodeIndex, TypeIndex>,
    pub specialized_symbol_types: HashMap<SymbolId, TypeIndex>,
    pub specialized_coercion_targets: HashMap<NodeIndex, TypeIndex>,
    pub trait_type: TypeIndex,
    pub visible_scope: Option<u32>,
    pub provider_scope: u32,
    pub method_name: str_interner::StrId,
    pub declaration: SymbolId,
    pub function: SymbolId,
    pub signature: TypeIndex,
    pub self_parameters: Vec<usize>,
    /// Only source Self occurrences (including aliases) specialize type values.
    pub self_type_values: HashMap<NodeIndex, TypeIndex>,
    /// Concrete nested function signatures; associated binders and source Self
    /// specialize here, while direct Self proof positions remain independent.
    pub self_function_types: HashMap<NodeIndex, TypeIndex>,
    pub self_function_parameters: HashMap<NodeIndex, Vec<usize>>,
}

fn has_nested_self_parameter(paths: &[Vec<TraitTypeStep>]) -> bool {
    paths.iter().any(|path| {
        let nested = path.iter().enumerate().any(|(index, step)| {
            matches!(step, TraitTypeStep::Parameter(_)) && index + 1 < path.len()
        });
        nested
            && !path.iter().any(|step| {
                matches!(
                    step,
                    TraitTypeStep::ErrorInner | TraitTypeStep::ErrorMember(_)
                )
            })
    })
}

pub(crate) fn register(
    r: &mut Resolver<'_>,
    ast: &Ast,
    declaration: SymbolId,
    implementor: TypeIndex,
    trait_type: TypeIndex,
    visible_scope: Option<u32>,
    provider_scope: u32,
) -> Result<SymbolId, String> {
    let mut symbol = r.symbols[declaration.0 as usize].clone();
    let source = crate::trait_signatures::signature(r, ast, trait_type, symbol.def_node)?;
    if crate::associated_types::has_declarations(r, trait_type)
        && has_nested_self_parameter(&source.self_paths)
    {
        return Err("associated default methods cannot carry Self evidence inside a parameter type; use a direct Self parameter".into());
    }
    let self_parameters = source
        .self_paths
        .iter()
        .filter_map(|path| match path.as_slice() {
            [TraitTypeStep::Parameter(index)] => Some(*index as usize),
            _ => None,
        })
        .collect();
    let key = TraitMethodKey {
        trait_owner: trait_type,
        name: symbol.name,
        signature: Some(source),
    };
    let signature = r
        .type_pool
        .instantiate_trait_method_signature_in_impl(&key, implementor, trait_type, visible_scope)
        .map_err(|error| format!("cannot specialize default method: {error}"))?;
    if crate::associated_types::contains_dynamic_view(r, signature, 0) {
        return Err("dynamic trait function signatures with associated types require an associated return proof contract, which is not supported yet".into());
    }
    let associated_bindings: Vec<_> = r
        .type_pool
        .associated_bindings_snapshot()
        .iter()
        .filter(|binding| {
            binding.implementor == implementor
                && binding.trait_type == trait_type
                && binding.visible_scope == visible_scope
        })
        .cloned()
        .collect();
    let replay = if crate::associated_types::has_declarations(r, trait_type)
        || crate::default_body::requires_replay(r, ast, symbol.def_node)
    {
        Some(check_body(
            r,
            ast,
            symbol.def_node,
            implementor,
            trait_type,
            visible_scope,
            &associated_bindings,
        )?)
    } else {
        None
    };
    let specialization = (|| -> Result<_, String> {
        let mut self_type_values = HashMap::new();
        let mut self_function_types = HashMap::new();
        let mut self_function_parameters = HashMap::new();
        let provenance =
            crate::self_provenance::SelfProvenance::new(r, ast, symbol.def_node, trait_type)?;
        let mut pending = vec![symbol.def_node];
        while let Some(node) = pending.pop() {
            if node.is_null() {
                continue;
            }
            if let Some(ty) = crate::trait_signatures::specialize_type_value(
                r,
                ast,
                node,
                trait_type,
                implementor,
            )? {
                self_type_values.insert(node, ty);
            }
            if replay.is_some()
                && matches!(
                    ast.node(node).kind,
                    NodeKind::Lambda | NodeKind::FunctionDef
                )
                && has_nested_self_parameter(&provenance.paths(r, ast, node, trait_type)?)
            {
                return Err("associated default methods cannot carry Self evidence inside a parameter type; use a direct Self parameter".into());
            }
            if let Some((ty, parameters)) = crate::trait_signatures::specialize_function_signature(
                r,
                ast,
                node,
                trait_type,
                implementor,
                &provenance,
            )? {
                if crate::associated_types::contains_dynamic_view(r, ty, 0) {
                    return Err("dynamic trait function signatures with associated types require an associated return proof contract, which is not supported yet".into());
                }
                self_function_types.insert(node, ty);
                self_function_parameters.insert(node, parameters);
            }
            if replay.is_some()
                && ast.node(node).kind == NodeKind::Lambda
                && !self_function_types.contains_key(&node)
                && r.node_types
                    .get(&node)
                    .is_some_and(|&ty| crate::associated_types::contains_dynamic_view(r, ty, 0))
            {
                return Err("dynamic trait function signatures with associated types require an associated return proof contract, which is not supported yet".into());
            }
            pending.extend(ast.fixed_children(node));
            pending.extend(ast.multi_children(node));
        }
        let function = SymbolId(
            u32::try_from(r.symbols.len()).map_err(|_| "too many default method symbols")?,
        );
        Ok((
            self_type_values,
            self_function_types,
            self_function_parameters,
            function,
        ))
    })();
    if let Some(replay) = &replay {
        replay.restore(r);
    }
    let (self_type_values, self_function_types, self_function_parameters, function) =
        specialization?;
    symbol.id = function;
    symbol.type_index = signature;
    let method_name = symbol.name;
    let (
        body_facts,
        specialized_node_types,
        specialized_symbol_types,
        specialized_coercion_targets,
    ) = if let Some(replay) = replay {
        (
            Some(replay.facts),
            replay.node_types,
            replay.symbol_types,
            replay.coercion_targets,
        )
    } else {
        (None, HashMap::new(), HashMap::new(), HashMap::new())
    };
    r.symbols.push(symbol);
    r.default_methods.push(DefaultMethodPlan {
        implementor,
        implementation_trait: trait_type,
        body_facts,
        associated_bindings,
        specialized_node_types,
        specialized_symbol_types,
        specialized_coercion_targets,
        trait_type,
        visible_scope,
        provider_scope,
        method_name,
        declaration,
        function,
        signature,
        self_parameters,
        self_type_values,
        self_function_types,
        self_function_parameters,
    });
    Ok(function)
}

#[cfg(test)]
mod tests {
    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};
    use type_pool::{Intrinsic, TypeKind};

    #[test]
    fn associated_default_nested_self_parameters_report_the_missing_carrier() {
        for body in [
            "derive fn read(self,pair:(Self,i64))->Item{self.next()}",
            "derive fn callback(self)->fn((Self,i64))->Item{|pair:(Self,i64)|self.next()}",
            "derive fn callback(self)->fn((Self,i64))->Item{fn inner(pair:(Self,i64))->Item{pair.0.next()};inner}",
        ] {
            let source = format!(
                "struct P{{}};trait Source{{assoc Item:Type=i64;fn next(self)->i64;{body}}};impl Source for P{{fn next(self)->i64{{42}}}};fn main(){{42}}"
            );
            let (tokens, errors) = lexer::tokenize(&source);
            assert!(errors.is_empty(), "{errors:?}");
            let map = SourceMap::new(FilePathMapping::empty());
            let file = map.new_source_file(
                FileName::Custom("nested-self-carrier.ns".into()),
                source.clone(),
            );
            let diagnostics = DiagnosticContext::new(&map);
            let ast = parser::Parser::new(&tokens, &source, &diagnostics, file.start_pos).parse();
            let resolved = crate::resolve(ast, &diagnostics);
            assert!(
                diagnostics.diagnostics().iter().any(|diagnostic| diagnostic
                    .message
                    .contains("cannot carry Self evidence inside a parameter type")),
                "{source}: {:?}",
                diagnostics.diagnostics()
            );
            assert!(resolved.default_methods.is_empty());
        }
    }

    #[test]
    fn associated_default_facts_preserve_each_concrete_field_layout() {
        let source = "struct P{pad:i64,value:i64};struct Q{value:i64};trait Source{assoc Item:Type=Self;fn next(self)->Item;derive fn read(self)->i64{self.next().value}};impl Source for P{fn next(self)->P{self}};impl Source for Q{fn next(self)->Q{self}}";
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("default-facts.ns".into()), source.into());
        let diagnostics = DiagnosticContext::new(&map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve(ast, &diagnostics);
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
        let plans: Vec<_> = resolved
            .default_methods
            .iter()
            .filter(|plan| plan.method_name == str_interner::intern("read"))
            .collect();
        assert_eq!(plans.len(), 2);
        for plan in plans {
            let facts = plan.body_facts.as_ref().unwrap();
            let expected =
                if resolved.type_pool.display_name(plan.implementor).as_deref() == Some("P") {
                    1
                } else {
                    0
                };
            assert_eq!(
                facts
                    .node_field_indices
                    .values()
                    .copied()
                    .collect::<Vec<_>>(),
                [expected]
            );
            assert!(
                facts
                    .node_field_indices
                    .keys()
                    .all(|node| facts.nodes.contains(node))
            );
            assert!(!facts.nodes.contains(&resolved.ast.root));
            assert!(
                plan.specialized_node_types
                    .keys()
                    .all(|node| facts.nodes.contains(node))
            );
            assert_eq!(plan.associated_bindings.len(), 1);
            assert_eq!(plan.associated_bindings[0].value, plan.implementor);
        }
    }

    #[test]
    fn nested_lambda_annotations_specialize_only_source_self_paths() {
        let source = "struct P{};trait Read{derive fn callback(self)->fn(Self)->Self{|other:Self|->Self{other}};derive fn explicit_callback(self)->fn(Read)->Read{|other:Read|->Read{other}}};impl Read for P{}";
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(
            FileName::Custom("default-lambda-plans.ns".into()),
            source.into(),
        );
        let diagnostics = DiagnosticContext::new(&map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve(ast, &diagnostics);
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
        for plan in &resolved.default_methods {
            if plan.method_name == str_interner::intern("callback") {
                assert_eq!(plan.self_function_types.len(), 1);
                let (&node, &signature) = plan.self_function_types.iter().next().unwrap();
                let TypeKind::Function { params, ret } = &resolved.type_pool.get(signature).kind
                else {
                    panic!("lambda signature");
                };
                assert_eq!(params, &[plan.implementor]);
                assert_eq!(*ret, plan.implementor);
                assert_eq!(plan.self_function_parameters[&node], [0]);
            } else {
                assert!(plan.self_function_types.is_empty());
            }
        }
    }

    #[test]
    fn defaults_have_independent_concrete_identities_and_source_self_provenance() {
        let source = "struct P{};struct Q{};trait Read{fn value(self)->i64;derive fn identity(self)->Self{self};derive fn own_type(self)->Type{Self};derive fn explicit_type(self)->Type{Read}};impl Read for P{pub fn value(self)->i64{40}};impl Read for Q{pub fn value(self)->i64{2}}";
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("default-plans.ns".into()), source.into());
        let diagnostics = DiagnosticContext::new(&map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve(ast, &diagnostics);
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
        assert_eq!(resolved.default_methods.len(), 6);
        let mut functions = std::collections::HashSet::new();
        for plan in &resolved.default_methods {
            assert!(functions.insert(plan.function));
            assert_ne!(plan.function, plan.declaration);
            assert_eq!(plan.self_parameters, [0]);
            let TypeKind::Function { params, ret } = &resolved.type_pool.get(plan.signature).kind
            else {
                panic!("signature");
            };
            assert_eq!(params, &[plan.implementor]);
            if plan.method_name == str_interner::intern("identity") {
                assert_eq!(*ret, plan.implementor);
            } else {
                assert_eq!(*ret, Intrinsic::Type.type_index());
            }
            if plan.method_name == str_interner::intern("explicit_type") {
                assert!(
                    plan.self_type_values.is_empty(),
                    "explicit Read is not Self"
                );
            }
            if plan.method_name == str_interner::intern("own_type") {
                assert!(
                    plan.self_type_values
                        .values()
                        .any(|&ty| ty == plan.implementor)
                );
            }
            assert!(
                resolved
                    .type_pool
                    .trait_impls_snapshot()
                    .iter()
                    .find(|record| record.implementor == plan.implementor
                        && record.trait_type == plan.trait_type
                        && record.visible_scope == plan.visible_scope)
                    .unwrap()
                    .methods
                    .iter()
                    .any(|method| method.name == plan.method_name
                        && method.func_id == plan.function.0)
            );
        }
    }

    #[test]
    fn inferred_and_contextual_function_paths_preserve_annotation_boundaries() {
        let source = "struct P{};trait Read{derive fn inferred(self)->fn()->Self{let copy=self;||copy};derive fn contextual(self)->fn(Self)->Self{|x|x};derive fn erased(self)->fn()->Read{let copy:Read=self;||copy};derive fn any(self)->fn()->Any{||self};derive fn named(self)->fn(Self)->Self{fn inner(x:Self)->Self{x};inner}};impl Read for P{}";
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(
            FileName::Custom("default-provenance.ns".into()),
            source.into(),
        );
        let diagnostics = DiagnosticContext::new(&map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve(ast, &diagnostics);
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
        for plan in &resolved.default_methods {
            let name = str_interner::get(plan.method_name);
            if matches!(name.as_str(), "erased" | "any") {
                assert!(
                    plan.self_function_types.is_empty(),
                    "{name} must retain its source interface/Any contract"
                );
                continue;
            }
            assert_eq!(plan.self_function_types.len(), 1, "{name}");
            let (&node, &signature) = plan.self_function_types.iter().next().unwrap();
            let TypeKind::Function { params, ret } = &resolved.type_pool.get(signature).kind else {
                panic!("specialized signature");
            };
            assert_eq!(*ret, plan.implementor);
            if name == "inferred" {
                assert!(params.is_empty());
                assert!(plan.self_function_parameters[&node].is_empty());
            } else {
                assert_eq!(params, &[plan.implementor]);
                assert_eq!(plan.self_function_parameters[&node], [0]);
            }
        }
    }
}
