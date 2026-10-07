//! Exact implementation bindings for source-declared associated types.

use std::collections::{HashMap, HashSet};

use str_interner::StrId;

use crate::{
    SnapshotError, TraitMethodSignature, TraitSignatureError, TraitTypeStep, TypeIndex, TypeKind,
    TypePool,
};

pub(crate) fn contains_trait(types: &[crate::TypeInfo], root: TypeIndex) -> bool {
    let mut pending = vec![root];
    let mut seen = HashSet::new();
    while let Some(ty) = pending.pop() {
        if !seen.insert(ty) {
            continue;
        }
        match types.get(ty.as_u32() as usize).map(|info| &info.kind) {
            Some(
                TypeKind::Trait { .. }
                | TypeKind::AssociatedType { .. }
                | TypeKind::IterationStepTemplate { .. },
            )
            | None => return true,
            Some(TypeKind::Typealias { target, .. }) => pending.push(*target),
            Some(TypeKind::Optional { inner }) => pending.push(*inner),
            Some(TypeKind::Tuple { elements }) => pending.extend(elements),
            Some(TypeKind::Function { params, ret } | TypeKind::Effect { params, ret, .. }) => {
                pending.extend(params);
                pending.push(*ret);
            }
            Some(TypeKind::ErrorQualified { errors, inner }) => {
                pending.extend(errors);
                pending.push(*inner);
            }
            Some(TypeKind::EffectQualified { effects, inner }) => {
                pending.extend(effects);
                pending.push(*inner);
            }
            _ => {}
        }
    }
    false
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssociatedTypeBinding {
    pub implementor: TypeIndex,
    pub trait_type: TypeIndex,
    pub visible_scope: Option<u32>,
    pub trait_owner: TypeIndex,
    pub name: StrId,
    pub value: TypeIndex,
}

pub(crate) fn ancestor(types: &[crate::TypeInfo], child: TypeIndex, owner: TypeIndex) -> bool {
    let mut pending = vec![child];
    let mut seen = HashSet::new();
    while let Some(ty) = pending.pop() {
        if !seen.insert(ty) {
            continue;
        }
        if ty == owner {
            return true;
        }
        if let Some(crate::TypeInfo {
            kind: TypeKind::Typealias { target, .. },
            ..
        }) = types.get(ty.as_u32() as usize)
        {
            pending.push(*target);
        }
        if let Some(crate::TypeInfo {
            kind: TypeKind::Trait { parents, .. },
            ..
        }) = types.get(ty.as_u32() as usize)
        {
            pending.extend(parents);
        }
    }
    false
}

fn declarations(
    types: &[crate::TypeInfo],
    root: TypeIndex,
) -> Result<HashSet<(TypeIndex, StrId)>, SnapshotError> {
    let mut pending = vec![root];
    let mut seen = HashSet::new();
    let mut result = HashSet::new();
    while let Some(owner) = pending.pop() {
        if !seen.insert(owner) {
            continue;
        }
        match types.get(owner.as_u32() as usize).map(|info| &info.kind) {
            Some(TypeKind::Typealias { target, .. }) => pending.push(*target),
            Some(TypeKind::Trait {
                parents,
                assoc_types,
                ..
            }) => {
                pending.extend(parents);
                result.extend(assoc_types.iter().map(|&(name, _)| (owner, name)));
            }
            _ => {
                return Err(SnapshotError::new(
                    "associated type view is not a valid trait",
                ));
            }
        }
    }
    Ok(result)
}

pub(crate) fn validate_bindings(
    types: &[crate::TypeInfo],
    records: &[crate::TraitImplRecord],
    bindings: &[AssociatedTypeBinding],
    complete: bool,
) -> Result<(), SnapshotError> {
    let records_index: HashSet<_> = records
        .iter()
        .map(|record| (record.implementor, record.trait_type, record.visible_scope))
        .collect();
    let mut declared = HashMap::new();
    for record in records {
        if let std::collections::hash_map::Entry::Vacant(entry) = declared.entry(record.trait_type)
        {
            entry.insert(declarations(types, record.trait_type)?);
        }
    }
    let mut seen = HashSet::new();
    for binding in bindings {
        if !seen.insert((
            binding.implementor,
            binding.trait_type,
            binding.visible_scope,
            binding.trait_owner,
            binding.name,
        )) {
            return Err(SnapshotError::new("duplicate associated type binding"));
        }
        if !records_index.contains(&(
            binding.implementor,
            binding.trait_type,
            binding.visible_scope,
        )) {
            return Err(SnapshotError::new(
                "associated type binding has no exact implementation",
            ));
        }
        if !declared
            .get(&binding.trait_type)
            .is_some_and(|declarations| declarations.contains(&(binding.trait_owner, binding.name)))
            || binding.value == TypeIndex::INVALID
            || types.get(binding.value.as_u32() as usize).is_none()
            || contains_trait(types, binding.value)
        {
            return Err(SnapshotError::new("invalid associated type binding"));
        }
    }
    if complete {
        for record in records {
            for &(owner, name) in &declared[&record.trait_type] {
                if !seen.contains(&(
                    record.implementor,
                    record.trait_type,
                    record.visible_scope,
                    owner,
                    name,
                )) {
                    return Err(SnapshotError::new(
                        "missing associated type implementation binding",
                    ));
                }
            }
        }
    }
    Ok(())
}

impl TypePool {
    /// Associated values currently require a concrete type shape, without a
    /// trait view whose return/storage evidence has no representation.
    pub fn is_static_associated_type(&self, ty: TypeIndex) -> bool {
        self.canonical_type(ty).is_some()
            && !contains_trait(&self.types, ty)
            && !self.contains_associated_type(ty)
    }
    /// Whether a trait view includes an associated declaration of its own or a parent.
    pub fn trait_has_associated_types(&self, view: TypeIndex) -> Result<bool, SnapshotError> {
        Ok(!declarations(&self.types, view)?.is_empty())
    }

    /// Check the frozen binding environment of one exact implementation without
    /// evaluating declaration defaults or selecting another visible provider.
    pub fn check_associated_implementation_bindings(
        &self,
        implementor: TypeIndex,
        trait_type: TypeIndex,
        visible_scope: Option<u32>,
    ) -> Result<(), SnapshotError> {
        if !self.trait_impls.iter().any(|record| {
            record.implementor == implementor
                && record.trait_type == trait_type
                && record.visible_scope == visible_scope
        }) {
            return Err(SnapshotError::new(
                "associated bindings have no exact implementation",
            ));
        }
        for (owner, name) in declarations(&self.types, trait_type)? {
            let mut matching = self.associated_bindings.iter().filter(|binding| {
                binding.implementor == implementor
                    && binding.trait_type == trait_type
                    && binding.visible_scope == visible_scope
                    && binding.trait_owner == owner
                    && binding.name == name
            });
            let binding = matching.next().ok_or_else(|| {
                SnapshotError::new("missing exact associated implementation binding")
            })?;
            if matching.next().is_some() || !self.is_static_associated_type(binding.value) {
                return Err(SnapshotError::new(
                    "invalid exact associated implementation binding",
                ));
            }
        }
        Ok(())
    }

    pub fn associated_bindings_snapshot(&self) -> &[AssociatedTypeBinding] {
        &self.associated_bindings
    }

    /// Register only after the corresponding implementation has been staged.
    pub fn register_associated_binding(
        &mut self,
        binding: AssociatedTypeBinding,
    ) -> Result<(), SnapshotError> {
        if self.vtables.iter().any(|table| {
            table.implementor == binding.implementor
                && table.trait_type == binding.trait_type
                && table.visible_scope == binding.visible_scope
        }) {
            return Err(SnapshotError::new(
                "associated bindings must precede vtable publication",
            ));
        }
        let mut staged = self.associated_bindings.clone();
        staged.push(binding);
        validate_bindings(&self.types, &self.trait_impls, &staged, false)?;
        self.associated_bindings = staged;
        Ok(())
    }

    pub(crate) fn associated_substitutions(
        &self,
        signature: &TraitMethodSignature,
        implementor: TypeIndex,
        implementation_trait: TypeIndex,
        scope: Option<u32>,
    ) -> Result<Vec<(Vec<TraitTypeStep>, TypeIndex)>, TraitSignatureError> {
        signature
            .associated_paths
            .iter()
            .map(|marker| {
                let value = self
                    .associated_bindings
                    .iter()
                    .find(|binding| {
                        binding.implementor == implementor
                            && binding.trait_type == implementation_trait
                            && binding.visible_scope == scope
                            && binding.trait_owner == marker.trait_owner
                            && binding.name == marker.name
                    })
                    .ok_or(TraitSignatureError::MissingAssociatedBinding)?
                    .value;
                Ok((marker.path.clone(), value))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Intrinsic, MethodAccess, MethodSlot, TraitAssociatedPath, TraitDispatchSchema,
        TraitImplRecord, TraitMethodKey, TraitMethodSignature, TraitParameterKind, TraitTypeStep,
        TypeId, TypeInfo, VTable,
    };

    #[test]
    fn associated_paths_replace_only_the_marked_default_and_reject_corruption() {
        let mut pool = TypePool::with_intrinsics();
        let name = str_interner::intern("Item");
        let owner = pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("Source"),
                parents: vec![],
                assoc_types: vec![(name, Intrinsic::Any.type_index())],
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let ret = pool.intern_structural(TypeKind::Tuple {
            elements: vec![Intrinsic::Any.type_index(), Intrinsic::Any.type_index()],
        });
        let declaration = pool.intern_structural(TypeKind::Function {
            params: vec![owner],
            ret,
        });
        let implementor = Intrinsic::I64.type_index();
        let method = MethodSlot {
            name: str_interner::intern("read"),
            func_id: 1,
            access: MethodAccess::Public,
            trait_impl: Some(owner),
            visible_scope: None,
        };
        pool.add_method(implementor, method.clone());
        pool.add_trait_impl(TraitImplRecord {
            implementor,
            trait_type: owner,
            visible_scope: None,
            methods: vec![method.clone()],
        });
        let key = TraitMethodKey {
            trait_owner: owner,
            name: method.name,
            signature: Some(TraitMethodSignature {
                declaration,
                self_paths: vec![vec![TraitTypeStep::Parameter(0)]],
                parameter_kinds: vec![TraitParameterKind::Receiver],
                associated_paths: vec![TraitAssociatedPath {
                    trait_owner: owner,
                    name,
                    path: vec![TraitTypeStep::Return, TraitTypeStep::TupleElement(0)],
                }],
            }),
        };
        pool.register_associated_binding(AssociatedTypeBinding {
            implementor,
            trait_type: owner,
            visible_scope: None,
            trait_owner: owner,
            name,
            value: Intrinsic::Str.type_index(),
        })
        .unwrap();
        pool.register_trait_schema(TraitDispatchSchema {
            trait_type: owner,
            slots: vec![key.clone()],
        })
        .unwrap();
        let specialized = pool
            .instantiate_trait_method_signature(&key, implementor)
            .unwrap();
        let TypeKind::Function { ret, .. } = pool.get(specialized).kind else {
            panic!()
        };
        assert!(
            matches!(&pool.get(ret).kind,TypeKind::Tuple {elements} if elements == &[Intrinsic::Str.type_index(),Intrinsic::Any.type_index()])
        );
        pool.check_trait_method_signature(&key, implementor, specialized)
            .unwrap();
        assert_eq!(
            pool.check_trait_method_signature_in_impl(
                &key,
                implementor,
                specialized,
                owner,
                Some(99)
            ),
            Err(TraitSignatureError::MissingAssociatedBinding)
        );
        pool.add_vtable(VTable {
            implementor,
            trait_type: owner,
            visible_scope: None,
            entries: vec![1],
        });
        let snapshot = pool.snapshot();
        TypePool::restore(snapshot.clone()).unwrap();
        let mut missing = snapshot.clone();
        missing.associated_bindings.clear();
        assert!(TypePool::restore(missing).is_err());
        let mut foreign = snapshot.clone();
        foreign.trait_schemas[0].slots[0]
            .signature
            .as_mut()
            .unwrap()
            .associated_paths[0]
            .trait_owner = pool.well_known.eq;
        assert!(TypePool::restore(foreign).is_err());
        let mut duplicate = snapshot;
        duplicate
            .associated_bindings
            .push(duplicate.associated_bindings[0].clone());
        assert!(TypePool::restore(duplicate).is_err());
        assert!(
            pool.register_associated_binding(pool.associated_bindings[0].clone())
                .is_err()
        );
    }
}
