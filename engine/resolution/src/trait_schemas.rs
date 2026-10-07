//! One canonical dispatch schema per trait, independent of implementations.

use std::collections::{HashMap, HashSet};

use ast::Ast;
use type_pool::{
    Intrinsic, TraitDispatchSchema, TraitMethodKey, TraitMethodSignature, TraitParameterKind,
    TraitTypeStep, TypeIndex, TypeKind,
};

use crate::resolver::Resolver;

fn bootstrap_names(r: &Resolver<'_>, ty: TypeIndex) -> Option<&'static [&'static str]> {
    let known = &r.type_pool.well_known;
    if ty == known.eq || ty == known.partial_eq {
        Some(&["eq"])
    } else if ty == known.ord {
        if crate::ordering::type_index(r).is_some() {
            Some(&["cmp", "lt", "gt", "lte", "gte"])
        } else {
            Some(&["cmp"])
        }
    } else if ty == known.partial_ord {
        Some(&["partial_cmp"])
    } else if ty == known.display {
        Some(&["to_string"])
    } else if ty == known.hash {
        Some(&["hash"])
    } else if ty == known.iterator {
        Some(&["next"])
    } else if ty == known.into_iterator {
        Some(&["into_iter"])
    } else {
        None
    }
}

/// These declarations come from the supported bootstrap protocol, never from
/// an implementation's signature. Unspecified associated-return slots stay None.
fn bootstrap_schema(r: &mut Resolver<'_>, ty: TypeIndex) -> TraitDispatchSchema {
    let names = bootstrap_names(r, ty).expect("well-known bootstrap trait");
    let ordering = crate::ordering::type_index(r);
    let mut slots = if ordering.is_some() && ty == r.type_pool.well_known.ord {
        bootstrap_schema(r, r.type_pool.well_known.eq).slots
    } else if ordering.is_some() && ty == r.type_pool.well_known.partial_ord {
        bootstrap_schema(r, r.type_pool.well_known.partial_eq).slots
    } else {
        Vec::new()
    };
    let own_slots: Vec<_> = names
        .iter()
        .map(|&name| {
            if let Some(signature) = crate::trait_loops::bootstrap_signature(r, ty) {
                return TraitMethodKey {
                    trait_owner: ty,
                    name: str_interner::intern(name),
                    signature: Some(signature),
                };
            }
            let contract =
                if ty == r.type_pool.well_known.eq || ty == r.type_pool.well_known.partial_eq {
                    Some((2, Intrinsic::Bool.type_index()))
                } else if ty == r.type_pool.well_known.display {
                    Some((1, Intrinsic::Str.type_index()))
                } else if ty == r.type_pool.well_known.ord {
                    if name == "cmp" {
                        ordering.map(|ty| (2, ty))
                    } else {
                        ordering.map(|_| (2, Intrinsic::Bool.type_index()))
                    }
                } else if ty == r.type_pool.well_known.partial_ord {
                    ordering.map(|inner| {
                        (
                            2,
                            r.type_pool.intern_structural(TypeKind::Optional { inner }),
                        )
                    })
                } else {
                    None
                };
            let signature = contract.map(|(count, ret)| {
                let declaration = r.type_pool.intern_structural(TypeKind::Function {
                    params: vec![ty; count],
                    ret,
                });
                TraitMethodSignature {
                    associated_paths: Vec::new(),
                    declaration,
                    self_paths: (0..count)
                        .map(|index| vec![TraitTypeStep::Parameter(index as u32)])
                        .collect(),
                    parameter_kinds: (0..count)
                        .map(|index| {
                            if index == 0 {
                                TraitParameterKind::Receiver
                            } else {
                                TraitParameterKind::Required
                            }
                        })
                        .collect(),
                }
            });
            TraitMethodKey {
                trait_owner: ty,
                name: str_interner::intern(name),
                signature,
            }
        })
        .collect();
    slots.extend(own_slots);
    TraitDispatchSchema {
        trait_type: ty,
        slots,
    }
}

/// Static lookup uses the same authoritative declaration as schema production.
/// Missing bootstrap contracts are not inferred from any concrete implementation.
pub(crate) fn bootstrap_method(
    r: &mut Resolver<'_>,
    owner: TypeIndex,
    name: str_interner::StrId,
) -> Option<TraitMethodKey> {
    let mut pending = vec![owner];
    let mut visited = HashSet::new();
    while let Some(owner) = pending.pop() {
        let owner = r.type_pool.canonical_type(owner)?;
        if !visited.insert(owner) {
            continue;
        }
        if bootstrap_names(r, owner).is_some() {
            if let Some(key) = bootstrap_schema(r, owner)
                .slots
                .into_iter()
                .find(|key| key.name == name && key.signature.is_some())
            {
                return Some(key);
            }
        } else if let TypeKind::Trait { parents, .. } = &r.type_pool.get(owner).kind {
            pending.extend(parents.iter().rev().copied());
        }
    }
    None
}

fn schema(
    r: &Resolver<'_>,
    ast: &Ast,
    ty: TypeIndex,
    complete: &mut HashMap<TypeIndex, TraitDispatchSchema>,
    visiting: &mut HashSet<TypeIndex>,
) -> Result<TraitDispatchSchema, String> {
    if let Some(schema) = complete.get(&ty) {
        return Ok(schema.clone());
    }
    if visiting.len() >= 256 {
        return Err("trait inheritance is too deep to define a dispatch schema".into());
    }
    if !visiting.insert(ty) {
        return Err("cyclic trait inheritance cannot define a dispatch schema".into());
    }
    let mut slots = Vec::new();
    if let Some(info) = r.traits.iter().find(|info| info.type_index == ty) {
        for &parent in &info.parent_traits {
            for slot in schema(r, ast, parent, complete, visiting)?.slots {
                if !slots
                    .iter()
                    .any(|existing: &TraitMethodKey| existing.name == slot.name)
                {
                    slots.push(slot);
                }
            }
        }
        for &name in info.required_methods.iter().chain(&info.derived_methods) {
            let declaration = r
                .symbols
                .iter()
                .find(|symbol| {
                    symbol.name == name
                        && !symbol.def_node.is_null()
                        && matches!(
                            ast.node(symbol.def_node).kind,
                            ast::NodeKind::TraitDefFn | ast::NodeKind::TraitDeriveFn
                        )
                        && r.scopes[symbol.scope.0 as usize].assoc_type == Some(ty)
                })
                .map(|symbol| symbol.def_node)
                .ok_or("trait method declaration is missing")?;
            let signature = crate::trait_signatures::signature(r, ast, ty, declaration)?;
            if let Some(inherited) = slots.iter().find(|slot| slot.name == name) {
                if inherited.signature.is_some() {
                    let redeclared = TraitMethodKey {
                        trait_owner: ty,
                        name,
                        signature: Some(signature.clone()),
                    };
                    r.type_pool.check_trait_method_redeclaration(inherited, &redeclared)
                        .map_err(|error| format!(
                            "inherited trait method `{}` signature Self bindings do not match its declaration: {error}",
                            str_interner::get(name)
                        ))?;
                }
            } else {
                slots.push(TraitMethodKey {
                    trait_owner: ty,
                    name,
                    signature: Some(signature),
                });
            }
        }
    } else {
        return Err("trait has no declared dispatch schema".into());
    }
    visiting.remove(&ty);
    let schema = TraitDispatchSchema {
        trait_type: ty,
        slots,
    };
    complete.insert(ty, schema.clone());
    Ok(schema)
}

pub(crate) fn register(r: &mut Resolver<'_>, ast: &Ast) {
    let mut complete = HashMap::new();
    let known = &r.type_pool.well_known;
    let bootstrap = [
        known.eq,
        known.partial_eq,
        known.ord,
        known.partial_ord,
        known.display,
        known.hash,
        known.iterator,
        known.into_iterator,
    ];
    for ty in bootstrap {
        complete.insert(ty, bootstrap_schema(r, ty));
    }
    let mut targets: Vec<_> = r.traits.iter().map(|info| info.type_index).collect();
    targets.extend(
        r.type_pool
            .trait_impls_snapshot()
            .iter()
            .map(|record| record.trait_type),
    );
    targets.sort_unstable_by_key(|ty| ty.as_u32());
    targets.dedup();
    for ty in targets {
        if let Err(message) = schema(r, ast, ty, &mut complete, &mut HashSet::new()) {
            let node = r
                .symbols
                .iter()
                .find(|symbol| symbol.type_index == ty && !symbol.def_node.is_null())
                .map(|symbol| symbol.def_node)
                .unwrap_or(ast.root);
            r.diag_ctx
                .error(message)
                .with_primary_span(ast.node(node).span)
                .emit(r.diag_ctx);
        }
    }
    // Install parents before descendants, including empty inherited schemas.
    let mut pending: Vec<_> = complete.into_values().collect();
    pending.sort_unstable_by_key(|schema| schema.trait_type.as_u32());
    while !pending.is_empty() {
        let ready =
            pending
                .iter()
                .position(|schema| match &r.type_pool.get(schema.trait_type).kind {
                    TypeKind::Trait { parents, .. } => parents
                        .iter()
                        .all(|&parent| r.type_pool.trait_schema(parent).is_some()),
                    _ => false,
                });
        let Some(index) = ready else {
            r.diag_ctx
                .error("trait dispatch schemas have unresolved parent dependencies".into())
                .emit(r.diag_ctx);
            break;
        };
        if let Err(error) = r.type_pool.register_trait_schema(pending.remove(index)) {
            r.diag_ctx
                .error(format!("invalid trait dispatch schema: {error}"))
                .emit(r.diag_ctx);
        }
    }
}

#[cfg(test)]
mod tests {
    use type_pool::{Intrinsic, TraitParameterKind, TraitTypeStep, TypeKind};

    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};

    fn resolve(source: &str) -> (crate::ResolvedAst, Vec<String>) {
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("trait-schema.ns".into()), source.into());
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
    fn one_schema_controls_all_implementation_orders_and_child_overrides() {
        let (resolved, errors) = resolve(
            "trait Base{fn base(self)->i64};trait Child(Base){fn first(self)->i64;fn second(self)->i64};struct A{};struct B{};impl Base for A{fn base(self)->i64{1}};impl Base for B{fn base(self)->i64{2}};impl Child for A{fn second(self)->i64{3};fn base(self)->i64{4};fn first(self)->i64{5}};impl Child for B{fn first(self)->i64{6};fn second(self)->i64{7}}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let child = resolved
            .traits
            .iter()
            .find(|info| info.name == str_interner::intern("Child"))
            .unwrap()
            .type_index;
        let base = resolved
            .traits
            .iter()
            .find(|info| info.name == str_interner::intern("Base"))
            .unwrap()
            .type_index;
        let schema = resolved.type_pool.trait_schema(child).unwrap();
        assert_eq!(
            schema
                .slots
                .iter()
                .map(|slot| str_interner::get(slot.name))
                .collect::<Vec<_>>(),
            ["base", "first", "second"]
        );
        assert_eq!(schema.slots[0].trait_owner, base);
        for table in resolved
            .type_pool
            .vtables_snapshot()
            .iter()
            .filter(|table| table.trait_type == child)
        {
            assert_eq!(table.entries.len(), schema.slots.len());
            let record = resolved
                .type_pool
                .find_trait_impl(table.implementor, child)
                .unwrap();
            for (slot, &entry) in schema.slots.iter().zip(&table.entries) {
                let method = record
                    .methods
                    .iter()
                    .find(|method| method.name == slot.name)
                    .or_else(|| {
                        resolved.type_pool.find_trait_method(
                            table.implementor,
                            slot.trait_owner,
                            slot.name,
                        )
                    })
                    .unwrap();
                assert_eq!(entry, method.func_id);
            }
        }
        resolved.type_pool.validate().unwrap();
    }

    #[test]
    fn empty_unimplemented_and_forward_parent_traits_still_have_schemas() {
        let (resolved, errors) = resolve(
            "trait Child(Parent){};trait Parent{};trait Unused{fn value(self)->i64};struct P{};extend Child for P{};extend Parent for P{}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        for info in &resolved.traits {
            let schema = resolved.type_pool.trait_schema(info.type_index).unwrap();
            assert_eq!(
                schema.slots.len(),
                usize::from(info.name == str_interner::intern("Unused"))
            );
        }
        resolved.type_pool.validate().unwrap();
    }

    #[test]
    fn bootstrap_schemas_are_fixed_and_partial_ord_derivation_is_explicitly_rejected() {
        let (resolved, errors) = resolve(
            "struct P{};extend PartialOrd for P{pub fn partial_cmp(self,other:Self)->i64{0}}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let schema = resolved
            .type_pool
            .trait_schema(resolved.type_pool.well_known.partial_ord)
            .unwrap();
        assert_eq!(schema.slots.len(), 1);
        assert_eq!(schema.slots[0].name, str_interner::intern("partial_cmp"));
        assert_eq!(resolved.type_pool.vtables_snapshot()[0].entries.len(), 1);
        for source in [
            "struct P{};impl Iterator for P{}",
            "struct P{};extend PartialOrd for P{fn cmp(self,other:Self)->i64{0}}",
            "struct P{};derive PartialOrd for P",
            "trait A(B){};trait B(A){}",
        ] {
            let (_, errors) = resolve(source);
            assert!(!errors.is_empty(), "accepted {source}");
        }
    }
    #[test]
    fn bootstrap_contracts_exist_without_implementations_and_preserve_self_paths() {
        let (resolved, errors) = resolve("fn consume(x:Eq){42}");
        assert!(errors.is_empty(), "{errors:?}");
        let pool = &resolved.type_pool;
        for (owner, name, count, ret) in [
            (pool.well_known.eq, "eq", 2, Intrinsic::Bool.type_index()),
            (
                pool.well_known.partial_eq,
                "eq",
                2,
                Intrinsic::Bool.type_index(),
            ),
            (
                pool.well_known.display,
                "to_string",
                1,
                Intrinsic::Str.type_index(),
            ),
        ] {
            let key = pool
                .trait_schema(owner)
                .unwrap()
                .slots
                .iter()
                .find(|key| key.name == str_interner::intern(name))
                .unwrap();
            let signature = key.signature.as_ref().unwrap();
            let TypeKind::Function {
                params,
                ret: actual_ret,
            } = &pool.get(signature.declaration).kind
            else {
                panic!("bootstrap declaration is not a function");
            };
            assert_eq!(params, &vec![owner; count]);
            assert_eq!(*actual_ret, ret);
            assert_eq!(
                signature.self_paths,
                (0..count)
                    .map(|index| vec![TraitTypeStep::Parameter(index as u32)])
                    .collect::<Vec<_>>()
            );
            assert_eq!(signature.parameter_kinds[0], TraitParameterKind::Receiver);
            assert!(
                signature.parameter_kinds[1..]
                    .iter()
                    .all(|kind| *kind == TraitParameterKind::Required)
            );
        }
        for (owner, name, binding, path) in [
            (
                pool.well_known.iterator,
                "next",
                "Item",
                vec![TraitTypeStep::Return, TraitTypeStep::IterationItem],
            ),
            (
                pool.well_known.into_iterator,
                "into_iter",
                "Iter",
                vec![TraitTypeStep::Return],
            ),
        ] {
            let schema = pool.trait_schema(owner).unwrap();
            assert_eq!(schema.slots.len(), 1);
            assert_eq!(schema.slots[0].name, str_interner::intern(name));
            let signature = schema.slots[0].signature.as_ref().unwrap();
            assert_eq!(
                signature.self_paths,
                vec![vec![TraitTypeStep::Parameter(0)]]
            );
            assert_eq!(
                signature.parameter_kinds,
                vec![TraitParameterKind::Receiver]
            );
            assert_eq!(
                signature.associated_paths,
                vec![type_pool::TraitAssociatedPath {
                    trait_owner: owner,
                    name: str_interner::intern(binding),
                    path
                }]
            );
        }
        for owner in [
            pool.well_known.hash,
            pool.well_known.ord,
            pool.well_known.partial_ord,
        ] {
            assert!(
                pool.trait_schema(owner)
                    .unwrap()
                    .slots
                    .iter()
                    .all(|key| key.signature.is_none())
            );
        }
        pool.validate().unwrap();
    }

    #[test]
    fn unused_bootstrap_implementations_are_checked_against_the_contract() {
        for source in [
            "struct P{};impl Display for P{fn to_string(self,extra:i64)->String{\"bad\"}}",
            "struct P{};impl Display for P{fn to_string(self)->bool{true}}",
            "struct P{};impl Display for P{fn to_string(x:i64)->String{\"bad\"}}",
            "struct P{};impl Display for P{fn to_string(self,extra?:i64)->String{\"bad\"}}",
            "struct P{};impl Iterator for P{assoc Item:Type=i64;fn next(self)->i64{42}}",
        ] {
            let (_, errors) = resolve(source);
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains("signature mismatch")
                        || error.contains("parameter kinds")),
                "accepted {source}: {errors:?}"
            );
        }
        let (_, errors) = resolve(
            "struct P{};impl Display for P{fn to_string(self)->String{\"ok\"}};impl Iterator for P{assoc Item:Type=i64;fn next(self)->IterationStep(i64){IterationStep(i64).done}}",
        );
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn excessive_forward_inheritance_is_a_source_diagnostic() {
        let mut source = String::new();
        for index in 0..256 {
            source.push_str(&format!("trait T{index}(T{}){{}};", index + 1));
        }
        source.push_str("trait T256{};fn main(){42}");
        let (_, errors) = resolve(&source);
        assert!(
            errors.iter().any(|error| error.contains("too deep")),
            "{errors:?}"
        );
    }
}
