//! Concrete tagged iteration results. Nominal enums never acquire this identity by name.

use crate::{
    FieldInfo, SnapshotError, TypeId, TypeIndex, TypeInfo, TypeKind, TypePool, VariantInfo,
};

pub const ITERATION_DONE_TAG: u32 = 0;
pub const ITERATION_YIELDED_TAG: u32 = 1;

pub(crate) fn step_payload(kind: &TypeKind) -> Option<TypeIndex> {
    let TypeKind::Enum { name, variants } = kind else {
        return None;
    };
    let [done, yielded] = variants.as_slice() else {
        return None;
    };
    let [value] = yielded.fields.as_slice() else {
        return None;
    };
    (str_interner::try_get(*name).as_deref() == Some("IterationStep")
        && str_interner::try_get(done.name).as_deref() == Some("done")
        && done.tag == ITERATION_DONE_TAG
        && done.fields.is_empty()
        && str_interner::try_get(yielded.name).as_deref() == Some("yielded")
        && yielded.tag == ITERATION_YIELDED_TAG
        && str_interner::try_get(value.name).as_deref() == Some("value")
        && value.offset == 0
        && !value.has_default)
        .then_some(value.ty)
}

impl TypePool {
    /// Build compiler-only metadata; callers must preserve source binding provenance.
    /// A concrete Item must use the concrete constructor instead.
    pub fn intern_iteration_step_template(
        &mut self,
        item: TypeIndex,
    ) -> Result<TypeIndex, SnapshotError> {
        let item = self
            .canonical_type(item)
            .filter(|&item| !self.is_static_associated_type(item))
            .ok_or_else(|| {
                SnapshotError::new("IterationStep template requires an abstract Item")
            })?;
        Ok(self.intern_structural(TypeKind::IterationStepTemplate { item }))
    }
    /// Intern a concrete result type with a checked, canonical Item payload.
    /// Symbolic associated types and trait carriers require specialization first.
    pub fn intern_iteration_step(&mut self, item: TypeIndex) -> Result<TypeIndex, SnapshotError> {
        let item = self
            .canonical_type(item)
            .filter(|&ty| self.is_static_associated_type(ty))
            .ok_or_else(|| SnapshotError::new("IterationStep requires a concrete Item"))?;
        for &ty in &self.structural_types {
            if matches!(self.get(ty).kind, TypeKind::Enum { .. })
                && self.validate_iteration_step_item(ty)? == item
            {
                return Ok(ty);
            }
        }
        let ty = self.register(TypeInfo {
            kind: TypeKind::Enum {
                name: str_interner::intern("IterationStep"),
                variants: vec![
                    VariantInfo {
                        name: str_interner::intern("done"),
                        tag: ITERATION_DONE_TAG,
                        fields: vec![],
                    },
                    VariantInfo {
                        name: str_interner::intern("yielded"),
                        tag: ITERATION_YIELDED_TAG,
                        fields: vec![FieldInfo {
                            name: str_interner::intern("value"),
                            ty: item,
                            offset: 0,
                            has_default: false,
                        }],
                    },
                ],
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        self.structural_types.push(ty);
        Ok(ty)
    }

    /// Return the Item of an explicitly interned result; ordinary enums return None.
    /// Invalid indices or malformed structural result descriptors return an error.
    pub fn checked_iteration_step_item(
        &self,
        ty: TypeIndex,
    ) -> Result<Option<TypeIndex>, SnapshotError> {
        let ty = self
            .canonical_type(ty)
            .ok_or_else(|| SnapshotError::new("invalid IterationStep index"))?;
        let info = self.get(ty);
        if !matches!(info.kind, TypeKind::Enum { .. }) || !self.structural_types.contains(&ty) {
            return Ok(None);
        }
        self.validate_iteration_step_item(ty).map(Some)
    }

    // Callers have already established structural provenance. Avoid rescanning
    // the provenance list once per descriptor during archive validation.
    pub(crate) fn validate_iteration_step_item(
        &self,
        ty: TypeIndex,
    ) -> Result<TypeIndex, SnapshotError> {
        let info = self.get(ty);
        let item = step_payload(&info.kind)
            .and_then(|item| self.canonical_type(item))
            .filter(|&item| self.is_static_associated_type(item))
            .filter(|_| {
                (info.type_id == TypeId::ZERO || self.identity_input.is_some())
                    && info.size == 0
                    && info.align == 0
            })
            .ok_or_else(|| SnapshotError::new("invalid structural IterationStep descriptor"))?;
        Ok(item)
    }

    /// Whether the pool carries result-type provenance requiring TPOL8.
    pub fn has_iteration_steps(&self) -> bool {
        self.structural_types.iter().any(|ty| {
            self.types
                .get(ty.as_u32() as usize)
                .is_some_and(|info| matches!(info.kind, TypeKind::Enum { .. }))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Intrinsic;

    #[test]
    fn compiler_templates_require_abstract_items_and_cannot_be_malformed() {
        let mut pool = TypePool::with_intrinsics();
        let owner = pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("TemplateOwner"),
                parents: vec![],
                assoc_types: vec![],
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let template = pool.intern_iteration_step_template(owner).unwrap();
        assert!(pool.contains_associated_type(template));
        assert!(!pool.is_static_associated_type(template));
        assert_eq!(
            pool.intern_iteration_step_template(owner).unwrap(),
            template
        );
        assert!(
            pool.intern_iteration_step_template(Intrinsic::I64.type_index())
                .is_err()
        );
        TypePool::restore(pool.snapshot()).unwrap();
        for corruption in 0..6 {
            let mut snapshot = pool.snapshot();
            let info = &mut snapshot.types[template.as_u32() as usize];
            match corruption {
                0 => {
                    info.kind = TypeKind::IterationStepTemplate {
                        item: Intrinsic::I64.type_index(),
                    }
                }
                1 => {
                    info.kind = TypeKind::IterationStepTemplate {
                        item: TypeIndex::INVALID,
                    }
                }
                2 => info.kind = TypeKind::IterationStepTemplate { item: template },
                3 => info.type_id = TypeId(123, 456),
                4 => info.size = 8,
                _ => snapshot.structural_types.retain(|&ty| ty != template),
            }
            assert!(
                TypePool::restore(snapshot).is_err(),
                "corruption {corruption}"
            );
        }
    }

    #[test]
    fn iteration_step_identity_is_precise_and_restored() {
        let mut pool = TypePool::with_intrinsics();
        let item = Intrinsic::I64.type_index();
        let step = pool.intern_iteration_step(item).unwrap();
        let alias = pool.register(TypeInfo {
            kind: TypeKind::Typealias {
                name: str_interner::intern("ItemAlias"),
                target: item,
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        assert_eq!(pool.intern_iteration_step(alias).unwrap(), step);
        assert_ne!(
            pool.intern_iteration_step(Intrinsic::Str.type_index())
                .unwrap(),
            step
        );
        let nominal = pool.register(pool.get(step).clone());
        assert_ne!(nominal, step);
        assert_eq!(pool.checked_iteration_step_item(nominal).unwrap(), None);
        assert_eq!(
            pool.display_name(step).as_deref(),
            Some("IterationStep(i64)")
        );
        let mut restored = TypePool::restore(pool.snapshot()).unwrap();
        assert_eq!(restored.intern_iteration_step(alias).unwrap(), step);
        assert_eq!(
            restored.checked_iteration_step_item(step).unwrap(),
            Some(item)
        );
        assert!(pool.intern_iteration_step(TypeIndex::INVALID).is_err());
        let trait_type = pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("ItemTrait"),
                parents: vec![],
                assoc_types: vec![],
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        assert!(pool.intern_iteration_step(trait_type).is_err());
    }

    #[test]
    fn malformed_and_duplicate_iteration_steps_are_rejected() {
        let mut pool = TypePool::with_intrinsics();
        let step = pool
            .intern_iteration_step(Intrinsic::I64.type_index())
            .unwrap();
        for corruption in 0..6 {
            let mut snapshot = pool.snapshot();
            let info = &mut snapshot.types[step.as_u32() as usize];
            let TypeKind::Enum { name, variants } = &mut info.kind else {
                panic!()
            };
            match corruption {
                0 => *name = str_interner::intern("Ordinary"),
                1 => variants[1].tag = 2,
                2 => variants[1].fields[0].offset = 8,
                3 => variants[1].fields[0].name = str_interner::intern("other"),
                4 => info.size = 16,
                _ => info.type_id = TypeId(123, 456),
            }
            assert!(
                TypePool::restore(snapshot).is_err(),
                "corruption {corruption}"
            );
        }
        let mut snapshot = pool.snapshot();
        let TypeKind::Enum { variants, .. } = &mut snapshot.types[step.as_u32() as usize].kind
        else {
            panic!()
        };
        variants[1].fields[0].has_default = true;
        assert!(TypePool::restore(snapshot).is_err());
        let mut snapshot = pool.snapshot();
        let duplicate = TypeIndex::from_raw(snapshot.types.len() as u32);
        snapshot
            .types
            .push(snapshot.types[step.as_u32() as usize].clone());
        snapshot.methods.push(vec![]);
        snapshot.structural_types.push(duplicate);
        assert!(TypePool::restore(snapshot).is_err());
    }
}
