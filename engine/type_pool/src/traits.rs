//! Trait implementation identity includes the extension's lexical scope.

use std::fmt;

use str_interner::StrId;

use crate::{
    Intrinsic, MethodSlot, TraitImplRecord, TypeIndex, TypeKind, TypePool, VTable, numeric,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraitLookupError {
    InvalidType(TypeIndex),
    NotTrait(TypeIndex),
    InvalidCaller(u32),
    InvalidVisibilityScope(u32),
    MissingScopeGraph,
    Ambiguous,
}

impl fmt::Display for TraitLookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidType(ty) => write!(f, "invalid trait lookup type {}", ty.as_u32()),
            Self::NotTrait(ty) => write!(f, "type {} is not a trait", ty.as_u32()),
            Self::InvalidCaller(scope) => write!(f, "invalid trait lookup caller scope {scope}"),
            Self::InvalidVisibilityScope(scope) => {
                write!(f, "invalid trait implementation scope {scope}")
            }
            Self::MissingScopeGraph => {
                f.write_str("scoped trait lookup requires a lexical scope graph")
            }
            Self::Ambiguous => {
                f.write_str("multiple trait implementations are visible from this scope")
            }
        }
    }
}

impl std::error::Error for TraitLookupError {}

impl TypePool {
    /// Append a staged method to one exact implementation before vtable publication.
    /// Existing tables are immutable dispatch evidence; this API never rewrites them.
    pub fn add_trait_method(
        &mut self,
        implementor: TypeIndex,
        trait_type: TypeIndex,
        visible_scope: Option<u32>,
        method: MethodSlot,
    ) -> Result<(), crate::SnapshotError> {
        let fail = crate::SnapshotError::new;
        let implementor = self
            .canonical_type(implementor)
            .ok_or_else(|| fail("invalid implementation owner"))?;
        let trait_type = self
            .canonical_type(trait_type)
            .ok_or_else(|| fail("invalid implementation trait"))?;
        if !matches!(self.get(trait_type).kind, TypeKind::Trait { .. })
            || method.trait_impl.and_then(|ty| self.canonical_type(ty)) != Some(trait_type)
            || method.visible_scope != visible_scope
        {
            return Err(fail("method does not match its exact implementation"));
        }
        let records: Vec<_> = self
            .trait_impls
            .iter()
            .enumerate()
            .filter(|(_, record)| {
                self.canonical_type(record.implementor) == Some(implementor)
                    && self.canonical_type(record.trait_type) == Some(trait_type)
                    && record.visible_scope == visible_scope
            })
            .map(|(index, _)| index)
            .collect();
        if records.len() != 1 {
            return Err(fail("method requires one exact implementation record"));
        }
        if self.trait_impls[records[0]]
            .methods
            .iter()
            .any(|slot| slot.name == method.name)
        {
            return Err(fail("duplicate implementation method"));
        }
        if self.vtables.iter().any(|table| {
            self.canonical_type(table.implementor) == Some(implementor)
                && self.canonical_type(table.trait_type) == Some(trait_type)
                && table.visible_scope == visible_scope
        }) {
            return Err(fail("cannot extend a published implementation table"));
        }
        crate::access::validate_method(&self.scopes, &method, &self.scope_packages)?;
        self.trait_impls[records[0]].methods.push(method);
        Ok(())
    }
}

pub(crate) fn validate_record_scope(
    scopes: &[crate::ScopeContext],
    record: &TraitImplRecord,
) -> Result<(), crate::SnapshotError> {
    if let Some(scope) = record.visible_scope {
        if scopes.is_empty() {
            if record.methods.is_empty()
                || record
                    .methods
                    .iter()
                    .any(|method| method.access != crate::MethodAccess::LegacyUnknown)
            {
                return Err(crate::SnapshotError::new(
                    "scoped trait implementation requires a lexical scope graph",
                ));
            }
        } else if scope as usize >= scopes.len() {
            return Err(crate::SnapshotError::new(
                "trait implementation scope is out of range",
            ));
        }
    }
    Ok(())
}

impl TypePool {
    fn trait_lookup_types(
        &self,
        ty: TypeIndex,
        trait_ty: TypeIndex,
    ) -> Result<(TypeIndex, TypeIndex), TraitLookupError> {
        let ty = self
            .canonical_type(ty)
            .ok_or(TraitLookupError::InvalidType(ty))?;
        let trait_ty = self
            .canonical_type(trait_ty)
            .ok_or(TraitLookupError::InvalidType(trait_ty))?;
        if !matches!(self.get(trait_ty).kind, TypeKind::Trait { .. }) {
            return Err(TraitLookupError::NotTrait(trait_ty));
        }
        Ok((ty, trait_ty))
    }

    fn trait_caller(&self, caller: u32) -> Result<(), TraitLookupError> {
        if self.scopes.is_empty() {
            return Err(TraitLookupError::MissingScopeGraph);
        }
        self.scope_context(caller)
            .ok_or(TraitLookupError::InvalidCaller(caller))
            .map(|_| ())
    }

    fn trait_scope_visible(
        &self,
        owner: Option<u32>,
        mut caller: u32,
    ) -> Result<bool, TraitLookupError> {
        let Some(owner) = owner else {
            return Ok(true);
        };
        let owner_context = self
            .scope_context(owner)
            .ok_or(TraitLookupError::InvalidVisibilityScope(owner))?;
        let caller_context = self
            .scope_context(caller)
            .ok_or(TraitLookupError::InvalidCaller(caller))?;
        if caller_context.package != owner_context.package {
            return Ok(false);
        }
        loop {
            if owner == caller {
                return Ok(true);
            }
            let Some(parent) = self.scopes[caller as usize].parent else {
                return Ok(false);
            };
            caller = parent;
        }
    }

    /// Select a unique global or lexically visible extension implementation.
    /// An inner extension does not silently override another visible record.
    pub fn find_trait_impl_scoped(
        &self,
        ty: TypeIndex,
        trait_ty: TypeIndex,
        caller_scope: u32,
    ) -> Result<Option<&TraitImplRecord>, TraitLookupError> {
        self.trait_caller(caller_scope)?;
        let (ty, trait_ty) = self.trait_lookup_types(ty, trait_ty)?;
        let mut result = None;
        for record in &self.trait_impls {
            if self.canonical_type(record.implementor) == Some(ty)
                && self.canonical_type(record.trait_type) == Some(trait_ty)
                && self.trait_scope_visible(record.visible_scope, caller_scope)?
            {
                if result.is_some() {
                    return Err(TraitLookupError::Ambiguous);
                }
                result = Some(record);
            }
        }
        Ok(result)
    }

    pub fn has_trait_impl_scoped(
        &self,
        ty: TypeIndex,
        trait_ty: TypeIndex,
        caller_scope: u32,
    ) -> Result<bool, TraitLookupError> {
        self.find_trait_impl_scoped(ty, trait_ty, caller_scope)
            .map(|record| record.is_some())
    }

    pub fn find_trait_method_scoped(
        &self,
        ty: TypeIndex,
        trait_ty: TypeIndex,
        name: StrId,
        caller_scope: u32,
    ) -> Result<Option<&MethodSlot>, TraitLookupError> {
        let Some(record) = self.find_trait_impl_scoped(ty, trait_ty, caller_scope)? else {
            return Ok(None);
        };
        let mut methods = record.methods.iter().filter(|method| method.name == name);
        let result = methods.next();
        if methods.next().is_some() {
            return Err(TraitLookupError::Ambiguous);
        }
        Ok(result)
    }

    pub fn find_vtable_scoped(
        &self,
        ty: TypeIndex,
        trait_ty: TypeIndex,
        caller_scope: u32,
    ) -> Result<Option<&VTable>, TraitLookupError> {
        let Some(record) = self.find_trait_impl_scoped(ty, trait_ty, caller_scope)? else {
            return Ok(None);
        };
        let (ty, trait_ty) = self.trait_lookup_types(ty, trait_ty)?;
        let mut tables = self.vtables.iter().filter(|table| {
            table.visible_scope == record.visible_scope
                && self.canonical_type(table.implementor) == Some(ty)
                && self.canonical_type(table.trait_type) == Some(trait_ty)
        });
        let result = tables.next();
        if tables.next().is_some() {
            return Err(TraitLookupError::Ambiguous);
        }
        Ok(result)
    }

    /// Existing subtyping rules with lexical trait evidence and checked ambiguity.
    pub fn is_subtype_scoped(
        &self,
        sub: TypeIndex,
        sup: TypeIndex,
        caller_scope: u32,
    ) -> Result<bool, TraitLookupError> {
        self.trait_caller(caller_scope)?;
        self.subtype_with_scope(
            sub,
            sup,
            self.types.len().saturating_add(1),
            Some(caller_scope),
        )
    }

    pub(super) fn subtype_with_scope(
        &self,
        sub: TypeIndex,
        sup: TypeIndex,
        remaining: usize,
        caller: Option<u32>,
    ) -> Result<bool, TraitLookupError> {
        if remaining == 0 {
            return Ok(false);
        }
        let sub = self
            .canonical_type(sub)
            .ok_or(TraitLookupError::InvalidType(sub))?;
        let sup = self
            .canonical_type(sup)
            .ok_or(TraitLookupError::InvalidType(sup))?;
        if sub == sup
            || sup == Intrinsic::Any.type_index()
            || sub == Intrinsic::NoReturn.type_index()
        {
            return Ok(true);
        }
        if let (Some(source), Some(target)) = (self.as_intrinsic(sub), self.as_intrinsic(sup))
            && numeric::can_widen(source, target)
        {
            return Ok(true);
        }
        if let (TypeKind::Tuple { elements: source }, TypeKind::Tuple { elements: target }) =
            (&self.get(sub).kind, &self.get(sup).kind)
        {
            if source.len() != target.len() {
                return Ok(false);
            }
            for (&source, &target) in source.iter().zip(target) {
                if !self.subtype_with_scope(source, target, remaining - 1, caller)? {
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        if let TypeKind::Optional { inner } = self.get(sup).kind {
            let source = match self.get(sub).kind {
                TypeKind::Optional { inner } => inner,
                _ => sub,
            };
            return self.subtype_with_scope(source, inner, remaining - 1, caller);
        }
        if let TypeKind::ErrorQualified { errors, inner } = &self.get(sup).kind {
            if !matches!(self.get(sub).kind, TypeKind::ErrorQualified { .. })
                && self.subtype_with_scope(sub, *inner, remaining - 1, caller)?
            {
                return Ok(true);
            }
            if let TypeKind::ErrorQualified {
                errors: source_errors,
                inner: source_inner,
            } = &self.get(sub).kind
                && self.subtype_with_scope(*source_inner, *inner, remaining - 1, caller)?
            {
                for &source in source_errors {
                    let source = self
                        .canonical_type(source)
                        .ok_or(TraitLookupError::InvalidType(source))?;
                    let mut found = false;
                    for &target in errors {
                        let target = self
                            .canonical_type(target)
                            .ok_or(TraitLookupError::InvalidType(target))?;
                        found |= source == target;
                    }
                    if !found {
                        return Ok(false);
                    }
                }
                return Ok(true);
            }
        }
        if matches!(self.get(sup).kind, TypeKind::Trait { .. }) {
            let implemented = match caller {
                Some(scope) => self.has_trait_impl_scoped(sub, sup, scope)?,
                None => self.has_trait_impl(sub, sup),
            };
            if implemented {
                return Ok(true);
            }
            if let TypeKind::Trait { parents, .. } = &self.get(sub).kind {
                for parent in parents {
                    if self.subtype_with_scope(*parent, sup, remaining - 1, caller)? {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MethodAccess, ScopeContext, TypeId, TypeInfo};

    #[test]
    fn staged_methods_update_only_the_exact_record_and_never_published_tables() {
        let mut pool = pool();
        let owner = Intrinsic::I64.type_index();
        let trait_type = pool.well_known.ord;
        for visible_scope in [None, Some(1)] {
            pool.add_trait_impl(TraitImplRecord {
                implementor: owner,
                trait_type,
                visible_scope,
                methods: Vec::new(),
            });
        }
        let method = MethodSlot {
            name: str_interner::intern("lt"),
            func_id: 40,
            access: MethodAccess::Public,
            trait_impl: Some(trait_type),
            visible_scope: Some(1),
        };
        assert!(
            pool.add_trait_method(owner, trait_type, None, method.clone())
                .is_err()
        );
        pool.add_trait_method(owner, trait_type, Some(1), method.clone())
            .unwrap();
        assert!(
            pool.find_trait_impl(owner, trait_type)
                .unwrap()
                .methods
                .is_empty()
        );
        assert_eq!(
            pool.find_trait_impl_scoped(owner, trait_type, 1)
                .unwrap_err(),
            TraitLookupError::Ambiguous
        );
        assert_eq!(pool.trait_impls_snapshot()[1].methods[0].func_id, 40);
        assert!(
            pool.add_trait_method(owner, trait_type, Some(1), method)
                .is_err()
        );
        pool.add_vtable(VTable {
            implementor: owner,
            trait_type,
            visible_scope: Some(1),
            entries: vec![40],
        });
        let method = MethodSlot {
            name: str_interner::intern("gt"),
            func_id: 41,
            access: MethodAccess::Public,
            trait_impl: Some(trait_type),
            visible_scope: Some(1),
        };
        assert!(
            pool.add_trait_method(owner, trait_type, Some(1), method)
                .is_err()
        );
        assert_eq!(pool.trait_impls_snapshot()[1].methods.len(), 1);
        assert_eq!(pool.vtables_snapshot()[0].entries, [40]);
    }

    fn pool() -> TypePool {
        let mut pool = TypePool::with_intrinsics();
        pool.install_scopes(vec![
            ScopeContext {
                parent: None,
                package: 0,
                assoc_type: None,
            },
            ScopeContext {
                parent: Some(0),
                package: 0,
                assoc_type: None,
            },
            ScopeContext {
                parent: Some(0),
                package: 0,
                assoc_type: None,
            },
            ScopeContext {
                parent: Some(1),
                package: 0,
                assoc_type: None,
            },
        ])
        .unwrap();
        pool
    }

    fn implementation(
        pool: &mut TypePool,
        owner: TypeIndex,
        trait_type: TypeIndex,
        scope: Option<u32>,
        function: u32,
    ) {
        let method = MethodSlot {
            name: str_interner::intern("eq"),
            func_id: function,
            trait_impl: Some(trait_type),
            visible_scope: scope,
            access: MethodAccess::Public,
        };
        pool.add_method(owner, method.clone());
        pool.add_trait_impl(TraitImplRecord {
            implementor: owner,
            trait_type,
            visible_scope: scope,
            methods: vec![method],
        });
        pool.add_vtable(VTable {
            implementor: owner,
            trait_type,
            visible_scope: scope,
            entries: vec![function],
        });
    }

    fn alias(pool: &mut TypePool, name: &str, target: TypeIndex) -> TypeIndex {
        pool.register(TypeInfo {
            kind: TypeKind::Typealias {
                name: str_interner::intern(name),
                target,
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        })
    }

    #[test]
    fn synthetic_parent_cannot_expose_foreign_scoped_trait_implementation() {
        let mut pool = pool();
        let mut scopes = pool.scopes().to_vec();
        let foreign = scopes.len() as u32;
        scopes.push(ScopeContext {
            parent: Some(1),
            package: 1,
            assoc_type: None,
        });
        pool.install_scopes(scopes).unwrap();
        let owner = Intrinsic::I64.type_index();
        let eq = pool.well_known.eq;
        implementation(&mut pool, owner, eq, Some(1), 17);
        let restored = TypePool::restore(pool.snapshot()).unwrap();
        for pool in [&pool, &restored] {
            assert!(pool.find_trait_impl_scoped(owner, eq, 3).unwrap().is_some());
            assert!(
                pool.find_trait_impl_scoped(owner, eq, foreign)
                    .unwrap()
                    .is_none()
            );
            assert!(
                pool.find_vtable_scoped(owner, eq, foreign)
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn disjoint_scopes_and_aliases_select_exact_records_and_tables_without_global_leaks() {
        let mut pool = pool();
        let owner = Intrinsic::I64.type_index();
        let eq = pool.well_known.eq;
        implementation(&mut pool, owner, eq, Some(1), 17);
        implementation(&mut pool, owner, eq, Some(2), 29);
        let owner_alias = alias(&mut pool, "ScopedOwnerAlias", owner);
        let trait_alias = alias(&mut pool, "ScopedTraitAlias", eq);
        let restored = TypePool::restore(pool.snapshot()).unwrap();
        assert!(!restored.has_trait_impl(owner_alias, trait_alias));
        assert_eq!(restored.trait_impls_of(owner_alias).count(), 0);
        assert!(
            restored
                .find_trait_method(owner, eq, str_interner::intern("eq"))
                .is_none()
        );
        assert!(restored.find_vtable(owner, eq).is_none());
        assert!(!restored.is_subtype(owner, eq));
        assert!(
            restored
                .find_trait_impl_scoped(owner, eq, 0)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            restored
                .find_trait_method_scoped(owner_alias, trait_alias, str_interner::intern("eq"), 3)
                .unwrap()
                .unwrap()
                .func_id,
            17
        );
        assert_eq!(
            restored
                .find_vtable_scoped(owner_alias, trait_alias, 2)
                .unwrap()
                .unwrap()
                .entries,
            vec![29]
        );
        assert_eq!(
            restored
                .find_trait_impl_scoped(owner, eq, 3)
                .unwrap()
                .unwrap()
                .visible_scope,
            Some(1)
        );
    }

    #[test]
    fn nested_and_global_overlaps_are_ambiguous_including_qualified_subtyping() {
        let mut pool = pool();
        let owner = Intrinsic::I64.type_index();
        let eq = pool.well_known.eq;
        implementation(&mut pool, owner, eq, Some(1), 17);
        let optional = pool.intern_structural(TypeKind::Optional { inner: eq });
        let qualified = pool.intern_structural(TypeKind::ErrorQualified {
            errors: vec![Intrinsic::Str.type_index()],
            inner: eq,
        });
        assert_eq!(pool.is_subtype_scoped(owner, optional, 3), Ok(true));
        assert_eq!(pool.is_subtype_scoped(owner, qualified, 3), Ok(true));
        assert_eq!(pool.is_subtype_scoped(owner, optional, 2), Ok(false));
        implementation(&mut pool, owner, eq, Some(3), 29);
        for target in [eq, optional, qualified] {
            assert_eq!(
                pool.is_subtype_scoped(owner, target, 3),
                Err(TraitLookupError::Ambiguous)
            );
        }
        assert_eq!(
            pool.find_vtable_scoped(owner, eq, 3).unwrap_err(),
            TraitLookupError::Ambiguous
        );
        implementation(&mut pool, owner, eq, None, 41);
        assert_eq!(
            pool.find_trait_impl_scoped(owner, eq, 1).unwrap_err(),
            TraitLookupError::Ambiguous
        );
        assert_eq!(
            pool.find_trait_method(owner, eq, str_interner::intern("eq"))
                .unwrap()
                .func_id,
            41
        );
        assert_eq!(
            pool.find_trait_method_scoped(owner, eq, str_interner::intern("eq"), 2)
                .unwrap()
                .unwrap()
                .func_id,
            41
        );
    }

    #[test]
    fn empty_trait_records_keep_independent_scope_identity() {
        let mut pool = pool();
        let owner = Intrinsic::I64.type_index();
        let marker = pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("ScopedMarker"),
                parents: vec![],

                assoc_types: vec![],
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        for scope in [1, 2] {
            pool.add_trait_impl(TraitImplRecord {
                implementor: owner,
                trait_type: marker,
                visible_scope: Some(scope),
                methods: vec![],
            });
            pool.add_vtable(VTable {
                implementor: owner,
                trait_type: marker,
                visible_scope: Some(scope),
                entries: vec![],
            });
        }
        let restored = TypePool::restore(pool.snapshot()).unwrap();
        assert_eq!(
            restored
                .find_trait_impl_scoped(owner, marker, 3)
                .unwrap()
                .unwrap()
                .visible_scope,
            Some(1)
        );
        assert_eq!(
            restored
                .find_vtable_scoped(owner, marker, 2)
                .unwrap()
                .unwrap()
                .visible_scope,
            Some(2)
        );
        assert_eq!(restored.is_subtype_scoped(owner, marker, 0), Ok(false));
    }

    #[test]
    fn malformed_scopes_duplicates_and_nonmatching_tables_are_rejected() {
        let mut pool = pool();
        let owner = Intrinsic::I64.type_index();
        let eq = pool.well_known.eq;
        implementation(&mut pool, owner, eq, Some(1), 17);
        for scenario in 0..5 {
            let mut snapshot = pool.snapshot();
            match scenario {
                0 => snapshot.trait_impls.push(snapshot.trait_impls[0].clone()),
                1 => snapshot.trait_impls[0].visible_scope = Some(99),
                2 => snapshot.trait_impls[0].methods[0].visible_scope = Some(2),
                3 => snapshot.vtables[0].visible_scope = Some(2),
                _ => snapshot.vtables.push(snapshot.vtables[0].clone()),
            }
            assert!(TypePool::restore(snapshot).is_err());
        }
        let owner_alias = alias(&mut pool, "DuplicateScopedOwner", owner);
        let mut duplicate = pool.trait_impls[0].clone();
        duplicate.implementor = owner_alias;
        pool.add_trait_impl(duplicate);
        assert!(pool.validate().is_err());
        assert_eq!(
            pool.find_trait_impl_scoped(owner, eq, 99).unwrap_err(),
            TraitLookupError::InvalidCaller(99)
        );
        assert_eq!(
            pool.find_trait_impl_scoped(TypeIndex::INVALID, eq, 0)
                .unwrap_err(),
            TraitLookupError::InvalidType(TypeIndex::INVALID)
        );
        assert_eq!(
            pool.find_trait_impl_scoped(owner, owner, 0).unwrap_err(),
            TraitLookupError::NotTrait(owner)
        );
    }

    #[test]
    fn legacy_unknown_scopes_roundtrip_but_cannot_supply_scoped_evidence() {
        let mut pool = TypePool::with_intrinsics();
        let owner = Intrinsic::I64.type_index();
        let eq = pool.well_known.eq;
        let method = MethodSlot {
            name: str_interner::intern("eq"),
            func_id: 17,
            trait_impl: Some(eq),
            visible_scope: Some(999),
            access: MethodAccess::LegacyUnknown,
        };
        pool.add_trait_impl(TraitImplRecord {
            implementor: owner,
            trait_type: eq,
            visible_scope: Some(999),
            methods: vec![method],
        });
        pool.add_vtable(VTable {
            implementor: owner,
            trait_type: eq,
            visible_scope: Some(999),
            entries: vec![17],
        });
        let restored = TypePool::restore(pool.snapshot()).unwrap();
        assert!(!restored.has_trait_impl(owner, eq));
        assert_eq!(
            restored.find_trait_impl_scoped(owner, eq, 0).unwrap_err(),
            TraitLookupError::MissingScopeGraph
        );
        let mut snapshot = pool.snapshot();
        snapshot.trait_impls[0].methods.clear();
        assert!(TypePool::restore(snapshot).is_err());
    }

    #[test]
    fn generated_derived_publication_updates_global_entries_only() {
        let mut pool = pool();
        let owner = Intrinsic::I64.type_index();
        let eq = pool.well_known.eq;
        implementation(&mut pool, owner, eq, None, crate::DERIVE_FUNC_ID);
        implementation(&mut pool, owner, eq, Some(1), 17);
        pool.resolve_derived_methods([(owner, eq, str_interner::intern("eq"), 41)])
            .unwrap();
        assert_eq!(
            pool.methods_of(owner)
                .iter()
                .map(|method| method.func_id)
                .collect::<Vec<_>>(),
            vec![41, 17]
        );
        assert_eq!(
            pool.trait_impls_snapshot()
                .iter()
                .map(|record| record.methods[0].func_id)
                .collect::<Vec<_>>(),
            vec![41, 17]
        );
        assert_eq!(
            pool.vtables_snapshot()
                .iter()
                .map(|table| table.entries[0])
                .collect::<Vec<_>>(),
            vec![41, 17]
        );
        TypePool::restore(pool.snapshot()).unwrap();
    }
}
