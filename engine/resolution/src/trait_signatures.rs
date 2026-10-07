//! Preserve source Self occurrences instead of guessing from equal type indices.

use std::collections::HashSet;

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{TraitMethodSignature, TraitParameterKind, TraitTypeStep, TypeIndex, TypeKind};

use crate::resolver::Resolver;

pub(crate) fn parameter_kinds(
    r: &Resolver<'_>,
    ast: &Ast,
    declaration: NodeIndex,
) -> Option<Vec<TraitParameterKind>> {
    ast.multi_children(declaration)
        .iter()
        .map(|&parameter| {
            Some(match ast.node(parameter).kind {
                NodeKind::ParamSelf => TraitParameterKind::Receiver,
                NodeKind::ParamTyped | NodeKind::ParamLambda => TraitParameterKind::Required,
                NodeKind::ParamOptional => TraitParameterKind::Optional,
                NodeKind::ParamVarargs => {
                    let layout =
                        crate::variadics::layout(r, ast, ast.multi_children(declaration)).ok()?;
                    if layout == crate::variadics::Layout::ListMap
                        && crate::typing::param_type_index(r, ast, parameter)
                            .and_then(|ty| r.type_pool.collection_role(ty))
                            == Some(type_pool::CollectionRole::Map)
                    {
                        TraitParameterKind::MapVariadic
                    } else {
                        TraitParameterKind::ListVariadic
                    }
                }
                _ => return None,
            })
        })
        .collect()
}

struct SelfPathContext {
    declaration: TypeIndex,
    owner: TypeIndex,
    paths: Vec<Vec<TraitTypeStep>>,
    aliases: HashSet<NodeIndex>,
    associated: Vec<type_pool::TraitAssociatedPath>,
}

pub(crate) fn type_at_path(
    r: &Resolver<'_>,
    mut ty: TypeIndex,
    path: &[TraitTypeStep],
) -> Option<TypeIndex> {
    for step in path {
        ty = r.type_pool.canonical_type(ty)?;
        ty = match (step, &r.type_pool.get(ty).kind) {
            (TraitTypeStep::IterationItem, TypeKind::IterationStepTemplate { item }) => *item,
            (TraitTypeStep::IterationItem, TypeKind::Enum { .. }) => {
                r.type_pool.checked_iteration_step_item(ty).ok()??
            }
            (
                TraitTypeStep::Parameter(index),
                TypeKind::Function { params, .. } | TypeKind::Effect { params, .. },
            ) => *params.get(*index as usize)?,
            (
                TraitTypeStep::Return,
                TypeKind::Function { ret, .. } | TypeKind::Effect { ret, .. },
            ) => *ret,
            (TraitTypeStep::TupleElement(index), TypeKind::Tuple { elements }) => {
                *elements.get(*index as usize)?
            }
            (TraitTypeStep::OptionalInner, TypeKind::Optional { inner })
            | (TraitTypeStep::ErrorInner, TypeKind::ErrorQualified { inner, .. })
            | (TraitTypeStep::EffectInner, TypeKind::EffectQualified { inner, .. }) => *inner,
            (TraitTypeStep::ErrorMember(index), TypeKind::ErrorQualified { errors, .. }) => {
                *errors.get(*index as usize)?
            }
            (TraitTypeStep::EffectMember(index), TypeKind::EffectQualified { effects, .. }) => {
                *effects.get(*index as usize)?
            }
            _ => return None,
        };
    }
    r.type_pool.canonical_type(ty)
}

fn qualifier_step(
    r: &Resolver<'_>,
    node: NodeIndex,
    path: &[TraitTypeStep],
    context: &SelfPathContext,
    is_error: bool,
) -> Result<TraitTypeStep, String> {
    let qualified = type_at_path(r, context.declaration, path)
        .ok_or("invalid qualified trait signature path")?;
    let members = match &r.type_pool.get(qualified).kind {
        TypeKind::ErrorQualified { errors, .. } if is_error => errors,
        TypeKind::EffectQualified { effects, .. } if !is_error => effects,
        _ => return Err("qualified trait signature has incompatible declaration shape".into()),
    };
    let source_type = r
        .node_type_values
        .get(&node)
        .copied()
        .or_else(|| {
            r.node_symbols
                .get(&node)
                .map(|symbol| r.symbols[symbol.0 as usize].type_index)
        })
        .and_then(|ty| r.type_pool.canonical_type(ty));
    let index = match source_type {
        Some(source) => members
            .iter()
            .position(|&member| r.type_pool.canonical_type(member) == Some(source)),
        // Source lowering represents a composite qualifier as one type. It can
        // only occupy the unique member; arbitrary metadata sets require an
        // identifiable source type, never a guessed sorted position.
        None if members.len() == 1 => Some(0),
        None => None,
    }
    .ok_or("cannot locate source qualifier in canonical trait signature")?;
    Ok(if is_error {
        TraitTypeStep::ErrorMember(index as u32)
    } else {
        TraitTypeStep::EffectMember(index as u32)
    })
}

/// Follow each source leaf through normalization, including flattened inner qualifiers.
fn error_set_paths(
    r: &Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    path: &mut Vec<TraitTypeStep>,
    context: &mut SelfPathContext,
    depth: usize,
) -> Result<(), String> {
    if depth >= 256 {
        return Err("Error set provenance nesting is too deep".into());
    }
    match ast.node(node).kind {
        NodeKind::ListOf => {
            for &child in ast.multi_children(node) {
                error_set_paths(r, ast, child, path, context, depth + 1)?;
            }
        }
        NodeKind::Concat => {
            for &child in ast.fixed_children(node) {
                error_set_paths(r, ast, child, path, context, depth + 1)?;
            }
        }
        _ => {
            let declaration = r
                .node_symbols
                .get(&node)
                .map(|symbol| r.symbols[symbol.0 as usize].def_node);
            if let Some(declaration) = declaration.filter(|declaration| {
                !declaration.is_null() && ast.node(*declaration).kind == NodeKind::ConstDecl
            }) {
                if !context.aliases.insert(declaration) {
                    return Err("cyclic const in Error set provenance".into());
                }
                error_set_paths(
                    r,
                    ast,
                    ast.fixed_children(declaration)[2],
                    path,
                    context,
                    depth + 1,
                )?;
                context.aliases.remove(&declaration);
            } else {
                let step = qualifier_step(r, node, path, context, true)?;
                path.push(step);
                self_paths(r, ast, node, path, context, depth + 1)?;
                path.pop();
            }
        }
    }
    Ok(())
}
fn error_qualifier_paths(
    r: &Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    path: &mut Vec<TraitTypeStep>,
    context: &mut SelfPathContext,
    depth: usize,
) -> Result<(), String> {
    if depth >= 256 {
        return Err("Error qualifier provenance nesting is too deep".into());
    }
    let children = ast.fixed_children(node);
    error_set_paths(r, ast, children[0], path, context, depth + 1)?;
    let inner = children[1];
    if ast.node(inner).kind == NodeKind::ErrorQualifiedType {
        return error_qualifier_paths(r, ast, inner, path, context, depth + 1);
    }
    if let Some(declaration) = r
        .node_symbols
        .get(&inner)
        .map(|symbol| r.symbols[symbol.0 as usize].def_node)
        .filter(|declaration| {
            !declaration.is_null() && ast.node(*declaration).kind == NodeKind::Typealias
        })
    {
        let target = ast.fixed_children(declaration)[1];
        if ast.node(target).kind == NodeKind::ErrorQualifiedType {
            if !context.aliases.insert(declaration) {
                return Err("cyclic Error qualifier alias".into());
            }
            error_qualifier_paths(r, ast, target, path, context, depth + 1)?;
            context.aliases.remove(&declaration);
            return Ok(());
        }
    }
    let qualified = type_at_path(r, context.declaration, path)
        .is_some_and(|ty| matches!(r.type_pool.get(ty).kind, TypeKind::ErrorQualified { .. }));
    if qualified {
        path.push(TraitTypeStep::ErrorInner);
    }
    self_paths(r, ast, inner, path, context, depth + 1)?;
    if qualified {
        path.pop();
    }
    Ok(())
}

fn self_paths(
    r: &Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    path: &mut Vec<TraitTypeStep>,
    context: &mut SelfPathContext,
    depth: usize,
) -> Result<(), String> {
    if node.is_null() {
        return Ok(());
    }
    if depth >= 256 {
        return Err("trait signature alias/type nesting is too deep".into());
    }
    macro_rules! child {
        ($node:expr, $step:expr) => {{
            path.push($step);
            let outcome = self_paths(r, ast, $node, path, context, depth + 1);
            path.pop();
            outcome?;
        }};
    }
    match ast.node(node).kind {
        NodeKind::Call | NodeKind::PatternCall
            if crate::type_factories::identity(r, ast.fixed_children(node)[0]).is_some() =>
        {
            let [item] = ast.multi_children(node) else {
                return Err("invalid IterationStep type argument count".into());
            };
            child!(*item, TraitTypeStep::IterationItem);
        }
        NodeKind::SelfUpper => {
            let actual = r
                .node_symbols
                .get(&node)
                .map(|symbol| r.symbols[symbol.0 as usize].type_index)
                .or_else(|| r.node_type_values.get(&node).copied());
            if actual.and_then(|ty| r.type_pool.canonical_type(ty)) == Some(context.owner) {
                context.paths.push(path.clone());
            }
        }
        NodeKind::Id | NodeKind::Projection => {
            if let Some(symbol) = r.node_symbols.get(&node) {
                let source = &r.symbols[symbol.0 as usize];
                let declaration = source.def_node;
                if !declaration.is_null() && ast.node(declaration).kind == NodeKind::AssocBinding {
                    let owner = r.scopes[source.scope.0 as usize]
                        .assoc_type
                        .and_then(|ty| r.type_pool.canonical_type(ty));
                    if let Some(owner) = owner
                        && matches!(r.type_pool.get(owner).kind, TypeKind::Trait { .. })
                    {
                        context.associated.push(type_pool::TraitAssociatedPath {
                            trait_owner: owner,
                            name: source.name,
                            path: path.clone(),
                        });
                    }
                }
                if !declaration.is_null() && ast.node(declaration).kind == NodeKind::Typealias {
                    if !context.aliases.insert(declaration) {
                        return Err("cyclic alias in trait signature".into());
                    }
                    self_paths(
                        r,
                        ast,
                        ast.fixed_children(declaration)[1],
                        path,
                        context,
                        depth + 1,
                    )?;
                    context.aliases.remove(&declaration);
                }
            }
        }
        NodeKind::OptionalType => child!(ast.fixed_children(node)[0], TraitTypeStep::OptionalInner),
        NodeKind::Tuple => {
            for (index, &element) in ast.multi_children(node).iter().enumerate() {
                child!(element, TraitTypeStep::TupleElement(index as u32));
            }
        }
        NodeKind::FnType | NodeKind::EffectType | NodeKind::AsyncEffectType => {
            for (index, &parameter) in ast.multi_children(node).iter().enumerate() {
                child!(parameter, TraitTypeStep::Parameter(index as u32));
            }
        }
        NodeKind::Arrow => {
            self_paths(
                r,
                ast,
                ast.fixed_children(node)[0],
                path,
                context,
                depth + 1,
            )?;
            child!(ast.fixed_children(node)[1], TraitTypeStep::Return);
        }
        NodeKind::ErrorQualifiedType => {
            error_qualifier_paths(r, ast, node, path, context, depth + 1)?;
        }
        NodeKind::EffectQualifiedType => {
            let step = qualifier_step(r, ast.fixed_children(node)[0], path, context, false)?;
            child!(ast.fixed_children(node)[0], step);
            child!(ast.fixed_children(node)[1], TraitTypeStep::EffectInner);
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn signature(
    r: &Resolver<'_>,
    ast: &Ast,
    owner: TypeIndex,
    declaration: NodeIndex,
) -> Result<TraitMethodSignature, String> {
    let children = ast.fixed_children(declaration);
    let symbol = r
        .node_symbols
        .get(&children[0])
        .ok_or("trait method has no resolved declaration")?;
    let declaration_type = r.symbols[symbol.0 as usize].type_index;
    let parameter_kinds =
        parameter_kinds(r, ast, declaration).ok_or("unsupported trait method parameter kind")?;
    let mut context = SelfPathContext {
        declaration: declaration_type,
        owner,
        paths: Vec::new(),
        aliases: HashSet::new(),
        associated: Vec::new(),
    };
    for (index, &parameter) in ast.multi_children(declaration).iter().enumerate() {
        let mut path = vec![TraitTypeStep::Parameter(index as u32)];
        if ast.node(parameter).kind == NodeKind::ParamSelf {
            context.paths.push(path);
        } else {
            let annotation = ast
                .fixed_children(parameter)
                .get(1)
                .copied()
                .unwrap_or(NodeIndex::NULL);
            self_paths(r, ast, annotation, &mut path, &mut context, 0)?;
        }
    }
    self_paths(
        r,
        ast,
        children[1],
        &mut vec![TraitTypeStep::Return],
        &mut context,
        0,
    )?;
    Ok(TraitMethodSignature {
        associated_paths: context.associated,
        declaration: declaration_type,
        self_paths: context.paths,
        parameter_kinds,
    })
}

/// Specialize a type expression from source Self provenance, never by index equality.
pub(crate) fn annotation_self_paths(
    r: &Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    owner: TypeIndex,
) -> Result<Vec<Vec<TraitTypeStep>>, String> {
    if node.is_null() {
        return Ok(Vec::new());
    }
    let declaration = r
        .node_type_values
        .get(&node)
        .copied()
        .or_else(|| r.node_types.get(&node).copied())
        .or_else(|| {
            r.node_symbols
                .get(&node)
                .map(|symbol| r.symbols[symbol.0 as usize].type_index)
        });
    // Composite annotations are not always entered in node_type_values. The
    // structural walk still identifies their source Self leaves; qualifier
    // membership requires real shape metadata and reports an error if absent.
    let declaration = declaration.unwrap_or(owner);
    let mut context = SelfPathContext {
        declaration,
        owner,
        paths: Vec::new(),
        aliases: HashSet::new(),
        associated: Vec::new(),
    };
    self_paths(r, ast, node, &mut Vec::new(), &mut context, 0)?;
    Ok(context.paths)
}

pub(crate) fn specialize_type_value(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    owner: TypeIndex,
    concrete: TypeIndex,
) -> Result<Option<TypeIndex>, String> {
    let Some(&ty) = r.node_type_values.get(&node) else {
        return Ok(None);
    };
    let mut context = SelfPathContext {
        declaration: ty,
        owner,
        paths: Vec::new(),
        aliases: HashSet::new(),
        associated: Vec::new(),
    };
    self_paths(r, ast, node, &mut Vec::new(), &mut context, 0)?;
    // Concrete replay may already have specialized a source Self leaf. Do not
    // publish it again as an abstract owner path in a synthetic signature.
    context
        .paths
        .retain(|path| type_at_path(r, ty, path) == r.type_pool.canonical_type(owner));
    if context.paths.is_empty() {
        return Ok(None);
    }
    let declaration = r.type_pool.intern_structural(TypeKind::Function {
        params: vec![ty],
        ret: type_pool::Intrinsic::Unit.type_index(),
    });
    let key = type_pool::TraitMethodKey {
        trait_owner: owner,
        name: str_interner::intern("__default_type"),
        signature: Some(TraitMethodSignature {
            associated_paths: Vec::new(),
            declaration,
            self_paths: context
                .paths
                .into_iter()
                .map(|path| {
                    let mut result = vec![TraitTypeStep::Parameter(0)];
                    result.extend(path);
                    result
                })
                .collect(),
            parameter_kinds: vec![TraitParameterKind::Required],
        }),
    };
    let specialized = r
        .type_pool
        .instantiate_trait_method_signature(&key, concrete)
        .map_err(|error| format!("cannot specialize default Self type: {error}"))?;
    let TypeKind::Function { params, .. } = &r.type_pool.get(specialized).kind else {
        return Err("invalid specialized Self type".into());
    };
    Ok(params.first().copied())
}

/// Lambda annotations bind Self just like a method declaration; inferred value
/// types are not sufficient evidence to replace an explicit interface return.
pub(crate) fn specialize_function_signature(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    owner: TypeIndex,
    concrete: TypeIndex,
    provenance: &crate::self_provenance::SelfProvenance,
) -> Result<Option<(TypeIndex, Vec<usize>)>, String> {
    if !matches!(
        ast.node(node).kind,
        NodeKind::Lambda | NodeKind::FunctionDef
    ) {
        return Ok(None);
    }
    let declaration = if ast.node(node).kind == NodeKind::FunctionDef {
        r.node_symbols
            .get(&ast.fixed_children(node)[0])
            .map(|symbol| r.symbols[symbol.0 as usize].type_index)
    } else {
        r.node_types.get(&node).copied()
    };
    let Some(declaration) = declaration else {
        return Ok(None);
    };
    let source_paths: Vec<_> = provenance
        .paths(r, ast, node, owner)?
        .into_iter()
        .filter(|path| {
            let endpoint = type_at_path(r, declaration, path);
            endpoint == r.type_pool.canonical_type(owner)
                || endpoint == r.type_pool.canonical_type(concrete)
        })
        .collect();
    let has_source_paths = !source_paths.is_empty();
    let parameters = source_paths
        .iter()
        .filter_map(|path| match path.as_slice() {
            [TraitTypeStep::Parameter(index)] => Some(*index as usize),
            _ => None,
        })
        .collect();
    let paths: Vec<_> = source_paths
        .into_iter()
        .filter(|path| type_at_path(r, declaration, path) == r.type_pool.canonical_type(owner))
        .collect();
    if paths.is_empty() {
        return Ok(
            (ast.node(node).kind == NodeKind::FunctionDef || has_source_paths)
                .then_some((declaration, parameters)),
        );
    }
    let kinds = parameter_kinds(r, ast, node).ok_or("invalid default function parameter mode")?;
    let key = type_pool::TraitMethodKey {
        trait_owner: owner,
        name: str_interner::intern("__default_function"),
        signature: Some(TraitMethodSignature {
            associated_paths: Vec::new(),
            declaration,
            self_paths: paths,
            parameter_kinds: kinds,
        }),
    };
    r.type_pool
        .instantiate_trait_method_signature(&key, concrete)
        .map(|ty| Some((ty, parameters)))
        .map_err(|error| format!("cannot specialize default function signature: {error}"))
}

pub(crate) fn validate(r: &Resolver<'_>, ast: &Ast) {
    for record in r.type_pool.trait_impls_snapshot() {
        let Some(schema) = r.type_pool.trait_schema(record.trait_type) else {
            continue;
        };
        for key in &schema.slots {
            let Some(signature) = &key.signature else {
                continue;
            };
            let own = record.methods.iter().find(|method| method.name == key.name);
            let inherited = if own.is_none() {
                match record.visible_scope {
                    Some(scope) => match r.type_pool.find_trait_method_scoped(
                        record.implementor,
                        key.trait_owner,
                        key.name,
                        scope,
                    ) {
                        Ok(method) => method,
                        Err(_) => continue, // Parent/vtable resolution emits the exact ambiguity.
                    },
                    None => {
                        r.type_pool
                            .find_trait_method(record.implementor, key.trait_owner, key.name)
                    }
                }
            } else {
                None
            };
            let Some(method) = own.or(inherited) else {
                continue;
            };
            let Some(symbol) = r.symbols.get(method.func_id as usize) else {
                continue;
            };
            let node = symbol.def_node;
            if node.is_null() {
                continue;
            }
            // Templates retain abstract Self; published adapters are checked
            // against their concrete implementor signatures.
            let implementor = if ast.node(node).kind == NodeKind::TraitDeriveFn
                && !r
                    .default_methods
                    .iter()
                    .any(|plan| plan.function.0 == method.func_id)
            {
                key.trait_owner
            } else {
                record.implementor
            };
            let outcome = r.type_pool.check_trait_method_signature_in_impl(
                key,
                implementor,
                symbol.type_index,
                if own.is_some() {
                    record.trait_type
                } else {
                    key.trait_owner
                },
                method.visible_scope,
            );
            let kinds = parameter_kinds(r, ast, node);
            let message = match outcome {
                Err(error) => Some(format!(
                    "trait method `{}` signature mismatch: {error}",
                    str_interner::get(key.name)
                )),
                Ok(()) if kinds.as_ref() != Some(&signature.parameter_kinds) => Some(format!(
                    "trait method `{}` parameter kinds do not match its declaration",
                    str_interner::get(key.name)
                )),
                Ok(()) => None,
            };
            if let Some(message) = message {
                r.diag_ctx
                    .error(message)
                    .with_primary_span(ast.node(node).span)
                    .emit(r.diag_ctx);
            }
        }
    }
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
        let file =
            map.new_source_file(FileName::Custom("trait-signature.ns".into()), source.into());
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

    fn method<'a>(resolved: &'a crate::ResolvedAst, name: &str) -> &'a TraitMethodSignature {
        resolved
            .type_pool
            .trait_schemas_snapshot()
            .iter()
            .flat_map(|schema| &schema.slots)
            .find(|key| key.name == str_interner::intern(name))
            .unwrap()
            .signature
            .as_ref()
            .unwrap()
    }

    #[test]
    fn nested_function_optional_and_explicit_trait_paths_are_distinct() {
        let (resolved, errors) =
            resolve("trait Read{fn map(self,callback:fn(?Self)->(Self,Read))->?Self}");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            method(&resolved, "map").self_paths,
            vec![
                vec![TraitTypeStep::Parameter(0)],
                vec![
                    TraitTypeStep::Parameter(1),
                    TraitTypeStep::Parameter(0),
                    TraitTypeStep::OptionalInner
                ],
                vec![
                    TraitTypeStep::Parameter(1),
                    TraitTypeStep::Return,
                    TraitTypeStep::TupleElement(0)
                ],
                vec![TraitTypeStep::Return, TraitTypeStep::OptionalInner],
            ]
        );
        resolved.type_pool.validate().unwrap();
    }

    #[test]
    fn qualifier_paths_follow_the_actual_canonical_type_tree() {
        let (resolved, errors) = resolve(
            "struct Other{};trait Read{fn errors(self)->!Other fn()->!Self i64;fn effects(self)->#Other fn()->#Self i64}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            method(&resolved, "errors").self_paths,
            vec![
                vec![TraitTypeStep::Parameter(0)],
                vec![
                    TraitTypeStep::Return,
                    TraitTypeStep::ErrorInner,
                    TraitTypeStep::Return,
                    TraitTypeStep::ErrorMember(0)
                ]
            ]
        );
        assert_eq!(
            method(&resolved, "effects").self_paths,
            vec![
                vec![TraitTypeStep::Parameter(0)],
                vec![
                    TraitTypeStep::Return,
                    TraitTypeStep::EffectInner,
                    TraitTypeStep::Return,
                    TraitTypeStep::EffectMember(0)
                ]
            ]
        );
        resolved.type_pool.validate().unwrap();
    }

    #[test]
    fn aliases_do_not_convert_explicit_interface_or_foreign_self_into_implementor_self() {
        let (resolved, errors) = resolve(
            "typealias Interface=Read;struct Holder{typealias This=Self};trait Read{fn value(self,explicit:Interface,foreign:Holder.This,own:Self)->i64}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            method(&resolved, "value").self_paths,
            vec![
                vec![TraitTypeStep::Parameter(0)],
                vec![TraitTypeStep::Parameter(3)]
            ]
        );
        let (_, errors) =
            resolve("trait Read{derive fn value(self)->i64{\"bad\"}};struct P{};impl Read for P{}");
        assert!(
            errors.iter().any(|error| error.contains("type mismatch")),
            "{errors:?}"
        );
    }
}
