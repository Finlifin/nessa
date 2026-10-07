//! Relocate compiler method identities before publishing executable metadata.

use std::collections::HashMap;

use str_interner::StrId;

use crate::{DERIVE_FUNC_ID, TypeIndex, TypeKind, TypePool};

/// A generated method plan could not be published consistently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedMethodError(String);

impl std::fmt::Display for DerivedMethodError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for DerivedMethodError {}

impl TypePool {
    /// Publish generated bodies for compiler-derived methods atomically.
    /// Alias owners are canonicalized; ordinary method identities and omitted
    /// legacy derived sentinels remain unchanged. Vtables without method names
    /// must identify the derived slot uniquely, otherwise publication fails.
    ///
    /// # Errors
    /// Invalid, duplicate, missing or ambiguous method plans return an error
    /// without changing any dispatch table.
    pub fn resolve_derived_methods(
        &mut self,
        mapping: impl IntoIterator<Item = (TypeIndex, TypeIndex, StrId, u32)>,
    ) -> Result<(), DerivedMethodError> {
        let fail = |message: &str| DerivedMethodError(message.to_owned());
        let mut plans = HashMap::new();
        for (implementor, trait_type, name, function) in mapping {
            let implementor = self
                .canonical_type(implementor)
                .ok_or_else(|| fail("derived method has an invalid implementor"))?;
            let trait_type = self
                .canonical_type(trait_type)
                .filter(|&ty| matches!(self.get(ty).kind, TypeKind::Trait { .. }))
                .ok_or_else(|| fail("derived method has an invalid trait"))?;
            if function >= DERIVE_FUNC_ID || str_interner::try_get(name).is_none() {
                return Err(fail("derived method has an invalid function or name"));
            }
            if plans
                .insert((implementor, trait_type, name), function)
                .is_some()
            {
                return Err(fail("duplicate generated derived method"));
            }
        }
        let mut method_updates = Vec::new();
        let mut impl_updates = Vec::new();
        let mut table_updates = Vec::new();
        for (&(owner, trait_type, name), &function) in &plans {
            let fail = |message: &str| {
                DerivedMethodError(format!(
                    "derived method {} on type {} for trait {}: {message}",
                    str_interner::try_get(name).unwrap_or_else(|| name.as_u32().to_string()),
                    owner.as_u32(),
                    trait_type.as_u32(),
                ))
            };
            let records: Vec<_> = self
                .trait_impls
                .iter()
                .enumerate()
                .filter(|(_, record)| {
                    record.visible_scope.is_none()
                        && self.canonical_type(record.implementor) == Some(owner)
                        && self.canonical_type(record.trait_type) == Some(trait_type)
                })
                .collect();
            if records.len() != 1 {
                return Err(fail(
                    "generated derived method has no unique trait implementation",
                ));
            }
            let (record_index, record) = records[0];
            let slots: Vec<_> = record
                .methods
                .iter()
                .enumerate()
                .filter(|(_, method)| method.name == name)
                .collect();
            if slots.len() != 1
                || slots[0].1.func_id != DERIVE_FUNC_ID
                || slots[0].1.trait_impl.and_then(|ty| self.canonical_type(ty)) != Some(trait_type)
            {
                return Err(fail(
                    "generated derived method does not match a trait sentinel",
                ));
            }
            impl_updates.push((record_index, slots[0].0, function));
            let mut found_method = false;
            for (index, methods) in self.methods.iter().enumerate() {
                if self.canonical_type(TypeIndex(index as u32)) != Some(owner) {
                    continue;
                }
                for (slot, method) in methods.iter().enumerate() {
                    if method.visible_scope.is_none()
                        && method.name == name
                        && method.trait_impl.and_then(|ty| self.canonical_type(ty))
                            == Some(trait_type)
                    {
                        if method.func_id != DERIVE_FUNC_ID {
                            return Err(fail(
                                "generated derived method would overwrite an ordinary method",
                            ));
                        }
                        found_method = true;
                        method_updates.push((index, slot, function));
                    }
                }
            }
            if !found_method {
                return Err(fail("generated derived method has no type method sentinel"));
            }
            for (index, table) in self.vtables.iter().enumerate() {
                if self.canonical_type(table.implementor) != Some(owner) {
                    continue;
                }
                if self.trait_schema(table.trait_type).is_some() {
                    let descriptors = self
                        .checked_vtable_descriptors(index)
                        .map_err(|error| fail(&error.to_string()))?;
                    for (slot, descriptor) in descriptors.iter().enumerate() {
                        if descriptor.implementation_scope.is_none()
                            && descriptor.implementation_trait == trait_type
                            && descriptor.name == name
                        {
                            if descriptor.func_id != DERIVE_FUNC_ID {
                                return Err(fail("generated inherited method is not a sentinel"));
                            }
                            table_updates.push((index, slot, function));
                        }
                    }
                    continue;
                }
                // Legacy tables have no slot identities. Preserve the old
                // unambiguous, exact-global publication rule without guessing
                // which inherited slot shares this sentinel value.
                if table.visible_scope.is_some()
                    || self.canonical_type(table.trait_type) != Some(trait_type)
                {
                    continue;
                }
                let slots: Vec<_> = table
                    .entries
                    .iter()
                    .enumerate()
                    .filter(|(_, id)| **id == DERIVE_FUNC_ID)
                    .collect();
                if slots.len() != 1
                    || record
                        .methods
                        .iter()
                        .filter(|method| method.func_id == DERIVE_FUNC_ID)
                        .count()
                        != 1
                {
                    return Err(fail(
                        "generated derived method has an ambiguous vtable slot",
                    ));
                }
                table_updates.push((index, slots[0].0, function));
            }
        }
        for (owner, slot, function) in method_updates {
            self.methods[owner][slot].func_id = function;
        }
        for (record, slot, function) in impl_updates {
            self.trait_impls[record].methods[slot].func_id = function;
        }
        for (table, slot, function) in table_updates {
            self.vtables[table].entries[slot] = function;
        }
        Ok(())
    }

    /// Rewrite method, trait implementation and vtable function identities.
    /// The compiler supplies source-symbol → generated-function mappings;
    /// compiler-derived method sentinels remain unchanged.
    ///
    /// # Errors
    /// Returns the first identity without a mapping. The pool is unchanged on
    /// failure, so an incomplete relocation never produces mixed metadata.
    pub fn remap_function_ids(
        &mut self,
        mut mapping: impl FnMut(u32) -> Option<u32>,
    ) -> Result<(), u32> {
        let mut replacements = HashMap::new();
        for id in self
            .methods
            .iter()
            .flatten()
            .map(|method| method.func_id)
            .chain(
                self.trait_impls
                    .iter()
                    .flat_map(|record| record.methods.iter().map(|method| method.func_id)),
            )
            .chain(
                self.vtables
                    .iter()
                    .flat_map(|table| table.entries.iter().copied()),
            )
        {
            if id != DERIVE_FUNC_ID && !replacements.contains_key(&id) {
                let replacement = mapping(id).ok_or(id)?;
                if replacement == DERIVE_FUNC_ID {
                    return Err(id);
                }
                replacements.insert(id, replacement);
            }
        }
        for method in self.methods.iter_mut().flatten().chain(
            self.trait_impls
                .iter_mut()
                .flat_map(|record| &mut record.methods),
        ) {
            if method.func_id != DERIVE_FUNC_ID {
                method.func_id = replacements[&method.func_id];
            }
        }
        for id in self.vtables.iter_mut().flat_map(|table| &mut table.entries) {
            if *id != DERIVE_FUNC_ID {
                *id = replacements[id];
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Intrinsic, MethodSlot, TraitImplRecord, VTable};

    #[test]
    fn relocation_updates_all_tables_and_preserves_derived_sentinels_atomically() {
        let mut pool = TypePool::with_intrinsics();
        let implementor = Intrinsic::I64.type_index();
        let method = MethodSlot {
            name: str_interner::intern("read"),
            func_id: 77,
            trait_impl: None,
            visible_scope: None,
            access: crate::MethodAccess::Public,
        };
        pool.add_method(implementor, method.clone());
        pool.add_trait_impl(TraitImplRecord {
            visible_scope: None,
            trait_type: pool.well_known.eq,
            implementor,
            methods: vec![method],
        });
        pool.add_vtable(VTable {
            visible_scope: None,
            trait_type: pool.well_known.eq,
            implementor,
            entries: vec![77, DERIVE_FUNC_ID],
        });
        assert_eq!(pool.remap_function_ids(|_| None), Err(77));
        assert_eq!(pool.methods_of(implementor)[0].func_id, 77);
        assert_eq!(pool.trait_impls.last().unwrap().methods[0].func_id, 77);
        assert_eq!(
            pool.vtables.last().unwrap().entries,
            vec![77, DERIVE_FUNC_ID]
        );
        pool.remap_function_ids(|id| (id == 77).then_some(2))
            .unwrap();
        assert_eq!(pool.methods_of(implementor)[0].func_id, 2);
        assert_eq!(pool.trait_impls.last().unwrap().methods[0].func_id, 2);
        assert_eq!(
            pool.vtables.last().unwrap().entries,
            vec![2, DERIVE_FUNC_ID]
        );
    }
    fn add_derived(pool: &mut TypePool, owner: TypeIndex, trait_type: TypeIndex) {
        let method = MethodSlot {
            name: str_interner::intern("eq"),
            func_id: DERIVE_FUNC_ID,
            trait_impl: Some(trait_type),
            visible_scope: None,
            access: crate::MethodAccess::Public,
        };
        pool.add_method(owner, method.clone());
        pool.add_trait_impl(TraitImplRecord {
            visible_scope: None,
            trait_type,
            implementor: owner,
            methods: vec![method],
        });
        pool.add_vtable(VTable {
            visible_scope: None,
            trait_type,
            implementor: owner,
            entries: vec![DERIVE_FUNC_ID],
        });
    }

    #[test]
    fn derived_publication_disambiguates_traits_and_aliases_and_roundtrips() {
        let mut pool = TypePool::with_intrinsics();
        let owner = Intrinsic::I64.type_index();
        let eq = pool.well_known.eq;
        let partial_eq = pool.well_known.partial_eq;
        add_derived(&mut pool, owner, eq);
        add_derived(&mut pool, owner, partial_eq);
        let alias = pool.register(crate::TypeInfo {
            kind: TypeKind::Typealias {
                name: str_interner::intern("DerivedOwner"),
                target: owner,
            },
            type_id: crate::TypeId::ZERO,
            size: 0,
            align: 0,
        });
        pool.add_method(
            owner,
            MethodSlot {
                name: str_interner::intern("ordinary"),
                func_id: 77,
                trait_impl: None,
                visible_scope: None,
                access: crate::MethodAccess::Public,
            },
        );
        let name = str_interner::intern("eq");
        pool.resolve_derived_methods([(alias, eq, name, 5), (owner, partial_eq, name, 6)])
            .unwrap();
        let restored = TypePool::restore(pool.snapshot()).unwrap();
        for (trait_type, id) in [(eq, 5), (partial_eq, 6)] {
            assert_eq!(
                restored
                    .find_trait_method(owner, trait_type, name)
                    .unwrap()
                    .func_id,
                id
            );
            assert_eq!(
                restored.find_vtable(owner, trait_type).unwrap().entries,
                vec![id]
            );
            assert_eq!(
                restored
                    .methods_of(owner)
                    .iter()
                    .find(|m| m.trait_impl == Some(trait_type))
                    .unwrap()
                    .func_id,
                id
            );
        }
        assert_eq!(
            restored
                .find_method(owner, str_interner::intern("ordinary"))
                .unwrap()
                .func_id,
            77
        );
    }

    #[test]
    fn incomplete_or_ambiguous_derived_publication_never_mutates_metadata() {
        let mut pool = TypePool::with_intrinsics();
        let owner = Intrinsic::I64.type_index();
        let eq = pool.well_known.eq;
        let partial_eq = pool.well_known.partial_eq;
        let name = str_interner::intern("eq");
        add_derived(&mut pool, owner, eq);
        assert!(
            pool.resolve_derived_methods([(owner, eq, name, 5), (owner, partial_eq, name, 6)])
                .is_err()
        );
        assert_eq!(
            pool.find_trait_method(owner, eq, name).unwrap().func_id,
            DERIVE_FUNC_ID
        );
        assert_eq!(pool.methods_of(owner)[0].func_id, DERIVE_FUNC_ID);
        assert_eq!(
            pool.find_vtable(owner, eq).unwrap().entries,
            vec![DERIVE_FUNC_ID]
        );
        pool.vtables[0].entries.push(DERIVE_FUNC_ID);
        assert!(
            pool.resolve_derived_methods([(owner, eq, name, 5)])
                .is_err()
        );
        assert_eq!(
            pool.find_trait_method(owner, eq, name).unwrap().func_id,
            DERIVE_FUNC_ID
        );
        assert!(
            pool.resolve_derived_methods([(owner, eq, name, 5), (owner, eq, name, 6)])
                .is_err()
        );
        pool.resolve_derived_methods([]).unwrap();
        assert_eq!(pool.methods_of(owner)[0].func_id, DERIVE_FUNC_ID);
    }
}
