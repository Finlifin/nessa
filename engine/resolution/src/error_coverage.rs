//! Conservative finite-constructor coverage. Guards and computed predicates never prove totality.
use crate::resolver::Resolver;
use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{TypeIndex, TypeKind};

pub(crate) fn irrefutable(
    r: &Resolver<'_>,
    ast: &Ast,
    pattern: NodeIndex,
    ty: TypeIndex,
    depth: usize,
) -> bool {
    if depth >= 256 || pattern.is_null() {
        return false;
    }
    let children = ast.fixed_children(pattern);
    match ast.node(pattern).kind {
        NodeKind::Underscore => true,
        NodeKind::Id => !r.enum_variants.contains_key(&pattern),
        NodeKind::PatternAsBind => irrefutable(r, ast, children[0], ty, depth + 1),
        NodeKind::PatternOr => {
            irrefutable(r, ast, children[0], ty, depth + 1)
                || irrefutable(r, ast, children[1], ty, depth + 1)
        }
        NodeKind::PatternTuple => {
            let Some(ty) = r.type_pool.canonical_type(ty) else {
                return false;
            };
            let TypeKind::Tuple { elements } = &r.type_pool.get(ty).kind else {
                return false;
            };
            let fields = ast.multi_children(pattern);
            fields.len() == elements.len()
                && fields
                    .iter()
                    .zip(elements)
                    .all(|(&pat, &ty)| irrefutable(r, ast, pat, ty, depth + 1))
        }
        NodeKind::Unit => r.type_pool.as_intrinsic(ty) == Some(type_pool::Intrinsic::Unit),
        _ => false,
    }
}
#[derive(Default)]
struct Coverage {
    all: bool,
    variants: std::collections::HashSet<u32>,
}
fn collect(
    r: &Resolver<'_>,
    ast: &Ast,
    pattern: NodeIndex,
    ty: TypeIndex,
    branch: crate::ErrorPatternBranch,
    out: &mut Coverage,
    depth: usize,
) {
    if depth >= 256 || pattern.is_null() {
        return;
    }
    let children = ast.fixed_children(pattern);
    match ast.node(pattern).kind {
        NodeKind::PatternIfGuard | NodeKind::PatternAndIs | NodeKind::PatternNot => {}
        NodeKind::PatternAsBind => collect(r, ast, children[0], ty, branch, out, depth + 1),
        NodeKind::PatternOr => {
            collect(r, ast, children[0], ty, branch.clone(), out, depth + 1);
            collect(r, ast, children[1], ty, branch, out, depth + 1);
        }
        NodeKind::PatternErrorOk if branch == crate::ErrorPatternBranch::Ok => {
            if r.type_pool.error_shape(ty).ok().flatten().is_some() {
                out.all |= irrefutable(r, ast, children[0], ty, depth + 1);
            } else {
                collect(
                    r,
                    ast,
                    children[0],
                    ty,
                    crate::ErrorPatternBranch::Error,
                    out,
                    depth + 1,
                );
            }
        }
        NodeKind::PatternError if branch == crate::ErrorPatternBranch::Error => {
            if r.type_pool.error_shape(ty).ok().flatten().is_some() {
                out.all |= irrefutable(r, ast, children[0], ty, depth + 1);
            } else {
                collect(r, ast, children[0], ty, branch, out, depth + 1);
            }
        }
        NodeKind::PatternErrorOk | NodeKind::PatternError => {}
        NodeKind::PatternTypeFamily if branch == crate::ErrorPatternBranch::Error => {
            out.all |= r
                .node_types
                .get(&pattern)
                .copied()
                .and_then(|family| r.type_pool.canonical_type(family))
                == r.type_pool.canonical_type(ty);
        }
        _ if branch == crate::ErrorPatternBranch::Error => {
            if irrefutable(r, ast, pattern, ty, depth + 1) {
                out.all = true;
                return;
            }
            let Some(reference) = r.enum_variants.get(&pattern) else {
                return;
            };
            if r.type_pool.canonical_type(reference.type_index) != r.type_pool.canonical_type(ty) {
                return;
            }
            let TypeKind::Enum { variants, .. } = &r.type_pool.get(reference.type_index).kind
            else {
                return;
            };
            let Some(variant) = variants
                .iter()
                .find(|variant| variant.tag == reference.variant_tag)
            else {
                return;
            };
            let fields = ast.multi_children(pattern);
            if fields.len() == variant.fields.len()
                && fields
                    .iter()
                    .zip(&variant.fields)
                    .all(|(&pat, field)| irrefutable(r, ast, pat, field.ty, depth + 1))
            {
                out.variants.insert(reference.variant_tag);
            }
        }
        _ => {}
    }
}
pub(crate) fn covers(
    r: &Resolver<'_>,
    ast: &Ast,
    arms: &[NodeIndex],
    ty: TypeIndex,
    branch: crate::ErrorPatternBranch,
) -> bool {
    let mut coverage = Coverage::default();
    for &arm in arms {
        if ast.node(arm).kind == NodeKind::CatchArm {
            if branch == crate::ErrorPatternBranch::Error {
                return true;
            }
            continue;
        }
        collect(
            r,
            ast,
            ast.fixed_children(arm)[0],
            ty,
            branch.clone(),
            &mut coverage,
            0,
        );
    }
    if coverage.all {
        return true;
    }
    if let Some(ty) = r.type_pool.canonical_type(ty)
        && let TypeKind::Enum { variants, .. } = &r.type_pool.get(ty).kind
        && variants
            .iter()
            .all(|variant| coverage.variants.contains(&variant.tag))
    {
        return true;
    }
    crate::error_matrix::covers(r, ast, arms, ty, branch)
}
/// Open cannot be exhausted by finitely many type families or variant constructors.
pub(crate) fn covers_open(r: &Resolver<'_>, ast: &Ast, arms: &[NodeIndex]) -> bool {
    covers(
        r,
        ast,
        arms,
        type_pool::Intrinsic::Any.type_index(),
        crate::ErrorPatternBranch::Error,
    )
}

/// Direct matching owns the complete wrapper; ordinary wildcard/binder covers both branches.
pub(crate) fn direct_exhaustive(
    r: &Resolver<'_>,
    ast: &Ast,
    arms: &[NodeIndex],
    input: TypeIndex,
) -> bool {
    if arms
        .iter()
        .any(|&arm| irrefutable(r, ast, ast.fixed_children(arm)[0], input, 0))
    {
        return true;
    }
    let Ok(Some(shape)) = r.type_pool.error_shape(input) else {
        return true;
    };
    let ok = shape.inner == type_pool::Intrinsic::NoReturn.type_index()
        || covers(r, ast, arms, shape.inner, crate::ErrorPatternBranch::Ok);
    let errors = match shape.domain {
        type_pool::ErrorDomain::Closed(members) => members
            .into_iter()
            .all(|member| covers(r, ast, arms, member, crate::ErrorPatternBranch::Error)),
        type_pool::ErrorDomain::Open => covers_open(r, ast, arms),
    };
    ok && errors
}
