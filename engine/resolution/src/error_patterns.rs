//! Qualified branch patterns and residual-value elimination.
use crate::{
    ErrorConversionKind, ErrorConversionPlan, ErrorEliminationPlan, ErrorPatternBranch,
    ErrorPatternPlan, errors, resolver::Resolver, typing,
};
use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{ErrorDomain, Intrinsic, TypeIndex, TypeKind};

fn family(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex) -> Option<TypeIndex> {
    if node.is_null() {
        return None;
    }
    match ast.node(node).kind {
        NodeKind::PatternTypeFamily => {
            typing::resolve_type_expr_inner(r, ast, ast.fixed_children(node)[0])
        }
        NodeKind::PatternAsBind | NodeKind::PatternIfGuard | NodeKind::PatternError => {
            family(r, ast, ast.fixed_children(node)[0])
        }
        NodeKind::PatternCall => crate::enums::descriptor(r, ast, ast.fixed_children(node)[0])
            .map(|(reference, _)| reference.type_index),
        NodeKind::Id | NodeKind::PropertyPattern | NodeKind::Projection => {
            crate::enums::descriptor(r, ast, node).map(|(reference, _)| reference.type_index)
        }
        NodeKind::PatternOr => {
            let children = ast.fixed_children(node);
            let a = family(r, ast, children[0]);
            let b = family(r, ast, children[1]);
            if a == b { a } else { None }
        }
        _ => None,
    }
}
fn payload(r: &mut Resolver<'_>, ast: &Ast, node: NodeIndex, domain: &ErrorDomain) -> TypeIndex {
    if matches!(ast.node(node).kind, NodeKind::Id | NodeKind::Underscore)
        && family(r, ast, node).is_none()
    {
        return Intrinsic::Any.type_index();
    }
    family(r, ast, node).unwrap_or_else(|| match domain {
        ErrorDomain::Closed(errors) if errors.len() == 1 => errors[0],
        _ => Intrinsic::Any.type_index(),
    })
}
pub(crate) fn qualified_pattern(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    input: TypeIndex,
) {
    let Some(shape) = r.type_pool.error_shape(input).ok().flatten() else {
        errors::report(
            r,
            ast,
            node,
            "Error branch pattern requires an Error-qualified input",
        );
        return;
    };
    let ok = ast.node(node).kind == NodeKind::PatternErrorOk;
    let child = ast.fixed_children(node)[0];
    let ty = if ok {
        shape.inner
    } else {
        payload(r, ast, child, &shape.domain)
    };
    let family = if ok { None } else { family(r, ast, child) };
    if let (Some(family), ErrorDomain::Closed(members)) = (family, &shape.domain)
        && !members
            .iter()
            .any(|member| r.type_pool.canonical_type(*member) == r.type_pool.canonical_type(family))
    {
        errors::report(
            r,
            ast,
            node,
            "error pattern family is not in the input Error set",
        );
    }
    r.error_patterns.insert(
        node,
        ErrorPatternPlan {
            branch: if ok {
                ErrorPatternBranch::Ok
            } else {
                ErrorPatternBranch::Error
            },
            payload_type: ty,
            type_test: family,
        },
    );
    crate::enums::pattern(r, ast, child, Some(ty));
}
pub(crate) fn family_pattern(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    expected: TypeIndex,
) {
    let Some(ty) = family(r, ast, node).and_then(|ty| r.type_pool.canonical_type(ty)) else {
        errors::report(r, ast, node, "family pattern requires a concrete type path");
        return;
    };
    if matches!(
        r.type_pool.get(ty).kind,
        TypeKind::Trait { .. }
            | TypeKind::Module { .. }
            | TypeKind::Effect { .. }
            | TypeKind::EffectQualified { .. }
    ) || matches!(
        r.type_pool.as_intrinsic(ty),
        Some(Intrinsic::Any | Intrinsic::NoReturn)
    ) {
        errors::report(
            r,
            ast,
            node,
            "family pattern requires a concrete payload type",
        );
        return;
    }
    if expected != Intrinsic::Any.type_index() && r.type_pool.canonical_type(expected) != Some(ty) {
        errors::report(
            r,
            ast,
            node,
            "family pattern does not match input payload type",
        );
    }
    r.node_types.insert(node, ty);
}
fn has_error_branch(ast: &Ast, node: NodeIndex) -> bool {
    if node.is_null() {
        return false;
    }
    match ast.node(node).kind {
        NodeKind::PatternErrorOk => false,
        NodeKind::PatternAsBind | NodeKind::PatternIfGuard => {
            has_error_branch(ast, ast.fixed_children(node)[0])
        }
        NodeKind::PatternOr => ast
            .fixed_children(node)
            .iter()
            .any(|&child| has_error_branch(ast, child)),
        _ => true,
    }
}
fn has_explicit_branch(ast: &Ast, node: NodeIndex) -> bool {
    if node.is_null() {
        return false;
    }
    match ast.node(node).kind {
        NodeKind::PatternErrorOk | NodeKind::PatternError => true,
        NodeKind::PatternAsBind
        | NodeKind::PatternIfGuard
        | NodeKind::PatternNot
        | NodeKind::PatternAndIs => has_explicit_branch(ast, ast.fixed_children(node)[0]),
        NodeKind::PatternOr => ast
            .fixed_children(node)
            .iter()
            .any(|&child| has_explicit_branch(ast, child)),
        _ => false,
    }
}
/// In elimination, unwrapped payload alternatives select Err; explicit branch
/// alternatives keep the wrapper until their own checked entry point.
fn elimination_pattern(
    r: &mut Resolver<'_>,
    ast: &Ast,
    pattern: NodeIndex,
    input: TypeIndex,
    domain: &ErrorDomain,
    depth: usize,
) {
    if depth >= 256 {
        errors::report(r, ast, pattern, "Error pattern nesting exceeds 256 levels");
        return;
    }
    let children = ast.fixed_children(pattern);
    if has_explicit_branch(ast, pattern) {
        r.node_types.insert(pattern, input);
        match ast.node(pattern).kind {
            NodeKind::PatternOr => {
                for &child in children {
                    elimination_pattern(r, ast, child, input, domain, depth + 1);
                }
                crate::pattern_bindings::check_alternative_types(r, ast, pattern);
            }
            NodeKind::PatternIfGuard => {
                elimination_pattern(r, ast, children[0], input, domain, depth + 1);
                let boolean = Intrinsic::Bool.type_index();
                typing::resolve_types_expected(r, ast, children[1], Some(boolean));
                typing::check_expected_type(r, ast, children[1], boolean, "pattern guard");
            }
            NodeKind::PatternAsBind => {
                elimination_pattern(r, ast, children[0], input, domain, depth + 1);
                let alias = children[1];
                r.node_types.insert(alias, input);
                if let Some(&symbol) = r.node_symbols.get(&alias) {
                    r.symbols[symbol.0 as usize].type_index = input;
                }
            }
            NodeKind::PatternNot => {
                elimination_pattern(r, ast, children[0], input, domain, depth + 1)
            }
            NodeKind::PatternAndIs => {
                elimination_pattern(r, ast, children[0], input, domain, depth + 1);
                typing::resolve_types(r, ast, children[1]);
                let ty = r.node_types.get(&children[1]).copied();
                crate::enums::pattern(r, ast, children[2], ty);
            }
            _ => crate::enums::pattern(r, ast, pattern, Some(input)),
        }
    } else {
        let payload_type = payload(r, ast, pattern, domain);
        let family = family(r, ast, pattern);
        if let (Some(family), ErrorDomain::Closed(members)) = (family, domain)
            && !members.iter().any(|member| {
                r.type_pool.canonical_type(*member) == r.type_pool.canonical_type(family)
            })
        {
            errors::report(
                r,
                ast,
                pattern,
                "error arm family is not in the input Error set",
            );
        }
        r.error_patterns.insert(
            pattern,
            ErrorPatternPlan {
                branch: ErrorPatternBranch::Error,
                payload_type,
                type_test: family,
            },
        );
        crate::enums::pattern(r, ast, pattern, Some(payload_type));
    }
}
pub(crate) fn elimination(
    r: &mut Resolver<'_>,
    ast: &Ast,
    node: NodeIndex,
    expected: Option<TypeIndex>,
) -> Option<TypeIndex> {
    let operand = ast.fixed_children(node)[0];
    typing::resolve_types(r, ast, operand);
    let input = *r.node_types.get(&operand)?;
    let Some(shape) = r.type_pool.error_shape(input).ok().flatten() else {
        errors::report(
            r,
            ast,
            node,
            "Error elimination requires an Error-qualified operand",
        );
        return None;
    };
    let arms = ast.multi_children(node).to_vec();
    let mut body_types = Vec::new();
    for &arm in &arms {
        let children = ast.fixed_children(arm);
        let pattern = children[0];
        if ast.node(arm).kind == NodeKind::CatchArm {
            crate::enums::pattern(r, ast, pattern, Some(Intrinsic::Any.type_index()));
            r.error_patterns.insert(
                pattern,
                ErrorPatternPlan {
                    branch: ErrorPatternBranch::Error,
                    payload_type: Intrinsic::Any.type_index(),
                    type_test: None,
                },
            );
        } else {
            elimination_pattern(r, ast, pattern, input, &shape.domain, 0);
        }
        typing::resolve_types_expected(r, ast, children[1], expected);
        let reachable = shape.inner != Intrinsic::NoReturn.type_index()
            || ast.node(arm).kind == NodeKind::CatchArm
            || has_error_branch(ast, pattern);
        if reachable && let Some(&ty) = r.node_types.get(&children[1]) {
            body_types.push((children[1], ty));
        }
    }
    let remaining = match &shape.domain {
        ErrorDomain::Closed(members) => members
            .iter()
            .copied()
            .filter(|&ty| {
                !crate::error_coverage::covers(r, ast, &arms, ty, ErrorPatternBranch::Error)
            })
            .collect(),
        ErrorDomain::Open if crate::error_coverage::covers_open(r, ast, &arms) => Vec::new(),
        ErrorDomain::Open => vec![Intrinsic::Any.type_index()],
    };
    let residual = errors::normalize(r, ast, node, remaining, Intrinsic::NoReturn.type_index())?;
    let implicit_ok = shape.inner != Intrinsic::NoReturn.type_index()
        && !crate::error_coverage::covers(r, ast, &arms, shape.inner, ErrorPatternBranch::Ok);
    let mut result = Intrinsic::NoReturn.type_index();
    for &(_, ty) in &body_types {
        result = errors::join(r, result, ty);
    }
    if implicit_ok {
        result = errors::join(r, result, shape.inner);
    }
    if residual != Intrinsic::NoReturn.type_index() {
        result = errors::join(r, result, residual);
    }
    if let Some(target) = expected
        && (crate::trait_typing::is_subtype(r, ast, node, result, target)
            || r.type_pool.is_gradually_consistent(result, target))
    {
        result = target;
    }
    for (body, _) in body_types {
        typing::coerce_result_value(r, ast, body, result);
        typing::check_expected_type(r, ast, body, result, "Error elimination arm");
    }
    r.error_eliminations.insert(
        node,
        ErrorEliminationPlan {
            operand,
            input,
            result,
            residual,
            arms,
            implicit_success_conversion: implicit_ok.then_some(ErrorConversionPlan {
                source: shape.inner,
                target: result,
                kind: ErrorConversionKind::LiftOk,
            }),
            residual_conversion: (residual != Intrinsic::NoReturn.type_index()).then_some(
                ErrorConversionPlan {
                    source: residual,
                    target: result,
                    kind: ErrorConversionKind::MapQualified,
                },
            ),
        },
    );
    Some(result)
}

/// An ordinary declaration/parameter has no failing-match continuation.
pub(crate) fn validate_binding(r: &Resolver<'_>, ast: &Ast, node: NodeIndex, depth: usize) {
    if node.is_null() || depth >= 256 {
        return;
    }
    if let Some(&input) = r.node_types.get(&node)
        && let Ok(Some(shape)) = r.type_pool.error_shape(input)
    {
        if crate::error_coverage::irrefutable(r, ast, node, input, 0) {
            return;
        }
        let ok_total = shape.inner == Intrinsic::NoReturn.type_index()
            || crate::error_matrix::covers_pattern(
                r,
                ast,
                node,
                shape.inner,
                ErrorPatternBranch::Ok,
            );
        let error_total = match shape.domain {
            ErrorDomain::Closed(members) => members.iter().all(|&ty| {
                crate::error_matrix::covers_pattern(r, ast, node, ty, ErrorPatternBranch::Error)
            }),
            ErrorDomain::Open => crate::error_matrix::covers_pattern(
                r,
                ast,
                node,
                Intrinsic::Any.type_index(),
                ErrorPatternBranch::Error,
            ),
        };
        if !ok_total || !error_total {
            errors::report(
                r,
                ast,
                node,
                "Error binding pattern must be irrefutable in a declaration or parameter",
            );
        }
        return;
    }
    // Check nested Error fields in ordinary composite patterns too.
    match ast.node(node).kind {
        NodeKind::PatternTuple | NodeKind::PatternList | NodeKind::PatternCall => {
            for &child in ast.multi_children(node) {
                validate_binding(r, ast, child, depth + 1);
            }
        }
        NodeKind::PatternAsBind
        | NodeKind::PatternIfGuard
        | NodeKind::PatternOr
        | NodeKind::PatternOptionSome => {
            for &child in ast.fixed_children(node) {
                validate_binding(r, ast, child, depth + 1);
            }
        }
        _ => {}
    }
}
