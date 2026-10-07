//! Checked source apply/update sugar shares declaration argument binding.

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::resolver::Resolver;
use crate::{CallArgumentValue, methods, typing};

pub(crate) fn is_instance(r: &Resolver<'_>, ty: TypeIndex) -> bool {
    r.type_pool.canonical_type(ty).is_some_and(|ty| {
        !matches!(
            r.type_pool.get(ty).kind,
            TypeKind::Function { .. } | TypeKind::Effect { .. }
        ) && !matches!(
            r.type_pool.as_intrinsic(ty),
            Some(Intrinsic::Any | Intrinsic::Type | Intrinsic::Closure | Intrinsic::Continuation)
        )
    })
}

fn bind(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    method: &methods::CheckedMethod,
    arguments: &[NodeIndex],
    rhs: Option<usize>,
) {
    let declaration = r.symbols[method.function.0 as usize].def_node;
    let plan = crate::arguments::plan_method(r, ast, node, declaration, arguments, rhs);
    let mut expected = vec![None; arguments.len()];
    if let Some(plan) = plan {
        for (index, binding) in plan.parameters.iter().enumerate() {
            match &binding.value {
                CallArgumentValue::Explicit { source_index } => {
                    expected[*source_index] = Some(method.parameters[index])
                }
                CallArgumentValue::Variadic { source_indices } => {
                    for &index in source_indices {
                        expected[index] = Some(Intrinsic::Any.type_index());
                    }
                }
                CallArgumentValue::Default { .. } | CallArgumentValue::MapVariadic { .. } => {}
            }
        }
        r.call_arguments.insert(node, plan);
    }
    for (&argument, expected) in arguments.iter().zip(expected) {
        typing::resolve_types_expected(r, ast, argument, expected);
        if let Some(expected) = expected {
            typing::check_expected_type(
                r,
                ast,
                crate::argument_value_node(ast, argument),
                expected,
                "application argument",
            );
        }
    }
}

pub(crate) fn apply(
    r: &mut Resolver<'_>,
    ast: &Ast,
    call: NodeIndex,
    ty: TypeIndex,
) -> Option<TypeIndex> {
    let method = methods::checked_method(r, ast, call, ty, str_interner::intern("apply"))?;
    bind(r, ast, call, &method, ast.multi_children(call), None);
    r.application_calls.insert(call, method.function);
    Some(method.result)
}

pub(crate) fn update(r: &mut Resolver<'_>, ast: &Ast, assignment: NodeIndex) {
    let children = ast.fixed_children(assignment);
    let call = children[0];
    let receiver = ast.fixed_children(call)[0];
    typing::resolve_types(r, ast, receiver);
    let Some(&ty) = r.node_types.get(&receiver) else {
        return;
    };
    if let Some(kind) = crate::collections::CollectionKind::from_type(&r.type_pool, ty) {
        let result = crate::collections::index_call(r, ast, call, kind);
        r.node_types.insert(call, result);
        typing::resolve_types_expected(r, ast, children[1], Some(result));
        return;
    }
    if r.type_pool.as_intrinsic(ty) == Some(Intrinsic::Any) {
        for &argument in ast.multi_children(call) {
            if ast.node(argument).kind == NodeKind::NamedArg {
                r.diag_ctx
                    .error("named update arguments require a statically known method".into())
                    .with_primary_span(ast.node(argument).span)
                    .emit(r.diag_ctx);
            }
            typing::resolve_types(r, ast, argument);
        }
        typing::resolve_types(r, ast, children[1]);
        return;
    }
    if !is_instance(r, ty) {
        r.diag_ctx
            .error("call assignment requires an instance with an update method".into())
            .with_primary_span(ast.node(assignment).span)
            .emit(r.diag_ctx);
        return;
    }
    let Some(method) =
        methods::checked_method(r, ast, assignment, ty, str_interner::intern("update"))
    else {
        return;
    };
    let mut arguments = ast.multi_children(call).to_vec();
    let rhs = arguments.len();
    arguments.push(children[1]);
    bind(r, ast, assignment, &method, &arguments, Some(rhs));
    r.update_calls.insert(assignment, method.function);
}

#[cfg(test)]
mod tests {
    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};

    use super::*;

    #[test]
    fn method_plans_omit_self_keep_source_indices_and_type_rhs_separately() {
        let source = "struct P{}\nimpl P{fn apply(self,x:i8,.scale:i64=1)->i64{x+scale};fn update(self,a:i64,b:i64,value:i64,.scale:i64=1)->String{\"ignored\"}}\nfn main(){let p=P{};let value=p(41);p(b=2,a=40)=0;value}";
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("apply.ns".into()), source.into());
        let diagnostics = DiagnosticContext::new(&map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve(ast, &diagnostics);
        assert!(!diagnostics.has_errors(), "{:?}", diagnostics.diagnostics());
        assert_eq!(resolved.application_calls.len(), 1);
        let (&call, _) = resolved.application_calls.iter().next().unwrap();
        assert_eq!(
            resolved.type_pool.as_intrinsic(resolved.node_types[&call]),
            Some(Intrinsic::I64)
        );
        assert_eq!(resolved.call_arguments[&call].parameters.len(), 2);
        let operand = resolved.ast.multi_children(call)[0];
        assert_eq!(
            resolved
                .type_pool
                .as_intrinsic(resolved.node_types[&operand]),
            Some(Intrinsic::I8)
        );
        assert_eq!(resolved.update_calls.len(), 1);
        let (&assignment, _) = resolved.update_calls.iter().next().unwrap();
        assert_eq!(
            resolved
                .type_pool
                .as_intrinsic(resolved.node_types[&assignment]),
            Some(Intrinsic::Unit)
        );
        let plan = &resolved.call_arguments[&assignment];
        assert_eq!(plan.parameters.len(), 4);
        for (binding, index) in plan.parameters.iter().take(3).zip([1, 0, 2]) {
            assert_eq!(
                binding.value,
                CallArgumentValue::Explicit {
                    source_index: index
                }
            );
        }
        assert!(matches!(
            plan.parameters[3].value,
            CallArgumentValue::Default { .. }
        ));
    }
}
