//! Finite semantic terms plus reachable nominal records. Cycles stop at anchors.
use std::collections::{BTreeMap, BTreeSet, HashSet};

use sha2::{Digest, Sha256};
use str_interner::StrId;

use crate::identity::{MAX_IDENTITY_BYTES, MAX_IDENTITY_ITEMS, anchors, fail};
use crate::{
    AssociatedTypeExpr, FieldInfo, TraitParameterKind, TraitTypeStep, TypeId, TypeIdentityError,
    TypeIdentityInput, TypeIndex, TypeKind, TypePool,
};

#[derive(Clone, Default)]
struct Term {
    bytes: Vec<u8>,
    nominal: BTreeSet<TypeIndex>,
}
impl Term {
    fn child(&mut self, other: &Self) -> Result<(), TypeIdentityError> {
        blob(&other.bytes, &mut self.bytes)?;
        self.nominal.extend(&other.nominal);
        Ok(())
    }
    fn name(&mut self, name: StrId) -> Result<(), TypeIdentityError> {
        text(
            &str_interner::try_get(name).ok_or_else(|| fail("invalid identity string handle"))?,
            &mut self.bytes,
        )
    }
}
pub(crate) fn count(value: usize, bytes: &mut Vec<u8>) -> Result<(), TypeIdentityError> {
    let value = u32::try_from(value).map_err(|_| fail("identity count overflow"))?;
    bytes.extend_from_slice(&value.to_be_bytes());
    budget(bytes)
}
pub(crate) fn text(value: &str, bytes: &mut Vec<u8>) -> Result<(), TypeIdentityError> {
    blob(value.as_bytes(), bytes)
}
fn blob(value: &[u8], bytes: &mut Vec<u8>) -> Result<(), TypeIdentityError> {
    if value.len()
        > MAX_IDENTITY_BYTES
            .saturating_sub(bytes.len())
            .saturating_sub(4)
    {
        return Err(fail("type identity byte budget exceeded"));
    }
    count(value.len(), bytes)?;
    bytes.extend_from_slice(value);
    Ok(())
}
fn budget(bytes: &[u8]) -> Result<(), TypeIdentityError> {
    if bytes.len() > MAX_IDENTITY_BYTES {
        Err(fail("type identity byte budget exceeded"))
    } else {
        Ok(())
    }
}
fn layout(pool: &TypePool, ty: TypeIndex, tag: u8, abi: u8) -> Term {
    let info = pool.get(ty);
    let mut bytes = vec![tag];
    bytes.extend_from_slice(&info.size.to_be_bytes());
    bytes.extend_from_slice(&info.align.to_be_bytes());
    bytes.push(abi);
    Term {
        bytes,
        nominal: BTreeSet::new(),
    }
}
fn get(terms: &[Option<Term>], ty: TypeIndex) -> Result<&Term, TypeIdentityError> {
    terms
        .get(ty.as_u32() as usize)
        .and_then(Option::as_ref)
        .ok_or_else(|| fail("identity term references an invalid or unresolved type"))
}

pub(crate) fn compute(
    pool: &TypePool,
    input: &TypeIdentityInput,
) -> Result<Vec<TypeId>, TypeIdentityError> {
    let anchors = anchors(pool, input)?;
    let structural = pool
        .structural_types
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let mut terms = vec![None; pool.len()];
    for (index, info) in pool.types.iter().enumerate() {
        let ty = TypeIndex::from_raw(index as u32);
        if pool.is_reserved_type(ty) && !matches!(info.kind, TypeKind::Typealias { .. }) {
            let mut term = Term::default();
            term.bytes.push(0);
            term.bytes
                .extend_from_slice(&info.type_id.hi().to_be_bytes());
            term.bytes
                .extend_from_slice(&info.type_id.lo().to_be_bytes());
            terms[index] = Some(term);
        } else if let Some(anchor) = anchors.get(&ty) {
            let mut term = Term::default();
            term.bytes.push(1);
            blob(anchor, &mut term.bytes)?;
            term.nominal.insert(ty);
            terms[index] = Some(term);
        }
    }
    let mut term_bytes = terms
        .iter()
        .flatten()
        .map(|term| term.bytes.len())
        .sum::<usize>();
    let mut active = HashSet::new();
    for index in 0..pool.len() {
        if terms[index].is_some() {
            continue;
        }
        let root = TypeIndex::from_raw(index as u32);
        let mut pending = vec![(root, false)];
        while let Some((ty, ready)) = pending.pop() {
            let position = ty.as_u32() as usize;
            if terms.get(position).is_none() {
                return Err(fail("identity term type index out of range"));
            }
            if terms[position].is_some() {
                continue;
            }
            if ready {
                let term = structural_term(pool, ty, &terms, structural.contains(&ty))?;
                term_bytes = term_bytes
                    .checked_add(term.bytes.len())
                    .ok_or_else(|| fail("identity memo size overflow"))?;
                if term_bytes > MAX_IDENTITY_BYTES {
                    return Err(fail("identity term memo exceeds byte budget"));
                }
                terms[position] = Some(term);
                active.remove(&ty);
            } else {
                if !active.insert(ty) {
                    return Err(fail("cyclic structural identity term"));
                }
                pending.push((ty, true));
                for child in term_children(&pool.get(ty).kind).into_iter().rev() {
                    if terms.get(child.as_u32() as usize).is_none() {
                        return Err(fail("invalid structural identity reference"));
                    }
                    if terms[child.as_u32() as usize].is_none() {
                        pending.push((child, false));
                    }
                }
            }
        }
    }
    let mut records = BTreeMap::new();
    let mut record_bytes = 0usize;
    for (&ty, anchor) in &anchors {
        let record = nominal_record(pool, ty, &terms)?;
        record_bytes = record_bytes
            .checked_add(record.bytes.len() + anchor.len())
            .ok_or_else(|| fail("identity record size overflow"))?;
        if record_bytes > MAX_IDENTITY_BYTES {
            return Err(fail("identity record memo exceeds byte budget"));
        }
        records.insert(ty, record);
    }
    let symbolic = crate::associated_defaults::symbolic_types(&pool.types);
    let mut ids = vec![TypeId::ZERO; pool.len()];
    let mut total_hashed = 0usize;
    for (index, info) in pool.types.iter().enumerate() {
        let ty = TypeIndex::from_raw(index as u32);
        if matches!(info.kind, TypeKind::Typealias { .. }) {
            continue;
        }
        if pool.is_reserved_type(ty) {
            ids[index] = info.type_id;
            continue;
        }
        if symbolic.contains(&ty) {
            continue;
        }
        let term = get(&terms, ty)?;
        let mut reachable = BTreeSet::new();
        let mut pending = term.nominal.iter().copied().collect::<Vec<_>>();
        while let Some(nominal) = pending.pop() {
            if reachable.insert(nominal) {
                pending.extend(
                    &records
                        .get(&nominal)
                        .ok_or_else(|| fail("missing nominal identity record"))?
                        .nominal,
                );
            }
            if reachable.len() > MAX_IDENTITY_ITEMS {
                return Err(fail("reachable identity graph item limit exceeded"));
            }
        }
        let mut sorted = reachable
            .iter()
            .map(|ty| {
                Ok((
                    anchors
                        .get(ty)
                        .ok_or_else(|| fail("missing nominal identity anchor"))?,
                    records
                        .get(ty)
                        .ok_or_else(|| fail("missing nominal identity layout"))?,
                ))
            })
            .collect::<Result<Vec<_>, TypeIdentityError>>()?;
        sorted.sort_by(|left, right| left.0.cmp(right.0));
        let mut bytes = b"nessa.type.identity\0".to_vec();
        bytes.extend_from_slice(&1u32.to_be_bytes());
        blob(&term.bytes, &mut bytes)?;
        count(sorted.len(), &mut bytes)?;
        for (anchor, record) in sorted {
            blob(anchor, &mut bytes)?;
            blob(&record.bytes, &mut bytes)?;
        }
        total_hashed = total_hashed
            .checked_add(bytes.len())
            .ok_or_else(|| fail("identity work budget overflow"))?;
        if total_hashed > 512 * 1024 * 1024 {
            return Err(fail("type identity aggregate hash work exceeds 512 MiB"));
        }
        let digest = Sha256::digest(bytes);
        let hi = u64::from_be_bytes(
            digest[..8]
                .try_into()
                .map_err(|_| fail("invalid hash width"))?,
        );
        let lo = u64::from_be_bytes(
            digest[8..16]
                .try_into()
                .map_err(|_| fail("invalid hash width"))?,
        );
        let id = TypeId(hi, lo);
        if id == TypeId::ZERO
            || [
                0x4e45_5353_4149_4e00,
                0x4e45_5353_434f_4c4c,
                crate::identity::BOOTSTRAP_NAMESPACE,
            ]
            .contains(&hi)
        {
            return Err(fail("computed identity collides with reserved namespace"));
        }
        ids[index] = id;
    }
    for (index, info) in pool.types.iter().enumerate() {
        if matches!(info.kind, TypeKind::Typealias { .. }) {
            let canonical = pool
                .canonical_type(TypeIndex::from_raw(index as u32))
                .ok_or_else(|| fail("invalid identity alias"))?;
            ids[index] = ids[canonical.as_u32() as usize];
        }
    }
    Ok(ids)
}

fn term_children(kind: &TypeKind) -> Vec<TypeIndex> {
    match kind {
        TypeKind::Typealias { target, .. } => vec![*target],
        TypeKind::Tuple { elements } => elements.clone(),
        TypeKind::Function { params, ret } | TypeKind::Effect { params, ret, .. } => {
            params.iter().copied().chain([*ret]).collect()
        }
        TypeKind::Optional { inner } | TypeKind::IterationStepTemplate { item: inner } => {
            vec![*inner]
        }
        TypeKind::AssociatedType { trait_owner, .. } => vec![*trait_owner],
        TypeKind::ErrorQualified { errors, inner } => {
            errors.iter().copied().chain([*inner]).collect()
        }
        TypeKind::EffectQualified { effects, inner } => {
            effects.iter().copied().chain([*inner]).collect()
        }
        TypeKind::Enum { variants, .. } => variants
            .iter()
            .flat_map(|variant| variant.fields.iter().map(|field| field.ty))
            .collect(),
        _ => Vec::new(),
    }
}
fn parameters(
    term: &mut Term,
    params: &[TypeIndex],
    ret: TypeIndex,
    terms: &[Option<Term>],
) -> Result<(), TypeIdentityError> {
    count(params.len(), &mut term.bytes)?;
    for &param in params {
        term.child(get(terms, param)?)?;
    }
    term.child(get(terms, ret)?)?;
    Ok(())
}
fn structural_term(
    pool: &TypePool,
    ty: TypeIndex,
    terms: &[Option<Term>],
    structural: bool,
) -> Result<Term, TypeIdentityError> {
    let kind = &pool.get(ty).kind;
    if let TypeKind::Typealias { target, .. } = kind {
        return Ok(get(terms, *target)?.clone());
    }
    let mut term = match kind {
        TypeKind::Tuple { .. } => layout(pool, ty, 2, 1),
        TypeKind::Function { .. } => layout(pool, ty, 3, 2),
        TypeKind::Effect { .. } => layout(pool, ty, 4, 2),
        TypeKind::Optional { .. } => layout(pool, ty, 5, 3),
        TypeKind::ErrorQualified { .. } => layout(pool, ty, 6, 4),
        TypeKind::EffectQualified { .. } => layout(pool, ty, 7, 4),
        TypeKind::AssociatedType { .. } => Term {
            bytes: vec![8],
            nominal: BTreeSet::new(),
        },
        TypeKind::IterationStepTemplate { .. } => layout(pool, ty, 9, 5),
        TypeKind::Enum { .. } if structural => layout(pool, ty, 10, 6),
        _ => return Err(fail("nominal type lacks an identity anchor")),
    };
    match kind {
        TypeKind::Tuple { elements } => {
            count(elements.len(), &mut term.bytes)?;
            for &element in elements {
                term.child(get(terms, element)?)?;
            }
        }
        TypeKind::Function { params, ret } => parameters(&mut term, params, *ret, terms)?,
        TypeKind::Effect {
            params,
            ret,
            is_async,
        } => {
            term.bytes.push(u8::from(*is_async));
            parameters(&mut term, params, *ret, terms)?;
        }
        TypeKind::Optional { inner } | TypeKind::IterationStepTemplate { item: inner } => {
            term.child(get(terms, *inner)?)?
        }
        TypeKind::AssociatedType { trait_owner, name } => {
            term.child(get(terms, *trait_owner)?)?;
            term.name(*name)?;
        }
        TypeKind::ErrorQualified {
            errors: members,
            inner,
        }
        | TypeKind::EffectQualified {
            effects: members,
            inner,
        } => {
            term.child(get(terms, *inner)?)?;
            let mut set = members
                .iter()
                .map(|&member| get(terms, member))
                .collect::<Result<Vec<_>, _>>()?;
            set.sort_by(|a, b| a.bytes.cmp(&b.bytes));
            set.dedup_by(|a, b| a.bytes == b.bytes);
            count(set.len(), &mut term.bytes)?;
            for member in set {
                term.child(member)?;
            }
        }
        TypeKind::Enum { .. } => {
            let item = crate::iteration_step::step_payload(kind)
                .ok_or_else(|| fail("malformed structural IterationStep"))?;
            term.child(get(terms, item)?)?;
        }
        _ => return Err(fail("unsupported structural identity term")),
    }
    Ok(term)
}
fn fields(
    record: &mut Term,
    fields: &[FieldInfo],
    terms: &[Option<Term>],
) -> Result<(), TypeIdentityError> {
    count(fields.len(), &mut record.bytes)?;
    for field in fields {
        record.name(field.name)?;
        record.child(get(terms, field.ty)?)?;
        record.bytes.extend_from_slice(&field.offset.to_be_bytes());
        record.bytes.push(u8::from(field.has_default));
    }
    budget(&record.bytes)
}
fn nominal_record(
    pool: &TypePool,
    ty: TypeIndex,
    terms: &[Option<Term>],
) -> Result<Term, TypeIdentityError> {
    let info = pool.get(ty);
    let tag = crate::identity::nominal_kind(&info.kind, false)
        .ok_or_else(|| fail("invalid nominal identity record"))?;
    let mut record = layout(pool, ty, tag, tag);
    match &info.kind {
        TypeKind::Struct {
            fields: members, ..
        } => fields(&mut record, members, terms)?,
        TypeKind::Enum { variants, .. } => {
            record.bytes.extend_from_slice(&[25, 8]);
            count(variants.len(), &mut record.bytes)?;
            for variant in variants {
                record.name(variant.name)?;
                record.bytes.extend_from_slice(&variant.tag.to_be_bytes());
                fields(&mut record, &variant.fields, terms)?;
            }
        }
        TypeKind::Newtype { inner, .. } => record.child(get(terms, *inner)?)?,
        TypeKind::Module { .. } => {}
        TypeKind::Effect {
            params,
            ret,
            is_async,
        } => {
            record.bytes.push(u8::from(*is_async));
            parameters(&mut record, params, *ret, terms)?;
        }
        TypeKind::Trait {
            parents,
            assoc_types,
            ..
        } => {
            count(parents.len(), &mut record.bytes)?;
            for &parent in parents {
                record.child(get(terms, parent)?)?;
            }
            count(assoc_types.len(), &mut record.bytes)?;
            for &(name, default) in assoc_types {
                record.name(name)?;
                record.bytes.push(u8::from(default != TypeIndex::INVALID));
                if default != TypeIndex::INVALID {
                    record.child(get(terms, default)?)?;
                }
                let expression = pool
                    .associated_defaults
                    .iter()
                    .find(|default| default.trait_owner == ty && default.name == name)
                    .map(|default| &default.expression);
                record.bytes.push(u8::from(expression.is_some()));
                if let Some(expression) = expression {
                    record.child(&default_expression(expression, terms)?)?;
                }
            }
            let schema = pool
                .trait_schemas
                .iter()
                .find(|schema| schema.trait_type == ty);
            let slots = schema.map_or(&[][..], |schema| schema.slots.as_slice());
            count(slots.len(), &mut record.bytes)?;
            for slot in slots {
                record.child(get(terms, slot.trait_owner)?)?;
                record.name(slot.name)?;
                record.bytes.push(u8::from(slot.signature.is_some()));
                if let Some(signature) = &slot.signature {
                    record.child(get(terms, signature.declaration)?)?;
                    let mut paths = signature
                        .self_paths
                        .iter()
                        .map(|path| type_path(pool, signature.declaration, terms, path))
                        .collect::<Result<Vec<_>, _>>()?;
                    paths.sort();
                    paths.dedup();
                    count(paths.len(), &mut record.bytes)?;
                    for path in paths {
                        blob(&path, &mut record.bytes)?;
                    }
                    let mut paths = Vec::new();
                    for marker in &signature.associated_paths {
                        let mut term = Term::default();
                        term.child(get(terms, marker.trait_owner)?)?;
                        term.name(marker.name)?;
                        blob(
                            &type_path(pool, signature.declaration, terms, &marker.path)?,
                            &mut term.bytes,
                        )?;
                        paths.push(term);
                    }
                    paths.sort_by(|a, b| a.bytes.cmp(&b.bytes));
                    paths.dedup_by(|a, b| a.bytes == b.bytes);
                    count(paths.len(), &mut record.bytes)?;
                    for path in paths {
                        record.child(&path)?;
                    }
                    count(signature.parameter_kinds.len(), &mut record.bytes)?;
                    for kind in &signature.parameter_kinds {
                        record.bytes.push(match kind {
                            TraitParameterKind::Receiver => 0,
                            TraitParameterKind::Required => 1,
                            TraitParameterKind::Optional => 2,
                            TraitParameterKind::ListVariadic => 3,
                            TraitParameterKind::MapVariadic => 4,
                        });
                    }
                }
            }
        }
        _ => return Err(fail("invalid nominal layout kind")),
    }
    budget(&record.bytes)?;
    Ok(record)
}
fn type_path(
    pool: &TypePool,
    root: TypeIndex,
    terms: &[Option<Term>],
    path: &[TraitTypeStep],
) -> Result<Vec<u8>, TypeIdentityError> {
    let mut bytes = Vec::new();
    count(path.len(), &mut bytes)?;
    let mut current = root;
    for step in path {
        current = pool
            .canonical_type(current)
            .ok_or_else(|| fail("invalid trait identity path type"))?;
        let kind = &pool.get(current).kind;
        let (tag, index, next) = match (kind, step) {
            (
                TypeKind::Function { params, .. } | TypeKind::Effect { params, .. },
                TraitTypeStep::Parameter(index),
            ) => (0, Some(*index), params.get(*index as usize).copied()),
            (
                TypeKind::Function { ret, .. } | TypeKind::Effect { ret, .. },
                TraitTypeStep::Return,
            ) => (1, None, Some(*ret)),
            (TypeKind::Tuple { elements }, TraitTypeStep::TupleElement(index)) => {
                (2, Some(*index), elements.get(*index as usize).copied())
            }
            (TypeKind::Optional { inner }, TraitTypeStep::OptionalInner) => (3, None, Some(*inner)),
            (TypeKind::ErrorQualified { inner, .. }, TraitTypeStep::ErrorInner) => {
                (4, None, Some(*inner))
            }
            (TypeKind::EffectQualified { inner, .. }, TraitTypeStep::EffectInner) => {
                (6, None, Some(*inner))
            }
            (
                TypeKind::ErrorQualified {
                    errors: members, ..
                },
                TraitTypeStep::ErrorMember(index),
            )
            | (
                TypeKind::EffectQualified {
                    effects: members, ..
                },
                TraitTypeStep::EffectMember(index),
            ) => {
                let selected = *members
                    .get(*index as usize)
                    .ok_or_else(|| fail("invalid qualified identity path member"))?;
                let mut sorted = members
                    .iter()
                    .map(|&member| Ok(&get(terms, member)?.bytes))
                    .collect::<Result<Vec<_>, TypeIdentityError>>()?;
                sorted.sort();
                sorted.dedup();
                let selected_bytes = &get(terms, selected)?.bytes;
                let semantic = sorted
                    .iter()
                    .position(|bytes| *bytes == selected_bytes)
                    .ok_or_else(|| fail("missing semantic qualified path member"))?;
                let tag = if matches!(step, TraitTypeStep::ErrorMember(_)) {
                    5
                } else {
                    7
                };
                (tag, Some(semantic as u32), Some(selected))
            }
            (TypeKind::IterationStepTemplate { item }, TraitTypeStep::IterationItem) => {
                (8, None, Some(*item))
            }
            _ => return Err(fail("trait identity path does not match its declaration")),
        };
        bytes.push(tag);
        if let Some(index) = index {
            bytes.extend_from_slice(&index.to_be_bytes());
        }
        current = next.ok_or_else(|| fail("trait identity path index out of range"))?;
    }
    Ok(bytes)
}

fn default_expression(
    root: &AssociatedTypeExpr,
    terms: &[Option<Term>],
) -> Result<Term, TypeIdentityError> {
    let mut pending = vec![(root, false)];
    let mut results = Vec::<Term>::new();
    let mut items = 0usize;
    while let Some((expression, ready)) = pending.pop() {
        items += 1;
        if items > MAX_IDENTITY_ITEMS {
            return Err(fail("associated identity expression item limit exceeded"));
        }
        let children: Vec<_> = match expression {
            AssociatedTypeExpr::Optional { inner }
            | AssociatedTypeExpr::IterationStep { item: inner } => vec![inner.as_ref()],
            AssociatedTypeExpr::Tuple { elements } => elements.iter().collect(),
            AssociatedTypeExpr::Function {
                parameters,
                return_type,
            } => parameters.iter().chain([return_type.as_ref()]).collect(),
            _ => Vec::new(),
        };
        if !ready && !children.is_empty() {
            pending.push((expression, true));
            pending.extend(children.iter().rev().map(|child| (*child, false)));
            continue;
        }
        let tag = match expression {
            AssociatedTypeExpr::Required => 0,
            AssociatedTypeExpr::Concrete(_) => 1,
            AssociatedTypeExpr::SelfType { .. } => 2,
            AssociatedTypeExpr::Binding { .. } => 3,
            AssociatedTypeExpr::Optional { .. } => 4,
            AssociatedTypeExpr::IterationStep { .. } => 5,
            AssociatedTypeExpr::Tuple { .. } => 6,
            AssociatedTypeExpr::Function { .. } => 7,
        };
        let mut result = Term {
            bytes: vec![tag],
            nominal: BTreeSet::new(),
        };
        match expression {
            AssociatedTypeExpr::Concrete(ty) => result.child(get(terms, *ty)?)?,
            AssociatedTypeExpr::SelfType { trait_owner } => {
                result.child(get(terms, *trait_owner)?)?
            }
            AssociatedTypeExpr::Binding { trait_owner, name } => {
                result.child(get(terms, *trait_owner)?)?;
                result.name(*name)?;
            }
            AssociatedTypeExpr::Tuple { elements } => count(elements.len(), &mut result.bytes)?,
            AssociatedTypeExpr::Function { parameters, .. } => {
                count(parameters.len(), &mut result.bytes)?
            }
            _ => {}
        }
        if children.len() > results.len() {
            return Err(fail("invalid associated identity expression stack"));
        }
        for child in results.split_off(results.len() - children.len()) {
            result.child(&child)?;
        }
        results.push(result);
    }
    if results.len() != 1 {
        return Err(fail("invalid associated identity expression result"));
    }
    results
        .pop()
        .ok_or_else(|| fail("missing associated identity expression"))
}
