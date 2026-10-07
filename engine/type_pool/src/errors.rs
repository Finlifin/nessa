//! Checked source normalization. Legacy descriptors and their identities stay intact.
use crate::{Intrinsic, TypeId, TypeIndex, TypeInfo, TypeKind, TypePool};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorDomain {
    Closed(Vec<TypeIndex>),
    /// Internal dynamic error domain. It is never a concrete payload tag.
    Open,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorShape {
    pub inner: TypeIndex,
    pub domain: ErrorDomain,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorTypeError {
    InvalidType(TypeIndex),
    ExcessiveShape,
    CyclicQualifier(TypeIndex),
}
impl std::fmt::Display for ErrorTypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExcessiveShape => {
                write!(f, "Error shape exceeds 262144 members or qualifier steps")
            }
            Self::InvalidType(ty) => write!(f, "invalid or cyclic type reference {ty:?}"),
            Self::CyclicQualifier(ty) => write!(f, "cyclic Error qualifier {ty:?}"),
        }
    }
}
impl std::error::Error for ErrorTypeError {}
impl TypePool {
    /// Read a checked alias-canonical shape without changing the stored graph.
    pub fn error_shape(&self, ty: TypeIndex) -> Result<Option<ErrorShape>, ErrorTypeError> {
        let ty = self
            .canonical_type(ty)
            .ok_or(ErrorTypeError::InvalidType(ty))?;
        let TypeKind::ErrorQualified { errors, inner } = &self.get(ty).kind else {
            return Ok(None);
        };
        self.flatten_error_shape(errors.clone(), *inner).map(Some)
    }
    fn flatten_error_shape(
        &self,
        mut errors: Vec<TypeIndex>,
        mut inner: TypeIndex,
    ) -> Result<ErrorShape, ErrorTypeError> {
        const LIMIT: usize = 262144;
        if errors.len() > LIMIT {
            return Err(ErrorTypeError::ExcessiveShape);
        }
        let mut visited = std::collections::HashSet::new();
        loop {
            if visited.len() >= LIMIT {
                return Err(ErrorTypeError::ExcessiveShape);
            }
            inner = self
                .canonical_type(inner)
                .ok_or(ErrorTypeError::InvalidType(inner))?;
            if !visited.insert(inner) {
                return Err(ErrorTypeError::CyclicQualifier(inner));
            }
            match &self.get(inner).kind {
                TypeKind::ErrorQualified {
                    errors: nested,
                    inner: next,
                } => {
                    if nested.len() > LIMIT - errors.len() {
                        return Err(ErrorTypeError::ExcessiveShape);
                    }
                    errors.extend(nested.iter().copied());
                    inner = *next;
                }
                _ => break,
            }
        }
        for error in &mut errors {
            *error = self
                .canonical_type(*error)
                .ok_or(ErrorTypeError::InvalidType(*error))?;
        }
        let domain = if errors.contains(&self.intrinsic(Intrinsic::Any)) {
            ErrorDomain::Open
        } else {
            errors.sort_unstable_by_key(|ty| ty.as_u32());
            errors.dedup();
            ErrorDomain::Closed(errors)
        };
        Ok(ErrorShape { inner, domain })
    }
    /// Construct a new executable source shape; never reuse a legacy size-zero graph.
    pub fn normalize_error_type(
        &mut self,
        errors: Vec<TypeIndex>,
        inner: TypeIndex,
    ) -> Result<TypeIndex, ErrorTypeError> {
        let shape = self.flatten_error_shape(errors, inner)?;
        let errors = match shape.domain {
            ErrorDomain::Closed(errors) if errors.is_empty() => return Ok(shape.inner),
            ErrorDomain::Closed(errors) => errors,
            ErrorDomain::Open => vec![self.intrinsic(Intrinsic::Any)],
        };
        let kind = TypeKind::ErrorQualified {
            errors,
            inner: shape.inner,
        };
        if let Some(index) = self.structural_types.iter().copied().find(|index| {
            let info = self.get(*index);
            info.size == 24 && info.align == 8 && crate::same_structural_shape(&info.kind, &kind)
        }) {
            return Ok(index);
        }
        let index = self.register(TypeInfo {
            kind,
            type_id: TypeId::ZERO,
            size: 24,
            align: 8,
        });
        self.structural_types.push(index);
        Ok(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalized_executable_shape_preserves_legacy_layout_and_provenance() {
        let mut pool = TypePool::with_intrinsics();
        let errors = vec![Intrinsic::Str.type_index()];
        let inner = Intrinsic::I64.type_index();
        let legacy = pool.intern_structural(TypeKind::ErrorQualified {
            errors: errors.clone(),
            inner,
        });
        let source = pool.normalize_error_type(errors, inner).unwrap();
        assert_ne!(legacy, source);
        assert_eq!((pool.get(legacy).size, pool.get(legacy).align), (0, 0));
        assert_eq!((pool.get(source).size, pool.get(source).align), (24, 8));
        let restored = TypePool::restore(pool.snapshot()).unwrap();
        assert_eq!(restored.get(legacy).size, 0);
        assert_eq!(restored.get(source).size, 24);
    }
    #[test]
    fn checked_constructor_flattens_deduplicates_and_erases_empty() {
        let mut pool = TypePool::with_intrinsics();
        let e = Intrinsic::Str.type_index();
        let f = Intrinsic::Bool.type_index();
        let inner = Intrinsic::I64.type_index();
        assert_eq!(pool.normalize_error_type(vec![], inner).unwrap(), inner);
        let a = pool.normalize_error_type(vec![e, e], inner).unwrap();
        let b = pool.normalize_error_type(vec![f, e], a).unwrap();
        let c = pool.normalize_error_type(vec![e, f], inner).unwrap();
        assert_eq!(b, c);
        assert!(
            matches!(pool.error_shape(b).unwrap().unwrap().domain,ErrorDomain::Closed(ref members) if members.len()==2)
        );
    }
    #[test]
    fn invalid_reference_cycles_and_budget_are_checked() {
        let mut pool = TypePool::with_intrinsics();
        assert!(
            pool.normalize_error_type(vec![TypeIndex::INVALID], Intrinsic::I64.type_index())
                .is_err()
        );
        assert!(matches!(
            pool.normalize_error_type(
                vec![Intrinsic::Str.type_index(); 262145],
                Intrinsic::I64.type_index()
            ),
            Err(ErrorTypeError::ExcessiveShape)
        ));
        let cycle = pool.intern_structural(TypeKind::ErrorQualified {
            errors: vec![],
            inner: Intrinsic::I64.type_index(),
        });
        pool.get_mut(cycle).kind = TypeKind::ErrorQualified {
            errors: vec![],
            inner: cycle,
        };
        assert!(matches!(
            pool.error_shape(cycle),
            Err(ErrorTypeError::CyclicQualifier(_))
        ));
    }
    #[test]
    fn qualified_any_inner_does_not_bypass_restricted_errors() {
        let mut pool = TypePool::with_intrinsics();
        let inner = Intrinsic::Any.type_index();
        let a = pool
            .normalize_error_type(vec![Intrinsic::Str.type_index()], inner)
            .unwrap();
        let b = pool
            .normalize_error_type(vec![Intrinsic::Bool.type_index()], inner)
            .unwrap();
        assert!(!pool.is_subtype(a, b));
        assert!(!pool.is_subtype(b, a));
        assert!(pool.is_subtype(Intrinsic::Str.type_index(), a));
    }
    #[test]
    fn empty_member_qualifier_chain_has_the_same_checked_step_budget() {
        let mut pool = TypePool::with_intrinsics();
        let mut inner = Intrinsic::I64.type_index();
        for _ in 0..262145 {
            inner = pool.register(TypeInfo {
                kind: TypeKind::ErrorQualified {
                    errors: Vec::new(),
                    inner,
                },
                type_id: TypeId::ZERO,
                size: 0,
                align: 0,
            });
        }
        assert!(matches!(
            pool.error_shape(inner),
            Err(ErrorTypeError::ExcessiveShape)
        ));
    }
}
