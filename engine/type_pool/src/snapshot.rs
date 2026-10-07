//! Exact-index type registry persistence and validation.

use std::collections::{HashMap, HashSet};
use std::fmt;

use str_interner::StrId;

use crate::{
    FieldInfo, Intrinsic, MethodSlot, TraitImplRecord, TypeId, TypeIndex, TypeInfo, TypeKind,
    TypePool, VTable, WellKnownTraits, intrinsic_layout, intrinsic_type_id,
};

/// Owned registry contents in their original index order. Structural provenance
/// is explicit: a nominal effect can have the same shape as an interned effect.
#[derive(Debug, Clone)]
pub struct TypePoolSnapshot {
    pub types: Vec<TypeInfo>,
    pub structural_types: Vec<TypeIndex>,
    pub methods: Vec<Vec<MethodSlot>>,
    pub scopes: Vec<crate::ScopeContext>,
    pub trait_impls: Vec<TraitImplRecord>,
    pub vtables: Vec<VTable>,
    pub trait_schemas: Vec<crate::TraitDispatchSchema>,
    pub associated_bindings: Vec<crate::AssociatedTypeBinding>,
    pub associated_defaults: Vec<crate::AssociatedTypeDefault>,
    pub well_known: WellKnownTraits,
    pub null_type: TypeIndex,
    pub identity_input: Option<crate::TypeIdentityInput>,
}

/// Invalid registry metadata. Restoration never repairs or renumbers input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotError(String);

impl SnapshotError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SnapshotError {}

fn invalid(message: impl Into<String>) -> SnapshotError {
    SnapshotError(message.into())
}

impl TypePool {
    /// Validate a live registry before exposing it to an artifact consumer.
    pub fn validate(&self) -> Result<(), SnapshotError> {
        self.snapshot().validate()?;
        self.validate_structural_provenance()?;
        self.validate_collection_layouts()?;
        self.validate_type_identities()
            .map_err(|error| invalid(error.to_string()))
    }

    pub fn snapshot(&self) -> TypePoolSnapshot {
        TypePoolSnapshot {
            types: self.types.clone(),
            structural_types: self.structural_types.clone(),
            methods: self.methods.clone(),
            scopes: self.scopes.clone(),
            trait_impls: self.trait_impls.clone(),
            vtables: self.vtables.clone(),
            trait_schemas: self.trait_schemas.clone(),
            associated_bindings: self.associated_bindings.clone(),
            associated_defaults: self.associated_defaults.clone(),
            well_known: self.well_known,
            null_type: self.null_type,
            identity_input: self.identity_input.clone(),
        }
    }

    /// Restore validated descriptors at their exact original indices, then
    /// rebuild identity lookup. ZERO identities remain distinct; nonzero
    /// identity sharing is permitted only by transparent aliases of one type.
    pub fn restore(snapshot: TypePoolSnapshot) -> Result<Self, SnapshotError> {
        snapshot.validate()?;
        let mut pool = Self {
            types: snapshot.types,
            structural_types: snapshot.structural_types,
            id_to_index: HashMap::new(),
            methods: snapshot.methods,
            scope_packages: snapshot.scopes.iter().map(|scope| scope.package).collect(),
            scopes: snapshot.scopes,
            trait_impls: snapshot.trait_impls,
            vtables: snapshot.vtables,
            trait_schemas: snapshot.trait_schemas,
            associated_bindings: snapshot.associated_bindings,
            associated_defaults: snapshot.associated_defaults,
            well_known: snapshot.well_known,
            null_type: snapshot.null_type,
            identity_input: snapshot.identity_input,
            identity_dirty: std::sync::atomic::AtomicBool::new(false),
            identity_known: [TypeIndex::INVALID; 8],
        };
        pool.identity_known = crate::identity::known_traits(&pool);
        let ids = pool
            .types
            .iter()
            .map(|info| info.type_id)
            .collect::<Vec<_>>();
        pool.id_to_index = pool
            .identity_lookup(&ids)
            .map_err(|error| invalid(error.to_string()))?;
        pool.validate_structural_provenance()?;
        pool.validate_collection_layouts()?;
        pool.validate_type_identities()
            .map_err(|error| invalid(error.to_string()))?;
        Ok(pool)
    }

    fn validate_structural_provenance(&self) -> Result<(), SnapshotError> {
        let mut seen = HashSet::new();
        let mut normalized = HashSet::new();
        for &index in &self.structural_types {
            if !seen.insert(index) {
                return Err(invalid("duplicate structural provenance index"));
            }
            let kind = &self.types[index.as_u32() as usize].kind;
            if matches!(kind, TypeKind::Enum { .. }) {
                self.validate_iteration_step_item(index)?;
            } else if let TypeKind::IterationStepTemplate { item } = kind {
                let info = self.get(index);
                if self.is_static_associated_type(*item)
                    || info.type_id != TypeId::ZERO
                    || info.size != 0
                    || info.align != 0
                {
                    return Err(invalid("invalid IterationStep template descriptor"));
                }
            } else if !matches!(
                kind,
                TypeKind::Tuple { .. }
                    | TypeKind::Function { .. }
                    | TypeKind::Effect { .. }
                    | TypeKind::Optional { .. }
                    | TypeKind::ErrorQualified { .. }
                    | TypeKind::EffectQualified { .. }
            ) {
                return Err(invalid("structural provenance refers to a nominal type"));
            }
            let shape = self.normalize_structural(kind.clone());
            let layout = if matches!(shape, TypeKind::ErrorQualified { .. }) {
                let info = self.get(index);
                Some((info.size, info.align))
            } else {
                None
            };
            if !normalized.insert((structural_key(&shape), layout)) {
                return Err(invalid("duplicate interned structural shape"));
            }
        }
        let mut identities = HashMap::new();
        for (index, info) in self.types.iter().enumerate() {
            if matches!(info.kind, TypeKind::IterationStepTemplate { .. })
                && !seen.contains(&TypeIndex::from_raw(index as u32))
            {
                return Err(invalid(
                    "IterationStep template lacks structural provenance",
                ));
            }
            if info.type_id == TypeId::ZERO {
                continue;
            }
            let index = TypeIndex::from_raw(index as u32);
            if let Some(previous) = identities.insert(info.type_id, index)
                && self.canonical_type(previous) != self.canonical_type(index)
            {
                return Err(invalid("conflicting nonzero type identity"));
            }
        }
        Ok(())
    }
}

impl TypePoolSnapshot {
    fn index(&self, ty: TypeIndex) -> Result<usize, SnapshotError> {
        let index = ty.as_u32() as usize;
        if index >= self.types.len() {
            return Err(invalid(format!(
                "type index {} is out of range",
                ty.as_u32()
            )));
        }
        Ok(index)
    }

    fn canonical_index(&self, mut ty: TypeIndex) -> Result<TypeIndex, SnapshotError> {
        for _ in 0..self.types.len() {
            match self.types[self.index(ty)?].kind {
                TypeKind::Typealias { target, .. } => ty = target,
                _ => return Ok(ty),
            }
        }
        Err(invalid("cyclic type alias"))
    }

    fn name(&self, name: StrId) -> Result<(), SnapshotError> {
        str_interner::try_get(name)
            .map(|_| ())
            .ok_or_else(|| invalid("invalid interned string handle"))
    }

    fn trait_index(&self, mut ty: TypeIndex) -> Result<(), SnapshotError> {
        for _ in 0..self.types.len() {
            match &self.types[self.index(ty)?].kind {
                TypeKind::Typealias { target, .. } => ty = *target,
                TypeKind::Trait { .. } => return Ok(()),
                _ => return Err(invalid("trait reference does not name a trait")),
            }
        }
        Err(invalid("cyclic trait alias"))
    }

    fn fields(&self, fields: &[FieldInfo], size: u32) -> Result<(), SnapshotError> {
        let mut names = HashSet::new();
        let mut offsets = HashSet::new();
        for field in fields {
            self.name(field.name)?;
            self.index(field.ty)?;
            if !names.insert(field.name) || !offsets.insert(field.offset) {
                return Err(invalid("duplicate field name or offset"));
            }
            // Runtime aggregate payloads contain eight-byte tagged slots,
            // irrespective of the source field's numeric width.
            let end = field
                .offset
                .checked_add(8)
                .ok_or_else(|| invalid("field offset overflow"))?;
            if field.offset % 8 != 0 || (size != 0 && end > size) {
                return Err(invalid("field lies outside the aggregate slot layout"));
            }
        }
        Ok(())
    }

    fn method(&self, method: &MethodSlot, packages: &HashSet<u32>) -> Result<(), SnapshotError> {
        crate::access::validate_method(&self.scopes, method, packages)?;
        self.name(method.name)?;
        if let Some(ty) = method.trait_impl {
            self.trait_index(ty)?;
        }
        Ok(())
    }

    pub(crate) fn validate(&self) -> Result<(), SnapshotError> {
        if self.types.len() < Intrinsic::COUNT
            || self.types.len() >= u32::MAX as usize
            || self.methods.len() != self.types.len()
        {
            return Err(invalid(
                "missing intrinsic prefix or invalid method table count",
            ));
        }
        crate::access::validate_scopes(&self.scopes)?;
        for scope in &self.scopes {
            if let Some(ty) = scope.assoc_type {
                self.index(ty)?;
                if matches!(
                    self.types[ty.as_u32() as usize].kind,
                    TypeKind::Typealias { .. }
                ) {
                    return Err(invalid("scope associated type must be canonical"));
                }
            }
        }
        self.validate_cycles(false)?;
        self.validate_cycles(true)?;
        for (index, info) in self.types.iter().enumerate() {
            if index < Intrinsic::COUNT {
                let intrinsic = Intrinsic::ALL[index];
                if !matches!(info.kind, TypeKind::Intrinsic(found) if found == intrinsic)
                    || (info.size, info.align) != intrinsic_layout(intrinsic)
                    || info.type_id != intrinsic_type_id(intrinsic)
                {
                    return Err(invalid("invalid intrinsic prefix"));
                }
            } else if matches!(info.kind, TypeKind::Intrinsic(_)) {
                return Err(invalid("intrinsic descriptor outside the fixed prefix"));
            }
            if (info.align == 0 && info.size != 0)
                || (info.align != 0 && !info.align.is_power_of_two())
                || (info.size != 0 && info.size % info.align != 0)
            {
                return Err(invalid("invalid size or alignment"));
            }
            match &info.kind {
                TypeKind::Intrinsic(_) => {}
                TypeKind::IterationStepTemplate { item } => {
                    self.index(*item)?;
                }
                TypeKind::AssociatedType { trait_owner, name } => {
                    self.trait_index(*trait_owner)?;
                    self.name(*name)?;
                }
                TypeKind::Struct { name, fields } => {
                    self.name(*name)?;
                    self.fields(fields, info.size)?;
                }
                TypeKind::Enum { name, variants } => {
                    self.name(*name)?;
                    let mut names = HashSet::new();
                    let mut tags = HashSet::new();
                    for variant in variants {
                        self.name(variant.name)?;
                        if variant.tag >= (1 << 25) || variant.fields.len() >= u16::MAX as usize {
                            return Err(invalid(
                                "enum tag or payload exceeds runtime representation capacity",
                            ));
                        }
                        if !names.insert(variant.name) || !tags.insert(variant.tag) {
                            return Err(invalid("duplicate enum variant name or tag"));
                        }
                        self.fields(&variant.fields, info.size)?;
                    }
                }
                TypeKind::Typealias { name, target } => {
                    self.name(*name)?;
                    self.index(*target)?;
                }
                TypeKind::Newtype { name, inner } => {
                    self.name(*name)?;
                    self.index(*inner)?;
                }
                TypeKind::Tuple { elements } => {
                    for &ty in elements {
                        self.index(ty)?;
                    }
                }
                TypeKind::Function { params, ret } | TypeKind::Effect { params, ret, .. } => {
                    for &ty in params {
                        self.index(ty)?;
                    }
                    self.index(*ret)?;
                }
                TypeKind::Optional { inner } => {
                    self.index(*inner)?;
                }
                TypeKind::ErrorQualified { errors, inner } => {
                    for &ty in errors {
                        self.index(ty)?;
                    }
                    self.index(*inner)?;
                }
                TypeKind::EffectQualified { effects, inner } => {
                    for &ty in effects {
                        self.index(ty)?;
                    }
                    self.index(*inner)?;
                }
                TypeKind::Module { name } => self.name(*name)?,
                TypeKind::Trait {
                    name,
                    parents,
                    assoc_types,
                } => {
                    self.name(*name)?;
                    let mut seen = HashSet::new();
                    for &parent in parents {
                        self.index(parent)?;
                        self.trait_index(parent)?;
                        if !seen.insert(parent) {
                            return Err(invalid("duplicate parent trait"));
                        }
                    }
                    let mut names = HashSet::new();
                    for &(name, default) in assoc_types {
                        self.name(name)?;
                        if !names.insert(name) {
                            return Err(invalid("duplicate associated type name"));
                        }
                        // INVALID explicitly means there is no default.
                        if default != TypeIndex::INVALID {
                            self.index(default)?;
                        }
                    }
                }
            }
        }
        for &ty in &self.structural_types {
            self.index(ty)?;
        }
        let known = [
            self.well_known.display,
            self.well_known.hash,
            self.well_known.eq,
            self.well_known.ord,
            self.well_known.partial_eq,
            self.well_known.partial_ord,
            self.well_known.iterator,
            self.well_known.into_iterator,
        ];
        let mut seen = HashSet::new();
        for (ty, expected) in known.into_iter().zip(WellKnownTraits::NAMES) {
            if !seen.insert(ty)
                || !matches!(&self.types[self.index(ty)?].kind,
                    TypeKind::Trait { name, .. } if str_interner::try_get(*name).as_deref() == Some(*expected))
            {
                return Err(invalid("invalid well-known trait descriptor"));
            }
        }
        if !matches!(&self.types[self.index(self.null_type)?].kind,
            TypeKind::Optional { inner } if *inner == Intrinsic::NoReturn.type_index())
            || !self.structural_types.contains(&self.null_type)
        {
            return Err(invalid("invalid canonical null descriptor"));
        }
        let packages = self.scopes.iter().map(|scope| scope.package).collect();
        for methods in &self.methods {
            for method in methods {
                self.method(method, &packages)?;
            }
        }
        let mut impl_pairs = HashSet::new();
        for record in &self.trait_impls {
            self.index(record.implementor)?;
            self.trait_index(record.trait_type)?;
            crate::traits::validate_record_scope(&self.scopes, record)?;
            let pair = (
                self.canonical_index(record.trait_type)?,
                self.canonical_index(record.implementor)?,
                record.visible_scope,
            );
            if !impl_pairs.insert(pair) {
                return Err(invalid("duplicate trait implementation"));
            }
            for method in &record.methods {
                self.method(method, &packages)?;
                if method.visible_scope != record.visible_scope {
                    return Err(invalid(
                        "trait method scope differs from its implementation",
                    ));
                }
                if method
                    .trait_impl
                    .map(|ty| self.canonical_index(ty))
                    .transpose()?
                    != Some(pair.0)
                {
                    return Err(invalid(
                        "trait implementation method has a different trait owner",
                    ));
                }
            }
        }
        let mut vtable_pairs = HashSet::new();
        for table in &self.vtables {
            self.index(table.implementor)?;
            self.trait_index(table.trait_type)?;
            let pair = (
                self.canonical_index(table.trait_type)?,
                self.canonical_index(table.implementor)?,
                table.visible_scope,
            );
            if !vtable_pairs.insert(pair) || !impl_pairs.contains(&pair) {
                return Err(invalid(
                    "duplicate vtable or vtable without an exact implementation",
                ));
            }
        }
        crate::associated_defaults::validate_defaults(
            &self.types,
            &self.associated_defaults,
            true,
            self.identity_input.is_some(),
        )?;
        crate::associated::validate_bindings(
            &self.types,
            &self.trait_impls,
            &self.associated_bindings,
            !self.associated_bindings.is_empty()
                || self
                    .trait_schemas
                    .iter()
                    .flat_map(|schema| &schema.slots)
                    .any(|key| {
                        key.signature
                            .as_ref()
                            .is_some_and(|signature| !signature.associated_paths.is_empty())
                    }),
        )?;
        crate::dispatch::Registry {
            types: &self.types,
            scopes: &self.scopes,
            records: &self.trait_impls,
            tables: &self.vtables,
            schemas: &self.trait_schemas,
        }
        .validate()
        .map_err(|error| invalid(error.to_string()))?;
        Ok(())
    }

    /// Iterative DFS rejects transparent recursion and inherited trait cycles.
    /// Nominal aggregate fields are deliberately not edges: recursive structs
    /// and enums have finite tagged-slot layouts and are valid.
    fn validate_cycles(&self, traits: bool) -> Result<(), SnapshotError> {
        let mut state = vec![0u8; self.types.len()];
        let mut heights = vec![0usize; self.types.len()];
        for start in 0..self.types.len() {
            if state[start] != 0 {
                continue;
            }
            let mut stack = vec![(start, false)];
            while let Some((index, exiting)) = stack.pop() {
                if exiting {
                    let mut height = 0;
                    for edge in self.graph_edges(index, traits) {
                        height = height.max(heights[self.index(edge)?] + 1);
                    }
                    if height > 256 {
                        return Err(invalid(
                            "type metadata nesting exceeds the runtime query limit",
                        ));
                    }
                    heights[index] = height;
                    state[index] = 2;
                    continue;
                }
                match state[index] {
                    2 => continue,
                    1 => return Err(invalid("cyclic transparent type or trait inheritance")),
                    _ => {}
                }
                state[index] = 1;
                stack.push((index, true));
                for edge in self.graph_edges(index, traits).into_iter().rev() {
                    stack.push((self.index(edge)?, false));
                }
            }
        }
        Ok(())
    }

    fn graph_edges(&self, index: usize, traits: bool) -> Vec<TypeIndex> {
        let mut edges = Vec::new();
        match &self.types[index].kind {
            TypeKind::IterationStepTemplate { item } if !traits => edges.push(*item),
            TypeKind::Typealias { target, .. } => edges.push(*target),
            TypeKind::Trait { parents, .. } if traits => edges.extend(parents),
            TypeKind::Newtype { inner, .. } | TypeKind::Optional { inner } if !traits => {
                edges.push(*inner)
            }
            TypeKind::Tuple { elements } if !traits => edges.extend(elements),
            TypeKind::Function { params, ret } | TypeKind::Effect { params, ret, .. }
                if !traits =>
            {
                edges.extend(params);
                edges.push(*ret);
            }
            TypeKind::ErrorQualified { errors, inner } if !traits => {
                edges.extend(errors);
                edges.push(*inner);
            }
            TypeKind::EffectQualified { effects, inner } if !traits => {
                edges.extend(effects);
                edges.push(*inner);
            }
            _ => {}
        }
        edges
    }
}

fn structural_key(kind: &TypeKind) -> (u8, Vec<u32>) {
    let indices = |types: &[TypeIndex]| types.iter().map(|ty| ty.as_u32()).collect::<Vec<_>>();
    match kind {
        TypeKind::IterationStepTemplate { item } => (7, vec![item.as_u32()]),
        TypeKind::Tuple { elements } => (0, indices(elements)),
        TypeKind::Function { params, ret } => {
            let mut key = indices(params);
            key.push(ret.as_u32());
            (1, key)
        }
        TypeKind::Effect {
            params,
            ret,
            is_async,
        } => {
            let mut key = indices(params);
            key.push(ret.as_u32());
            key.push(u32::from(*is_async));
            (2, key)
        }
        TypeKind::Optional { inner } => (3, vec![inner.as_u32()]),
        TypeKind::Enum { .. } => (
            6,
            crate::iteration_step::step_payload(kind)
                .into_iter()
                .map(TypeIndex::as_u32)
                .collect(),
        ),
        TypeKind::ErrorQualified { errors, inner } => {
            let mut key = indices(errors);
            key.push(inner.as_u32());
            (4, key)
        }
        TypeKind::EffectQualified { effects, inner } => {
            let mut key = indices(effects);
            key.push(inner.as_u32());
            (5, key)
        }
        _ => (u8::MAX, Vec::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::VariantInfo;

    fn register(pool: &mut TypePool, kind: TypeKind) -> TypeIndex {
        pool.register(TypeInfo {
            kind,
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        })
    }

    #[test]
    fn exact_indices_and_structural_provenance_survive_restore() {
        let mut pool = TypePool::with_intrinsics();
        let shape = TypeKind::Effect {
            params: vec![Intrinsic::I64.type_index()],
            ret: Intrinsic::Bool.type_index(),
            is_async: true,
        };
        let nominal = register(&mut pool, shape.clone());
        let structural = pool.intern_structural(shape.clone());
        let other_nominal = register(&mut pool, shape.clone());
        let alias = register(
            &mut pool,
            TypeKind::Typealias {
                name: str_interner::intern("ResultAlias"),
                target: Intrinsic::Bool.type_index(),
            },
        );
        let trait_type = pool.well_known.display;
        let method = MethodSlot {
            name: str_interner::intern("display"),
            func_id: crate::DERIVE_FUNC_ID,
            trait_impl: Some(trait_type),
            visible_scope: Some(13),
            access: crate::MethodAccess::LegacyUnknown,
        };
        pool.add_method(nominal, method.clone());
        pool.add_trait_impl(TraitImplRecord {
            visible_scope: Some(13),
            trait_type,
            implementor: nominal,
            methods: vec![method],
        });
        pool.add_vtable(VTable {
            visible_scope: Some(13),
            trait_type,
            implementor: nominal,
            entries: vec![crate::DERIVE_FUNC_ID],
        });
        let mut restored = TypePool::restore(pool.snapshot()).unwrap();
        assert_eq!(restored.len(), pool.len());
        assert_eq!(restored.intern_structural(shape), structural);
        assert_ne!(nominal, structural);
        assert_ne!(other_nominal, structural);
        assert_eq!(
            restored.canonical_type(alias),
            Some(Intrinsic::Bool.type_index())
        );
        assert_eq!(restored.methods_of(nominal)[0].visible_scope, Some(13));
        assert_eq!(
            restored.vtables_snapshot()[0].entries,
            vec![crate::DERIVE_FUNC_ID]
        );
        assert_eq!(restored.null_type(), pool.null_type());
        assert_eq!(
            restored.lookup_by_id(restored.get(Intrinsic::I64.type_index()).type_id),
            Some(Intrinsic::I64.type_index())
        );
    }

    #[test]
    fn nominal_recursive_aggregate_is_valid() {
        let mut pool = TypePool::with_intrinsics();
        let ty = TypeIndex::from_raw(pool.len() as u32);
        register(
            &mut pool,
            TypeKind::Struct {
                name: str_interner::intern("Recursive"),
                fields: vec![FieldInfo {
                    name: str_interner::intern("next"),
                    ty,
                    has_default: false,
                    offset: 0,
                }],
            },
        );
        let restored = TypePool::restore(pool.snapshot()).unwrap();
        let TypeKind::Struct { fields, .. } = &restored.get(ty).kind else {
            panic!("struct")
        };
        assert_eq!(fields[0].ty, ty);
    }

    #[test]
    fn enum_tags_and_payload_words_respect_the_runtime_encoding() {
        for (tag, count, valid) in [
            ((1 << 25) - 1, 1, true),
            (1 << 25, 0, false),
            (0, u16::MAX as usize, false),
        ] {
            let mut pool = TypePool::with_intrinsics();
            let field = FieldInfo {
                name: str_interner::intern("field"),
                ty: Intrinsic::Any.type_index(),
                has_default: false,
                offset: 0,
            };
            register(
                &mut pool,
                TypeKind::Enum {
                    name: str_interner::intern("Enum"),
                    variants: vec![crate::VariantInfo {
                        name: str_interner::intern("Case"),
                        tag,
                        fields: vec![field; count],
                    }],
                },
            );
            assert_eq!(pool.validate().is_ok(), valid);
            assert_eq!(TypePool::restore(pool.snapshot()).is_ok(), valid);
        }
    }

    #[test]
    fn damaged_prefix_indices_layout_and_canonical_types_are_rejected() {
        let pool = TypePool::with_intrinsics();
        let original = pool.snapshot();
        let mut cases = Vec::new();
        let mut bad = original.clone();
        bad.types[0].align = 3;
        cases.push(bad);
        let mut bad = original.clone();
        bad.types[0].type_id = TypeId::ZERO;
        cases.push(bad);
        let mut bad = original.clone();
        bad.types[0].kind = TypeKind::Intrinsic(Intrinsic::I8);
        cases.push(bad);
        let mut bad = original.clone();
        bad.methods.pop();
        cases.push(bad);
        let mut bad = original.clone();
        bad.structural_types.push(TypeIndex::INVALID);
        cases.push(bad);
        let mut bad = original.clone();
        bad.structural_types.push(bad.null_type);
        cases.push(bad);
        let mut bad = original.clone();
        bad.structural_types.push(Intrinsic::I64.type_index());
        cases.push(bad);
        let mut bad = original.clone();
        bad.null_type = Intrinsic::Unit.type_index();
        cases.push(bad);
        let mut bad = original.clone();
        bad.well_known.eq = bad.well_known.ord;
        cases.push(bad);
        let mut bad = original.clone();
        bad.types[bad.null_type.as_u32() as usize].kind = TypeKind::Optional {
            inner: TypeIndex::INVALID,
        };
        cases.push(bad);
        for case in cases {
            assert!(TypePool::restore(case).is_err());
        }
    }

    #[test]
    fn aliases_traits_and_transparent_cycles_are_rejected() {
        for kind in 0..4 {
            let mut pool = TypePool::with_intrinsics();
            let ty = TypeIndex::from_raw(pool.len() as u32);
            let descriptor = match kind {
                0 => TypeKind::Typealias {
                    name: str_interner::intern("Cycle"),
                    target: ty,
                },
                1 => TypeKind::Trait {
                    name: str_interner::intern("Cycle"),
                    parents: vec![ty],
                    assoc_types: Vec::new(),
                },
                2 => TypeKind::Tuple { elements: vec![ty] },
                _ => TypeKind::ErrorQualified {
                    errors: Vec::new(),
                    inner: ty,
                },
            };
            register(&mut pool, descriptor);
            assert!(TypePool::restore(pool.snapshot()).is_err());
        }
    }

    #[test]
    fn invalid_names_duplicate_shapes_and_type_id_conflicts_are_rejected() {
        let mut pool = TypePool::with_intrinsics();
        register(
            &mut pool,
            TypeKind::Module {
                name: StrId::from_raw(u32::MAX),
            },
        );
        assert!(TypePool::restore(pool.snapshot()).is_err());
        let mut pool = TypePool::with_intrinsics();
        let duplicate = register(
            &mut pool,
            TypeKind::Optional {
                inner: Intrinsic::NoReturn.type_index(),
            },
        );
        let mut snapshot = pool.snapshot();
        snapshot.structural_types.push(duplicate);
        assert!(TypePool::restore(snapshot).is_err());
        let id = pool.get(Intrinsic::I64.type_index()).type_id;
        pool.get_mut(duplicate).type_id = id;
        assert!(TypePool::restore(pool.snapshot()).is_err());
        pool.get_mut(duplicate).kind = TypeKind::Typealias {
            name: str_interner::intern("AliasWithIdentity"),
            target: Intrinsic::I64.type_index(),
        };
        assert!(TypePool::restore(pool.snapshot()).is_ok());
    }

    #[test]
    fn invalid_aggregate_and_trait_metadata_are_rejected() {
        let mut pool = TypePool::with_intrinsics();
        pool.register(TypeInfo {
            kind: TypeKind::Struct {
                name: str_interner::intern("InvalidLayout"),
                fields: vec![FieldInfo {
                    name: str_interner::intern("outside"),
                    ty: Intrinsic::I64.type_index(),
                    has_default: false,
                    offset: 8,
                }],
            },
            type_id: TypeId::ZERO,
            size: 8,
            align: 8,
        });
        assert!(TypePool::restore(pool.snapshot()).is_err());
        let mut pool = TypePool::with_intrinsics();
        register(
            &mut pool,
            TypeKind::Enum {
                name: str_interner::intern("Enum"),
                variants: vec![
                    VariantInfo {
                        name: str_interner::intern("One"),
                        tag: 1,
                        fields: Vec::new(),
                    },
                    VariantInfo {
                        name: str_interner::intern("Two"),
                        tag: 1,
                        fields: Vec::new(),
                    },
                ],
            },
        );
        assert!(TypePool::restore(pool.snapshot()).is_err());
        let mut pool = TypePool::with_intrinsics();
        pool.add_trait_impl(TraitImplRecord {
            visible_scope: None,
            trait_type: Intrinsic::Bool.type_index(),
            implementor: Intrinsic::I64.type_index(),
            methods: Vec::new(),
        });
        assert!(TypePool::restore(pool.snapshot()).is_err());
        let mut pool = TypePool::with_intrinsics();
        pool.add_vtable(VTable {
            visible_scope: None,
            trait_type: pool.well_known.display,
            implementor: Intrinsic::I64.type_index(),
            entries: Vec::new(),
        });
        assert!(TypePool::restore(pool.snapshot()).is_err());
    }

    #[test]
    fn excessive_transparent_nesting_is_rejected_independent_of_index_order() {
        let mut pool = TypePool::with_intrinsics();
        let mut inner = Intrinsic::I64.type_index();
        for _ in 0..258 {
            inner = register(&mut pool, TypeKind::Optional { inner });
        }
        assert!(TypePool::restore(pool.snapshot()).is_err());
    }
}
