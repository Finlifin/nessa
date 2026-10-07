use crate::{
    FieldInfo, IdentityPathSegment, Intrinsic, NominalTypeProvenance, PackageTypeContext, TypeId,
    TypeIdentityInput, TypeIndex, TypeInfo, TypeKind, TypePool,
};

fn input(declarations: &[(TypeIndex, &str)]) -> TypeIdentityInput {
    TypeIdentityInput {
        schema: 1,
        packages: vec![PackageTypeContext {
            identity_schema: 1,
            identity: [
                0xe9, 0x02, 0x86, 0x6d, 0x7a, 0x93, 0xe4, 0x74, 0xb9, 0xf6, 0x34, 0xb7, 0x6d, 0xf1,
                0x2e, 0xd5,
            ],
            qualified_name: "org.test/package".into(),
            version: "1.2.3".into(),
        }],
        declarations: declarations
            .iter()
            .map(|&(type_index, name)| NominalTypeProvenance {
                type_index,
                package: 0,
                path: vec![IdentityPathSegment::Named(name.into())],
                last_stable_version: "1.2.3".into(),
            })
            .collect(),
    }
}
fn structure(pool: &mut TypePool, name: &str, field: TypeIndex) -> TypeIndex {
    pool.register(TypeInfo {
        kind: TypeKind::Struct {
            name: str_interner::intern(name),
            fields: vec![FieldInfo {
                name: str_interner::intern("n"),
                ty: field,
                offset: 0,
                has_default: false,
            }],
        },
        type_id: TypeId::ZERO,
        size: 8,
        align: 8,
    })
}

#[test]
fn independent_schema_one_golden_alias_identity_and_canonical_reverse_lookup() {
    let mut pool = TypePool::with_intrinsics();
    let p = structure(&mut pool, "P", Intrinsic::I64.type_index());
    let alias = pool.register(TypeInfo {
        kind: TypeKind::Typealias {
            name: str_interner::intern("OtherName"),
            target: p,
        },
        type_id: TypeId::ZERO,
        size: 0,
        align: 0,
    });
    assert!(pool.lookup_by_id(TypeId::ZERO).is_none());
    assert!(pool.stable_type_id(p).is_err());
    pool.finalize_type_identities(input(&[(p, "P")])).unwrap();
    let expected = TypeId(0x67e86291f31bf3c9, 0x777c2a6f4508fed2);
    assert_eq!(pool.stable_type_id(p).unwrap(), expected);
    assert_eq!(pool.stable_type_id(alias).unwrap(), expected);
    assert_eq!(pool.lookup_by_id(expected), Some(p));
    let restored = TypePool::restore(pool.snapshot()).unwrap();
    assert_eq!(restored.stable_type_id(alias).unwrap(), expected);
    restored.validate().unwrap();
}

fn recursive(reverse: bool, extra: bool, rename: bool) -> (TypePool, TypeIndex, TypeIndex) {
    let mut pool = TypePool::with_intrinsics();
    let dummy = |pool: &mut TypePool, name: &str| {
        pool.register(TypeInfo {
            kind: TypeKind::Struct {
                name: str_interner::intern(name),
                fields: vec![],
            },
            type_id: TypeId::ZERO,
            size: 8,
            align: 8,
        })
    };
    let (a, b) = if reverse {
        let b = dummy(&mut pool, "B");
        let a = dummy(&mut pool, "A");
        (a, b)
    } else {
        let a = dummy(&mut pool, "A");
        let b = dummy(&mut pool, "B");
        (a, b)
    };
    let optional = pool.intern_structural(TypeKind::Optional { inner: b });
    for (ty, target) in [(a, optional), (b, a)] {
        let TypeKind::Struct { fields, .. } = &mut pool.get_mut(ty).kind else {
            panic!()
        };
        fields.push(FieldInfo {
            name: str_interner::intern(if rename && ty == b { "changed" } else { "next" }),
            ty: target,
            offset: 0,
            has_default: false,
        });
    }
    let mut declarations = vec![(a, "A"), (b, "B")];
    if extra {
        let unrelated = structure(&mut pool, "Unrelated", Intrinsic::Str.type_index());
        declarations.push((unrelated, "Unrelated"));
    }
    pool.finalize_type_identities(input(&declarations)).unwrap();
    (pool, a, b)
}
#[test]
fn recursive_graphs_ignore_indices_and_unrelated_declarations_but_include_transitive_layout() {
    let (left, a, b) = recursive(false, false, false);
    for index in 0..2000 {
        str_interner::intern(&format!("identity-noise-{index}"));
    }
    let (right, ra, rb) = recursive(true, true, false);
    assert_eq!(
        left.stable_type_id(a).unwrap(),
        right.stable_type_id(ra).unwrap()
    );
    assert_eq!(
        left.stable_type_id(b).unwrap(),
        right.stable_type_id(rb).unwrap()
    );
    let (changed, ca, _) = recursive(false, false, true);
    assert_ne!(
        left.stable_type_id(a).unwrap(),
        changed.stable_type_id(ca).unwrap()
    );
}
#[test]
fn finalization_failure_is_atomic_and_live_corruption_invalidates_lookup() {
    let mut pool = TypePool::with_intrinsics();
    let p = structure(&mut pool, "P", Intrinsic::I64.type_index());
    pool.finalize_type_identities(input(&[(p, "P")])).unwrap();
    let id = pool.stable_type_id(p).unwrap();
    let old = pool.identity_input().unwrap().clone();
    let mut invalid = old.clone();
    invalid.declarations.push(invalid.declarations[0].clone());
    assert!(pool.finalize_type_identities(invalid).is_err());
    assert_eq!(pool.identity_input(), Some(&old));
    assert_eq!(pool.stable_type_id(p).unwrap(), id);
    assert_eq!(pool.lookup_by_id(id), Some(p));
    pool.get_mut(p).type_id = TypeId(id.hi() ^ 1, id.lo());
    assert!(pool.stable_type_id(p).is_err());
    assert!(pool.lookup_by_id(id).is_none());
    assert!(pool.validate().is_err());
    assert!(TypePool::restore(pool.snapshot()).is_err());
}
#[test]
fn abstract_propagation_keeps_trait_views_executable_and_concrete_steps_stable() {
    let mut pool = TypePool::with_intrinsics();
    let name = str_interner::intern("Item");
    let owner = pool.register(TypeInfo {
        kind: TypeKind::Trait {
            name: str_interner::intern("Owner"),
            parents: vec![],
            assoc_types: vec![(name, TypeIndex::INVALID)],
        },
        type_id: TypeId::ZERO,
        size: 0,
        align: 0,
    });
    let binder = pool.register(TypeInfo {
        kind: TypeKind::AssociatedType {
            trait_owner: owner,
            name,
        },
        type_id: TypeId::ZERO,
        size: 0,
        align: 0,
    });
    let TypeKind::Trait { assoc_types, .. } = &mut pool.get_mut(owner).kind else {
        panic!()
    };
    assoc_types[0].1 = binder;
    pool.register_associated_default(crate::AssociatedTypeDefault {
        trait_owner: owner,
        name,
        expression: crate::AssociatedTypeExpr::Required,
    })
    .unwrap();
    let template = pool.intern_iteration_step_template(binder).unwrap();
    let abstract_fn = pool.intern_structural(TypeKind::Function {
        params: vec![binder],
        ret: binder,
    });
    let carrier = pool.intern_structural(TypeKind::Function {
        params: vec![owner],
        ret: Intrinsic::I64.type_index(),
    });
    let holder = structure(&mut pool, "Holder", binder);
    let step = pool
        .intern_iteration_step(Intrinsic::I64.type_index())
        .unwrap();
    pool.finalize_type_identities(input(&[(owner, "Owner"), (holder, "Holder")]))
        .unwrap();
    for ty in [binder, template, abstract_fn, holder] {
        assert_eq!(pool.get(ty).type_id, TypeId::ZERO);
        assert!(pool.stable_type_id(ty).is_err());
    }
    for ty in [owner, carrier, step] {
        assert_ne!(pool.stable_type_id(ty).unwrap(), TypeId::ZERO);
    }
    let restored = TypePool::restore(pool.snapshot()).unwrap();
    assert_eq!(
        restored.checked_iteration_step_item(step).unwrap(),
        Some(Intrinsic::I64.type_index())
    );
}
#[test]
fn bootstrap_ids_are_fixed_and_mutable_catalog_cannot_grant_native_identity() {
    let mut pool = TypePool::with_intrinsics();
    assert_eq!(
        pool.get(pool.well_known.display).type_id,
        TypeId(0x4e45535354524954, 0x0000000100000001)
    );
    let fake = pool.register(TypeInfo {
        kind: TypeKind::Trait {
            name: str_interner::intern("Display"),
            parents: vec![],
            assoc_types: vec![],
        },
        type_id: TypeId(0x4e45535354524954, 0x0000000100000001),
        size: 0,
        align: 0,
    });
    pool.well_known.display = fake;
    assert!(!pool.is_reserved_type(fake));
    assert!(pool.stable_type_id(fake).is_err());
    assert!(
        pool.finalize_type_identities(input(&[(fake, "Fake")]))
            .is_err()
    );
}
#[test]
fn five_hundred_thirteen_nominal_edges_finalize_and_restore_without_recursive_hashing() {
    let mut pool = TypePool::with_intrinsics();
    let mut nodes = Vec::new();
    let mut next = Intrinsic::I64.type_index();
    for index in (0..514).rev() {
        let node = structure(&mut pool, &format!("N{index}"), next);
        nodes.push((node, format!("N{index}")));
        next = node;
    }
    let declarations = nodes
        .iter()
        .map(|(ty, name)| (*ty, name.as_str()))
        .collect::<Vec<_>>();
    pool.finalize_type_identities(input(&declarations)).unwrap();
    assert_ne!(pool.stable_type_id(next).unwrap(), TypeId::ZERO);
    let id = pool.stable_type_id(next).unwrap();
    let restored = TypePool::restore(pool.snapshot()).unwrap();
    assert_eq!(restored.stable_type_id(next).unwrap(), id);
}

fn qualified_interface(reverse: bool, effect: bool) -> (TypePool, TypeIndex) {
    let mut pool = TypePool::with_intrinsics();
    let create_trait = |pool: &mut TypePool| {
        pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern("Contract"),
                parents: vec![],
                assoc_types: vec![],
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        })
    };
    let (owner, a) = if reverse {
        let a = structure(&mut pool, "Error", Intrinsic::I64.type_index());
        let owner = create_trait(&mut pool);
        (owner, a)
    } else {
        let owner = create_trait(&mut pool);
        let a = structure(&mut pool, "Error", Intrinsic::I64.type_index());
        (owner, a)
    };
    let members = if owner < a {
        vec![owner, a]
    } else {
        vec![a, owner]
    };
    let position = members.iter().position(|&member| member == owner).unwrap() as u32;
    let qualified = pool.intern_structural(if effect {
        TypeKind::EffectQualified {
            effects: members,
            inner: Intrinsic::I64.type_index(),
        }
    } else {
        TypeKind::ErrorQualified {
            errors: members,
            inner: Intrinsic::I64.type_index(),
        }
    });
    let declaration = pool.intern_structural(TypeKind::Function {
        params: vec![owner],
        ret: qualified,
    });
    pool.register_trait_schema(crate::TraitDispatchSchema {
        trait_type: owner,
        slots: vec![crate::TraitMethodKey {
            trait_owner: owner,
            name: str_interner::intern("result"),
            signature: Some(crate::TraitMethodSignature {
                declaration,
                self_paths: vec![
                    vec![crate::TraitTypeStep::Parameter(0)],
                    vec![
                        crate::TraitTypeStep::Return,
                        if effect {
                            crate::TraitTypeStep::EffectMember(position)
                        } else {
                            crate::TraitTypeStep::ErrorMember(position)
                        },
                    ],
                ],
                associated_paths: vec![],
                parameter_kinds: vec![crate::TraitParameterKind::Receiver],
            }),
        }],
    })
    .unwrap();
    pool.finalize_type_identities(input(&[(owner, "Contract"), (a, "Error")]))
        .unwrap();
    (pool, owner)
}
#[test]
fn qualified_self_paths_use_semantic_member_positions_under_registration_shuffle() {
    for effect in [false, true] {
        let (left, owner) = qualified_interface(false, effect);
        let (right, other) = qualified_interface(true, effect);
        assert_eq!(
            left.stable_type_id(owner).unwrap(),
            right.stable_type_id(other).unwrap()
        );
    }
}

#[test]
fn finalized_collection_aliases_share_exact_native_identity_without_owning_roles() {
    let mut pool = TypePool::with_intrinsics();
    let list = pool.list_type().unwrap();
    let map = pool.map_type().unwrap();
    let mut aliases = Vec::new();
    for (name, target) in [("PublicList", list), ("PublicMap", map)] {
        let alias = pool.register(TypeInfo {
            kind: TypeKind::Typealias {
                name: str_interner::intern(name),
                target,
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        let nested = pool.register(TypeInfo {
            kind: TypeKind::Typealias {
                name: str_interner::intern(&format!("Nested{name}")),
                target: alias,
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        aliases.push((alias, nested, target));
    }
    pool.finalize_type_identities(input(&[])).unwrap();
    for &(alias, nested, target) in &aliases {
        let id = pool.stable_type_id(target).unwrap();
        assert_eq!(pool.get(alias).type_id, id);
        assert_eq!(pool.get(nested).type_id, id);
        assert_eq!(pool.stable_type_id(nested).unwrap(), id);
        assert_eq!(pool.lookup_by_id(id), Some(target));
    }
    let restored = TypePool::restore(pool.snapshot()).unwrap();
    restored.validate_collection_layouts().unwrap();
    assert_eq!(restored.list_type(), Some(list));
    assert_eq!(restored.map_type(), Some(map));

    let mut mismatch = pool.snapshot();
    mismatch.types[aliases[0].0.as_u32() as usize].kind = TypeKind::Typealias {
        name: str_interner::intern("WrongTarget"),
        target: map,
    };
    assert!(TypePool::restore(mismatch).is_err());
    let mut duplicate = pool.snapshot();
    duplicate.types.push(pool.get(list).clone());
    assert!(TypePool::restore(duplicate).is_err());
    let mut spoof = pool.snapshot();
    spoof.types[aliases[0].0.as_u32() as usize].kind = TypeKind::Struct {
        name: str_interner::intern("Spoof"),
        fields: vec![],
    };
    assert!(TypePool::restore(spoof).is_err());
}
