//! Bounded constructor-matrix coverage; never evaluates guards or expands recursive values.
use crate::{ErrorPatternBranch, resolver::Resolver};
use ast::{Ast, NodeIndex, NodeKind};
use type_pool::{Intrinsic, TypeIndex, TypeKind};

const LIMIT: usize = 262144;
struct Budget {
    remaining: usize,
}
impl Budget {
    fn spend(&mut self, amount: usize) -> bool {
        match self.remaining.checked_sub(amount) {
            Some(next) => {
                self.remaining = next;
                true
            }
            None => false,
        }
    }
}
fn strip_as(ast: &Ast, mut pattern: NodeIndex) -> Option<NodeIndex> {
    for _ in 0..256 {
        if pattern.is_null() || ast.node(pattern).kind != NodeKind::PatternAsBind {
            return Some(pattern);
        }
        pattern = ast.fixed_children(pattern)[0];
    }
    None
}
fn payload_patterns(
    r: &Resolver<'_>,
    ast: &Ast,
    pattern: NodeIndex,
    target: (TypeIndex, ErrorPatternBranch),
    rows: &mut Vec<Vec<NodeIndex>>,
    budget: &mut Budget,
    depth: usize,
) -> bool {
    if depth >= 256 || !budget.spend(1) {
        return false;
    }
    let (ty, branch) = target;
    let Some(pattern) = strip_as(ast, pattern) else {
        return false;
    };
    if pattern.is_null() {
        return false;
    }
    match ast.node(pattern).kind {
        NodeKind::PatternIfGuard | NodeKind::PatternAndIs | NodeKind::PatternNot => {}
        NodeKind::PatternOr => {
            for &child in ast.fixed_children(pattern) {
                if !payload_patterns(r, ast, child, (ty, branch.clone()), rows, budget, depth + 1) {
                    return false;
                }
            }
        }
        NodeKind::PatternErrorOk if branch == ErrorPatternBranch::Ok => {
            rows.push(vec![ast.fixed_children(pattern)[0]])
        }
        NodeKind::PatternError if branch == ErrorPatternBranch::Error => {
            rows.push(vec![ast.fixed_children(pattern)[0]])
        }
        NodeKind::PatternErrorOk | NodeKind::PatternError => {}
        NodeKind::PatternTypeFamily if branch == ErrorPatternBranch::Error => {
            if r.node_types
                .get(&pattern)
                .and_then(|&family| r.type_pool.canonical_type(family))
                == r.type_pool.canonical_type(ty)
            {
                rows.push(vec![NodeIndex::NULL]);
            }
        }
        _ if branch == ErrorPatternBranch::Error => rows.push(vec![pattern]),
        _ => {}
    }
    true
}
fn expand_heads(
    ast: &Ast,
    rows: Vec<Vec<NodeIndex>>,
    budget: &mut Budget,
) -> Option<Vec<Vec<NodeIndex>>> {
    let mut pending = rows;
    let mut expanded = Vec::new();
    while let Some(mut row) = pending.pop() {
        if !budget.spend(row.len() + 1) {
            return None;
        }
        let Some(first) = row.first_mut() else {
            expanded.push(row);
            continue;
        };
        *first = strip_as(ast, *first)?;
        if !first.is_null() && ast.node(*first).kind == NodeKind::PatternOr {
            let alternatives = ast.fixed_children(*first);
            for &alternative in alternatives {
                let mut branch = row.clone();
                branch[0] = alternative;
                pending.push(branch);
            }
        } else {
            expanded.push(row);
        }
    }
    Some(expanded)
}
fn wildcard(r: &Resolver<'_>, ast: &Ast, pattern: NodeIndex, ty: TypeIndex) -> bool {
    pattern.is_null()
        || crate::error_coverage::irrefutable(r, ast, pattern, ty, 0)
        || (ast.node(pattern).kind == NodeKind::PatternTypeFamily
            && r.node_types
                .get(&pattern)
                .and_then(|&family| r.type_pool.canonical_type(family))
                == r.type_pool.canonical_type(ty))
}
fn matrix(
    r: &Resolver<'_>,
    ast: &Ast,
    rows: Vec<Vec<NodeIndex>>,
    types: &[TypeIndex],
    budget: &mut Budget,
    depth: usize,
) -> bool {
    if depth >= 256 || !budget.spend(types.len() + 1) {
        return false;
    }
    if types.is_empty() {
        return !rows.is_empty();
    }
    let Some(rows) = expand_heads(ast, rows, budget) else {
        return false;
    };
    let Some(head) = r.type_pool.canonical_type(types[0]) else {
        return false;
    };
    if r.type_pool.as_intrinsic(head) == Some(Intrinsic::NoReturn) {
        return true;
    }
    let default: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.first()
                .is_some_and(|&pattern| wildcard(r, ast, pattern, head))
        })
        .map(|row| row[1..].to_vec())
        .collect();
    if !default.is_empty() && matrix(r, ast, default, &types[1..], budget, depth + 1) {
        return true;
    }
    // Finite enum constructors retain field correlation; fields are never enumerated as values.
    match &r.type_pool.get(head).kind {
        TypeKind::Enum { variants, .. } => variants.iter().all(|variant| {
            let mut specialized = Vec::new();
            for row in &rows {
                let Some(&pattern) = row.first() else {
                    continue;
                };
                let fields = if wildcard(r, ast, pattern, head) {
                    Some(vec![NodeIndex::NULL; variant.fields.len()])
                } else if r.enum_variants.get(&pattern).is_some_and(|reference| {
                    r.type_pool.canonical_type(reference.type_index) == Some(head)
                        && reference.variant_tag == variant.tag
                }) {
                    Some(ast.multi_children(pattern).to_vec())
                } else {
                    None
                };
                if let Some(mut fields) =
                    fields.filter(|fields| fields.len() == variant.fields.len())
                {
                    if !budget.spend(fields.len() + row.len()) {
                        return false;
                    }
                    fields.extend_from_slice(&row[1..]);
                    specialized.push(fields);
                }
            }
            let fields: Vec<_> = variant
                .fields
                .iter()
                .map(|field| field.ty)
                .chain(types[1..].iter().copied())
                .collect();
            matrix(r, ast, specialized, &fields, budget, depth + 1)
        }),
        TypeKind::Intrinsic(Intrinsic::Bool) => [false, true].into_iter().all(|value| {
            let specialized: Vec<_> = rows
                .iter()
                .filter(|row| {
                    row.first().is_some_and(|&pattern| {
                        wildcard(r, ast, pattern, head)
                            || (ast.node(pattern).kind == NodeKind::Bool
                                && (str_interner::get(ast.node(pattern).str_id) == "true") == value)
                    })
                })
                .map(|row| row[1..].to_vec())
                .collect();
            matrix(r, ast, specialized, &types[1..], budget, depth + 1)
        }),
        TypeKind::ErrorQualified { errors, inner } => {
            let domain = if errors.contains(&Intrinsic::Any.type_index()) {
                vec![Intrinsic::Any.type_index()]
            } else {
                errors.clone()
            };
            let mut cases = std::iter::once((true, *inner))
                .filter(|(_, inner)| *inner != Intrinsic::NoReturn.type_index())
                .chain(domain.into_iter().map(|error| (false, error)));
            cases.all(|(ok, payload)| {
                let mut specialized = Vec::new();
                for row in &rows {
                    let Some(&pattern) = row.first() else {
                        continue;
                    };
                    let child = if wildcard(r, ast, pattern, head) {
                        Some(NodeIndex::NULL)
                    } else if (ok && ast.node(pattern).kind == NodeKind::PatternErrorOk)
                        || (!ok
                            && ast.node(pattern).kind == NodeKind::PatternError
                            && r.error_patterns.get(&pattern).is_some_and(|plan| {
                                plan.type_test.is_none()
                                    || plan
                                        .type_test
                                        .and_then(|family| r.type_pool.canonical_type(family))
                                        == r.type_pool.canonical_type(payload)
                            }))
                    {
                        Some(ast.fixed_children(pattern)[0])
                    } else {
                        None
                    };
                    if let Some(child) = child {
                        if !budget.spend(row.len() + 1) {
                            return false;
                        }
                        let mut fields = vec![child];
                        fields.extend_from_slice(&row[1..]);
                        specialized.push(fields);
                    }
                }
                let fields: Vec<_> = std::iter::once(payload)
                    .chain(types[1..].iter().copied())
                    .collect();
                matrix(r, ast, specialized, &fields, budget, depth + 1)
            })
        }
        TypeKind::Optional { inner } => [false, true]
            .into_iter()
            .filter(|some| !some || *inner != Intrinsic::NoReturn.type_index())
            .all(|some| {
                let mut specialized = Vec::new();
                for row in &rows {
                    let Some(&pattern) = row.first() else {
                        continue;
                    };
                    let child = if wildcard(r, ast, pattern, head) {
                        Some(NodeIndex::NULL)
                    } else if some && ast.node(pattern).kind == NodeKind::PatternOptionSome {
                        Some(ast.fixed_children(pattern)[0])
                    } else if !some && ast.node(pattern).kind == NodeKind::Null {
                        Some(NodeIndex::NULL)
                    } else {
                        None
                    };
                    if let Some(child) = child {
                        if !budget.spend(row.len() + 1) {
                            return false;
                        }
                        let mut fields = if some { vec![child] } else { Vec::new() };
                        fields.extend_from_slice(&row[1..]);
                        specialized.push(fields);
                    }
                }
                let fields: Vec<_> = some
                    .then_some(*inner)
                    .into_iter()
                    .chain(types[1..].iter().copied())
                    .collect();
                matrix(r, ast, specialized, &fields, budget, depth + 1)
            }),
        TypeKind::Tuple { elements } => {
            let mut specialized = Vec::new();
            for row in rows {
                let Some(&pattern) = row.first() else {
                    continue;
                };
                let fields = if wildcard(r, ast, pattern, head) {
                    Some(vec![NodeIndex::NULL; elements.len()])
                } else if ast.node(pattern).kind == NodeKind::PatternTuple {
                    Some(ast.multi_children(pattern).to_vec())
                } else {
                    None
                };
                if let Some(mut fields) = fields.filter(|fields| fields.len() == elements.len()) {
                    if !budget.spend(fields.len() + row.len()) {
                        return false;
                    }
                    fields.extend_from_slice(&row[1..]);
                    specialized.push(fields);
                }
            }
            let fields: Vec<_> = elements
                .iter()
                .copied()
                .chain(types[1..].iter().copied())
                .collect();
            matrix(r, ast, specialized, &fields, budget, depth + 1)
        }
        _ => false,
    }
}
pub(crate) fn covers(
    r: &Resolver<'_>,
    ast: &Ast,
    arms: &[NodeIndex],
    ty: TypeIndex,
    branch: ErrorPatternBranch,
) -> bool {
    let mut budget = Budget { remaining: LIMIT };
    let mut rows = Vec::new();
    for &arm in arms {
        if ast.node(arm).kind == NodeKind::CatchArm {
            if branch == ErrorPatternBranch::Error {
                rows.push(vec![NodeIndex::NULL]);
            }
            continue;
        }
        if !payload_patterns(
            r,
            ast,
            ast.fixed_children(arm)[0],
            (ty, branch.clone()),
            &mut rows,
            &mut budget,
            0,
        ) {
            return false;
        }
    }
    matrix(r, ast, rows, &[ty], &mut budget, 0)
}

/// Coverage of one binding pattern without constructing a synthetic arm AST.
pub(crate) fn covers_pattern(
    r: &Resolver<'_>,
    ast: &Ast,
    pattern: NodeIndex,
    ty: TypeIndex,
    branch: ErrorPatternBranch,
) -> bool {
    let mut budget = Budget { remaining: LIMIT };
    let mut rows = Vec::new();
    payload_patterns(r, ast, pattern, (ty, branch), &mut rows, &mut budget, 0)
        && matrix(r, ast, rows, &[ty], &mut budget, 0)
}
