//! Trusted contracts for the legacy native derived-method entry point.

use crate::{
    Intrinsic, TraitMethodKey, TraitParameterKind, TraitTypeStep, TypeIndex, TypeKind, TypePool,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeDerivedMethod {
    Equality,
    Display,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeDerivedError(&'static str);

impl std::fmt::Display for NativeDerivedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for NativeDerivedError {}

impl TypePool {
    /// Check the exact trait ownership of a selected sentinel slot. `None`
    /// denotes legacy metadata without a signed interface.
    pub fn check_native_derived_slot(
        &self,
        implementor: TypeIndex,
        method: &crate::MethodSlot,
    ) -> Result<Option<NativeDerivedMethod>, NativeDerivedError> {
        let Some(trait_type) = method.trait_impl else {
            return Ok(None);
        };
        let Some(schema) = self.trait_schema(trait_type) else {
            return Ok(None);
        };
        let Some(key) = schema.slots.iter().find(|key| key.name == method.name) else {
            return Err(NativeDerivedError("native method has no interface slot"));
        };
        if key.signature.is_none() {
            return Ok(None);
        }
        if self.canonical_type(trait_type) != self.canonical_type(key.trait_owner) {
            return Err(NativeDerivedError(
                "native method has an untrusted implementation trait",
            ));
        }
        self.check_native_derived_method(key, implementor).map(Some)
    }

    /// Validate a signed interface before using the legacy native sentinel.
    /// Naming a custom trait `Eq` does not grant access to this entry point.
    pub fn check_native_derived_method(
        &self,
        key: &TraitMethodKey,
        implementor: TypeIndex,
    ) -> Result<NativeDerivedMethod, NativeDerivedError> {
        let fail = NativeDerivedError;
        let owner = self
            .canonical_type(key.trait_owner)
            .ok_or(fail("invalid native trait"))?;
        if !matches!(self.get(owner).kind, TypeKind::Trait { .. }) {
            return Err(fail("native owner is not a trait"));
        }
        let name = str_interner::try_get(key.name).ok_or(fail("invalid native method name"))?;
        let (method, count, result) =
            if (owner == self.well_known.eq || owner == self.well_known.partial_eq) && name == "eq"
            {
                (NativeDerivedMethod::Equality, 2, Intrinsic::Bool)
            } else if owner == self.well_known.display && name == "to_string" {
                (NativeDerivedMethod::Display, 1, Intrinsic::Str)
            } else {
                return Err(fail("unsupported native derived interface"));
            };
        // The current artifact contract reserves the bootstrap trait prefix
        // immediately after intrinsics. This authenticates only native privilege,
        // not general trait or collection identity.
        // The mutable well-known catalog alone cannot authenticate a trait:
        // host metadata could redirect it to a user trait with the same name.
        let ordinal = match (method, owner == self.well_known.partial_eq) {
            (NativeDerivedMethod::Equality, false) => 2,
            (NativeDerivedMethod::Equality, true) => 4,
            (NativeDerivedMethod::Display, _) => 0,
        };
        if owner.as_u32() as usize != Intrinsic::COUNT + ordinal
            || self.get(owner).type_id.0 != 0x4E45_5353_5452_4954
            || (self.identity_input().is_some() && !self.is_reserved_type(owner))
        {
            return Err(fail("untrusted native trait identity"));
        }
        let signature = key
            .signature
            .as_ref()
            .ok_or(fail("native interface has no signature"))?;
        let declaration = self
            .canonical_type(signature.declaration)
            .ok_or(fail("invalid native signature"))?;
        let TypeKind::Function { params, ret } = &self.get(declaration).kind else {
            return Err(fail("native signature is not a function"));
        };
        let expected_paths: Vec<_> = (0..count)
            .map(|i| vec![TraitTypeStep::Parameter(i as u32)])
            .collect();
        let expected_kinds: Vec<_> = (0..count)
            .map(|i| {
                if i == 0 {
                    TraitParameterKind::Receiver
                } else {
                    TraitParameterKind::Required
                }
            })
            .collect();
        if params.len() != count
            || params
                .iter()
                .any(|&ty| self.canonical_type(ty) != Some(owner))
            || self.canonical_type(*ret) != Some(result.type_index())
            || signature.self_paths != expected_paths
            || signature.parameter_kinds != expected_kinds
        {
            return Err(fail(
                "native derived signature does not match its trusted contract",
            ));
        }
        let implementor = self
            .canonical_type(implementor)
            .ok_or(fail("invalid native receiver"))?;
        if self
            .checked_collection_role(implementor)
            .map_err(|_| fail("invalid native receiver layout"))?
            .is_some()
        {
            return Err(fail(
                "collection receivers cannot use native derived methods",
            ));
        }
        let info = self.get(implementor);
        let TypeKind::Struct { fields, .. } = &info.kind else {
            return Err(fail("native derived receiver is not a struct"));
        };
        if fields.len() > u16::MAX as usize
            || info.align != 8
            || usize::try_from(info.size).ok() != fields.len().checked_mul(8)
            || fields.iter().enumerate().any(|(i, field)| {
                usize::try_from(field.offset).ok() != i.checked_mul(8)
                    || self.canonical_type(field.ty).is_none()
            })
        {
            return Err(fail("invalid native struct layout"));
        }
        Ok(method)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FieldInfo, TraitMethodSignature, TypeId, TypeInfo};

    fn fixture() -> (TypePool, TypeIndex, TraitMethodKey) {
        let mut pool = TypePool::with_intrinsics();
        let owner = pool.well_known.eq;
        let declaration = pool.intern_structural(TypeKind::Function {
            params: vec![owner, owner],
            ret: Intrinsic::Bool.type_index(),
        });
        let key = TraitMethodKey {
            trait_owner: owner,
            name: str_interner::intern("eq"),
            signature: Some(TraitMethodSignature {
                associated_paths: Vec::new(),
                declaration,
                self_paths: vec![
                    vec![TraitTypeStep::Parameter(0)],
                    vec![TraitTypeStep::Parameter(1)],
                ],
                parameter_kinds: vec![TraitParameterKind::Receiver, TraitParameterKind::Required],
            }),
        };
        let receiver = pool.register(TypeInfo {
            kind: TypeKind::Struct {
                name: str_interner::intern("NativeReceiver"),
                fields: vec![FieldInfo {
                    name: str_interner::intern("n"),
                    ty: Intrinsic::I64.type_index(),
                    has_default: false,
                    offset: 0,
                }],
            },
            type_id: TypeId(917, 1),
            size: 8,
            align: 8,
        });
        (pool, receiver, key)
    }

    #[test]
    fn trusted_native_contract_rejects_trait_swaps_and_signature_forgery() {
        let (mut pool, receiver, key) = fixture();
        assert_eq!(
            pool.check_native_derived_method(&key, receiver),
            Ok(NativeDerivedMethod::Equality)
        );
        let fake = pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("Eq"),
                parents: vec![],
                assoc_types: vec![],
            },
            type_id: TypeId(0x4E45_5353_5452_4954, 917),
            size: 0,
            align: 0,
        });
        let mut bad = key.clone();
        bad.trait_owner = fake;
        assert!(pool.check_native_derived_method(&bad, receiver).is_err());
        let old_eq = pool.well_known.eq;
        pool.well_known.eq = fake;
        let mut swapped = bad.clone();
        swapped.signature.as_mut().unwrap().declaration =
            pool.intern_structural(TypeKind::Function {
                params: vec![fake, fake],
                ret: Intrinsic::Bool.type_index(),
            });
        assert!(
            pool.check_native_derived_method(&swapped, receiver)
                .is_err()
        );
        pool.well_known.eq = old_eq;
        bad.trait_owner = pool.well_known.display;
        assert!(pool.check_native_derived_method(&bad, receiver).is_err());
        bad = key.clone();
        bad.signature.as_mut().unwrap().self_paths.pop();
        assert!(pool.check_native_derived_method(&bad, receiver).is_err());
        bad = key.clone();
        bad.signature.as_mut().unwrap().declaration = pool.intern_structural(TypeKind::Function {
            params: vec![key.trait_owner; 2],
            ret: Intrinsic::I64.type_index(),
        });
        assert!(pool.check_native_derived_method(&bad, receiver).is_err());
        bad = key.clone();
        bad.signature.as_mut().unwrap().parameter_kinds[1] = TraitParameterKind::Optional;
        assert!(pool.check_native_derived_method(&bad, receiver).is_err());
    }

    #[test]
    fn trusted_native_contract_rejects_non_structs_reserved_roles_and_bad_layouts() {
        let (mut pool, receiver, key) = fixture();
        assert!(
            pool.check_native_derived_method(&key, Intrinsic::I64.type_index())
                .is_err()
        );
        assert!(
            pool.check_native_derived_method(&key, pool.list_type().unwrap())
                .is_err()
        );
        pool.get_mut(receiver).size = 16;
        assert!(pool.check_native_derived_method(&key, receiver).is_err());
        pool.get_mut(receiver).size = 8;
        if let TypeKind::Struct { fields, .. } = &mut pool.get_mut(receiver).kind {
            fields[0].offset = 8;
        }
        assert!(pool.check_native_derived_method(&key, receiver).is_err());
    }
}
