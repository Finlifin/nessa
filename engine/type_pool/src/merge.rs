//! Incremental type-pool merging for per-module `.nsbc` dynamic loading.
//!
//! Merges nominal types by 128-bit [`TypeId`], interns structural types by canonical shape,
//! isolates archive-local lexical scopes and package-visibility IDs, preserves authenticated
//! [`TypeIdentityInput`] provenance, and publishes [`MethodSlot`] / [`TraitImplRecord`] / [`VTable`]
//! entries incrementally as individual modules (`mod`) are loaded into the VM.

use std::collections::{HashMap, HashSet};

use crate::{
    AssociatedTypeBinding, AssociatedTypeDefault, AssociatedTypeExpr, FieldInfo, MethodAccess,
    MethodSlot, NominalTypeProvenance, ScopeContext, SnapshotError, TraitAssociatedPath,
    TraitDispatchSchema, TraitImplRecord, TraitMethodKey, TraitMethodSignature, TypeId,
    TypeIdentityInput, TypeIndex, TypeInfo, TypeKind, TypePool, VTable, VariantInfo,
};

/// Remapping tables produced when merging a [`ModularArtifact`](crate) [`TypePool`] into a VM [`TypePool`].
#[derive(Debug, Clone)]
pub struct MergedPoolMapping {
    /// Maps every `src` [`TypeIndex`] (`0 .. src.len()`) to its canonical `dst` [`TypeIndex`].
    pub type_map: Vec<TypeIndex>,
    /// Base offset added to `src` lexical scope IDs (`dst_scope = src_scope + scope_base`).
    pub scope_base: u32,
    /// Maps `src` archive-local `ScopeContext::package` IDs to isolated `dst` package IDs.
    pub scope_pkg_map: HashMap<u32, u32>,
}

impl MergedPoolMapping {
    #[inline]
    pub fn map_type(&self, src: TypeIndex) -> Option<TypeIndex> {
        if src == TypeIndex::INVALID {
            return Some(TypeIndex::INVALID);
        }
        self.type_map.get(src.as_u32() as usize).copied()
    }

    #[inline]
    pub fn map_scope(&self, src_scope: u32) -> u32 {
        src_scope + self.scope_base
    }

    pub fn map_method_access(&self, access: MethodAccess) -> Result<MethodAccess, SnapshotError> {
        match access {
            MethodAccess::Public => Ok(MethodAccess::Public),
            MethodAccess::Private(scope) => Ok(MethodAccess::Private(self.map_scope(scope))),
            MethodAccess::Package(pkg) => self
                .scope_pkg_map
                .get(&pkg)
                .copied()
                .map(MethodAccess::Package)
                .ok_or_else(|| {
                    SnapshotError::new("method package ID missing from merged scope map")
                }),
        }
    }
}

/// Tracks which dispatch entries from a specific source [`TypePool`] have already been published
/// into the VM [`TypePool`] across incremental per-`mod` loads.
#[derive(Debug, Default, Clone)]
pub struct MergedDispatchState {
    installed_methods: HashSet<(usize, usize)>,
    installed_trait_impls: HashSet<usize>,
    installed_vtables: HashSet<usize>,
}

fn remap_assoc_expr(
    expr: &AssociatedTypeExpr,
    map: &[TypeIndex],
) -> Result<AssociatedTypeExpr, SnapshotError> {
    let m = |ty: TypeIndex| {
        map.get(ty.as_u32() as usize)
            .copied()
            .filter(|&t| t != TypeIndex::INVALID)
            .ok_or_else(|| SnapshotError::new("invalid TypeIndex in associated default expression"))
    };
    Ok(match expr {
        AssociatedTypeExpr::Required => AssociatedTypeExpr::Required,
        AssociatedTypeExpr::Concrete(ty) => AssociatedTypeExpr::Concrete(m(*ty)?),
        AssociatedTypeExpr::SelfType { trait_owner } => AssociatedTypeExpr::SelfType {
            trait_owner: m(*trait_owner)?,
        },
        AssociatedTypeExpr::Binding { trait_owner, name } => AssociatedTypeExpr::Binding {
            trait_owner: m(*trait_owner)?,
            name: *name,
        },
        AssociatedTypeExpr::Optional { inner } => AssociatedTypeExpr::Optional {
            inner: Box::new(remap_assoc_expr(inner, map)?),
        },
        AssociatedTypeExpr::Future { inner } => AssociatedTypeExpr::Future {
            inner: Box::new(remap_assoc_expr(inner, map)?),
        },
        AssociatedTypeExpr::IterationStep { item } => AssociatedTypeExpr::IterationStep {
            item: Box::new(remap_assoc_expr(item, map)?),
        },
        AssociatedTypeExpr::Tuple { elements } => AssociatedTypeExpr::Tuple {
            elements: elements
                .iter()
                .map(|e| remap_assoc_expr(e, map))
                .collect::<Result<Vec<_>, _>>()?,
        },
        AssociatedTypeExpr::Function {
            parameters,
            return_type,
        } => AssociatedTypeExpr::Function {
            parameters: parameters
                .iter()
                .map(|p| remap_assoc_expr(p, map))
                .collect::<Result<Vec<_>, _>>()?,
            return_type: Box::new(remap_assoc_expr(return_type, map)?),
        },
    })
}

fn remap_trait_sig(
    sig: &TraitMethodSignature,
    map: &[TypeIndex],
) -> Result<TraitMethodSignature, SnapshotError> {
    let m = |ty: TypeIndex| {
        map.get(ty.as_u32() as usize)
            .copied()
            .filter(|&t| t != TypeIndex::INVALID)
            .ok_or_else(|| SnapshotError::new("invalid TypeIndex in trait method signature"))
    };
    let mut associated_paths = Vec::with_capacity(sig.associated_paths.len());
    for ap in &sig.associated_paths {
        associated_paths.push(TraitAssociatedPath {
            trait_owner: m(ap.trait_owner)?,
            name: ap.name,
            path: ap.path.clone(),
        });
    }
    Ok(TraitMethodSignature {
        associated_paths,
        declaration: m(sig.declaration)?,
        self_paths: sig.self_paths.clone(),
        parameter_kinds: sig.parameter_kinds.clone(),
    })
}

impl TypePool {
    /// Merge passive type definitions, lexical scopes, trait schemas, associated bindings/defaults,
    /// and finalized [`TypeIdentityInput`] provenance from `src` into `self`.
    ///
    /// Method tables, trait implementations, and vtables are published separately via
    /// [`Self::install_merged_dispatch`] as modules' `FuncId`s are loaded into the VM.
    pub fn merge_types_and_scopes(
        &mut self,
        src: &TypePool,
    ) -> Result<MergedPoolMapping, SnapshotError> {
        src.validate()?;
        let src_snap = src.snapshot();
        let src_input = src_snap.identity_input.as_ref().ok_or_else(|| {
            SnapshotError::new("cannot merge TypePool without finalized TypeIdentityInput")
        })?;

        if self.identity_input.is_none() {
            self.identity_input = Some(TypeIdentityInput {
                schema: src_input.schema,
                packages: Vec::new(),
                declarations: Vec::new(),
            });
            self.identity_known = crate::identity::known_traits(self);
        }

        let mut identity_pkg_map = Vec::with_capacity(src_input.packages.len());
        {
            let dst_input = self.identity_input.as_mut().unwrap();
            if dst_input.schema != src_input.schema {
                return Err(SnapshotError::new("incompatible TypeIdentityInput schema"));
            }
            for pkg in &src_input.packages {
                if let Some(pos) = dst_input.packages.iter().position(|existing| {
                    existing.identity == pkg.identity
                        && existing.qualified_name == pkg.qualified_name
                        && existing.version == pkg.version
                }) {
                    identity_pkg_map.push(pos as u32);
                } else {
                    let idx = dst_input.packages.len() as u32;
                    dst_input.packages.push(pkg.clone());
                    identity_pkg_map.push(idx);
                }
            }
        }

        let src_structural: HashSet<TypeIndex> =
            src_snap.structural_types.iter().copied().collect();
        let src_decl_by_ty: HashMap<TypeIndex, &NominalTypeProvenance> = src_input
            .declarations
            .iter()
            .map(|d| (d.type_index, d))
            .collect();

        let mut resolved: Vec<Option<TypeIndex>> = vec![None; src_snap.types.len()];
        let mut newly_added_nominals: Vec<(TypeIndex, TypeIndex)> = Vec::new();
        let mut new_declarations: Vec<NominalTypeProvenance> = Vec::new();

        // Pass 1: Match or allocate every non-structural, non-alias type carrying a nonzero stable `TypeId`
        // (intrinsics, well-known traits, collection roles, and user nominal declarations).
        for (raw_i, info) in src_snap.types.iter().enumerate() {
            let src_ty = TypeIndex::from_raw(raw_i as u32);
            if matches!(info.kind, TypeKind::Typealias { .. })
                || src_structural.contains(&src_ty)
                || info.type_id == TypeId::ZERO
            {
                continue;
            }
            if let Some(&existing) = self.id_to_index.get(&info.type_id) {
                resolved[raw_i] = Some(existing);
            } else {
                let decl = src_decl_by_ty.get(&src_ty).ok_or_else(|| {
                    SnapshotError::new("missing NominalTypeProvenance for merged nominal type")
                })?;
                let dst_pkg_idx = identity_pkg_map[decl.package as usize];
                if let Some(dst_input) = self.identity_input.as_ref()
                    && let Some(prev) = dst_input.declarations.iter().find(|d| {
                        d.package == dst_pkg_idx
                            && d.path == decl.path
                            && std::mem::discriminant(&self.get(d.type_index).kind)
                                == std::mem::discriminant(&info.kind)
                    })
                {
                    let pkg_name = &dst_input.packages[dst_pkg_idx as usize].qualified_name;
                    return Err(SnapshotError::new(format!(
                        "nominal TypeId mismatch across separate compilation for `{pkg_name}`::{:?}: existing {:?} ({:?}) vs incoming {:?} ({:?})",
                        decl.path,
                        self.get(prev.type_index).type_id,
                        self.get(prev.type_index).kind,
                        info.type_id,
                        info.kind,
                    )));
                }
                let placeholder_kind = match &info.kind {
                    TypeKind::Module { name } => TypeKind::Module { name: *name },
                    _ => TypeKind::Intrinsic(crate::Intrinsic::Unit),
                };
                let new_dst = self.push(TypeInfo {
                    kind: placeholder_kind,
                    type_id: info.type_id,
                    size: info.size,
                    align: info.align,
                });
                resolved[raw_i] = Some(new_dst);
                newly_added_nominals.push((src_ty, new_dst));
                new_declarations.push(NominalTypeProvenance {
                    type_index: new_dst,
                    package: identity_pkg_map[decl.package as usize],
                    path: decl.path.clone(),
                    last_stable_version: decl.last_stable_version.clone(),
                });
            }
        }

        // Pass 2: Resolve `AssociatedType` binders, structural types, and transparent `Typealias`es.
        fn resolve_entry(
            dst: &mut TypePool,
            src: &TypePool,
            src_snap: &crate::TypePoolSnapshot,
            src_structural: &HashSet<TypeIndex>,
            resolved: &mut [Option<TypeIndex>],
            visiting: &mut HashSet<TypeIndex>,
            src_ty: TypeIndex,
        ) -> Result<TypeIndex, SnapshotError> {
            let idx = src_ty.as_u32() as usize;
            if let Some(done) = resolved
                .get(idx)
                .copied()
                .ok_or_else(|| SnapshotError::new("TypeIndex out of range during pool merge"))?
            {
                return Ok(done);
            }
            if !visiting.insert(src_ty) {
                return Err(SnapshotError::new(
                    "cyclic non-nominal descriptor during pool merge",
                ));
            }
            let info = &src_snap.types[idx];
            let mapped = match &info.kind {
                TypeKind::Typealias { target, .. } => {
                    let canon = src.canonical_type(*target).ok_or_else(|| {
                        SnapshotError::new("invalid Typealias target in source pool")
                    })?;
                    resolve_entry(
                        dst,
                        src,
                        src_snap,
                        src_structural,
                        resolved,
                        visiting,
                        canon,
                    )?
                }
                TypeKind::AssociatedType { trait_owner, name } => {
                    let dst_owner = resolve_entry(
                        dst,
                        src,
                        src_snap,
                        src_structural,
                        resolved,
                        visiting,
                        *trait_owner,
                    )?;
                    if let Some(existing) =
                        dst.types.iter().enumerate().find_map(|(i, existing_info)| {
                            match &existing_info.kind {
                                TypeKind::AssociatedType {
                                    trait_owner: cand_owner,
                                    name: cand_name,
                                } if *cand_owner == dst_owner && *cand_name == *name => {
                                    Some(TypeIndex::from_raw(i as u32))
                                }
                                _ => None,
                            }
                        })
                    {
                        existing
                    } else {
                        dst.push(TypeInfo {
                            kind: TypeKind::AssociatedType {
                                trait_owner: dst_owner,
                                name: *name,
                            },
                            type_id: TypeId::ZERO,
                            size: 0,
                            align: 0,
                        })
                    }
                }
                TypeKind::Enum { .. } if src_structural.contains(&src_ty) => {
                    let src_item = src
                        .checked_iteration_step_item(src_ty)?
                        .ok_or_else(|| SnapshotError::new("expected structural IterationStep"))?;
                    let dst_item = resolve_entry(
                        dst,
                        src,
                        src_snap,
                        src_structural,
                        resolved,
                        visiting,
                        src_item,
                    )?;
                    dst.intern_iteration_step(dst_item)?
                }
                TypeKind::IterationStepTemplate { item } => {
                    let dst_item = resolve_entry(
                        dst,
                        src,
                        src_snap,
                        src_structural,
                        resolved,
                        visiting,
                        *item,
                    )?;
                    dst.intern_iteration_step_template(dst_item)?
                }
                TypeKind::Optional { inner } => {
                    let dst_inner = resolve_entry(
                        dst,
                        src,
                        src_snap,
                        src_structural,
                        resolved,
                        visiting,
                        *inner,
                    )?;
                    dst.intern_structural(TypeKind::Optional { inner: dst_inner })
                }
                TypeKind::Future { inner } => {
                    let dst_inner = resolve_entry(
                        dst,
                        src,
                        src_snap,
                        src_structural,
                        resolved,
                        visiting,
                        *inner,
                    )?;
                    dst.intern_future(dst_inner)
                }
                TypeKind::Tuple { elements } => {
                    let mut dst_elems = Vec::with_capacity(elements.len());
                    for &el in elements {
                        dst_elems.push(resolve_entry(
                            dst,
                            src,
                            src_snap,
                            src_structural,
                            resolved,
                            visiting,
                            el,
                        )?);
                    }
                    dst.intern_structural(TypeKind::Tuple {
                        elements: dst_elems,
                    })
                }
                TypeKind::Function { params, ret } => {
                    let mut dst_params = Vec::with_capacity(params.len());
                    for &p in params {
                        dst_params.push(resolve_entry(
                            dst,
                            src,
                            src_snap,
                            src_structural,
                            resolved,
                            visiting,
                            p,
                        )?);
                    }
                    let dst_ret = resolve_entry(
                        dst,
                        src,
                        src_snap,
                        src_structural,
                        resolved,
                        visiting,
                        *ret,
                    )?;
                    dst.intern_structural(TypeKind::Function {
                        params: dst_params,
                        ret: dst_ret,
                    })
                }
                TypeKind::Effect {
                    params,
                    ret,
                    is_async,
                } if src_structural.contains(&src_ty) => {
                    let mut dst_params = Vec::with_capacity(params.len());
                    for &p in params {
                        dst_params.push(resolve_entry(
                            dst,
                            src,
                            src_snap,
                            src_structural,
                            resolved,
                            visiting,
                            p,
                        )?);
                    }
                    let dst_ret = resolve_entry(
                        dst,
                        src,
                        src_snap,
                        src_structural,
                        resolved,
                        visiting,
                        *ret,
                    )?;
                    dst.intern_structural(TypeKind::Effect {
                        params: dst_params,
                        ret: dst_ret,
                        is_async: *is_async,
                    })
                }
                TypeKind::ErrorQualified { errors, inner } => {
                    let mut dst_errs = Vec::with_capacity(errors.len());
                    for &e in errors {
                        dst_errs.push(resolve_entry(
                            dst,
                            src,
                            src_snap,
                            src_structural,
                            resolved,
                            visiting,
                            e,
                        )?);
                    }
                    let dst_inner = resolve_entry(
                        dst,
                        src,
                        src_snap,
                        src_structural,
                        resolved,
                        visiting,
                        *inner,
                    )?;
                    dst.intern_structural(TypeKind::ErrorQualified {
                        errors: dst_errs,
                        inner: dst_inner,
                    })
                }
                TypeKind::EffectQualified { effects, inner } => {
                    let mut dst_effs = Vec::with_capacity(effects.len());
                    for &e in effects {
                        dst_effs.push(resolve_entry(
                            dst,
                            src,
                            src_snap,
                            src_structural,
                            resolved,
                            visiting,
                            e,
                        )?);
                    }
                    let dst_inner = resolve_entry(
                        dst,
                        src,
                        src_snap,
                        src_structural,
                        resolved,
                        visiting,
                        *inner,
                    )?;
                    dst.intern_structural(TypeKind::EffectQualified {
                        effects: dst_effs,
                        inner: dst_inner,
                    })
                }
                other => {
                    return Err(SnapshotError::new(format!(
                        "unexpected unfinalized nominal descriptor during pool merge: {other:?}"
                    )));
                }
            };
            if !matches!(info.kind, TypeKind::Typealias { .. }) {
                let dst_info = &mut dst.types[mapped.as_u32() as usize];
                if dst_info.size == 0 && dst_info.align == 0 {
                    dst_info.size = info.size;
                    dst_info.align = info.align;
                }
            }
            visiting.remove(&src_ty);
            resolved[idx] = Some(mapped);
            Ok(mapped)
        }

        let mut visiting = HashSet::new();
        for raw_i in 0..src_snap.types.len() {
            if resolved[raw_i].is_none() {
                resolve_entry(
                    self,
                    src,
                    &src_snap,
                    &src_structural,
                    &mut resolved,
                    &mut visiting,
                    TypeIndex::from_raw(raw_i as u32),
                )?;
            }
        }

        let type_map: Vec<TypeIndex> = resolved
            .into_iter()
            .map(|opt| opt.expect("every source TypeIndex resolved"))
            .collect();

        // Pass 3: Populate remapped `TypeKind` payloads on newly registered nominal types.
        for &(src_ty, dst_ty) in &newly_added_nominals {
            let remapped = match &src_snap.types[src_ty.as_u32() as usize].kind {
                TypeKind::Struct { name, fields } => TypeKind::Struct {
                    name: *name,
                    fields: fields
                        .iter()
                        .map(|f| FieldInfo {
                            name: f.name,
                            ty: type_map[f.ty.as_u32() as usize],
                            has_default: f.has_default,
                            offset: f.offset,
                        })
                        .collect(),
                },
                TypeKind::Enum { name, variants } => TypeKind::Enum {
                    name: *name,
                    variants: variants
                        .iter()
                        .map(|v| VariantInfo {
                            name: v.name,
                            tag: v.tag,
                            fields: v
                                .fields
                                .iter()
                                .map(|f| FieldInfo {
                                    name: f.name,
                                    ty: type_map[f.ty.as_u32() as usize],
                                    has_default: f.has_default,
                                    offset: f.offset,
                                })
                                .collect(),
                        })
                        .collect(),
                },
                TypeKind::Newtype { name, inner } => TypeKind::Newtype {
                    name: *name,
                    inner: type_map[inner.as_u32() as usize],
                },
                TypeKind::Module { name } => TypeKind::Module { name: *name },
                TypeKind::Trait {
                    name,
                    parents,
                    assoc_types,
                } => TypeKind::Trait {
                    name: *name,
                    parents: parents
                        .iter()
                        .map(|p| type_map[p.as_u32() as usize])
                        .collect(),
                    assoc_types: assoc_types
                        .iter()
                        .map(|(aname, aty)| (*aname, type_map[aty.as_u32() as usize]))
                        .collect(),
                },
                TypeKind::Effect {
                    params,
                    ret,
                    is_async,
                } => TypeKind::Effect {
                    params: params
                        .iter()
                        .map(|p| type_map[p.as_u32() as usize])
                        .collect(),
                    ret: type_map[ret.as_u32() as usize],
                    is_async: *is_async,
                },
                other => other.clone(),
            };
            self.types[dst_ty.as_u32() as usize].kind = remapped;
        }

        // Also sync bootstrap `Iterator` / `IntoIterator` `assoc_types` if `self` started from
        // `with_intrinsics()` (where well-known traits initially had empty `assoc_types` / `parents`)!
        for &known_name in crate::WellKnownTraits::NAMES {
            let sid = str_interner::intern(known_name);
            if let Some(src_wk_idx) = src_snap.types.iter().position(|t| {
                t.kind.trait_name() == Some(sid)
                    && t.type_id.hi() == crate::identity::BOOTSTRAP_NAMESPACE
            }) {
                let src_wk_tid = src_snap.types[src_wk_idx].type_id;
                if let Some(&dst_wk_ty) = self.id_to_index.get(&src_wk_tid)
                    && let TypeKind::Trait {
                        parents: src_parents,
                        assoc_types: src_assocs,
                        ..
                    } = &src_snap.types[src_wk_idx].kind
                {
                    let mapped_parents: Vec<TypeIndex> = src_parents
                        .iter()
                        .map(|p| type_map[p.as_u32() as usize])
                        .collect();
                    let mapped_assocs: Vec<(str_interner::StrId, TypeIndex)> = src_assocs
                        .iter()
                        .map(|(n, t)| (*n, type_map[t.as_u32() as usize]))
                        .collect();
                    if let TypeKind::Trait {
                        parents: dst_parents,
                        assoc_types: dst_assocs,
                        ..
                    } = &mut self.types[dst_wk_ty.as_u32() as usize].kind
                    {
                        if dst_parents.is_empty() && !mapped_parents.is_empty() {
                            *dst_parents = mapped_parents;
                        }
                        if dst_assocs.is_empty() && !mapped_assocs.is_empty() {
                            *dst_assocs = mapped_assocs;
                        }
                    }
                }
            }
        }

        self.future_index_dirty = true;
        self.concrete_types
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();

        if let Some(dst_input) = self.identity_input.as_mut() {
            dst_input.declarations.extend(new_declarations);
        }

        // Pass 4: Append isolated lexical scopes (`ScopeContext`).
        let scope_base = u32::try_from(self.scopes.len())
            .map_err(|_| SnapshotError::new("merged scope count overflow"))?;
        let mut next_scope_pkg = self
            .scope_packages
            .iter()
            .copied()
            .max()
            .map_or(0, |m| m + 1);
        let mut scope_pkg_map: HashMap<u32, u32> = HashMap::new();
        for sc in &src_snap.scopes {
            let dst_pkg = *scope_pkg_map.entry(sc.package).or_insert_with(|| {
                let allocated = next_scope_pkg;
                next_scope_pkg += 1;
                allocated
            });
            self.scope_packages.insert(dst_pkg);
            self.scopes.push(ScopeContext {
                parent: sc.parent.map(|p| p + scope_base),
                package: dst_pkg,
                assoc_type: sc.assoc_type.map(|t| type_map[t.as_u32() as usize]),
            });
        }

        // Pass 5: Merge `TraitDispatchSchema`s, `AssociatedTypeDefault`s, and `AssociatedTypeBinding`s.
        for schema in &src_snap.trait_schemas {
            let dst_trait = type_map[schema.trait_type.as_u32() as usize];
            if self.trait_schemas.iter().any(|s| s.trait_type == dst_trait) {
                continue;
            }
            let mut slots = Vec::with_capacity(schema.slots.len());
            for slot in &schema.slots {
                slots.push(TraitMethodKey {
                    trait_owner: type_map[slot.trait_owner.as_u32() as usize],
                    name: slot.name,
                    signature: slot
                        .signature
                        .as_ref()
                        .map(|sig| remap_trait_sig(sig, &type_map))
                        .transpose()?,
                });
            }
            self.trait_schemas.push(TraitDispatchSchema {
                trait_type: dst_trait,
                slots,
            });
        }

        for def in &src_snap.associated_defaults {
            let dst_owner = type_map[def.trait_owner.as_u32() as usize];
            if self
                .associated_defaults
                .iter()
                .any(|d| d.trait_owner == dst_owner && d.name == def.name)
            {
                continue;
            }
            self.associated_defaults.push(AssociatedTypeDefault {
                trait_owner: dst_owner,
                name: def.name,
                expression: remap_assoc_expr(&def.expression, &type_map)?,
            });
        }

        // Note: `AssociatedTypeBinding`s are installed atomically alongside their exact
        // `TraitImplRecord` in `install_merged_dispatch` so `TypePool::validate()` holds
        // both before any modules load and after every incremental module load!
        let finalized_input = self
            .identity_input
            .take()
            .expect("initialized identity_input");
        self.finalize_type_identities(finalized_input)
            .map_err(|e| SnapshotError::new(e.to_string()))?;
        self.validate()?;

        Ok(MergedPoolMapping {
            type_map,
            scope_base,
            scope_pkg_map,
        })
    }

    /// Incrementally publish [`MethodSlot`]s, [`TraitImplRecord`]s, and [`VTable`]s from `src`
    /// whose target `func_id`s are now resolved in `func_map` (as modules load on demand).
    pub fn install_merged_dispatch(
        &mut self,
        src: &TypePool,
        mapping: &MergedPoolMapping,
        func_map: &HashMap<u32, u32>,
        state: &mut MergedDispatchState,
    ) -> Result<bool, SnapshotError> {
        let src_snap = src.snapshot();
        let mut changed = false;

        let remap_slot =
            |slot: &MethodSlot, dst_func_id: u32| -> Result<MethodSlot, SnapshotError> {
                Ok(MethodSlot {
                    access: mapping.map_method_access(slot.access)?,
                    name: slot.name,
                    func_id: dst_func_id,
                    trait_impl: slot
                        .trait_impl
                        .map(|t| mapping.type_map[t.as_u32() as usize]),
                    visible_scope: slot.visible_scope.map(|s| mapping.map_scope(s)),
                })
            };

        for (src_ty_idx, slots) in src_snap.methods.iter().enumerate() {
            let dst_ty = mapping.type_map[src_ty_idx];
            for (slot_idx, slot) in slots.iter().enumerate() {
                if state.installed_methods.contains(&(src_ty_idx, slot_idx)) {
                    continue;
                }
                let Some(&dst_fid) = func_map.get(&slot.func_id) else {
                    continue;
                };
                state.installed_methods.insert((src_ty_idx, slot_idx));
                let remapped = remap_slot(slot, dst_fid)?;
                if remapped.visible_scope.is_none()
                    && matches!(remapped.access, MethodAccess::Public)
                    && self.methods[dst_ty.as_u32() as usize]
                        .iter()
                        .any(|existing| {
                            existing.visible_scope.is_none()
                                && existing.name == remapped.name
                                && existing.trait_impl == remapped.trait_impl
                        })
                {
                    continue;
                }
                self.methods[dst_ty.as_u32() as usize].push(remapped);
                changed = true;
            }
        }

        for (impl_idx, record) in src_snap.trait_impls.iter().enumerate() {
            if state.installed_trait_impls.contains(&impl_idx) {
                continue;
            }
            if !record
                .methods
                .iter()
                .all(|m| func_map.contains_key(&m.func_id))
            {
                continue;
            }
            state.installed_trait_impls.insert(impl_idx);
            let dst_trait = mapping.type_map[record.trait_type.as_u32() as usize];
            let dst_impl = mapping.type_map[record.implementor.as_u32() as usize];
            let dst_vis = record.visible_scope.map(|s| mapping.map_scope(s));
            let mut methods = Vec::with_capacity(record.methods.len());
            for m in &record.methods {
                methods.push(remap_slot(m, func_map[&m.func_id])?);
            }
            if dst_vis.is_none()
                && let Some(existing) = self.trait_impls.iter_mut().find(|existing| {
                    existing.visible_scope.is_none()
                        && existing.trait_type == dst_trait
                        && existing.implementor == dst_impl
                })
            {
                for m in methods {
                    if !existing.methods.iter().any(|em| em.name == m.name) {
                        existing.methods.push(m);
                        changed = true;
                    }
                }
                continue;
            }
            self.trait_impls.push(TraitImplRecord {
                visible_scope: dst_vis,
                trait_type: dst_trait,
                implementor: dst_impl,
                methods,
            });
            for bind in &src_snap.associated_bindings {
                if bind.implementor != record.implementor
                    || bind.trait_type != record.trait_type
                    || bind.visible_scope != record.visible_scope
                {
                    continue;
                }
                let dst_owner = mapping.type_map[bind.trait_owner.as_u32() as usize];
                let dst_val = mapping.type_map[bind.value.as_u32() as usize];
                if dst_vis.is_none()
                    && self.associated_bindings.iter().any(|b| {
                        b.visible_scope.is_none()
                            && b.implementor == dst_impl
                            && b.trait_type == dst_trait
                            && b.trait_owner == dst_owner
                            && b.name == bind.name
                    })
                {
                    continue;
                }
                self.associated_bindings.push(AssociatedTypeBinding {
                    implementor: dst_impl,
                    trait_type: dst_trait,
                    visible_scope: dst_vis,
                    trait_owner: dst_owner,
                    name: bind.name,
                    value: dst_val,
                });
            }
            changed = true;
        }

        for (vt_idx, table) in src_snap.vtables.iter().enumerate() {
            if state.installed_vtables.contains(&vt_idx) {
                continue;
            }
            if !table.entries.iter().all(|fid| func_map.contains_key(fid)) {
                continue;
            }
            state.installed_vtables.insert(vt_idx);
            let dst_trait = mapping.type_map[table.trait_type.as_u32() as usize];
            let dst_impl = mapping.type_map[table.implementor.as_u32() as usize];
            let dst_vis = table.visible_scope.map(|s| mapping.map_scope(s));
            if dst_vis.is_none()
                && self.vtables.iter().any(|existing| {
                    existing.visible_scope.is_none()
                        && existing.trait_type == dst_trait
                        && existing.implementor == dst_impl
                })
            {
                continue;
            }
            let entries = table.entries.iter().map(|fid| func_map[fid]).collect();
            self.vtables.push(VTable {
                visible_scope: dst_vis,
                trait_type: dst_trait,
                implementor: dst_impl,
                entries,
            });
            changed = true;
        }

        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IdentityPathSegment, Intrinsic, PackageTypeContext};

    fn make_pkg_pool(
        qname: &str,
        id_byte: u8,
        struct_name: &str,
        shared_from: Option<&TypePool>,
    ) -> (TypePool, TypeIndex) {
        let mut pool = TypePool::with_intrinsics();
        pool.install_scopes(vec![ScopeContext {
            parent: None,
            package: 0,
            assoc_type: None,
        }])
        .unwrap();
        let mut packages = vec![PackageTypeContext {
            identity_schema: 1,
            identity: [id_byte; 16],
            qualified_name: qname.into(),
            version: "0.1.0".into(),
        }];
        let mut declarations = Vec::new();

        let field_ty = if let Some(dep) = shared_from {
            let dep_snap = dep.snapshot();
            let dep_input = dep_snap.identity_input.as_ref().unwrap();
            packages.push(dep_input.packages[0].clone());
            let dep_decl = &dep_input.declarations[0];
            let imported_ty =
                pool.register(dep_snap.types[dep_decl.type_index.as_u32() as usize].clone());
            declarations.push(NominalTypeProvenance {
                type_index: imported_ty,
                package: 1,
                path: dep_decl.path.clone(),
                last_stable_version: dep_decl.last_stable_version.clone(),
            });
            imported_ty
        } else {
            Intrinsic::I64.type_index()
        };

        let s_ty = pool.register(TypeInfo {
            kind: TypeKind::Struct {
                name: str_interner::intern(struct_name),
                fields: vec![FieldInfo {
                    name: str_interner::intern("val"),
                    ty: field_ty,
                    has_default: false,
                    offset: 0,
                }],
            },
            type_id: TypeId::ZERO,
            size: 8,
            align: 8,
        });
        declarations.push(NominalTypeProvenance {
            type_index: s_ty,
            package: 0,
            path: vec![IdentityPathSegment::Named(struct_name.into())],
            last_stable_version: "0.1.0".into(),
        });
        pool.add_method(
            s_ty,
            MethodSlot {
                access: MethodAccess::Package(0),
                name: str_interner::intern("pkg_only"),
                func_id: 0,
                trait_impl: None,
                visible_scope: None,
            },
        );
        pool.finalize_type_identities(TypeIdentityInput {
            schema: 1,
            packages,
            declarations,
        })
        .unwrap();
        (pool, s_ty)
    }

    #[test]
    fn merges_multiple_packages_deduplicating_nominals_and_isolating_package_visibility() {
        let (pool_a, a_ty) = make_pkg_pool("org.demo/pkg_a", 0xAA, "SharedBox", None);
        let (pool_b, b_ty) = make_pkg_pool("org.demo/pkg_b", 0xBB, "Consumer", Some(&pool_a));

        let mut vm_pool = TypePool::with_intrinsics();
        let map_a = vm_pool.merge_types_and_scopes(&pool_a).unwrap();
        let map_b = vm_pool.merge_types_and_scopes(&pool_b).unwrap();

        // `SharedBox` in `pool_a` and `pool_b` must resolve to the EXACT same `TypeIndex` in `vm_pool`!
        let dst_shared_from_a = map_a.map_type(a_ty).unwrap();
        let TypeKind::Struct { fields, .. } = &vm_pool.get(map_b.map_type(b_ty).unwrap()).kind
        else {
            panic!("expected Consumer struct");
        };
        assert_eq!(fields[0].ty, dst_shared_from_a);
        assert_eq!(
            vm_pool.stable_type_id(dst_shared_from_a).unwrap(),
            pool_a.stable_type_id(a_ty).unwrap()
        );

        // Install dispatch for `pkg_a` and verify `pkg_b`'s scope CANNOT access `pkg_a`'s `Package(0)` method!
        let mut state_a = MergedDispatchState::default();
        let fmap_a = HashMap::from([(0u32, 10u32)]);
        assert!(
            vm_pool
                .install_merged_dispatch(&pool_a, &map_a, &fmap_a, &mut state_a)
                .unwrap()
        );
        let slot = vm_pool
            .find_method(dst_shared_from_a, str_interner::intern("pkg_only"))
            .unwrap();
        assert!(vm_pool.method_accessible(slot, map_a.map_scope(0)).unwrap());
        assert!(!vm_pool.method_accessible(slot, map_b.map_scope(0)).unwrap());
        vm_pool.validate().unwrap();
    }
}
