//! Tuple shapes shared by construction, numeric projection and declaration binding.

use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{TypeIndex, TypeKind};

use crate::resolver::Resolver;
use crate::typing;

fn report(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, message: impl Into<String>) {
    r.diag_ctx
        .error(message.into())
        .with_primary_span(ast.node(node).span)
        .emit(r.diag_ctx);
}

fn elements(r: &Resolver<'_>, ty: TypeIndex) -> Option<Vec<TypeIndex>> {
    let ty = r.type_pool.canonical_type(ty)?;
    match &r.type_pool.get(ty).kind {
        TypeKind::Tuple { elements } => Some(elements.clone()),
        _ => None,
    }
}

pub(crate) fn literal(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    expected: Option<TypeIndex>,
) -> Option<TypeIndex> {
    let children = ast.multi_children(node);
    let shape = expected.and_then(|ty| elements(r, ty));
    let mut inferred = Vec::with_capacity(children.len());
    for (index, &child) in children.iter().enumerate() {
        let target = shape.as_ref().and_then(|shape| shape.get(index)).copied();
        typing::resolve_types_expected(r, ast, child, target);
        if let Some(target) = target {
            typing::check_expected_type(
                r,
                ast,
                child,
                target,
                &format!("tuple element {}", index + 1),
            );
        }
        inferred.push(
            r.node_types
                .get(&child)
                .copied()
                .unwrap_or(TypeIndex::INVALID),
        );
    }
    if let Some(shape) = shape {
        if shape.len() != children.len() {
            report(
                r,
                ast,
                node,
                format!(
                    "tuple has {} elements, but expected {}",
                    children.len(),
                    shape.len()
                ),
            );
        } else {
            // Element coercions establish this target layout before allocation.
            return expected.and_then(|ty| r.type_pool.canonical_type(ty));
        }
    }
    Some(r.register_type(TypeKind::Tuple { elements: inferred }))
}

pub(crate) fn projection(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) -> Option<TypeIndex> {
    let children = ast.fixed_children(node);
    let member = children[1];
    let text = str_interner::get(ast.node(member).str_id);
    let index = text.parse::<u32>().ok();
    let Some(index) = index else {
        report(
            r,
            ast,
            member,
            "tuple index must be a nonnegative decimal integer within u32",
        );
        return None;
    };
    let Some(shape) = r
        .node_types
        .get(&children[0])
        .copied()
        .and_then(|ty| elements(r, ty))
    else {
        report(
            r,
            ast,
            node,
            "numeric projection requires a statically known tuple type",
        );
        return None;
    };
    let Some(&ty) = shape.get(index as usize) else {
        report(
            r,
            ast,
            member,
            format!(
                "tuple index {index} is out of range for {} elements",
                shape.len()
            ),
        );
        return None;
    };
    r.node_field_indices.insert(node, index);
    Some(ty)
}

pub(crate) fn pattern(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    expected: Option<TypeIndex>,
    resolve_child: fn(&mut Resolver<'_>, &Ast, NodeIndex, Option<TypeIndex>),
) {
    let Some(ty) = expected else {
        report(
            r,
            ast,
            node,
            "tuple destructuring requires an explicit or inferred tuple type",
        );
        return;
    };
    let Some(shape) = elements(r, ty) else {
        report(
            r,
            ast,
            node,
            "tuple destructuring requires a statically known tuple type",
        );
        return;
    };
    r.node_types.insert(node, ty);
    let children = ast.multi_children(node);
    if children.len() != shape.len() {
        report(
            r,
            ast,
            node,
            format!(
                "tuple pattern has {} elements, but value has {}",
                children.len(),
                shape.len()
            ),
        );
    }
    for (&child, ty) in children.iter().zip(shape) {
        if !matches!(
            ast.node(child).kind,
            NodeKind::Id | NodeKind::Underscore | NodeKind::PatternTuple
        ) {
            report(
                r,
                ast,
                child,
                "tuple declaration bindings require identifiers, underscores or nested tuple bindings",
            );
            continue;
        }
        r.node_types.insert(child, ty);
        resolve_child(r, ast, child, Some(ty));
    }
}

#[cfg(test)]
mod tests {
    use ast::NodeKind;
    use diagnostic::DiagnosticContext;
    use rustc_span::{FileName, SourceMap, source_map::FilePathMapping};
    use type_pool::Intrinsic;

    use super::*;

    fn resolve(source: &str) -> (crate::ResolvedAst, Vec<String>) {
        let (tokens, errors) = lexer::tokenize(source);
        assert!(errors.is_empty(), "{errors:?}");
        let map = SourceMap::new(FilePathMapping::empty());
        let file = map.new_source_file(FileName::Custom("tuple.ns".into()), source.into());
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
    fn expected_alias_layout_and_element_coercions_are_preserved() {
        let (resolved, errors) = resolve(
            "typealias Pair = (i8, bool)\nfn main(x: Any) { let t: Pair = (x, true); t.0 }",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let tuple = resolved
            .ast
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.kind == NodeKind::Tuple)
            .map(|(index, _)| NodeIndex(index as u32))
            .next_back()
            .unwrap();
        let ty = resolved.node_types[&tuple];
        let TypeKind::Tuple { elements } = &resolved.type_pool.get(ty).kind else {
            panic!("tuple layout");
        };
        assert_eq!(
            elements,
            &[Intrinsic::I8.type_index(), Intrinsic::Bool.type_index()]
        );
        let value = resolved.ast.multi_children(tuple)[0];
        assert_eq!(
            resolved.node_coercions[&value].target,
            Intrinsic::I8.type_index()
        );
        assert_eq!(
            resolved
                .node_field_indices
                .values()
                .copied()
                .collect::<Vec<_>>(),
            vec![0]
        );
    }

    #[test]
    fn nested_declarations_and_parameters_bind_element_types() {
        let (resolved, errors) = resolve(
            "fn sum((a, (b, flag)): (i64, (i8, bool))) -> i64 { a + b }\nfn main() { var (x, y) = (1, true); let (n, (m, ok)) = (2, (3, false)); sum((n, (4, ok))) }",
        );
        assert!(errors.is_empty(), "{errors:?}");
        for (name, expected) in [
            ("a", Intrinsic::I64),
            ("b", Intrinsic::I8),
            ("flag", Intrinsic::Bool),
            ("y", Intrinsic::Bool),
            ("ok", Intrinsic::Bool),
        ] {
            let name = str_interner::intern(name);
            assert!(resolved.symbols.iter().any(|symbol| symbol.name == name && symbol.type_index == expected.type_index()));
        }
        let function = resolved
            .symbols
            .iter()
            .find(|symbol| symbol.name == str_interner::intern("sum"))
            .unwrap();
        let TypeKind::Function { params, .. } = &resolved.type_pool.get(function.type_index).kind
        else {
            panic!("signature");
        };
        assert!(matches!(
            resolved.type_pool.get(params[0]).kind,
            TypeKind::Tuple { .. }
        ));
    }

    #[test]
    fn invalid_shapes_and_unsupported_consumers_are_explicit_errors() {
        for (source, message) in [
            ("fn main() { (1, true).2 }", "out of range"),
            ("fn main(x: Any) { x.0 }", "statically known tuple"),
            ("fn main() { let (a, b) = (1, 2, 3) }", "tuple pattern has"),
            (
                "fn main(x: Any) { let (a, b) = x }",
                "statically known tuple",
            ),
            ("fn f((a, b)) {}", "tuple destructuring requires"),
            (
                "fn main() { let t: (i64, bool) = (1, 2) }",
                "tuple element 2",
            ),
            (
                "let (a, b) = (1, 2)\nfn main() {}",
                "module value destructuring",
            ),
            (
                "fn main() { let (1, b) = (1, 2) }",
                "tuple declaration bindings require",
            ),
            (
                "fn main() { let t = (1, true); t.0 = false }",
                "type mismatch in assignment",
            ),
        ] {
            let (_, errors) = resolve(source);
            assert!(
                errors.iter().any(|error| error.contains(message)),
                "{source}: {errors:?}"
            );
        }
    }
}
