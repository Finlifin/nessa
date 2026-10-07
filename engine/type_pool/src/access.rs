//! Archive-local method visibility and lexical authorization metadata.

use std::collections::HashSet;
use std::fmt;

use crate::{MethodSlot, SnapshotError, TypeIndex, TypeKind, TypePool};

/// Declaration visibility. Legacy metadata contains insufficient information
/// to authorize a call; it must never be interpreted as public visibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodAccess {
    LegacyUnknown,
    Public,
    Package(u32),
    Private(u32),
}

/// A lexical scope, addressed by its index in the pool's scope table.
/// Package identities are local to the artifact, not process-global IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScopeContext {
    pub parent: Option<u32>,
    pub package: u32,
    /// Canonical associated type, for private access across its impl scopes.
    pub assoc_type: Option<TypeIndex>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodAccessError {
    UnknownAccess,
    InvalidCaller(u32),
    InvalidMetadata(String),
}

impl fmt::Display for MethodAccessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownAccess => f.write_str("legacy method visibility cannot authorize a call"),
            Self::InvalidCaller(scope) => write!(f, "invalid caller scope {scope}"),
            Self::InvalidMetadata(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for MethodAccessError {}

pub(crate) fn validate_scopes(scopes: &[ScopeContext]) -> Result<(), SnapshotError> {
    if scopes.len() >= u32::MAX as usize {
        return Err(SnapshotError::new("too many lexical scopes"));
    }
    let mut roots = HashSet::new();
    for scope in scopes {
        let parent = scope
            .parent
            .map(|id| {
                scopes
                    .get(id as usize)
                    .ok_or_else(|| SnapshotError::new("scope parent is out of range"))
            })
            .transpose()?;
        if parent.is_none_or(|parent| parent.package != scope.package)
            && !roots.insert(scope.package)
        {
            return Err(SnapshotError::new("package has multiple lexical roots"));
        }
    }
    // Each node is visited at most twice, including adversarial deep graphs.
    let mut state = vec![0_u8; scopes.len()];
    for start in 0..scopes.len() {
        let mut current = Some(start);
        while let Some(index) = current {
            match state[index] {
                1 => return Err(SnapshotError::new("cyclic lexical scope graph")),
                2 => break,
                _ => state[index] = 1,
            }
            current = scopes[index].parent.map(|id| id as usize);
        }
        let mut current = Some(start);
        while let Some(index) = current {
            if state[index] != 1 {
                break;
            }
            state[index] = 2;
            current = scopes[index].parent.map(|id| id as usize);
        }
    }
    Ok(())
}

pub(crate) fn validate_method(
    scopes: &[ScopeContext],
    method: &MethodSlot,
    packages: &HashSet<u32>,
) -> Result<(), SnapshotError> {
    if scopes.is_empty() {
        return match method.access {
            MethodAccess::LegacyUnknown => Ok(()), // Preserve untrusted TPOL1 extend IDs.
            MethodAccess::Public if method.visible_scope.is_none() => Ok(()),
            _ => Err(SnapshotError::new(
                "method visibility requires a lexical scope graph",
            )),
        };
    }
    if method
        .visible_scope
        .is_some_and(|id| id as usize >= scopes.len())
    {
        return Err(SnapshotError::new("method extend scope is out of range"));
    }
    match method.access {
        MethodAccess::Private(id) if id as usize >= scopes.len() => {
            Err(SnapshotError::new("method private scope is out of range"))
        }
        MethodAccess::Package(id) if !packages.contains(&id) => Err(SnapshotError::new(
            "method package is absent from scope graph",
        )),
        _ => Ok(()),
    }
}

impl TypePool {
    /// Install a checked graph atomically. Existing method visibility and
    /// canonical associated type references must remain valid.
    pub fn install_scopes(&mut self, scopes: Vec<ScopeContext>) -> Result<(), SnapshotError> {
        validate_scopes(&scopes)?;
        for scope in &scopes {
            if let Some(ty) = scope.assoc_type {
                let info = self
                    .types
                    .get(ty.as_u32() as usize)
                    .ok_or_else(|| SnapshotError::new("scope associated type is out of range"))?;
                if matches!(info.kind, TypeKind::Typealias { .. }) {
                    return Err(SnapshotError::new(
                        "scope associated type must be canonical",
                    ));
                }
            }
        }
        let packages = scopes.iter().map(|scope| scope.package).collect();
        for method in self
            .methods
            .iter()
            .flatten()
            .chain(self.trait_impls.iter().flat_map(|record| &record.methods))
        {
            validate_method(&scopes, method, &packages)?;
        }
        for record in &self.trait_impls {
            crate::traits::validate_record_scope(&scopes, record)?;
        }
        for table in &self.vtables {
            if !scopes.is_empty()
                && table
                    .visible_scope
                    .is_some_and(|id| id as usize >= scopes.len())
            {
                return Err(SnapshotError::new("vtable scope is out of range"));
            }
        }
        self.scope_packages = packages;
        self.scopes = scopes;
        Ok(())
    }

    pub fn scopes(&self) -> &[ScopeContext] {
        &self.scopes
    }

    pub fn scope_context(&self, id: u32) -> Option<&ScopeContext> {
        self.scopes.get(id as usize)
    }

    /// Evaluate declaration visibility and the independent extend restriction.
    /// This is a metadata query; callers still need to enforce it at dispatch.
    /// The installed/restored graph is validated and cannot be mutated directly.
    pub fn method_accessible(
        &self,
        method: &MethodSlot,
        caller_scope: u32,
    ) -> Result<bool, MethodAccessError> {
        if method.access == MethodAccess::LegacyUnknown {
            return Err(MethodAccessError::UnknownAccess);
        }
        let caller = self
            .scope_context(caller_scope)
            .ok_or(MethodAccessError::InvalidCaller(caller_scope))?;
        validate_method(&self.scopes, method, &self.scope_packages)
            .map_err(|error| MethodAccessError::InvalidMetadata(error.to_string()))?;
        let allowed = match method.access {
            MethodAccess::LegacyUnknown => return Err(MethodAccessError::UnknownAccess),
            MethodAccess::Public => true,
            MethodAccess::Package(package) => caller.package == package,
            MethodAccess::Private(owner) => {
                let owner_context = &self.scopes[owner as usize];
                self.within_scope(owner, caller_scope)
                    || owner_context.assoc_type.is_some_and(|ty| {
                        let mut scope = Some(caller_scope);
                        while let Some(id) = scope {
                            let context = &self.scopes[id as usize];
                            if context.assoc_type == Some(ty) {
                                return context.package == owner_context.package;
                            }
                            scope = context.parent;
                        }
                        false
                    })
            }
        };
        Ok(allowed
            && method
                .visible_scope
                .is_none_or(|owner| self.within_scope(owner, caller_scope)))
    }

    fn within_scope(&self, owner: u32, mut scope: u32) -> bool {
        // Combined AST containers do not make one package a lexical child of
        // another. Extensions and private access stay within their package.
        if self.scopes[owner as usize].package != self.scopes[scope as usize].package {
            return false;
        }
        loop {
            if owner == scope {
                return true;
            }
            let Some(parent) = self.scopes[scope as usize].parent else {
                return false;
            };
            scope = parent;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Intrinsic;

    fn context(parent: Option<u32>, package: u32, assoc_type: Option<TypeIndex>) -> ScopeContext {
        ScopeContext {
            parent,
            package,
            assoc_type,
        }
    }

    fn method(access: MethodAccess, visible_scope: Option<u32>) -> MethodSlot {
        MethodSlot {
            access,
            name: str_interner::intern("access_test"),
            func_id: 0,
            trait_impl: None,
            visible_scope,
        }
    }

    fn graph() -> Vec<ScopeContext> {
        let ty = Some(Intrinsic::I64.type_index());
        vec![
            context(None, 10, None),
            context(Some(0), 10, ty),
            context(Some(1), 10, None),
            context(Some(0), 10, ty),
            context(Some(3), 10, None),
            context(Some(0), 20, None),
            context(Some(5), 20, ty),
        ]
    }

    #[test]
    fn visibility_and_extend_restrictions_are_independent() {
        let mut pool = TypePool::with_intrinsics();
        pool.install_scopes(graph()).unwrap();
        assert_eq!(
            pool.method_accessible(&method(MethodAccess::Public, None), 6),
            Ok(true)
        );
        assert_eq!(
            pool.method_accessible(&method(MethodAccess::Package(10), None), 4),
            Ok(true)
        );
        assert_eq!(
            pool.method_accessible(&method(MethodAccess::Package(10), None), 6),
            Ok(false)
        );
        let private = method(MethodAccess::Private(1), None);
        assert_eq!(pool.method_accessible(&private, 2), Ok(true));
        assert_eq!(pool.method_accessible(&private, 4), Ok(true));
        assert_eq!(pool.method_accessible(&private, 0), Ok(false));
        assert_eq!(pool.method_accessible(&private, 6), Ok(false));
        let extended = method(MethodAccess::Private(1), Some(3));
        assert_eq!(pool.method_accessible(&extended, 2), Ok(false));
        assert_eq!(pool.method_accessible(&extended, 4), Ok(true));
        let public_extended = method(MethodAccess::Public, Some(1));
        assert_eq!(pool.method_accessible(&public_extended, 4), Ok(false));
        assert_eq!(pool.method_accessible(&public_extended, 2), Ok(true));
    }

    #[test]
    fn synthetic_parent_cannot_authorize_foreign_private_or_extended_methods() {
        let mut pool = TypePool::with_intrinsics();
        pool.install_scopes(graph()).unwrap();
        let restored = TypePool::restore(pool.snapshot()).unwrap();
        for pool in [&pool, &restored] {
            for slot in [
                method(MethodAccess::Private(0), None),
                method(MethodAccess::Public, Some(0)),
            ] {
                assert_eq!(pool.method_accessible(&slot, 2), Ok(true));
                assert_eq!(pool.method_accessible(&slot, 6), Ok(false));
            }
            assert_eq!(
                pool.method_accessible(&method(MethodAccess::Public, None), 6),
                Ok(true)
            );
        }
    }

    #[test]
    fn invalid_and_legacy_metadata_never_become_permission_denials() {
        let mut pool = TypePool::with_intrinsics();
        let legacy = method(MethodAccess::LegacyUnknown, Some(123));
        pool.add_method(Intrinsic::I64.type_index(), legacy.clone());
        let restored = TypePool::restore(pool.snapshot()).unwrap();
        assert_eq!(
            restored.methods_of(Intrinsic::I64.type_index())[0].visible_scope,
            Some(123)
        );
        assert_eq!(
            restored.method_accessible(&legacy, 0),
            Err(MethodAccessError::UnknownAccess)
        );
        assert!(pool.install_scopes(graph()).is_err());
        assert!(pool.scopes().is_empty());
        let mut pool = TypePool::with_intrinsics();
        pool.install_scopes(graph()).unwrap();
        assert_eq!(
            pool.method_accessible(&method(MethodAccess::Public, None), 99),
            Err(MethodAccessError::InvalidCaller(99))
        );
        for invalid in [
            method(MethodAccess::Private(99), None),
            method(MethodAccess::Package(99), None),
            method(MethodAccess::Public, Some(99)),
        ] {
            assert!(matches!(
                pool.method_accessible(&invalid, 0),
                Err(MethodAccessError::InvalidMetadata(_))
            ));
        }
        for invalid in [
            method(MethodAccess::Private(0), None),
            method(MethodAccess::Package(10), None),
            method(MethodAccess::Public, Some(0)),
        ] {
            let mut snapshot = TypePool::with_intrinsics().snapshot();
            snapshot.methods[0].push(invalid);
            assert!(TypePool::restore(snapshot).is_err());
        }
    }

    #[test]
    fn snapshot_retains_scopes_access_and_rejects_damaged_references() {
        let mut pool = TypePool::with_intrinsics();
        pool.install_scopes(graph()).unwrap();
        for access in [
            MethodAccess::Public,
            MethodAccess::Package(10),
            MethodAccess::Private(1),
            MethodAccess::LegacyUnknown,
        ] {
            pool.add_method(Intrinsic::I64.type_index(), method(access, Some(1)));
        }
        let restored = TypePool::restore(pool.snapshot()).unwrap();
        assert_eq!(restored.scopes(), pool.scopes());
        let expected: Vec<_> = pool
            .methods_of(Intrinsic::I64.type_index())
            .iter()
            .map(|slot| slot.access)
            .collect();
        let actual: Vec<_> = restored
            .methods_of(Intrinsic::I64.type_index())
            .iter()
            .map(|slot| slot.access)
            .collect();
        assert_eq!(actual, expected);
        let mut snapshot = pool.snapshot();
        snapshot.scopes[1].assoc_type = Some(TypeIndex::INVALID);
        assert!(TypePool::restore(snapshot).is_err());
        let mut snapshot = pool.snapshot();
        snapshot.methods[Intrinsic::I64.type_index().as_u32() as usize][0].visible_scope = Some(99);
        assert!(TypePool::restore(snapshot).is_err());
    }

    #[test]
    fn malformed_graphs_and_noncanonical_associated_types_are_rejected_atomically() {
        let mut pool = TypePool::with_intrinsics();
        pool.install_scopes(graph()).unwrap();
        let original = pool.scopes().to_vec();
        for damaged in [
            vec![context(Some(99), 10, None)],
            vec![context(Some(0), 10, None)],
            vec![context(Some(1), 10, None), context(Some(0), 10, None)],
            vec![context(None, 10, None), context(None, 10, None)],
            vec![
                context(None, 10, None),
                context(Some(0), 20, None),
                context(Some(1), 10, None),
            ],
            vec![context(None, 10, Some(TypeIndex::INVALID))],
        ] {
            assert!(pool.install_scopes(damaged.clone()).is_err());
            assert_eq!(pool.scopes(), original);
            let mut snapshot = pool.snapshot();
            snapshot.scopes = damaged;
            assert!(TypePool::restore(snapshot).is_err());
        }
        let alias = pool.register(crate::TypeInfo {
            kind: TypeKind::Typealias {
                name: str_interner::intern("AccessAlias"),
                target: Intrinsic::I64.type_index(),
            },
            type_id: crate::TypeId::ZERO,
            size: 0,
            align: 0,
        });
        assert!(
            pool.install_scopes(vec![context(None, 10, Some(alias))])
                .is_err()
        );
    }
}
