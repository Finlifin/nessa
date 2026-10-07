//! Stable IDs describe semantic source types, never local pool or interner ordinals.
mod common;

use driver::Driver;
use type_pool::{
    FieldInfo, IdentityPathSegment, Intrinsic, NominalTypeProvenance, PackageTypeContext, TypeId,
    TypeIdentityInput, TypeIndex, TypeInfo, TypeKind, TypePool,
};

fn compile(source: &str) -> driver::CompileResult {
    let result = Driver::new().compile(source);
    assert!(!result.has_errors, "{source}: {:?}", result.diagnostics);
    assert!(result.type_pool.identity_input().is_some());
    result.type_pool.validate().unwrap();
    result
}

fn named(pool: &TypePool, name: &str) -> TypeIndex {
    let found: Vec<_> = (0..pool.len())
        .filter_map(|i| {
            let index = TypeIndex::from_raw(i as u32);
            let symbol = match pool.get(index).kind {
                TypeKind::Struct { name, .. }
                | TypeKind::Enum { name, .. }
                | TypeKind::Typealias { name, .. }
                | TypeKind::Newtype { name, .. }
                | TypeKind::Module { name }
                | TypeKind::Trait { name, .. } => name,
                _ => return None,
            };
            (str_interner::get(symbol) == name).then_some(index)
        })
        .collect();
    assert_eq!(found.len(), 1, "{name}: {found:?}");
    found[0]
}

fn alias_matching(pool: &TypePool, name: &str, matches: impl Fn(TypeIndex) -> bool) -> TypeIndex {
    let candidates:Vec<_>=(0..pool.len()).map(|i|TypeIndex::from_raw(i as u32)).filter(|&i|matches!(pool.get(i).kind,TypeKind::Typealias{name:n,..}if str_interner::get(n)==name)).filter(|&i|matches(pool.canonical_type(i).unwrap())).collect();
    assert!(!candidates.is_empty(), "no semantic alias {name}");
    let targets: std::collections::HashSet<_> = candidates
        .iter()
        .map(|&i| pool.canonical_type(i).unwrap())
        .collect();
    assert_eq!(targets.len(), 1, "ambiguous semantic alias {name}");
    let target = *targets.iter().next().unwrap();
    let expected = id(pool, target);
    for &alias in &candidates {
        assert_eq!(id(pool, alias), expected);
    }
    target
}

fn std_ids(pool: &TypePool) -> std::collections::BTreeMap<Vec<u8>, TypeId> {
    fn path_key(path: &[IdentityPathSegment]) -> Vec<u8> {
        let mut key = vec![];
        key.extend_from_slice(&(path.len() as u32).to_be_bytes());
        for segment in path {
            match segment {
                IdentityPathSegment::Named(n) => {
                    key.push(0);
                    key.extend_from_slice(&(n.len() as u32).to_be_bytes());
                    key.extend_from_slice(n.as_bytes());
                }
                IdentityPathSegment::Lexical { kind, ordinal } => {
                    key.extend_from_slice(&[1, *kind]);
                    key.extend_from_slice(&ordinal.to_be_bytes());
                }
            }
        }
        key
    }
    let mut found = std::collections::BTreeMap::new();
    found.insert(vec![255, 1], id(pool, pool.well_known.display));
    found.insert(vec![255, 2], id(pool, pool.well_known.eq));
    let mut modules = 0;
    let mut other_nominals = 0;
    for declaration in &pool.identity_input().unwrap().declarations {
        if matches!(declaration.path.first(),Some(IdentityPathSegment::Named(n))if n=="std") {
            if matches!(
                pool.get(declaration.type_index).kind,
                TypeKind::Module { .. }
            ) {
                modules += 1;
            } else {
                other_nominals += 1;
            }
            let identity = pool.get(declaration.type_index).type_id;
            if identity == TypeId::ZERO {
                assert!(pool.stable_type_id(declaration.type_index).is_err());
            } else {
                assert_eq!(id(pool, declaration.type_index), identity);
            }
            assert!(
                found
                    .insert(path_key(&declaration.path), identity)
                    .is_none()
            );
        }
    }
    assert!(
        modules > 0 && other_nominals > 0,
        "actual std modules and nominal interfaces/layouts required"
    );
    found
}

fn id(pool: &TypePool, index: TypeIndex) -> TypeId {
    let identity = pool.stable_type_id(index).unwrap();
    assert_ne!(identity, TypeId::ZERO);
    assert_eq!(identity, pool.get(index).type_id);
    let canonical = pool.canonical_type(index).unwrap();
    assert_eq!(
        pool.canonical_type(pool.lookup_by_id(identity).unwrap()),
        Some(canonical)
    );
    identity
}

fn package() -> PackageTypeContext {
    PackageTypeContext {
        identity_schema: 1,
        identity: [0x29; 16],
        qualified_name: "example.org/identity-tests".into(),
        version: "1.2.3".into(),
    }
}

fn provenance(index: TypeIndex, path: &str) -> NominalTypeProvenance {
    NominalTypeProvenance {
        type_index: index,
        package: 0,
        path: path
            .split('.')
            .map(|s| IdentityPathSegment::Named(s.into()))
            .collect(),
        last_stable_version: "1.2.3".into(),
    }
}

fn input(declarations: Vec<NominalTypeProvenance>) -> TypeIdentityInput {
    TypeIdentityInput {
        schema: 1,
        packages: vec![package()],
        declarations,
    }
}

fn structure(pool: &mut TypePool, name: &str, field: TypeIndex) -> TypeIndex {
    pool.register(TypeInfo {
        kind: TypeKind::Struct {
            name: str_interner::intern(name),
            fields: vec![FieldInfo {
                name: str_interner::intern("value"),
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
fn source_nominal_alias_and_concrete_structural_types_have_full_stable_ids() {
    let source = "struct Payload{n:i64};enum Choice{empty,value(payload:Payload)};typealias Alias=Payload;typealias Maybe=?Payload;typealias Pair=(Payload,i64);typealias Callback=fn(Payload)->i64;fn read(p:Alias)->i64{p.n};fn main(){let value:Any=Payload{n:42};read(value.as(Alias))}";
    let result = compile(source);
    let pool = &result.type_pool;
    let payload = id(pool, named(pool, "Payload"));
    assert_eq!(
        id(
            pool,
            alias_matching(pool, "Alias", |t| t
                == pool.canonical_type(named(pool, "Payload")).unwrap())
        ),
        payload
    );
    let identities = [
        payload,
        id(pool, named(pool, "Choice")),
        id(
            pool,
            alias_matching(
                pool,
                "Maybe",
                |t| matches!(pool.get(t).kind,TypeKind::Optional{inner}if pool.canonical_type(inner)==Some(named(pool,"Payload"))),
            ),
        ),
        id(
            pool,
            alias_matching(
                pool,
                "Pair",
                |t| matches!(&pool.get(t).kind,TypeKind::Tuple{elements}if elements.len()==2&&pool.canonical_type(elements[0])==Some(named(pool,"Payload"))&&elements[1]==Intrinsic::I64.type_index()),
            ),
        ),
        id(
            pool,
            alias_matching(
                pool,
                "Callback",
                |t| matches!(&pool.get(t).kind,TypeKind::Function{params,ret}if params.len()==1&&pool.canonical_type(params[0])==Some(named(pool,"Payload"))&&*ret==Intrinsic::I64.type_index()),
            ),
        ),
    ];
    for (i, left) in identities.iter().enumerate() {
        for right in &identities[i + 1..] {
            assert_ne!(left, right);
        }
    }
    assert_eq!(common::run_value(source), Ok(42));
}

#[test]
fn scratch_normalization_skips_comments_and_spaces_but_retains_literal_tokens() {
    let a = compile("struct Payload{n:i64};fn main(){Payload{n:42}.n}");
    let b = compile(
        "struct  Payload { n : i64 }; {- ignored -} fn main ( ) { Payload { n : 42 } . n }",
    );
    assert_eq!(
        id(&a.type_pool, named(&a.type_pool, "Payload")),
        id(&b.type_pool, named(&b.type_pool, "Payload"))
    );
    let changed = compile("struct Payload{n:i64};fn main(){Payload{n:41}.n}");
    assert_ne!(
        id(&a.type_pool, named(&a.type_pool, "Payload")),
        id(&changed.type_pool, named(&changed.type_pool, "Payload"))
    );
    let string_a = compile("struct Payload{n:String};fn main(){Payload{n:\"a b\"}.n}");
    let string_b = compile("struct Payload{n:String};fn main(){Payload{n:\"ab\"}.n}");
    assert_ne!(
        id(&string_a.type_pool, named(&string_a.type_pool, "Payload")),
        id(&string_b.type_pool, named(&string_b.type_pool, "Payload"))
    );
    let crlf = compile("struct Payload{n:i64}\r\nfn main(){42}\r\n");
    let lf = compile("struct Payload{n:i64}\nfn main(){42}\n");
    assert_eq!(
        id(&crlf.type_pool, named(&crlf.type_pool, "Payload")),
        id(&lf.type_pool, named(&lf.type_pool, "Payload"))
    );
}

#[test]
fn source_map_and_interner_history_do_not_change_user_or_std_ids() {
    let source = "struct Payload{n:i64};fn main(){Payload{n:42}.n}";
    let driver = Driver::new();
    let first = driver.compile(source);
    assert!(!first.has_errors);
    for i in 0..6000 {
        str_interner::intern(&format!("identity-unrelated-{i}"));
    }
    let _ = driver.compile("fn main(){99}");
    let second = driver.compile(source);
    assert!(!second.has_errors);
    assert_eq!(
        id(&first.type_pool, named(&first.type_pool, "Payload")),
        id(&second.type_pool, named(&second.type_pool, "Payload"))
    );
    assert_eq!(std_ids(&first.type_pool), std_ids(&second.type_pool));
    let unrelated = compile("struct Other{x:bool};fn main(){0}");
    assert_eq!(std_ids(&first.type_pool), std_ids(&unrelated.type_pool));
}

#[test]
fn concrete_iteration_aliases_are_stable_but_abstract_default_templates_are_not_values() {
    let source = "struct Payload{n:i64};trait Source{assoc Item:Type=Self;derive fn next(self)->IterationStep(Item){IterationStep(Item).yielded(self)}};impl Source for Payload{};typealias Step=IterationStep(Payload);fn main(){Payload{n:42}.next() match{Step.yielded(p)=>p.n,_=>0}}";
    let result = compile(source);
    let pool = &result.type_pool;
    let concrete = id(
        pool,
        alias_matching(pool, "Step", |t| {
            pool.checked_iteration_step_item(t).unwrap() == Some(named(pool, "Payload"))
        }),
    );
    let mut abstract_count = 0;
    for i in 0..pool.len() {
        let t = TypeIndex::from_raw(i as u32);
        match pool.get(t).kind {
            TypeKind::AssociatedType { .. } | TypeKind::IterationStepTemplate { .. } => {
                assert_eq!(pool.get(t).type_id, TypeId::ZERO);
                assert!(pool.stable_type_id(t).is_err());
                abstract_count += 1;
            }
            _ => {}
        }
    }
    assert!(abstract_count >= 1);
    assert_eq!(common::run_value(source), Ok(42));
    let mut manual = TypePool::with_intrinsics();
    let ints = manual
        .intern_iteration_step(Intrinsic::I64.type_index())
        .unwrap();
    let strings = manual
        .intern_iteration_step(Intrinsic::Str.type_index())
        .unwrap();
    manual.finalize_type_identities(input(vec![])).unwrap();
    assert_ne!(id(&manual, ints), id(&manual, strings));
    assert_ne!(concrete, id(&manual, ints));
}

#[test]
fn package_path_version_and_transitive_layout_changes_affect_identity() {
    fn fixture() -> (TypePool, TypeIndex, TypeIndex, TypeIdentityInput) {
        let mut pool = TypePool::with_intrinsics();
        let leaf = structure(&mut pool, "Leaf", Intrinsic::I64.type_index());
        let owner = structure(&mut pool, "Owner", leaf);
        let inputs = input(vec![
            provenance(leaf, "api.Leaf"),
            provenance(owner, "api.Owner"),
        ]);
        (pool, leaf, owner, inputs)
    }
    let (mut original, leaf, owner, original_input) = fixture();
    original
        .finalize_type_identities(original_input.clone())
        .unwrap();
    let expected = id(&original, owner);
    for mutation in 0..6 {
        let (mut pool, l, o, mut inputs) = fixture();
        match mutation {
            0 => inputs.packages[0].identity[15] ^= 1,
            1 => {
                inputs.declarations[1].path = vec![
                    IdentityPathSegment::Named("other".into()),
                    IdentityPathSegment::Named("Owner".into()),
                ]
            }
            2 => inputs.declarations[1].last_stable_version = "1.2.4".into(),
            3 => {
                if let TypeKind::Struct { fields, .. } = &mut pool.get_mut(l).kind {
                    fields[0].ty = Intrinsic::Bool.type_index()
                }
            }
            4 => {
                if let TypeKind::Struct { fields, .. } = &mut pool.get_mut(l).kind {
                    fields[0].name = str_interner::intern("renamed")
                }
            }
            5 => pool.get_mut(l).size = 16,
            _ => unreachable!(),
        }
        pool.finalize_type_identities(inputs).unwrap();
        assert_ne!(id(&pool, o), expected, "mutation {mutation}");
    }
    let (mut extra, l, o, mut inputs) = fixture();
    let unrelated = structure(&mut extra, "Unrelated", Intrinsic::Bool.type_index());
    inputs.declarations.push(provenance(unrelated, "Unrelated"));
    extra.finalize_type_identities(inputs).unwrap();
    assert_eq!(id(&extra, o), expected);
    assert_eq!(id(&extra, l), id(&original, leaf));
}

#[test]
fn recursive_graph_and_registration_order_use_paths_instead_of_indices() {
    fn build(reverse: bool) -> (TypePool, TypeIndex, TypeIndex) {
        let mut pool = TypePool::with_intrinsics();
        let names = if reverse {
            ["Right", "Left"]
        } else {
            ["Left", "Right"]
        };
        let first = structure(&mut pool, names[0], Intrinsic::I64.type_index());
        let second = structure(&mut pool, names[1], first);
        if let TypeKind::Struct { fields, .. } = &mut pool.get_mut(first).kind {
            fields[0].ty = second;
        }
        let left = if reverse { second } else { first };
        let right = if reverse { first } else { second };
        pool.finalize_type_identities(input(vec![
            provenance(left, "graph.Left"),
            provenance(right, "graph.Right"),
        ]))
        .unwrap();
        (pool, left, right)
    }
    let (a, left_a, right_a) = build(false);
    let (b, left_b, right_b) = build(true);
    assert_eq!(id(&a, left_a), id(&b, left_b));
    assert_eq!(id(&a, right_a), id(&b, right_b));
    assert_ne!(id(&a, left_a), id(&a, right_a));
    let restored = TypePool::restore(a.snapshot()).unwrap();
    assert_eq!(id(&restored, left_a), id(&a, left_a));
    let source = "struct Node{next:?Node,n:i64};fn main(){let root=Node{next:Node{next:null,n:42},n:0};root.next.unwrap().n}";
    let result = compile(source);
    id(&result.type_pool, named(&result.type_pool, "Node"));
    assert_eq!(common::run_value(source), Ok(42));
}

#[test]
fn deep_nominal_dependency_graph_finalizes_and_replays_all_513_identities() {
    let mut pool = TypePool::with_intrinsics();
    let mut declarations = vec![];
    let mut indices = vec![];
    let mut previous = Intrinsic::I64.type_index();
    for i in 0..513 {
        let t = structure(&mut pool, &format!("Node{i}"), previous);
        declarations.push(provenance(t, &format!("deep.Node{i}")));
        indices.push(t);
        previous = t;
    }
    pool.finalize_type_identities(input(declarations)).unwrap();
    let ids: Vec<_> = indices.iter().map(|&t| id(&pool, t)).collect();
    let restored = TypePool::restore(pool.snapshot()).unwrap();
    assert_eq!(
        ids,
        indices
            .iter()
            .map(|&t| id(&restored, t))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        513
    );
}

#[test]
fn alias_and_manual_newtype_have_transparent_and_distinct_identities() {
    let mut pool = TypePool::with_intrinsics();
    let base = structure(&mut pool, "Base", Intrinsic::I64.type_index());
    let alias = pool.register(TypeInfo {
        kind: TypeKind::Typealias {
            name: str_interner::intern("Alias"),
            target: base,
        },
        type_id: TypeId::ZERO,
        size: 8,
        align: 8,
    });
    let newtype = pool.register(TypeInfo {
        kind: TypeKind::Newtype {
            name: str_interner::intern("Wrapper"),
            inner: base,
        },
        type_id: TypeId::ZERO,
        size: 8,
        align: 8,
    });
    pool.finalize_type_identities(input(vec![
        provenance(base, "Base"),
        provenance(newtype, "Wrapper"),
    ]))
    .unwrap();
    assert_eq!(id(&pool, base), id(&pool, alias));
    assert_ne!(id(&pool, base), id(&pool, newtype));
    let rejected = Driver::new().compile("newtype Distance=i64;fn main(){42}");
    assert!(rejected.has_errors, "source newtype remains unsupported");
    assert!(rejected.codegen_output.functions.is_empty());
    assert!(rejected.into_artifact().is_err());
}

#[test]
fn invalid_finalization_is_atomic_and_zero_never_resolves_to_a_descriptor() {
    let mut pool = TypePool::with_intrinsics();
    let a = structure(&mut pool, "A", Intrinsic::I64.type_index());
    let b = structure(&mut pool, "B", Intrinsic::I64.type_index());
    assert_eq!(pool.lookup_by_id(TypeId::ZERO), None);
    assert!(pool.stable_type_id(a).is_err());
    let before = format!("{:?}", pool.snapshot());
    let mut bad = input(vec![provenance(a, "same"), provenance(b, "same")]);
    assert!(pool.finalize_type_identities(bad.clone()).is_err());
    assert_eq!(format!("{:?}", pool.snapshot()), before);
    assert_eq!(pool.lookup_by_id(TypeId::ZERO), None);
    bad.declarations[1].path = vec![IdentityPathSegment::Named("B".into())];
    bad.declarations[0].path = vec![IdentityPathSegment::Named("A".into())];
    pool.finalize_type_identities(bad.clone()).unwrap();
    let old = id(&pool, a);
    let before = format!("{:?}", pool.snapshot());
    bad.packages[0].version = "not semver".into();
    assert!(pool.finalize_type_identities(bad).is_err());
    assert_eq!(format!("{:?}", pool.snapshot()), before);
    assert_eq!(pool.lookup_by_id(old), Some(a));
    pool.get_mut(a).type_id = TypeId(old.hi() ^ 1, old.lo());
    assert!(pool.validate().is_err());
}

#[test]
fn typed_path_framing_and_lexical_ordinals_do_not_alias_named_segments() {
    fn identity(mut path: Vec<IdentityPathSegment>) -> TypeId {
        path.push(IdentityPathSegment::Named("Same".into()));
        let mut pool = TypePool::with_intrinsics();
        let t = structure(&mut pool, "Same", Intrinsic::I64.type_index());
        let mut declaration = provenance(t, "unused");
        declaration.path = path;
        pool.finalize_type_identities(input(vec![declaration]))
            .unwrap();
        id(&pool, t)
    }
    let values = [
        identity(vec![IdentityPathSegment::Named("a.b".into())]),
        identity(vec![
            IdentityPathSegment::Named("a".into()),
            IdentityPathSegment::Named("b".into()),
        ]),
        identity(vec![
            IdentityPathSegment::Named("a".into()),
            IdentityPathSegment::Lexical {
                kind: 1,
                ordinal: 0,
            },
        ]),
        identity(vec![
            IdentityPathSegment::Named("a".into()),
            IdentityPathSegment::Lexical {
                kind: 1,
                ordinal: 1,
            },
        ]),
        identity(vec![
            IdentityPathSegment::Named("a".into()),
            IdentityPathSegment::Lexical {
                kind: 2,
                ordinal: 0,
            },
        ]),
    ];
    assert_eq!(
        values
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        values.len()
    );
}

#[test]
fn qualified_type_sets_are_unordered_but_function_and_tuple_roles_are_ordered() {
    let mut pool = TypePool::with_intrinsics();
    let a = structure(&mut pool, "A", Intrinsic::I64.type_index());
    let b = structure(&mut pool, "B", Intrinsic::Bool.type_index());
    let first = pool.intern_structural(TypeKind::ErrorQualified {
        errors: vec![a, b],
        inner: Intrinsic::I64.type_index(),
    });
    let reversed = pool.intern_structural(TypeKind::ErrorQualified {
        errors: vec![b, a, a],
        inner: Intrinsic::I64.type_index(),
    });
    let tuple_a = pool.intern_structural(TypeKind::Tuple {
        elements: vec![a, b],
    });
    let tuple_b = pool.intern_structural(TypeKind::Tuple {
        elements: vec![b, a],
    });
    let function_a = pool.intern_structural(TypeKind::Function {
        params: vec![a],
        ret: b,
    });
    let function_b = pool.intern_structural(TypeKind::Function {
        params: vec![b],
        ret: a,
    });
    pool.finalize_type_identities(input(vec![provenance(a, "A"), provenance(b, "B")]))
        .unwrap();
    assert_eq!(id(&pool, first), id(&pool, reversed));
    assert_ne!(id(&pool, tuple_a), id(&pool, tuple_b));
    assert_ne!(id(&pool, function_a), id(&pool, function_b));
}

#[test]
fn malformed_missing_duplicate_and_out_of_range_provenance_leave_pool_unchanged() {
    for mutation in 0..7 {
        let mut pool = TypePool::with_intrinsics();
        let t = structure(&mut pool, "Valid", Intrinsic::I64.type_index());
        let mut inputs = input(vec![provenance(t, "Valid")]);
        match mutation {
            0 => inputs.schema = 999,
            1 => inputs.packages[0].identity_schema = 999,
            2 => inputs.declarations.clear(),
            3 => inputs.declarations.push(inputs.declarations[0].clone()),
            4 => inputs.declarations[0].package = 99,
            5 => inputs.declarations[0].type_index = TypeIndex::from_raw(u32::MAX),
            6 => inputs.declarations[0].last_stable_version = "1.2".into(),
            _ => unreachable!(),
        }
        let before = format!("{:?}", pool.snapshot());
        assert!(
            pool.finalize_type_identities(inputs).is_err(),
            "mutation {mutation}"
        );
        assert_eq!(format!("{:?}", pool.snapshot()), before);
        assert_eq!(pool.lookup_by_id(TypeId::ZERO), None);
        assert!(pool.identity_input().is_none());
    }
}

#[test]
fn independent_full128_golden_matches_nominal_and_recursive_schema_bytes() {
    // Independently encoded with Python stdlib from the frozen public schema.
    let mut pool = TypePool::with_intrinsics();
    let base = structure(&mut pool, "Base", Intrinsic::I64.type_index());
    pool.finalize_type_identities(input(vec![provenance(base, "Base")]))
        .unwrap();
    assert_eq!(
        id(&pool, base),
        TypeId(0x3234b9e72ba6e9a1, 0xdba17438381755cf)
    );
    let mut pool = TypePool::with_intrinsics();
    let left = structure(&mut pool, "Left", Intrinsic::I64.type_index());
    let right = structure(&mut pool, "Right", left);
    if let TypeKind::Struct { fields, .. } = &mut pool.get_mut(left).kind {
        fields[0].ty = right;
    }
    pool.finalize_type_identities(input(vec![
        provenance(left, "graph.Left"),
        provenance(right, "graph.Right"),
    ]))
    .unwrap();
    assert_eq!(
        id(&pool, left),
        TypeId(0x8fed4296134a2d36, 0x16e0a562faa1870d)
    );
    assert_eq!(
        id(&pool, right),
        TypeId(0x294dbc736d383f66, 0xed9dd2ae8a320194)
    );
}

fn context() -> driver::CompilationIdentityContext {
    driver::CompilationIdentityContext {
        package: package(),
        last_stable_version: None,
        type_versions: vec![],
    }
}
fn compile_in(source: &str, context: driver::CompilationIdentityContext) -> driver::CompileResult {
    let result = Driver::new().compile_with_identity(source, context);
    assert!(!result.has_errors, "{source}: {:?}", result.diagnostics);
    result.type_pool.validate().unwrap();
    assert!(result.type_pool.identity_input().is_some());
    result
}

#[test]
fn explicit_context_keeps_unrelated_source_and_declaration_order_out_of_ids() {
    let first = compile_in(
        "struct Leaf{n:i64};struct Owner{leaf:Leaf};fn main(){Owner{leaf:Leaf{n:42}}.leaf.n}",
        context(),
    );
    let second = compile_in(
        "struct Unrelated{x:bool};struct Owner{leaf:Leaf};struct Leaf{n:i64};fn main(){Owner{leaf:Leaf{n:42}}.leaf.n}",
        context(),
    );
    for name in ["Leaf", "Owner"] {
        assert_eq!(
            id(&first.type_pool, named(&first.type_pool, name)),
            id(&second.type_pool, named(&second.type_pool, name))
        );
    }
    let mut another = context();
    another.package.identity[0] ^= 0x80;
    let other = compile_in(
        "struct Leaf{n:i64};struct Owner{leaf:Leaf};fn main(){42}",
        another,
    );
    assert_ne!(
        id(&first.type_pool, named(&first.type_pool, "Owner")),
        id(&other.type_pool, named(&other.type_pool, "Owner"))
    );
    let mut version_only = context();
    version_only.package.qualified_name = "different.org/name".into();
    let name_changed = compile_in(
        "struct Leaf{n:i64};struct Owner{leaf:Leaf};fn main(){42}",
        version_only,
    );
    // A claimed package name is metadata, whereas its actual Merkle hash is the identity input.
    assert_eq!(
        id(&first.type_pool, named(&first.type_pool, "Owner")),
        id(
            &name_changed.type_pool,
            named(&name_changed.type_pool, "Owner")
        )
    );
}

#[test]
fn source_versions_default_to_package_allow_path_override_and_leave_std_independent() {
    let source = "struct Payload{n:i64};fn main(){Payload{n:42}.n}";
    let first = compile_in(source, context());
    let mut changed = context();
    changed.package.version = "1.2.4".into();
    let second = compile_in(source, changed.clone());
    assert_ne!(
        id(&first.type_pool, named(&first.type_pool, "Payload")),
        id(&second.type_pool, named(&second.type_pool, "Payload"))
    );
    changed.last_stable_version = Some("1.2.3".into());
    let pinned = compile_in(source, changed);
    assert_eq!(
        id(&first.type_pool, named(&first.type_pool, "Payload")),
        id(&pinned.type_pool, named(&pinned.type_pool, "Payload"))
    );
    let mut override_context = context();
    override_context.type_versions.push((
        vec![IdentityPathSegment::Named("Payload".into())],
        "2.0.0".into(),
    ));
    let overridden = compile_in(source, override_context);
    assert_ne!(
        id(&first.type_pool, named(&first.type_pool, "Payload")),
        id(
            &overridden.type_pool,
            named(&overridden.type_pool, "Payload")
        )
    );
    assert_eq!(std_ids(&first.type_pool), std_ids(&second.type_pool));
    assert_eq!(std_ids(&first.type_pool), std_ids(&overridden.type_pool));
}

#[test]
fn invalid_duplicate_or_unused_source_version_overrides_reject_artifacts() {
    for mutation in 0..4 {
        let mut c = context();
        let path = vec![IdentityPathSegment::Named("Payload".into())];
        match mutation {
            0 => c.last_stable_version = Some("invalid".into()),
            1 => c.type_versions.push((
                vec![IdentityPathSegment::Named("Missing".into())],
                "1.2.3".into(),
            )),
            2 => {
                c.type_versions.push((path.clone(), "1.2.3".into()));
                c.type_versions.push((path, "1.2.4".into()));
            }
            3 => c.type_versions.push((path, "1.2".into())),
            _ => unreachable!(),
        }
        let result = Driver::new().compile_with_identity("struct Payload{n:i64};fn main(){42}", c);
        assert!(result.has_errors, "mutation {mutation}");
        assert!(result.codegen_output.functions.is_empty());
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.level == diagnostic::Level::Error
                    && (d.message.contains("version")
                        || d.message.contains("identity")
                        || d.message.contains("override"))),
            "{:?}",
            result.diagnostics
        );
        assert!(result.into_artifact().is_err());
    }
}

#[test]
fn original_namespace_and_lexical_declaration_paths_survive_source_aliases() {
    let source = "mod left{pub struct Payload{n:i64}};mod right{pub struct Payload{n:i64}};use left.Payload as Imported;typealias Alias=Imported;fn main(){Alias{n:42}.n}";
    let result = compile_in(source, context());
    let inputs = result.type_pool.identity_input().unwrap();
    let found: Vec<_> = inputs
        .declarations
        .iter()
        .filter(|d| matches!(d.path.last(),Some(IdentityPathSegment::Named(n))if n=="Payload"))
        .collect();
    assert_eq!(found.len(), 2);
    assert_ne!(
        id(&result.type_pool, found[0].type_index),
        id(&result.type_pool, found[1].type_index)
    );
    let left = found
        .iter()
        .find(|d| {
            d.path
                == vec![
                    IdentityPathSegment::Named("left".into()),
                    IdentityPathSegment::Named("Payload".into()),
                ]
        })
        .unwrap();
    assert_eq!(
        id(
            &result.type_pool,
            alias_matching(&result.type_pool, "Alias", |t| t
                == result.type_pool.canonical_type(left.type_index).unwrap())
        ),
        id(&result.type_pool, left.type_index)
    );
    let lexical = compile_in(
        "fn make(){struct Local{n:i64};Local{n:42}.n};fn other(){struct Local{n:i64};Local{n:0}.n};fn main(){make()}",
        context(),
    );
    let declarations: Vec<_> = lexical
        .type_pool
        .identity_input()
        .unwrap()
        .declarations
        .iter()
        .filter(|d| matches!(d.path.last(),Some(IdentityPathSegment::Named(n))if n=="Local"))
        .collect();
    assert_eq!(declarations.len(), 2);
    assert_ne!(declarations[0].path, declarations[1].path);
    assert_ne!(
        id(&lexical.type_pool, declarations[0].type_index),
        id(&lexical.type_pool, declarations[1].type_index)
    );
}

#[test]
fn implementation_bodies_and_remapping_do_not_change_declared_type_interfaces() {
    let first = compile_in(
        "struct Payload{n:i64};trait Score{derive fn score(self)->i64{42}};impl Score for Payload{};fn main(){Payload{n:0}.score()}",
        context(),
    );
    let second = compile_in(
        "struct Payload{n:i64};trait Score{derive fn score(self)->i64{40+2}};impl Score for Payload{};fn unrelated()->i64{1};fn main(){Payload{n:0}.score()}",
        context(),
    );
    for name in ["Payload", "Score"] {
        assert_eq!(
            id(&first.type_pool, named(&first.type_pool, name)),
            id(&second.type_pool, named(&second.type_pool, name))
        );
    }
    let changed = compile_in(
        "struct Payload{n:i64};trait Score{derive fn score(self)->bool{true}};impl Score for Payload{};fn main(){42}",
        context(),
    );
    assert_ne!(
        id(&first.type_pool, named(&first.type_pool, "Score")),
        id(&changed.type_pool, named(&changed.type_pool, "Score"))
    );
    assert_eq!(
        id(&first.type_pool, named(&first.type_pool, "Payload")),
        id(&changed.type_pool, named(&changed.type_pool, "Payload"))
    );
}

#[test]
fn trait_qualified_member_markers_follow_semantic_targets_across_registration_order() {
    use type_pool::{
        TraitAssociatedPath, TraitDispatchSchema, TraitMethodKey, TraitMethodSignature,
        TraitParameterKind, TraitTypeStep,
    };
    fn build(reverse: bool, effect: bool, other_target: bool) -> (TypePool, TypeIndex) {
        let mut pool = TypePool::with_intrinsics();
        let item = str_interner::intern("Item");
        let alternative = str_interner::intern("Alternative");
        let make_trait = |pool: &mut TypePool| {
            pool.register(TypeInfo {
                kind: TypeKind::Trait {
                    name: str_interner::intern("Contract"),
                    parents: vec![],
                    assoc_types: vec![
                        (item, Intrinsic::I64.type_index()),
                        (alternative, Intrinsic::I64.type_index()),
                    ],
                },
                type_id: TypeId::ZERO,
                size: 0,
                align: 0,
            })
        };
        let (owner, a, b) = if reverse {
            let b = structure(&mut pool, "B", Intrinsic::Bool.type_index());
            let a = structure(&mut pool, "A", Intrinsic::I64.type_index());
            let owner = make_trait(&mut pool);
            (owner, a, b)
        } else {
            let owner = make_trait(&mut pool);
            let a = structure(&mut pool, "A", Intrinsic::I64.type_index());
            let b = structure(&mut pool, "B", Intrinsic::Bool.type_index());
            (owner, a, b)
        };
        if let TypeKind::Trait { assoc_types, .. } = &mut pool.get_mut(owner).kind {
            assoc_types[0].1 = a;
            assoc_types[1].1 = b;
        }
        let qualified = pool.intern_structural(if effect {
            TypeKind::EffectQualified {
                effects: vec![owner, a, b],
                inner: Intrinsic::I64.type_index(),
            }
        } else {
            TypeKind::ErrorQualified {
                errors: vec![owner, a, b],
                inner: Intrinsic::I64.type_index(),
            }
        });
        // Descriptor positions may be normalized by artifact-local type indices.
        // Locate the semantic member, then author the corresponding valid marker.
        let members = match &pool.get(qualified).kind {
            TypeKind::ErrorQualified { errors, .. } => errors,
            TypeKind::EffectQualified { effects, .. } => effects,
            _ => unreachable!(),
        };
        let member_step = |target: TypeIndex| {
            let at = members.iter().position(|&t| t == target).unwrap() as u32;
            if effect {
                TraitTypeStep::EffectMember(at)
            } else {
                TraitTypeStep::ErrorMember(at)
            }
        };
        let self_member = member_step(owner);
        let associated_member = member_step(if other_target { b } else { a });
        let signature = pool.intern_structural(TypeKind::Function {
            params: vec![owner],
            ret: qualified,
        });
        pool.register_trait_schema(TraitDispatchSchema {
            trait_type: owner,
            slots: vec![TraitMethodKey {
                trait_owner: owner,
                name: str_interner::intern("read"),
                signature: Some(TraitMethodSignature {
                    declaration: signature,
                    self_paths: vec![
                        vec![TraitTypeStep::Parameter(0)],
                        vec![TraitTypeStep::Return, self_member],
                    ],
                    parameter_kinds: vec![TraitParameterKind::Receiver],
                    associated_paths: vec![TraitAssociatedPath {
                        trait_owner: owner,
                        name: if other_target { alternative } else { item },
                        path: vec![TraitTypeStep::Return, associated_member],
                    }],
                }),
            }],
        })
        .unwrap();
        pool.finalize_type_identities(input(vec![
            provenance(owner, "api.Contract"),
            provenance(a, "api.A"),
            provenance(b, "api.B"),
        ]))
        .unwrap();
        (pool, owner)
    }
    for effect in [false, true] {
        let (first, a) = build(false, effect, false);
        let (second, b) = build(true, effect, false);
        assert_ne!(a, b, "fixture must really change local indices");
        assert_eq!(
            id(&first, a),
            id(&second, b),
            "same semantic Self/Item targets, effect={effect}"
        );
        let (changed, c) = build(false, effect, true);
        assert_ne!(
            id(&first, a),
            id(&changed, c),
            "changing Item/A to Alternative/B changes interface, effect={effect}"
        );
        let restored = TypePool::restore(second.snapshot()).unwrap();
        assert_eq!(id(&restored, b), id(&second, b));
    }
}

#[test]
fn catalog_redirection_cannot_grant_native_reserved_identity_and_failure_is_atomic() {
    for role in ["Display", "Eq", "Iterator"] {
        let mut pool = TypePool::with_intrinsics();
        let fake = pool.register(TypeInfo {
            kind: TypeKind::Trait {
                name: str_interner::intern(role),
                parents: vec![],
                assoc_types: vec![],
            },
            type_id: TypeId::ZERO,
            size: 0,
            align: 0,
        });
        match role {
            "Display" => pool.well_known.display = fake,
            "Eq" => pool.well_known.eq = fake,
            "Iterator" => pool.well_known.iterator = fake,
            _ => unreachable!(),
        }
        let before = format!("{:?}", pool.snapshot());
        assert!(
            pool.finalize_type_identities(input(vec![provenance(fake, &format!("user.{role}"))]))
                .is_err(),
            "redirected {role} obtained native authority"
        );
        assert_eq!(format!("{:?}", pool.snapshot()), before);
        assert_eq!(pool.get(fake).type_id, TypeId::ZERO);
        assert_eq!(pool.lookup_by_id(TypeId::ZERO), None);
        assert!(pool.identity_input().is_none());
    }
}
