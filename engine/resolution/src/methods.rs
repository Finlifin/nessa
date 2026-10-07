//! Source method signatures for operators and statically bound instance calls.

use ast::{Ast, NodeIndex, NodeKind};
use str_interner::StrId;
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::resolver::Resolver;
use crate::{SymbolId, SymbolKind, typing};

pub(crate) struct CheckedMethod {
    pub(crate) function: SymbolId,
    pub(crate) parameters: Vec<TypeIndex>,
    pub(crate) result: TypeIndex,
}

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: impl Into<String>) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}

/// Prefer the child's compatible declaration; its inherited slot contract is
/// validated by schema construction. Parent order matches schema name dedup,
/// and diamond inheritance visits each declaration once.
fn trait_member_owner(
    r: &Resolver<'_>,
    receiver: TypeIndex,
    name: StrId,
) -> Result<Option<TypeIndex>, String> {
    let mut pending = vec![(receiver, 0usize)];
    let mut visited = std::collections::HashSet::new();
    while let Some((owner, depth)) = pending.pop() {
        if depth >= 256 {
            return Err("trait inheritance is too deep to select a method".into());
        }
        let owner = r
            .type_pool
            .canonical_type(owner)
            .ok_or("invalid trait receiver")?;
        if !visited.insert(owner) {
            continue;
        }
        if r.scopes.iter().any(|candidate| {
            candidate.assoc_type == Some(owner) && candidate.bindings.contains_key(&name)
        }) {
            return Ok(Some(owner));
        }
        if let TypeKind::Trait { parents, .. } = &r.type_pool.get(owner).kind {
            pending.extend(parents.iter().rev().map(|&parent| (parent, depth + 1)));
        }
    }
    Ok(None)
}

fn trait_member(
    r: &Resolver<'_>,
    receiver: TypeIndex,
    name: StrId,
    scope: crate::ScopeId,
) -> Result<SymbolId, String> {
    let owner = trait_member_owner(r, receiver, name)?
        .ok_or_else(|| format!("unknown member `{}`", str_interner::get(name)))?;
    crate::associated::member_from(r, owner, name, scope)
}

pub(crate) fn checked_method(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    receiver: TypeIndex,
    name: StrId,
) -> Option<CheckedMethod> {
    let scope = r.node_scopes.get(&node).copied().unwrap_or(r.current_scope);
    let trait_receiver = r
        .type_pool
        .canonical_type(receiver)
        .filter(|&ty| matches!(r.type_pool.get(ty).kind, TypeKind::Trait { .. }));
    let internal_default = r.default_body_context.as_ref().is_some_and(|context| {
        let receiver_node = ast.fixed_children(node).first().copied().unwrap_or(node);
        context
            .self_value_paths
            .get(&receiver_node)
            .is_some_and(|paths| paths.iter().any(Vec::is_empty))
    });
    if !internal_default
        && trait_receiver.is_some_and(|ty| crate::associated_types::has_declarations(r, ty))
    {
        report(
            r,
            ast,
            node,
            "dynamic trait methods with associated types have no associated return proof contract",
        );
        return None;
    }
    let selected = if let Some(receiver) = trait_receiver {
        trait_member(r, receiver, name, scope)
    } else {
        crate::associated::member_from(r, receiver, name, scope).or_else(|error| {
            crate::default_methods::source_member(r, ast, receiver, name, scope)?.ok_or(error)
        })
    };
    let function = match selected {
        Ok(function) => function,
        Err(error) => {
            report(r, ast, node, error);
            return None;
        }
    };
    let symbol = &r.symbols[function.0 as usize];
    if symbol.kind != SymbolKind::Function || symbol.def_node.is_null() {
        report(
            r,
            ast,
            node,
            "instance operator/member requires a source method function",
        );
        return None;
    }
    if ast
        .multi_children(symbol.def_node)
        .first()
        .is_none_or(|&parameter| ast.node(parameter).kind != NodeKind::ParamSelf)
    {
        report(
            r,
            ast,
            node,
            "instance operator/member requires a self parameter",
        );
        return None;
    }
    let declaration = symbol.def_node;
    if matches!(
        ast.node(declaration).kind,
        NodeKind::FunctionDef | NodeKind::TraitDeriveFn
    ) && ast.fixed_children(declaration)[1].is_null()
        && !r.function_inference.bodies.contains_key(&declaration)
    {
        // Static receiver dependencies are scheduled before consumers. This
        // fallback also handles methods introduced by contextual trait replay;
        // the shared body cache bounds recursive gradual calls in that context.
        crate::inference::resolve_function_dependencies(r, ast, declaration);
    }
    let mut signature = r
        .type_pool
        .canonical_type(r.symbols[function.0 as usize].type_index)?;
    if let Some(receiver) = trait_receiver.or_else(|| {
        (ast.node(declaration).kind == NodeKind::TraitDeriveFn
            || crate::ordering::default_template(r, name) == Some(function))
        .then_some(receiver)
    }) {
        let owner = r.scopes[r.symbols[function.0 as usize].scope.0 as usize].assoc_type?;
        let key = type_pool::TraitMethodKey {
            trait_owner: owner,
            name,
            signature: match crate::trait_signatures::signature(r, ast, owner, declaration) {
                Ok(signature) => Some(signature),
                Err(error) => {
                    report(r, ast, node, error);
                    return None;
                }
            },
        };
        let specialized = if internal_default && let Some(context) = &r.default_body_context {
            r.type_pool.instantiate_trait_method_signature_in_impl(
                &key,
                context.implementor,
                context.implementation_trait,
                context.scope,
            )
        } else if trait_receiver.is_none() && crate::associated_types::has_declarations(r, owner) {
            let record = r
                .type_pool
                .find_trait_impl_scoped(receiver, owner, scope.0)
                .ok()
                .flatten()
                .map(|record| (record.trait_type, record.visible_scope));
            if let Some((actual_trait, actual_scope)) = record {
                r.type_pool.instantiate_trait_method_signature_in_impl(
                    &key,
                    receiver,
                    actual_trait,
                    actual_scope,
                )
            } else {
                Err(type_pool::TraitSignatureError::MissingAssociatedBinding)
            }
        } else {
            r.type_pool
                .instantiate_trait_method_signature(&key, receiver)
        };
        signature = match specialized {
            Ok(signature) => signature,
            Err(error) => {
                report(
                    r,
                    ast,
                    node,
                    format!("cannot specialize trait method signature: {error}"),
                );
                return None;
            }
        };
    }
    let TypeKind::Function { params, ret } = &r.type_pool.get(signature).kind else {
        report(
            r,
            ast,
            node,
            "instance method requires a function signature",
        );
        return None;
    };
    if params
        .first()
        .and_then(|&ty| r.type_pool.canonical_type(ty))
        != r.type_pool.canonical_type(if internal_default {
            r.default_body_context
                .as_ref()
                .map(|context| context.implementor)
                .unwrap_or(receiver)
        } else {
            receiver
        })
    {
        report(
            r,
            ast,
            node,
            "instance method receiver does not match its owning type",
        );
        return None;
    }
    Some(CheckedMethod {
        function,
        parameters: params[1..].to_vec(),
        result: *ret,
    })
}

pub(crate) fn concat(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) -> Option<TypeIndex> {
    let operands = ast.fixed_children(node);
    typing::resolve_types(r, ast, operands[0]);
    let receiver = r.node_types.get(&operands[0]).copied()?;
    if r.type_pool.as_intrinsic(receiver) == Some(Intrinsic::Any) {
        typing::resolve_types(r, ast, operands[1]);
        return Some(Intrinsic::Any.type_index());
    }
    let method = checked_method(r, ast, node, receiver, str_interner::intern("concat"))?;
    let declaration = r.symbols[method.function.0 as usize].def_node;
    if method.parameters.len() != 1
        || ast
            .multi_children(declaration)
            .get(1)
            .is_none_or(|&parameter| {
                matches!(
                    ast.node(parameter).kind,
                    NodeKind::ParamOptional | NodeKind::ParamVarargs | NodeKind::ParamSelf
                )
            })
    {
        report(
            r,
            ast,
            node,
            "++ requires concat(self, other) with one fixed operand parameter",
        );
        return None;
    }
    let expected = method.parameters[0];
    typing::resolve_types_expected(r, ast, operands[1], Some(expected));
    typing::check_expected_type(r, ast, operands[1], expected, "concat operand");
    r.concat_calls.insert(node, method.function);
    Some(method.result)
}

pub(crate) fn projection(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    receiver: TypeIndex,
) -> Option<TypeIndex> {
    let name = ast.node(ast.fixed_children(node)[1]).str_id;
    if let Some(owner) = r.type_pool.canonical_type(receiver)
        && matches!(r.type_pool.get(owner).kind, TypeKind::Trait { .. })
        && matches!(trait_member_owner(r, owner, name), Ok(None))
        && let Some(key) = crate::trait_schemas::bootstrap_method(r, owner, name)
    {
        if r.current_call_callee != Some(node) {
            report(
                r,
                ast,
                node,
                "bound method values are not implemented; call the method directly",
            );
            return None;
        }
        let signature = match r.type_pool.instantiate_trait_method_signature(&key, owner) {
            Ok(signature) => signature,
            Err(error) => {
                report(
                    r,
                    ast,
                    node,
                    format!("cannot specialize bootstrap method signature: {error}"),
                );
                return None;
            }
        };
        let TypeKind::Function { params, ret } = r.type_pool.get(signature).kind.clone() else {
            return None;
        };
        return Some(r.register_type(TypeKind::Function {
            params: params[1..].to_vec(),
            ret,
        }));
    }
    let method = checked_method(r, ast, node, receiver, name)?;
    if r.current_call_callee != Some(node) {
        report(
            r,
            ast,
            node,
            "bound method values are not implemented; call the method directly",
        );
        return None;
    }
    r.instance_methods.insert(node, method.function);
    Some(r.register_type(TypeKind::Function {
        params: method.parameters,
        ret: method.result,
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
        let file = map.new_source_file(FileName::Custom("concat.ns".into()), source.into());
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
    fn bootstrap_trait_methods_use_declared_results_and_inherited_self_views() {
        for source in [
            "fn equal(a:Eq,b:Eq)->bool{a.eq(b)}",
            "fn equal(a:PartialEq,b:PartialEq)->bool{a.eq(b)}",
            "fn display(value:Display)->String{value.to_string()}",
            "trait Child(Eq){};fn equal(a:Child,b:Child)->bool{a.eq(b)}",
        ] {
            let (_, errors) = resolve(source);
            assert!(errors.is_empty(), "{source}: {errors:?}");
        }
        for source in [
            "fn display(value:Display)->bool{value.to_string()}",
            "fn equal(a:Eq)->bool{a.eq(true)}",
            "fn equal(a:Eq)->bool{a.eq()}",
            "trait Child(Eq){};fn equal(a:Child,b:Eq)->bool{a.eq(b)}",
            "fn display(value:Display){let method=value.to_string;method}",
        ] {
            let (_, errors) = resolve(source);
            assert!(!errors.is_empty(), "accepted {source}");
        }
    }

    #[test]
    fn omitted_result_annotation_is_inferred_before_and_after_caller() {
        for source in [
            "struct P{x:i64}\nfn main(){let p=P{x:40};p++2}\nimpl P{fn concat(self,other:i64){self.x+other}}",
            "struct P{x:i64}\nimpl P{fn concat(self,other:i64){self.x+other}}\nfn main(){let p=P{x:40};p++2}",
        ] {
            let (resolved, errors) = resolve(source);
            assert!(errors.is_empty(), "{errors:?}");
            assert_eq!(resolved.concat_calls.len(), 1);
            let (&node, &function) = resolved.concat_calls.iter().next().unwrap();
            assert_eq!(
                resolved.type_pool.as_intrinsic(resolved.node_types[&node]),
                Some(Intrinsic::I64)
            );
            let TypeKind::Function { params, ret } = &resolved
                .type_pool
                .get(resolved.symbols[function.0 as usize].type_index)
                .kind
            else {
                panic!("method")
            };
            assert_eq!(params.len(), 2);
            assert_eq!(resolved.type_pool.as_intrinsic(*ret), Some(Intrinsic::I64));
        }
        let (_, errors) = resolve(
            "struct P{}\nfn main(){let p=P{};let wrong:i64=p++2;wrong}\nimpl P{fn concat(self,other:i64){true}}",
        );
        assert!(
            errors.iter().any(|error| error.contains("type mismatch")),
            "{errors:?}"
        );
    }

    #[test]
    fn recursive_unannotated_method_check_is_bounded_and_retains_gradual_result() {
        let (resolved, errors) = resolve(
            "struct P{}\nimpl P{fn concat(self,other:i64){self++other}}\nfn main(){let p=P{};p++42}",
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(resolved.concat_calls.len(), 2);
        for node in resolved.concat_calls.keys() {
            assert_eq!(
                resolved.type_pool.as_intrinsic(resolved.node_types[node]),
                Some(Intrinsic::Any)
            );
        }
    }
}
