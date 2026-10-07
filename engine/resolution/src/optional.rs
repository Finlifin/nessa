//! Null-or-payload Optional operations and their callable control boundary.

use ast::{Ast, NodeIndex};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

use crate::{resolver::Resolver, typing};

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: &str) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}

pub(crate) fn inner(r: &Resolver<'_>, ty: TypeIndex) -> Option<TypeIndex> {
    let ty = r.type_pool.canonical_type(ty)?;
    match r.type_pool.get(ty).kind {
        TypeKind::Optional { inner } => Some(inner),
        TypeKind::Intrinsic(Intrinsic::Any) => Some(ty),
        _ => None,
    }
}

pub(crate) fn propagate(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) -> Option<TypeIndex> {
    let operand = ast.fixed_children(node)[0];
    typing::resolve_types(r, ast, operand);
    let ty = r.node_types.get(&operand).copied()?;
    let Some(payload) = inner(r, ty) else {
        report(
            r,
            ast,
            node,
            "Optional propagation requires an Optional or Any operand",
        );
        return None;
    };
    if let Some(expected) = r.return_types.last().copied().flatten()
        && !r.type_pool.is_subtype(r.type_pool.null_type(), expected)
        && r.type_pool.as_intrinsic(expected) != Some(Intrinsic::Any)
    {
        report(
            r,
            ast,
            node,
            "Optional propagation requires an Optional or Any callable result",
        );
    }
    let null = r.type_pool.null_type();
    if let Some(returns) = r.inferred_returns.last_mut() {
        returns.push(null);
    }
    Some(payload)
}

pub(crate) fn pattern(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex, expected: TypeIndex) {
    let Some(payload) = inner(r, expected) else {
        report(
            r,
            ast,
            node,
            "some pattern requires an Optional or Any input",
        );
        return;
    };
    crate::enums::pattern(r, ast, ast.fixed_children(node)[0], Some(payload));
}

/// Optional's intrinsic applies only to a static Optional. An Any receiver and
/// ordinary source-defined unwrap methods keep their existing dispatch rules.
pub(crate) fn unwrap_signature(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    receiver: TypeIndex,
) -> Option<TypeIndex> {
    let receiver = r.type_pool.canonical_type(receiver)?;
    let TypeKind::Optional { inner } = r.type_pool.get(receiver).kind else {
        return None;
    };
    let member = ast.fixed_children(node)[1];
    if str_interner::get(ast.node(member).str_id) != "unwrap" {
        return None;
    }
    if r.current_call_callee != Some(node) {
        report(
            r,
            ast,
            node,
            "bound Optional unwrap values are not implemented; call unwrap directly",
        );
        return None;
    }
    Some(r.register_type(TypeKind::Function {
        params: Vec::new(),
        ret: inner,
    }))
}

pub(crate) fn join(r: &mut Resolver<'_>, left: TypeIndex, right: TypeIndex) -> TypeIndex {
    let left = r.type_pool.canonical_type(left).unwrap_or(left);
    let right = r.type_pool.canonical_type(right).unwrap_or(right);
    if left == right {
        return left;
    }
    if r.type_pool.as_intrinsic(left) == Some(Intrinsic::NoReturn) {
        return right;
    }
    if r.type_pool.as_intrinsic(right) == Some(Intrinsic::NoReturn) {
        return left;
    }
    let left_inner = match r.type_pool.get(left).kind {
        TypeKind::Optional { inner } => Some(inner),
        _ => None,
    };
    let right_inner = match r.type_pool.get(right).kind {
        TypeKind::Optional { inner } => Some(inner),
        _ => None,
    };
    if left_inner.is_some() || right_inner.is_some() {
        let left = left_inner.unwrap_or(left);
        let right = right_inner.unwrap_or(right);
        let inner = if r.type_pool.as_intrinsic(left) == Some(Intrinsic::NoReturn) {
            right
        } else if r.type_pool.as_intrinsic(right) == Some(Intrinsic::NoReturn) || left == right {
            left
        } else {
            r.type_pool
                .numeric_lift(left, right)
                .unwrap_or(Intrinsic::Any.type_index())
        };
        return r.register_type(TypeKind::Optional { inner });
    }
    r.type_pool
        .numeric_lift(left, right)
        .unwrap_or(Intrinsic::Any.type_index())
}

#[cfg(test)]
mod tests {
    use type_pool::{Intrinsic, TypeKind};

    fn resolve(source: &str) -> (crate::ResolvedAst, Vec<String>) {
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = rustc_span::SourceMap::new(rustc_span::source_map::FilePathMapping::empty());
        let file = map.new_source_file(
            rustc_span::FileName::Custom("optional.ns".into()),
            source.into(),
        );
        let diagnostics = diagnostic::DiagnosticContext::new(&map);
        let ast = parser::Parser::new(&tokens, source, &diagnostics, file.start_pos).parse();
        let resolved = crate::resolve(ast, &diagnostics);
        let errors = diagnostics
            .diagnostics()
            .iter()
            .map(|error| error.message.clone())
            .collect();
        (resolved, errors)
    }

    fn result(resolved: &crate::ResolvedAst, name: &str) -> type_pool::TypeIndex {
        let symbol = resolved
            .symbols
            .iter()
            .find(|symbol| {
                symbol.kind == crate::SymbolKind::Function && str_interner::get(symbol.name) == name
            })
            .unwrap();
        let TypeKind::Function { ret, .. } = resolved.type_pool.get(symbol.type_index).kind else {
            panic!("missing function signature")
        };
        ret
    }

    #[test]
    fn implicit_null_exit_preserves_payload_and_bottom_facts() {
        for (source, payload) in [
            ("fn get()->?i32{42};fn read(){get()?}", Intrinsic::I32),
            ("fn read(){null?}", Intrinsic::NoReturn),
            (
                "typealias Maybe=?i8;fn get()->Maybe{42};fn read(){get()?}",
                Intrinsic::I8,
            ),
            ("fn get()->Any{42};fn read(){get()?}", Intrinsic::Any),
            (
                "fn get()->?i8{42};fn read(){let n=get()?;if true{n}else{1000}}",
                Intrinsic::I64,
            ),
        ] {
            let (resolved, errors) = resolve(source);
            assert!(errors.is_empty(), "{source}: {errors:?}");
            let TypeKind::Optional { inner } =
                resolved.type_pool.get(result(&resolved, "read")).kind
            else {
                panic!("{source}: missing Optional result")
            };
            assert_eq!(inner, payload.type_index(), "{source}");
        }
    }

    #[test]
    fn non_null_operations_publish_inner_types() {
        for source in [
            "fn get()->?i8{42};fn read(){get().unwrap()}",
            "fn get()->?i8{42};fn read(){get() match {x?=>x,_=>0.as(i8)}}",
        ] {
            let (resolved, errors) = resolve(source);
            assert!(errors.is_empty(), "{source}: {errors:?}");
            assert_eq!(result(&resolved, "read"), Intrinsic::I8.type_index());
        }
    }

    #[test]
    fn propagation_rejects_operand_result_and_default_boundary_errors() {
        for (source, fragment) in [
            ("fn read(){42?}", "requires an Optional or Any operand"),
            (
                "fn get()->?i64{42};fn read()->i64{get()?}",
                "requires an Optional or Any callable result",
            ),
            (
                "fn get()->?i64{42};fn read(.x:i64=get()?){x}",
                "cannot cross a parameter default boundary",
            ),
            (
                "fn get()->?i64{42};let x=get()?",
                "only allowed inside a function",
            ),
        ] {
            let (_, errors) = resolve(source);
            assert!(
                errors.iter().any(|error| error.contains(fragment)),
                "{source}: {errors:?}"
            );
        }
        let (_, errors) = resolve("fn get()->?i64{42};fn read(.f:fn()->?i64=||get()?){f()}");
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn some_pattern_bindings_follow_inner_and_private_scopes() {
        let (resolved, errors) = resolve("fn read(value:?i8){value match {x?=>x,_=>0.as(i8)}}");
        assert!(errors.is_empty(), "{errors:?}");
        let x = resolved
            .symbols
            .iter()
            .find(|symbol| str_interner::get(symbol.name) == "x")
            .unwrap();
        assert_eq!(x.type_index, Intrinsic::I8.type_index());
        for (source, fragment) in [
            (
                "fn read(value:i64){value match {x?=>x,_=>0}}",
                "some pattern requires an Optional or Any input",
            ),
            (
                "fn read(value:?i64){value match {not x?=>x,_=>0}}",
                "undefined name `x`",
            ),
            (
                "fn read(value:?i64){let x?=value;x}",
                "some patterns are supported",
            ),
        ] {
            let (_, errors) = resolve(source);
            assert!(
                errors.iter().any(|error| error.contains(fragment)),
                "{source}: {errors:?}"
            );
        }
    }
    #[test]
    fn noreturn_payload_does_not_erase_a_completing_result() {
        let (resolved, errors) = resolve("fn read(){if true{42}else{null.unwrap()}}");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(result(&resolved, "read"), Intrinsic::I64.type_index());
    }
}
