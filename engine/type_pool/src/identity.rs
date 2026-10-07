//! Persisted semantic identity inputs; local indices are references, never hash bytes.
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::sync::atomic::Ordering;

use crate::{TypeId, TypeIndex, TypeKind, TypePool};

pub(crate) const IDENTITY_SCHEMA: u32 = 1;
pub(crate) const MAX_IDENTITY_ITEMS: usize = 262_144;
pub(crate) const MAX_IDENTITY_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const BOOTSTRAP_NAMESPACE: u64 = 0x4e45_5353_5452_4954;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageTypeContext {
    pub identity_schema: u32,
    pub identity: [u8; 16],
    pub qualified_name: String,
    pub version: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdentityPathSegment {
    Named(String),
    Lexical { kind: u8, ordinal: u32 },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NominalTypeProvenance {
    pub type_index: TypeIndex,
    pub package: u32,
    pub path: Vec<IdentityPathSegment>,
    pub last_stable_version: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeIdentityInput {
    pub schema: u32,
    pub packages: Vec<PackageTypeContext>,
    pub declarations: Vec<NominalTypeProvenance>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeIdentityError(pub(crate) String);
impl fmt::Display for TypeIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for TypeIdentityError {}
pub(crate) fn fail(message: impl Into<String>) -> TypeIdentityError {
    TypeIdentityError(message.into())
}
pub(crate) const fn bootstrap_id(ordinal: u64) -> TypeId {
    TypeId(BOOTSTRAP_NAMESPACE, (1 << 32) | ordinal)
}
pub(crate) fn known_traits(pool: &TypePool) -> [TypeIndex; 8] {
    let k = pool.well_known;
    [
        k.display,
        k.hash,
        k.eq,
        k.ord,
        k.partial_eq,
        k.partial_ord,
        k.iterator,
        k.into_iterator,
    ]
}

impl TypePool {
    /// True only for a checked intrinsic, native collection, or deterministic bootstrap role.
    pub fn is_reserved_type(&self, ty: TypeIndex) -> bool {
        let Some(ty) = self.canonical_type(ty) else {
            return false;
        };
        let Some(info) = self.types.get(ty.as_u32() as usize) else {
            return false;
        };
        match info.kind {
            TypeKind::Intrinsic(intrinsic) => {
                info.type_id == crate::intrinsic_type_id(intrinsic)
                    && ty == intrinsic.type_index()
                    && (info.size, info.align) == crate::intrinsic_layout(intrinsic)
            }
            _ if self
                .checked_collection_role(ty)
                .is_ok_and(|role| role.is_some()) =>
            {
                true
            }
            TypeKind::Trait { .. } => known_traits(self)
                .iter()
                .position(|&known| known == ty)
                .is_some_and(|ordinal| {
                    info.type_id == bootstrap_id(ordinal as u64 + 1)
                        && ty.as_u32() as usize == crate::Intrinsic::COUNT + ordinal
                        && info.size == 0
                        && info.align == 0
                        && info
                            .kind
                            .trait_name()
                            .and_then(str_interner::try_get)
                            .as_deref()
                            == Some(crate::WellKnownTraits::NAMES[ordinal])
                }),
            _ => false,
        }
    }
    pub fn identity_input(&self) -> Option<&TypeIdentityInput> {
        self.identity_input.as_ref()
    }
    pub(crate) fn invalidate_type_identities(&self) {
        if self.identity_input.is_some() {
            self.identity_dirty.store(true, Ordering::Relaxed);
        }
    }
    pub(crate) fn identities_need_validation(&self) -> bool {
        self.identity_input.is_some()
            && (self.identity_dirty.load(Ordering::Acquire)
                || self.identity_known != known_traits(self))
    }

    /// All assignment and lookup state is prepared and checked before publication.
    pub fn finalize_type_identities(
        &mut self,
        input: TypeIdentityInput,
    ) -> Result<(), TypeIdentityError> {
        self.validate_collection_layouts()
            .map_err(|error| fail(error.to_string()))?;
        let ids = crate::identity_encoding::compute(self, &input)?;
        self.identity_lookup(&ids)?;
        let mut snapshot = self.snapshot();
        for (info, id) in snapshot.types.iter_mut().zip(ids) {
            info.type_id = id;
        }
        snapshot.identity_input = Some(input);
        // Restore validates the complete candidate graph, IDs and reverse map.
        // Nothing in the original registry changes if any check fails.
        let candidate = Self::restore(snapshot).map_err(|error| fail(error.to_string()))?;
        *self = candidate;
        Ok(())
    }
    /// Stable user tags require finalized provenance. Checked immutable lookups
    /// are constant-time; a descriptor/interface mutation requires revalidation.
    pub fn stable_type_id(&self, ty: TypeIndex) -> Result<TypeId, TypeIdentityError> {
        let ty = self
            .canonical_type(ty)
            .ok_or_else(|| fail("invalid stable type index"))?;
        if self.identity_input.is_none() && !self.is_reserved_type(ty) {
            return Err(fail("type identity is legacy or unfinalized"));
        }
        if self.identities_need_validation() {
            self.validate_type_identities()?;
        }
        let id = self.types[ty.as_u32() as usize].type_id;
        if id == TypeId::ZERO {
            return Err(fail("abstract type has no executable stable identity"));
        }
        Ok(id)
    }
    pub fn validate_type_identities(&self) -> Result<(), TypeIdentityError> {
        let Some(input) = &self.identity_input else {
            return Ok(());
        };
        self.snapshot()
            .validate()
            .map_err(|error| fail(error.to_string()))?;
        let ids = crate::identity_encoding::compute(self, input)?;
        if self
            .types
            .iter()
            .zip(&ids)
            .any(|(info, id)| info.type_id != *id)
        {
            return Err(fail(
                "persisted type identity does not match provenance and descriptor graph",
            ));
        }
        let lookup = self.identity_lookup(&ids)?;
        if lookup != self.id_to_index || self.identity_known != known_traits(self) {
            return Err(fail(
                "stale stable identity reverse lookup or bootstrap roles",
            ));
        }
        self.identity_dirty.store(false, Ordering::Release);
        Ok(())
    }
    pub(crate) fn identity_lookup(
        &self,
        ids: &[TypeId],
    ) -> Result<HashMap<TypeId, TypeIndex>, TypeIdentityError> {
        let mut lookup = HashMap::new();
        for (index, &id) in ids.iter().enumerate() {
            if id == TypeId::ZERO {
                continue;
            }
            let ty = TypeIndex::from_raw(index as u32);
            let canonical = self
                .canonical_type(ty)
                .ok_or_else(|| fail("invalid canonical identity index"))?;
            if let Some(previous) = lookup.insert(id, canonical)
                && previous != canonical
            {
                return Err(fail(
                    "stable type identity collision between distinct canonical descriptors",
                ));
            }
        }
        Ok(lookup)
    }
}

pub(crate) fn anchors(
    pool: &TypePool,
    input: &TypeIdentityInput,
) -> Result<BTreeMap<TypeIndex, Vec<u8>>, TypeIdentityError> {
    if input.schema != IDENTITY_SCHEMA {
        return Err(fail("unsupported type identity schema"));
    }
    if pool.len() > MAX_IDENTITY_ITEMS
        || input.packages.len() > MAX_IDENTITY_ITEMS
        || input.declarations.len() > MAX_IDENTITY_ITEMS
    {
        return Err(fail("type identity item budget exceeded"));
    }
    for ordinal in 0..crate::WellKnownTraits::NAMES.len() {
        if !pool.is_reserved_type(TypeIndex::from_raw(
            (crate::Intrinsic::COUNT + ordinal) as u32,
        )) {
            return Err(fail(
                "finalized identities require the fixed deterministic bootstrap prefix",
            ));
        }
    }
    let mut input_bytes = 0usize;
    let mut input_items = input.packages.len() + input.declarations.len();
    for package in &input.packages {
        input_bytes = input_bytes
            .checked_add(package.qualified_name.len() + package.version.len() + 16)
            .ok_or_else(|| fail("identity input size overflow"))?;
    }
    for declaration in &input.declarations {
        input_bytes = input_bytes
            .checked_add(declaration.last_stable_version.len())
            .ok_or_else(|| fail("identity input size overflow"))?;
        input_items = input_items
            .checked_add(declaration.path.len())
            .ok_or_else(|| fail("identity input count overflow"))?;
        for segment in &declaration.path {
            input_bytes = input_bytes
                .checked_add(match segment {
                    IdentityPathSegment::Named(name) => name.len() + 5,
                    IdentityPathSegment::Lexical { .. } => 6,
                })
                .ok_or_else(|| fail("identity input size overflow"))?;
        }
    }
    if input_bytes > MAX_IDENTITY_BYTES || input_items > MAX_IDENTITY_ITEMS {
        return Err(fail("identity input exceeds byte or item budget"));
    }
    let mut package_keys = HashSet::new();
    for package in &input.packages {
        if package.identity_schema != 1
            || package.qualified_name.split('/').count() != 2
            || package.qualified_name.split('/').any(|part| {
                part.is_empty()
                    || part
                        .chars()
                        .any(|character| character.is_whitespace() || character.is_control())
            })
        {
            return Err(fail("invalid package type identity context"));
        }
        version(&package.version)?;
        if !package_keys.insert((
            package.identity,
            package.qualified_name.clone(),
            package.version.clone(),
        )) {
            return Err(fail("duplicate package type context"));
        }
    }
    let structural = pool
        .structural_types
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let mut anchors = BTreeMap::new();
    let mut distinct = HashSet::new();
    for declaration in &input.declarations {
        let info = pool
            .types
            .get(declaration.type_index.as_u32() as usize)
            .ok_or_else(|| fail("identity provenance type index out of range"))?;
        let kind = nominal_kind(&info.kind, structural.contains(&declaration.type_index))
            .ok_or_else(|| fail("identity provenance is not a nominal declaration"))?;
        if pool.is_reserved_type(declaration.type_index) {
            return Err(fail(
                "reserved type cannot have source declaration provenance",
            ));
        }
        let package = input
            .packages
            .get(declaration.package as usize)
            .ok_or_else(|| fail("identity provenance package index out of range"))?;
        version(&declaration.last_stable_version)?;
        if declaration.path.is_empty() || declaration.path.len() > 256 {
            return Err(fail("missing or excessive nominal declaration path"));
        }
        let declared_name = match &info.kind {
            TypeKind::Struct { name, .. }
            | TypeKind::Enum { name, .. }
            | TypeKind::Newtype { name, .. }
            | TypeKind::Trait { name, .. }
            | TypeKind::Module { name } => str_interner::try_get(*name),
            _ => None,
        };
        if let Some(name) = declared_name
            && !matches!(declaration.path.last(),Some(IdentityPathSegment::Named(segment)) if segment==&name)
        {
            return Err(fail("nominal path does not end with its declared name"));
        }
        let mut bytes = vec![kind];
        bytes.extend_from_slice(&package.identity);
        crate::identity_encoding::text(&declaration.last_stable_version, &mut bytes)?;
        crate::identity_encoding::count(declaration.path.len(), &mut bytes)?;
        for segment in &declaration.path {
            match segment {
                IdentityPathSegment::Named(name) => {
                    if name.is_empty() || name.chars().any(char::is_control) {
                        return Err(fail("invalid nominal path name"));
                    }
                    bytes.push(0);
                    crate::identity_encoding::text(name, &mut bytes)?;
                }
                IdentityPathSegment::Lexical { kind, ordinal } => {
                    bytes.extend_from_slice(&[1, *kind]);
                    bytes.extend_from_slice(&ordinal.to_be_bytes());
                }
            }
        }
        if !distinct.insert(bytes.clone())
            || anchors.insert(declaration.type_index, bytes).is_some()
        {
            return Err(fail("duplicate nominal declaration provenance or anchor"));
        }
    }
    for (index, info) in pool.types.iter().enumerate() {
        let ty = TypeIndex::from_raw(index as u32);
        if nominal_kind(&info.kind, structural.contains(&ty)).is_some()
            && !pool.is_reserved_type(ty)
            && !anchors.contains_key(&ty)
        {
            return Err(fail(format!(
                "missing nominal identity provenance for type {index}"
            )));
        }
        if info.type_id.hi() == BOOTSTRAP_NAMESPACE && !pool.is_reserved_type(ty) {
            return Err(fail("invalid deterministic bootstrap identity"));
        }
    }
    Ok(anchors)
}
pub(crate) fn nominal_kind(kind: &TypeKind, structural: bool) -> Option<u8> {
    match kind {
        TypeKind::Struct { .. } => Some(1),
        TypeKind::Enum { .. } if !structural => Some(2),
        TypeKind::Newtype { .. } => Some(3),
        TypeKind::Trait { .. } => Some(4),
        TypeKind::Module { .. } => Some(5),
        TypeKind::Effect { .. } if !structural => Some(6),
        _ => None,
    }
}
fn version(value: &str) -> Result<(), TypeIdentityError> {
    let version =
        semver::Version::parse(value).map_err(|_| fail("invalid identity semantic version"))?;
    if version.to_string() != value {
        return Err(fail("identity version is not normalized"));
    }
    Ok(())
}
