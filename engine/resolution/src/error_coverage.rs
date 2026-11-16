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
        NodeKind::PatternExtendedCall => {
            let Some(ty) = r.type_pool.canonical_type(ty) else {
                return false;
            };
            let Some(plan) = r.struct_patterns.get(&pattern) else {
                return false;
            };
            if r.type_pool.canonical_type(plan.type_index) != Some(ty) {
                return false;
            }
            let TypeKind::Struct { fields, .. } = &r.type_pool.get(ty).kind else {
                return false;
            };
            plan.fields.iter().all(|&(slot, subpat)| {
                fields
                    .get(slot as usize)
                    .is_some_and(|field| irrefutable(r, ast, subpat, field.ty, depth + 1))
            })
        }
        NodeKind::PatternCall => {
            let Some(ty) = r.type_pool.canonical_type(ty) else {
                return false;
            };
            let Some(&nt_ty) = r.newtype_patterns.get(&pattern) else {
                return false;
            };
            if r.type_pool.canonical_type(nt_ty) != Some(ty) {
                return false;
            }
            let TypeKind::Newtype { inner, .. } = r.type_pool.get(ty).kind else {
                return false;
            };
            let subpats = ast.multi_children(pattern);
            subpats.len() == 1 && irrefutable(r, ast, subpats[0], inner, depth + 1)
        }
        NodeKind::PatternTyped => {
            let Some(ty_canon) = r.type_pool.canonical_type(ty) else {
                return false;
            };
            let Some(ann_ty) = r
                .node_types
                .get(&children[1])
                .copied()
                .and_then(|t| r.type_pool.canonical_type(t))
            else {
                return false;
            };
            ann_ty == ty_canon && irrefutable(r, ast, children[0], ann_ty, depth + 1)
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

fn expand_match_patterns(
    r: &Resolver<'_>,
    ast: &Ast,
    pattern: NodeIndex,
    canon_ty: TypeIndex,
    out: &mut Vec<NodeIndex>,
    depth: usize,
) {
    if depth >= 256 || pattern.is_null() {
        return;
    }
    let children = ast.fixed_children(pattern);
    match ast.node(pattern).kind {
        NodeKind::PatternIfGuard | NodeKind::PatternAndIs | NodeKind::PatternNot => {}
        NodeKind::PatternAsBind => {
            expand_match_patterns(r, ast, children[0], canon_ty, out, depth + 1);
        }
        NodeKind::PatternOr => {
            expand_match_patterns(r, ast, children[0], canon_ty, out, depth + 1);
            expand_match_patterns(r, ast, children[1], canon_ty, out, depth + 1);
        }
        NodeKind::PatternTyped => {
            if r.node_types
                .get(&children[1])
                .copied()
                .and_then(|t| r.type_pool.canonical_type(t))
                == Some(canon_ty)
            {
                expand_match_patterns(r, ast, children[0], canon_ty, out, depth + 1);
            }
        }
        _ => out.push(pattern),
    }
}

fn missing_tuple_witnesses(
    r: &Resolver<'_>,
    ast: &Ast,
    rows: &[Vec<NodeIndex>],
    tys: &[TypeIndex],
    depth: usize,
) -> Vec<Vec<String>> {
    if tys.is_empty() {
        return if rows.is_empty() {
            vec![Vec::new()]
        } else {
            Vec::new()
        };
    }
    if depth >= 16 {
        return vec![vec!["_".to_string(); tys.len()]];
    }
    if rows.iter().any(|row| {
        row.len() == tys.len()
            && row
                .iter()
                .zip(tys)
                .all(|(&p, &t)| irrefutable(r, ast, p, t, depth + 1))
    }) {
        return Vec::new();
    }
    let head_ty = match r.type_pool.canonical_type(tys[0]) {
        Some(t) => t,
        None => return Vec::new(),
    };
    let head_pats: Vec<NodeIndex> = rows.iter().filter_map(|row| row.first().copied()).collect();
    let head_missing = missing_witnesses(r, ast, &head_pats, head_ty, depth + 1);
    if !head_missing.is_empty() {
        let mut out = Vec::new();
        for w in head_missing {
            let mut witness = vec![w];
            witness.extend(std::iter::repeat_n("_".to_string(), tys.len() - 1));
            out.push(witness);
        }
        return out;
    }
    // Head column is covered across rows; check tail columns for rows whose head is irrefutable
    // or specialize finite Bool/Optional/Enum head constructors.
    match &r.type_pool.get(head_ty).kind.clone() {
        TypeKind::Intrinsic(type_pool::Intrinsic::Bool) => {
            let mut out = Vec::new();
            for (lit, label) in [(true, "true"), (false, "false")] {
                let mut sub_rows = Vec::new();
                for row in rows {
                    if row.len() != tys.len() {
                        continue;
                    }
                    let mut expanded = Vec::new();
                    expand_match_patterns(r, ast, row[0], head_ty, &mut expanded, depth + 1);
                    let matches_lit = expanded.iter().any(|&p| {
                        irrefutable(r, ast, p, head_ty, depth + 1)
                            || (ast.node(p).kind == NodeKind::Bool
                                && (str_interner::get(ast.node(p).str_id) == "true") == lit)
                    });
                    if matches_lit {
                        sub_rows.push(row[1..].to_vec());
                    }
                }
                for mut tail in missing_tuple_witnesses(r, ast, &sub_rows, &tys[1..], depth + 1) {
                    tail.insert(0, label.to_string());
                    out.push(tail);
                }
            }
            out
        }
        _ => {
            let wild_rows: Vec<Vec<NodeIndex>> = rows
                .iter()
                .filter(|row| {
                    row.len() == tys.len() && irrefutable(r, ast, row[0], head_ty, depth + 1)
                })
                .map(|row| row[1..].to_vec())
                .collect();
            missing_tuple_witnesses(r, ast, &wild_rows, &tys[1..], depth + 1)
                .into_iter()
                .map(|mut tail| {
                    tail.insert(0, "_".to_string());
                    tail
                })
                .collect()
        }
    }
}

fn missing_witnesses(
    r: &Resolver<'_>,
    ast: &Ast,
    patterns: &[NodeIndex],
    ty: TypeIndex,
    depth: usize,
) -> Vec<String> {
    let Some(canon_ty) = r.type_pool.canonical_type(ty) else {
        return Vec::new();
    };
    let mut expanded = Vec::new();
    for &pat in patterns {
        expand_match_patterns(r, ast, pat, canon_ty, &mut expanded, depth);
    }
    if expanded
        .iter()
        .any(|&pat| irrefutable(r, ast, pat, canon_ty, depth))
    {
        return Vec::new();
    }
    if depth >= 16 {
        return vec!["_".to_string()];
    }
    match &r.type_pool.get(canon_ty).kind.clone() {
        TypeKind::Intrinsic(type_pool::Intrinsic::NoReturn) => Vec::new(),
        TypeKind::Intrinsic(type_pool::Intrinsic::Unit) => {
            if expanded.iter().any(|&p| ast.node(p).kind == NodeKind::Unit) {
                Vec::new()
            } else {
                vec!["()".to_string()]
            }
        }
        TypeKind::Intrinsic(type_pool::Intrinsic::Bool) => {
            let mut missing = Vec::new();
            for label in ["true", "false"] {
                let covered = expanded.iter().any(|&p| {
                    ast.node(p).kind == NodeKind::Bool
                        && str_interner::get(ast.node(p).str_id) == label
                });
                if !covered {
                    missing.push(label.to_string());
                }
            }
            missing
        }
        TypeKind::Optional { inner } => {
            let mut missing = Vec::new();
            let has_null = expanded.iter().any(|&p| ast.node(p).kind == NodeKind::Null);
            if !has_null {
                missing.push("null".to_string());
            }
            let mut some_pats = Vec::new();
            for &p in &expanded {
                match ast.node(p).kind {
                    NodeKind::Null => {}
                    NodeKind::PatternOptionSome => {
                        some_pats.push(ast.fixed_children(p)[0]);
                    }
                    _ => {
                        some_pats.push(p);
                    }
                }
            }
            for w in missing_witnesses(r, ast, &some_pats, *inner, depth + 1) {
                missing.push(format!("{w}?"));
            }
            missing
        }
        TypeKind::Enum { name, variants } => {
            let enum_str = str_interner::get(*name);
            let mut missing = Vec::new();
            for variant in variants {
                let var_str = str_interner::get(variant.name);
                let matching: Vec<NodeIndex> = expanded
                    .iter()
                    .copied()
                    .filter(|&p| {
                        r.enum_variants.get(&p).is_some_and(|vref| {
                            r.type_pool.canonical_type(vref.type_index) == Some(canon_ty)
                                && vref.variant_tag == variant.tag
                        })
                    })
                    .collect();
                if matching.is_empty() {
                    missing.push(format!("{enum_str}.{var_str}"));
                    continue;
                }
                if variant.fields.is_empty() {
                    continue;
                }
                let field_tys: Vec<TypeIndex> = variant.fields.iter().map(|f| f.ty).collect();
                let rows: Vec<Vec<NodeIndex>> = matching
                    .into_iter()
                    .map(|p| ast.multi_children(p).to_vec())
                    .filter(|cols| cols.len() == field_tys.len())
                    .collect();
                for witness_cols in missing_tuple_witnesses(r, ast, &rows, &field_tys, depth + 1) {
                    missing.push(format!("{enum_str}.{var_str}({})", witness_cols.join(", ")));
                }
            }
            missing
        }
        TypeKind::Tuple { elements } => {
            let mut rows = Vec::new();
            for &p in &expanded {
                if ast.node(p).kind == NodeKind::PatternTuple {
                    let cols = ast.multi_children(p).to_vec();
                    if cols.len() == elements.len() {
                        rows.push(cols);
                    }
                }
            }
            missing_tuple_witnesses(r, ast, &rows, elements, depth + 1)
                .into_iter()
                .map(|cols| format!("({})", cols.join(", ")))
                .collect()
        }
        TypeKind::Newtype { name, inner } => {
            let nt_str = str_interner::get(*name);
            let mut inner_pats = Vec::new();
            for &p in &expanded {
                if ast.node(p).kind == NodeKind::PatternCall
                    && r.newtype_patterns
                        .get(&p)
                        .is_some_and(|&nt| r.type_pool.canonical_type(nt) == Some(canon_ty))
                {
                    let subpats = ast.multi_children(p);
                    if subpats.len() == 1 {
                        inner_pats.push(subpats[0]);
                    }
                }
            }
            if inner_pats.is_empty() {
                vec![format!("{nt_str}(_)")]
            } else {
                missing_witnesses(r, ast, &inner_pats, *inner, depth + 1)
                    .into_iter()
                    .map(|w| format!("{nt_str}({w})"))
                    .collect()
            }
        }
        _ => vec!["_".to_string()],
    }
}

pub(crate) fn check_match_exhaustiveness(
    r: &Resolver<'_>,
    ast: &Ast,
    match_node: NodeIndex,
    scrutinee_ty: TypeIndex,
) {
    if r.diag_ctx.has_errors() {
        return;
    }
    if r.type_pool
        .error_shape(scrutinee_ty)
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }
    let Some(canon_ty) = r.type_pool.canonical_type(scrutinee_ty) else {
        return;
    };
    let checkable = matches!(
        r.type_pool.get(canon_ty).kind,
        TypeKind::Enum { .. }
            | TypeKind::Optional { .. }
            | TypeKind::Intrinsic(type_pool::Intrinsic::Bool)
    );
    if !checkable {
        return;
    }
    let patterns: Vec<NodeIndex> = ast
        .multi_children(match_node)
        .iter()
        .map(|&arm| ast.fixed_children(arm)[0])
        .collect();
    let missing = missing_witnesses(r, ast, &patterns, canon_ty, 0);
    if missing.is_empty() {
        return;
    }
    let formatted = if missing.len() <= 4 {
        missing
            .iter()
            .map(|m| format!("`{m}`"))
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        let head = missing[..4]
            .iter()
            .map(|m| format!("`{m}`"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{head}, and {} more", missing.len() - 4)
    };
    r.diag_ctx
        .warning(format!("non-exhaustive `match`: missing {formatted}"))
        .with_primary_span(ast.node(match_node).span)
        .emit(r.diag_ctx);
}
