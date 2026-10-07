//! Persisted trait slot identities and checked, frozen implementation selection.

use std::collections::HashSet;
use std::fmt;

use str_interner::StrId;

use crate::{
    MethodAccess, MethodSlot, ScopeContext, SnapshotError, TraitImplRecord, TypeIndex, TypeInfo,
    TypeKind, TypePool, VTable,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TraitMethodKey {
    pub trait_owner: TypeIndex,
    pub name: StrId,
    pub signature: Option<crate::TraitMethodSignature>,
}

/// Parent-first interface slots, deduplicated by name as in source resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitDispatchSchema {
    pub trait_type: TypeIndex,
    pub slots: Vec<TraitMethodKey>,
}

/// One validated slot. Declaration identity and selected implementation identity
/// are separate: a child implementation can supply an inherited method name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitMethodDescriptor {
    pub trait_owner: TypeIndex,
    pub name: StrId,
    pub signature: Option<crate::TraitMethodSignature>,
    pub implementation_trait: TypeIndex,
    pub implementation_scope: Option<u32>,
    pub access: MethodAccess,
    pub func_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TraitProofError {
    InvalidTable(usize),
    InvalidType(TypeIndex),
    NotTrait(TypeIndex),
    MissingSchema(TypeIndex),
    InvalidSchema(&'static str),
    InvalidScope(u32),
    MissingScopeGraph,
    MissingImplementation(TypeIndex),
    AmbiguousImplementation(TypeIndex),
    MissingMethod(TraitMethodKey),
    InvalidMethod(TraitMethodKey),
    EntryCount,
    WrongEntry {
        slot: usize,
        expected: u32,
        actual: u32,
    },
}

impl fmt::Display for TraitProofError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid trait dispatch proof: {self:?}")
    }
}
impl std::error::Error for TraitProofError {}

pub(crate) struct Registry<'a> {
    pub types: &'a [TypeInfo],
    pub scopes: &'a [ScopeContext],
    pub records: &'a [TraitImplRecord],
    pub tables: &'a [VTable],
    pub schemas: &'a [TraitDispatchSchema],
}

impl Registry<'_> {
    fn canonical(&self, mut ty: TypeIndex) -> Result<TypeIndex, TraitProofError> {
        for _ in 0..self.types.len() {
            match self.types.get(ty.as_u32() as usize).map(|info| &info.kind) {
                Some(TypeKind::Typealias { target, .. }) => ty = *target,
                Some(_) => return Ok(ty),
                None => break,
            }
        }
        Err(TraitProofError::InvalidType(ty))
    }

    fn trait_type(&self, ty: TypeIndex) -> Result<TypeIndex, TraitProofError> {
        let canonical = self.canonical(ty)?;
        if !matches!(
            self.types[canonical.as_u32() as usize].kind,
            TypeKind::Trait { .. }
        ) {
            return Err(TraitProofError::NotTrait(ty));
        }
        Ok(canonical)
    }

    fn schema(&self, ty: TypeIndex) -> Result<&TraitDispatchSchema, TraitProofError> {
        let ty = self.trait_type(ty)?;
        let mut found = None;
        for schema in self.schemas {
            if self.trait_type(schema.trait_type)? == ty {
                if found.is_some() {
                    return Err(TraitProofError::InvalidSchema("duplicate trait schema"));
                }
                found = Some(schema);
            }
        }
        found.ok_or(TraitProofError::MissingSchema(ty))
    }

    fn reachable(&self, root: TypeIndex, owner: TypeIndex) -> Result<bool, TraitProofError> {
        let mut pending = vec![root];
        let mut visited = HashSet::new();
        while let Some(ty) = pending.pop() {
            let ty = self.trait_type(ty)?;
            if !visited.insert(ty) {
                continue;
            }
            if ty == owner {
                return Ok(true);
            }
            if let TypeKind::Trait { parents, .. } = &self.types[ty.as_u32() as usize].kind {
                pending.extend(parents);
            }
        }
        Ok(false)
    }

    fn validate_schema(&self, schema: &TraitDispatchSchema) -> Result<(), TraitProofError> {
        let root = self.trait_type(schema.trait_type)?;
        if root != schema.trait_type {
            return Err(TraitProofError::InvalidSchema("noncanonical trait"));
        }
        let mut names = HashSet::new();
        for key in &schema.slots {
            crate::signatures::validate_signature(self.types, key)
                .map_err(|_| TraitProofError::InvalidSchema("invalid method signature"))?;
            let owner = self.trait_type(key.trait_owner)?;
            if owner != key.trait_owner || !self.reachable(root, owner)? {
                return Err(TraitProofError::InvalidSchema(
                    "method owner is not a canonical ancestor",
                ));
            }
            if str_interner::try_get(key.name).is_none() || !names.insert(key.name) {
                return Err(TraitProofError::InvalidSchema(
                    "invalid or duplicate method name",
                ));
            }
        }
        Ok(())
    }

    fn validate_order(&self, schema: &TraitDispatchSchema) -> Result<(), TraitProofError> {
        let mut expected = Vec::new();
        let mut names = HashSet::new();
        if let TypeKind::Trait { parents, .. } =
            &self.types[schema.trait_type.as_u32() as usize].kind
        {
            for parent in parents {
                for key in &self.schema(*parent)?.slots {
                    if names.insert(key.name) {
                        expected.push(key.clone());
                    }
                }
            }
        }
        for key in &schema.slots {
            if key.trait_owner == schema.trait_type && names.insert(key.name) {
                expected.push(key.clone());
            }
        }
        if expected != schema.slots {
            return Err(TraitProofError::InvalidSchema(
                "slots violate parent-first name shadowing",
            ));
        }
        Ok(())
    }

    fn visible(&self, owner: Option<u32>, origin: Option<u32>) -> Result<bool, TraitProofError> {
        let Some(owner) = owner else {
            return Ok(true);
        };
        let Some(mut origin) = origin else {
            return Ok(false);
        };
        if self.scopes.is_empty() {
            return Err(TraitProofError::MissingScopeGraph);
        }
        if owner as usize >= self.scopes.len() {
            return Err(TraitProofError::InvalidScope(owner));
        }
        for _ in 0..self.scopes.len() {
            let scope = self
                .scopes
                .get(origin as usize)
                .ok_or(TraitProofError::InvalidScope(origin))?;
            if scope.package != self.scopes[owner as usize].package {
                return Ok(false);
            }
            if origin == owner {
                return Ok(true);
            }
            let Some(parent) = scope.parent else {
                return Ok(false);
            };
            origin = parent;
        }
        Err(TraitProofError::InvalidScope(origin))
    }

    fn record(
        &self,
        owner: TypeIndex,
        trait_ty: TypeIndex,
        origin: Option<u32>,
        exact: bool,
    ) -> Result<&TraitImplRecord, TraitProofError> {
        let mut result = None;
        for record in self.records {
            if self.canonical(record.implementor)? == owner
                && self.trait_type(record.trait_type)? == trait_ty
                && (if exact {
                    record.visible_scope == origin
                } else {
                    self.visible(record.visible_scope, origin)?
                })
            {
                if result.is_some() {
                    return Err(TraitProofError::AmbiguousImplementation(trait_ty));
                }
                result = Some(record);
            }
        }
        result.ok_or(TraitProofError::MissingImplementation(trait_ty))
    }

    fn method<'a>(
        &self,
        record: &'a TraitImplRecord,
        key: &TraitMethodKey,
    ) -> Result<Option<&'a MethodSlot>, TraitProofError> {
        let mut result = None;
        for method in &record.methods {
            if method.name == key.name {
                if result.is_some()
                    || method.visible_scope != record.visible_scope
                    || method
                        .trait_impl
                        .map(|ty| self.trait_type(ty))
                        .transpose()?
                        != Some(self.trait_type(record.trait_type)?)
                {
                    return Err(TraitProofError::InvalidMethod(key.clone()));
                }
                result = Some(method);
            }
        }
        Ok(result)
    }

    pub fn descriptors(&self, index: usize) -> Result<Vec<TraitMethodDescriptor>, TraitProofError> {
        let table = self
            .tables
            .get(index)
            .ok_or(TraitProofError::InvalidTable(index))?;
        let owner = self.canonical(table.implementor)?;
        let trait_ty = self.trait_type(table.trait_type)?;
        let schema = self.schema(trait_ty)?;
        let mut pending = vec![(trait_ty, false)];
        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();
        while let Some((ancestor, exiting)) = pending.pop() {
            let ancestor = self.trait_type(ancestor)?;
            if exiting {
                visiting.remove(&ancestor);
                visited.insert(ancestor);
                continue;
            }
            if visited.contains(&ancestor) {
                continue;
            }
            if !visiting.insert(ancestor) {
                return Err(TraitProofError::InvalidSchema("cyclic trait ancestry"));
            }
            let ancestor_schema = self.schema(ancestor)?;
            self.validate_schema(ancestor_schema)?;
            self.validate_order(ancestor_schema)?;
            pending.push((ancestor, true));
            if let TypeKind::Trait { parents, .. } = &self.types[ancestor.as_u32() as usize].kind {
                pending.extend(parents.iter().rev().map(|&parent| (parent, false)));
            }
        }
        if schema.slots.len() != table.entries.len() {
            return Err(TraitProofError::EntryCount);
        }
        let root = self.record(owner, trait_ty, table.visible_scope, true)?;
        if let Some(scope) = root.visible_scope {
            // Empty marker traits still require a valid origin for evidence.
            self.visible(Some(scope), Some(scope))?;
        }
        let mut descriptors = Vec::with_capacity(schema.slots.len());
        for (slot, key) in schema.slots.iter().enumerate() {
            let (record, method) = if let Some(method) = self.method(root, key)? {
                (root, method)
            } else {
                let parent = self.record(owner, key.trait_owner, root.visible_scope, false)?;
                (
                    parent,
                    self.method(parent, key)?
                        .ok_or(TraitProofError::MissingMethod(key.clone()))?,
                )
            };
            if method.func_id != table.entries[slot] {
                return Err(TraitProofError::WrongEntry {
                    slot,
                    expected: method.func_id,
                    actual: table.entries[slot],
                });
            }
            descriptors.push(TraitMethodDescriptor {
                trait_owner: key.trait_owner,
                name: key.name,
                signature: key.signature.clone(),
                implementation_trait: self.trait_type(record.trait_type)?,
                implementation_scope: record.visible_scope,
                access: method.access,
                func_id: method.func_id,
            });
        }
        Ok(descriptors)
    }

    pub fn validate(&self) -> Result<(), TraitProofError> {
        let mut seen = HashSet::new();
        for schema in self.schemas {
            self.validate_schema(schema)?;
            if !seen.insert(schema.trait_type) {
                return Err(TraitProofError::InvalidSchema("duplicate trait schema"));
            }
        }
        for schema in self.schemas {
            self.validate_order(schema)?;
        }
        for (index, table) in self.tables.iter().enumerate() {
            // Schema-less legacy metadata remains readable, never a proof.
            if self
                .schemas
                .iter()
                .any(|schema| self.canonical(table.trait_type).ok() == Some(schema.trait_type))
            {
                self.descriptors(index)?;
            }
        }
        Ok(())
    }
}

impl TypePool {
    fn dispatch_registry(&self) -> Registry<'_> {
        Registry {
            types: &self.types,
            scopes: &self.scopes,
            records: &self.trait_impls,
            tables: &self.vtables,
            schemas: &self.trait_schemas,
        }
    }

    /// Register canonical slot identities atomically. Parent schemas can be
    /// registered later; complete parent order is checked when publishing.
    pub fn register_trait_schema(
        &mut self,
        schema: TraitDispatchSchema,
    ) -> Result<(), SnapshotError> {
        self.invalidate_type_identities();
        let registry = self.dispatch_registry();
        registry
            .validate_schema(&schema)
            .map_err(|error| SnapshotError::new(error.to_string()))?;
        if self.trait_schema(schema.trait_type).is_some() {
            return Err(SnapshotError::new("duplicate trait schema"));
        }
        self.trait_schemas.push(schema);
        Ok(())
    }

    pub fn trait_schemas_snapshot(&self) -> &[TraitDispatchSchema] {
        &self.trait_schemas
    }

    pub fn trait_schema(&self, ty: TypeIndex) -> Option<&TraitDispatchSchema> {
        let ty = self.canonical_type(ty)?;
        self.trait_schemas
            .iter()
            .find(|schema| schema.trait_type == ty)
    }

    /// Resolve an immutable table index against its persisted declaration
    /// schema and exact implementation scope. No caller scope is consulted.
    /// Descriptors retain access restrictions; this query does not authorize
    /// calling a method or materialize a runtime trait witness.
    pub fn checked_vtable_descriptors(
        &self,
        index: usize,
    ) -> Result<Vec<TraitMethodDescriptor>, TraitProofError> {
        self.dispatch_registry().descriptors(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Intrinsic, TypeId};

    fn trait_type(pool: &mut TypePool, name: &str, parents: Vec<TypeIndex>) -> TypeIndex {
        pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern(name),
                parents,
                assoc_types: Vec::new(),
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        })
    }
    fn key(owner: TypeIndex, name: &str) -> TraitMethodKey {
        TraitMethodKey {
            trait_owner: owner,
            name: str_interner::intern(name),
            signature: None,
        }
    }
    fn implementation(
        pool: &mut TypePool,
        ty: TypeIndex,
        scope: Option<u32>,
        methods: &[(&str, u32)],
        entries: Vec<u32>,
    ) {
        let owner = Intrinsic::I64.type_index();
        let methods: Vec<_> = methods
            .iter()
            .map(|&(name, func_id)| MethodSlot {
                name: str_interner::intern(name),
                func_id,
                trait_impl: Some(ty),
                visible_scope: scope,
                access: MethodAccess::Public,
            })
            .collect();
        for method in &methods {
            pool.add_method(owner, method.clone());
        }
        pool.add_trait_impl(TraitImplRecord {
            implementor: owner,
            trait_type: ty,
            visible_scope: scope,
            methods,
        });
        pool.add_vtable(VTable {
            implementor: owner,
            trait_type: ty,
            visible_scope: scope,
            entries,
        });
    }
    fn fixture() -> (TypePool, TypeIndex, TypeIndex) {
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
        let parent = trait_type(&mut pool, "ProofParent", Vec::new());
        let child = trait_type(&mut pool, "ProofChild", vec![parent]);
        pool.register_trait_schema(TraitDispatchSchema {
            trait_type: parent,
            slots: vec![key(parent, "read")],
        })
        .unwrap();
        pool.register_trait_schema(TraitDispatchSchema {
            trait_type: child,
            slots: vec![key(parent, "read"), key(child, "extra")],
        })
        .unwrap();
        implementation(&mut pool, parent, Some(1), &[("read", 40)], vec![40]);
        implementation(&mut pool, parent, Some(2), &[("read", 2)], vec![2]);
        implementation(&mut pool, child, Some(3), &[("extra", 41)], vec![40, 41]);
        (pool, parent, child)
    }

    #[test]
    fn inherited_descriptors_freeze_declaration_scope_and_roundtrip() {
        let (pool, parent, child) = fixture();
        let restored = TypePool::restore(pool.snapshot()).unwrap();
        let descriptors = restored.checked_vtable_descriptors(2).unwrap();
        assert_eq!(
            descriptors
                .iter()
                .map(|slot| slot.func_id)
                .collect::<Vec<_>>(),
            vec![40, 41]
        );
        assert_eq!(descriptors[0].trait_owner, parent);
        assert_eq!(descriptors[0].implementation_trait, parent);
        assert_eq!(descriptors[0].implementation_scope, Some(1));
        assert_eq!(descriptors[1].implementation_trait, child);
        assert_eq!(descriptors[1].implementation_scope, Some(3));
        assert_eq!(
            restored.trait_schemas_snapshot(),
            pool.trait_schemas_snapshot()
        );
    }

    #[test]
    fn schema_owners_names_parent_order_and_table_entries_are_checked() {
        let (pool, parent, child) = fixture();
        let snapshot = pool.snapshot();
        let mut wrong = snapshot.clone();
        wrong.trait_schemas[1].slots.swap(0, 1);
        assert!(TypePool::restore(wrong).is_err());
        let mut wrong = snapshot.clone();
        wrong.trait_schemas[1].slots[0].trait_owner = pool.well_known.eq;
        assert!(TypePool::restore(wrong).is_err());
        let mut wrong = snapshot.clone();
        wrong.trait_schemas[1].slots[1] = key(child, "read");
        assert!(TypePool::restore(wrong).is_err());
        let mut wrong = snapshot.clone();
        wrong.vtables[2].entries.swap(0, 1);
        assert!(TypePool::restore(wrong).is_err());
        let mut wrong = snapshot.clone();
        wrong.vtables[2].entries.pop();
        assert!(TypePool::restore(wrong).is_err());
        let mut wrong = snapshot.clone();
        wrong.trait_impls[2].methods[0].trait_impl = Some(parent);
        assert!(TypePool::restore(wrong).is_err());
        let mut wrong = snapshot;
        let duplicate = wrong.trait_impls[0].methods[0].clone();
        wrong.trait_impls[0].methods.push(duplicate);
        assert!(TypePool::restore(wrong).is_err());
    }

    #[test]
    fn own_override_preserves_slot_owner_but_validates_selected_implementation() {
        let (mut pool, parent, child) = fixture();
        let override_method = MethodSlot {
            name: key(parent, "read").name,
            func_id: 99,
            trait_impl: Some(child),
            visible_scope: Some(3),
            access: MethodAccess::Public,
        };
        pool.trait_impls[2].methods.push(override_method);
        pool.vtables[2].entries[0] = 99;
        let descriptors = pool.checked_vtable_descriptors(2).unwrap();
        assert_eq!(descriptors[0].trait_owner, parent);
        assert_eq!(descriptors[0].implementation_trait, child);
        assert_eq!(descriptors[0].func_id, 99);
        pool.vtables[2].entries[0] = 40;
        assert!(matches!(
            pool.checked_vtable_descriptors(2),
            Err(TraitProofError::WrongEntry { .. })
        ));
    }

    #[test]
    fn missing_schema_legacy_and_parent_ambiguity_never_supply_proofs() {
        let (mut pool, parent, _) = fixture();
        let mut legacy = pool.snapshot();
        legacy.trait_schemas.clear();
        let legacy = TypePool::restore(legacy).unwrap();
        assert!(matches!(
            legacy.checked_vtable_descriptors(2),
            Err(TraitProofError::MissingSchema(_))
        ));
        implementation(&mut pool, parent, None, &[("read", 7)], vec![7]);
        assert_eq!(
            pool.checked_vtable_descriptors(2),
            Err(TraitProofError::AmbiguousImplementation(parent))
        );
        assert!(TypePool::restore(pool.snapshot()).is_err());
    }

    #[test]
    fn schema_registration_is_atomic_and_queries_canonicalize_aliases() {
        let (mut pool, parent, _) = fixture();
        assert!(
            pool.register_trait_schema(TraitDispatchSchema {
                trait_type: Intrinsic::I64.type_index(),
                slots: Vec::new(),
            })
            .is_err()
        );
        let before = pool.trait_schemas_snapshot().to_vec();
        assert!(
            pool.register_trait_schema(TraitDispatchSchema {
                trait_type: parent,
                slots: vec![key(parent, "read")]
            })
            .is_err()
        );
        assert_eq!(pool.trait_schemas_snapshot(), before);
        let alias = pool.register(TypeInfo {
            kind: TypeKind::Typealias {
                name: str_interner::intern("ProofAlias"),
                target: parent,
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        assert_eq!(pool.trait_schema(alias), pool.trait_schema(parent));
        assert!(pool.trait_schema(TypeIndex::INVALID).is_none());
        assert_eq!(
            pool.checked_vtable_descriptors(99),
            Err(TraitProofError::InvalidTable(99))
        );
    }

    #[test]
    fn marker_tables_require_schemas_and_live_cycles_are_checked() {
        let mut pool = TypePool::with_intrinsics();
        let marker = trait_type(&mut pool, "ProofMarker", Vec::new());
        pool.register_trait_schema(TraitDispatchSchema {
            trait_type: marker,
            slots: Vec::new(),
        })
        .unwrap();
        implementation(&mut pool, marker, None, &[], Vec::new());
        assert_eq!(pool.checked_vtable_descriptors(0).unwrap(), Vec::new());
        if let TypeKind::Trait { parents, .. } = &mut pool.get_mut(marker).kind {
            parents.push(marker);
        }
        assert!(matches!(
            pool.checked_vtable_descriptors(0),
            Err(TraitProofError::InvalidSchema("cyclic trait ancestry"))
        ));
    }

    #[test]
    fn generated_parent_publication_updates_inherited_slots_without_overwriting_scoped_implementations()
     {
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
        ])
        .unwrap();
        let parent = pool.well_known.eq;
        let child = trait_type(&mut pool, "DerivedProofChild", vec![parent]);
        pool.register_trait_schema(TraitDispatchSchema {
            trait_type: parent,
            slots: vec![key(parent, "eq")],
        })
        .unwrap();
        pool.register_trait_schema(TraitDispatchSchema {
            trait_type: child,
            slots: vec![key(parent, "eq"), key(child, "extra")],
        })
        .unwrap();
        implementation(
            &mut pool,
            parent,
            None,
            &[("eq", crate::DERIVE_FUNC_ID)],
            vec![crate::DERIVE_FUNC_ID],
        );
        implementation(
            &mut pool,
            child,
            None,
            &[("extra", 41)],
            vec![crate::DERIVE_FUNC_ID, 41],
        );
        implementation(
            &mut pool,
            child,
            Some(1),
            &[("eq", 2), ("extra", 3)],
            vec![2, 3],
        );
        pool.resolve_derived_methods([(
            Intrinsic::I64.type_index(),
            parent,
            key(parent, "eq").name,
            40,
        )])
        .unwrap();
        assert_eq!(pool.vtables[0].entries, vec![40]);
        assert_eq!(pool.vtables[1].entries, vec![40, 41]);
        assert_eq!(pool.vtables[2].entries, vec![2, 3]);
        assert_eq!(pool.checked_vtable_descriptors(1).unwrap()[0].func_id, 40);
        TypePool::restore(pool.snapshot()).unwrap();
    }
}
