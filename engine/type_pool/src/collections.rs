//! Versioned collection identities without changing the intrinsic index prefix.

use crate::{FieldInfo, Intrinsic, SnapshotError, TypeId, TypeIndex, TypeInfo, TypeKind, TypePool};

const COLLECTION_NAMESPACE: u64 = 0x4e45_5353_434f_4c4c;
/// Engine List wrapper identity, layout revision one.
pub const LIST_TYPE_ID: TypeId = TypeId(COLLECTION_NAMESPACE, 0x0000_0001_0000_0001);
/// Engine tagged-element buffer identity, layout revision one.
pub const LIST_BUFFER_TYPE_ID: TypeId = TypeId(COLLECTION_NAMESPACE, 0x0000_0001_0000_0002);

/// Engine String-keyed Map wrapper identity, layout revision one.
pub const MAP_TYPE_ID: TypeId = TypeId(COLLECTION_NAMESPACE, 0x0000_0001_0000_0003);
/// Engine opaque four-tagged-words-per-bucket buffer identity, revision one.
pub const MAP_BUFFER_TYPE_ID: TypeId = TypeId(COLLECTION_NAMESPACE, 0x0000_0001_0000_0004);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionRole {
    List,
    Buffer,
    Map,
    MapBuffer,
}

impl CollectionRole {
    pub const fn type_id(self) -> TypeId {
        match self {
            Self::List => LIST_TYPE_ID,
            Self::Buffer => LIST_BUFFER_TYPE_ID,
            Self::Map => MAP_TYPE_ID,
            Self::MapBuffer => MAP_BUFFER_TYPE_ID,
        }
    }

    fn from_id(id: TypeId) -> Option<Self> {
        match id {
            LIST_TYPE_ID => Some(Self::List),
            LIST_BUFFER_TYPE_ID => Some(Self::Buffer),
            MAP_TYPE_ID => Some(Self::Map),
            MAP_BUFFER_TYPE_ID => Some(Self::MapBuffer),
            _ => None,
        }
    }
}

impl TypePool {
    pub(crate) fn register_collection_roles(&mut self) {
        for (wrapper, buffer, name, buffer_name) in [
            (LIST_TYPE_ID, LIST_BUFFER_TYPE_ID, "List", "__ListBuffer"),
            (MAP_TYPE_ID, MAP_BUFFER_TYPE_ID, "Map", "__MapBuffer"),
        ] {
            let field = |name: &str, ty, offset| FieldInfo {
                name: str_interner::intern(name),
                ty,
                has_default: false,
                offset,
            };
            self.register(TypeInfo {
                kind: TypeKind::Struct {
                    name: str_interner::intern(name),
                    fields: vec![
                        field("len", Intrinsic::U64.type_index(), 0),
                        field("capacity", Intrinsic::U64.type_index(), 8),
                        field("buffer", Intrinsic::Any.type_index(), 16),
                    ],
                },
                type_id: wrapper,
                size: 24,
                align: 8,
            });
            self.register(TypeInfo {
                kind: TypeKind::Struct {
                    name: str_interner::intern(buffer_name),
                    fields: Vec::new(),
                },
                type_id: buffer,
                size: 0,
                align: 8,
            });
        }
    }

    /// Identify a reserved collection layout, following transparent aliases.
    pub fn collection_role(&self, ty: TypeIndex) -> Option<CollectionRole> {
        let ty = self.canonical_type(ty)?;
        CollectionRole::from_id(self.get(ty).type_id)
    }

    /// Check a single object descriptor before interpreting its payload. This
    /// bounded layout check does not replace whole-registry artifact validation.
    pub fn checked_collection_role(
        &self,
        ty: TypeIndex,
    ) -> Result<Option<CollectionRole>, SnapshotError> {
        let ty = self
            .canonical_type(ty)
            .ok_or_else(|| SnapshotError::new("invalid collection type index"))?;
        let info = self.get(ty);
        let Some(role) = CollectionRole::from_id(info.type_id) else {
            return if info.type_id.hi() == COLLECTION_NAMESPACE {
                Err(SnapshotError::new("unknown reserved collection identity"))
            } else {
                Ok(None)
            };
        };
        validate_layout(info, role)?;
        Ok(Some(role))
    }

    pub fn is_reserved_collection_role(&self, ty: TypeIndex) -> bool {
        self.collection_role(ty).is_some()
    }

    pub fn list_type(&self) -> Option<TypeIndex> {
        self.collection_type(CollectionRole::List)
    }

    pub fn list_buffer_type(&self) -> Option<TypeIndex> {
        self.collection_type(CollectionRole::Buffer)
    }

    pub fn map_type(&self) -> Option<TypeIndex> {
        self.collection_type(CollectionRole::Map)
    }

    pub fn map_buffer_type(&self) -> Option<TypeIndex> {
        self.collection_type(CollectionRole::MapBuffer)
    }

    fn collection_type(&self, role: CollectionRole) -> Option<TypeIndex> {
        let index = self.lookup_by_id(role.type_id())?;
        let info = self.types.get(index.as_u32() as usize)?;
        (info.type_id == role.type_id() && validate_layout(info, role).is_ok()).then_some(index)
    }

    /// Legacy registries may omit either collection pair. Present roles must be unique,
    /// paired, and have the exact tagged-slot layout expected by the VM.
    pub fn validate_collection_layouts(&self) -> Result<(), SnapshotError> {
        let mut seen_roles = [false; 4];
        for info in &self.types {
            if info.type_id.hi() != COLLECTION_NAMESPACE {
                continue;
            }
            let role = CollectionRole::from_id(info.type_id)
                .ok_or_else(|| SnapshotError::new("unknown reserved collection identity"))?;
            if let TypeKind::Typealias { target, .. } = info.kind {
                let target = self.canonical_type(target).ok_or_else(|| {
                    SnapshotError::new("invalid reserved collection alias target")
                })?;
                let target_info = self.get(target);
                if target_info.type_id != info.type_id {
                    return Err(SnapshotError::new(
                        "reserved collection alias identity mismatch",
                    ));
                }
                validate_layout(target_info, role)?;
                // Transparent aliases share their canonical descriptor's identity;
                // only the descriptor itself owns the native role.
                continue;
            }
            let seen = match role {
                CollectionRole::List => &mut seen_roles[0],
                CollectionRole::Buffer => &mut seen_roles[1],
                CollectionRole::Map => &mut seen_roles[2],
                CollectionRole::MapBuffer => &mut seen_roles[3],
            };
            if *seen {
                return Err(SnapshotError::new("duplicate reserved collection identity"));
            }
            *seen = true;
            validate_layout(info, role)?;
        }
        if seen_roles[0] != seen_roles[1] || seen_roles[2] != seen_roles[3] {
            return Err(SnapshotError::new(
                "collection roles must be present together",
            ));
        }
        Ok(())
    }
}

fn validate_layout(info: &TypeInfo, role: CollectionRole) -> Result<(), SnapshotError> {
    let TypeKind::Struct { fields, .. } = &info.kind else {
        return Err(SnapshotError::new(
            "reserved collection identity requires a nominal struct",
        ));
    };
    let valid = match role {
        CollectionRole::List | CollectionRole::Map => {
            info.size == 24
                && info.align == 8
                && fields.len() == 3
                && fields
                    .iter()
                    .zip([Intrinsic::U64, Intrinsic::U64, Intrinsic::Any])
                    .enumerate()
                    .all(|(index, (field, kind))| {
                        field.ty == kind.type_index()
                            && field.offset == index as u32 * 8
                            && !field.has_default
                    })
        }
        CollectionRole::Buffer | CollectionRole::MapBuffer => {
            fields.is_empty() && info.size == 0 && info.align == 8
        }
    };
    if !valid {
        return Err(SnapshotError::new("invalid reserved collection layout"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_preserve_prefix_and_survive_exact_index_restore_and_aliases() {
        let mut pool = TypePool::with_intrinsics();
        assert_eq!(pool.null_type().as_u32(), Intrinsic::COUNT as u32 + 8);
        let list = pool.list_type().unwrap();
        let buffer = pool.list_buffer_type().unwrap();
        assert_eq!(pool.collection_role(list), Some(CollectionRole::List));
        assert_eq!(pool.collection_role(buffer), Some(CollectionRole::Buffer));
        let alias = pool.register(TypeInfo {
            kind: TypeKind::Typealias {
                name: str_interner::intern("Alias"),
                target: list,
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let restored = TypePool::restore(pool.snapshot()).unwrap();
        assert_eq!(restored.list_type(), Some(list));
        assert_eq!(restored.list_buffer_type(), Some(buffer));
        assert_eq!(restored.collection_role(alias), Some(CollectionRole::List));
    }

    #[test]
    fn bounded_runtime_lookup_rejects_mutated_layouts() {
        let mut pool = TypePool::with_intrinsics();
        let list = pool.list_type().unwrap();
        pool.get_mut(list).size = 8;
        assert!(pool.checked_collection_role(list).is_err());
        assert_eq!(pool.list_type(), None);
        assert_eq!(
            pool.checked_collection_role(Intrinsic::I64.type_index())
                .unwrap(),
            None
        );
        assert!(pool.checked_collection_role(TypeIndex::INVALID).is_err());
    }

    #[test]
    fn legacy_pool_without_roles_is_valid() {
        let pool = TypePool::with_intrinsics();
        let mut snapshot = pool.snapshot();
        snapshot.types.truncate(snapshot.types.len() - 4);
        snapshot.methods.truncate(snapshot.methods.len() - 4);
        let restored = TypePool::restore(snapshot).unwrap();
        assert_eq!(restored.list_type(), None);
        assert_eq!(restored.list_buffer_type(), None);
    }

    #[test]
    fn malformed_or_duplicate_roles_are_rejected_on_restore_and_live_validation() {
        for mutation in 0..5 {
            let mut pool = TypePool::with_intrinsics();
            let list = pool.list_type().unwrap();
            match mutation {
                0 => pool.get_mut(list).size = 32,
                1 => {
                    if let TypeKind::Struct { fields, .. } = &mut pool.get_mut(list).kind {
                        fields[0].ty = Intrinsic::Usize.type_index();
                    }
                }
                2 => {
                    pool.get_mut(list).kind = TypeKind::Tuple {
                        elements: Vec::new(),
                    }
                }
                3 => {
                    let duplicate = pool.get(list).clone();
                    pool.register(duplicate);
                }
                _ => pool.get_mut(list).type_id = TypeId(COLLECTION_NAMESPACE, 99),
            }
            assert!(pool.validate().is_err());
            assert!(TypePool::restore(pool.snapshot()).is_err());
        }
        let mut snapshot = TypePool::with_intrinsics().snapshot();
        snapshot.types.pop();
        snapshot.methods.pop();
        assert!(TypePool::restore(snapshot).is_err());
    }
    #[test]
    fn independent_collection_pairs_restore_without_changing_the_prefix() {
        let pool = TypePool::with_intrinsics();
        let list = pool.list_type().unwrap();
        let map = pool.map_type().unwrap();
        assert_eq!(map.as_u32(), list.as_u32() + 2);
        assert_eq!(pool.collection_role(map), Some(CollectionRole::Map));
        assert_eq!(
            pool.collection_role(pool.map_buffer_type().unwrap()),
            Some(CollectionRole::MapBuffer)
        );
        for (keep_list, keep_map) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut snapshot = pool.snapshot();
            let retain = |info: &TypeInfo| match CollectionRole::from_id(info.type_id) {
                Some(CollectionRole::List | CollectionRole::Buffer) => keep_list,
                Some(CollectionRole::Map | CollectionRole::MapBuffer) => keep_map,
                None => true,
            };
            snapshot.methods = snapshot
                .methods
                .into_iter()
                .zip(&snapshot.types)
                .filter_map(|(methods, info)| retain(info).then_some(methods))
                .collect();
            snapshot.types.retain(retain);
            let restored = TypePool::restore(snapshot).unwrap();
            assert_eq!(restored.list_type().is_some(), keep_list);
            assert_eq!(restored.map_type().is_some(), keep_map);
            assert_eq!(restored.null_type(), pool.null_type());
        }
        let restored = TypePool::restore(pool.snapshot()).unwrap();
        assert_eq!(restored.map_type(), Some(map));
        assert_eq!(restored.map_buffer_type(), pool.map_buffer_type());
    }

    #[test]
    fn malformed_map_roles_are_rejected_live_and_on_restore() {
        for mutation in 0..4 {
            let mut pool = TypePool::with_intrinsics();
            let map = pool.map_type().unwrap();
            let buffer = pool.map_buffer_type().unwrap();
            match mutation {
                0 => pool.get_mut(map).size = 32,
                1 => {
                    if let TypeKind::Struct { fields, .. } = &mut pool.get_mut(map).kind {
                        fields[2].has_default = true;
                    }
                }
                2 => pool.get_mut(buffer).size = 8,
                _ => {
                    pool.register(pool.get(map).clone());
                }
            }
            assert!(pool.validate().is_err());
            assert!(TypePool::restore(pool.snapshot()).is_err());
        }
        let mut pool = TypePool::with_intrinsics();
        let map = pool.map_type().unwrap();
        pool.get_mut(map).align = 4;
        assert!(pool.checked_collection_role(map).is_err());
        assert_eq!(pool.map_type(), None);
    }
}
