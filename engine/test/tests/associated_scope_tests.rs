//! Associated namespaces preserve declaration identity through execution and archives.

mod common;

#[test]
fn a_documented_associated_declaration_preserves_its_default_in_archives() {
    let source = "trait Iter { assoc Item: Type = i64 }\nfn main() { 42 }";
    let result = driver::Driver::new().compile(source);
    assert!(!result.has_errors, "{:?}", result.diagnostics);
    let artifact = result.into_artifact().unwrap();
    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
    let restored = nsbc_io::read_artifact(&bytes).unwrap();
    let snapshot = restored.type_pool.snapshot();
    let (index, info) = snapshot.types.iter().enumerate().find(|(_, info)|
        matches!(&info.kind,type_pool::TypeKind::Trait{name,..} if str_interner::get(*name)=="Iter")).unwrap();
    let owner = type_pool::TypeIndex::from_raw(index as u32);
    let type_pool::TypeKind::Trait { assoc_types, .. } = &info.kind else {
        unreachable!()
    };
    assert_eq!(assoc_types.len(), 1);
    let (name, marker) = assoc_types[0];
    assert_eq!(str_interner::get(name), "Item");
    assert!(matches!(restored.type_pool.get(marker).kind,
        type_pool::TypeKind::AssociatedType { trait_owner, name: marker_name }
            if trait_owner == owner && marker_name == name));
    assert_eq!(
        restored
            .type_pool
            .associated_defaults_snapshot()
            .iter()
            .filter(|default| default.trait_owner == owner)
            .cloned()
            .collect::<Vec<_>>(),
        [type_pool::AssociatedTypeDefault {
            trait_owner: owner,
            name,
            expression: type_pool::AssociatedTypeExpr::Concrete(
                restored.type_pool.intrinsic(type_pool::Intrinsic::I64)
            ),
        }]
    );
    assert_eq!(archive_value(source), 42);
}

#[test]
fn associated_bindings_and_invalid_placement_never_emit_partial_artifacts() {
    for (source, message) in [
        (
            "struct P {}\nimpl P { assoc SIZE: i64 = 42 }\nfn main() { 42 }",
            "associated type bindings require a trait declaration or trait implementation",
        ),
        (
            "assoc Item: Type = i64\nfn main() { 42 }",
            "only allowed directly",
        ),
        (
            "fn main() { assoc const SIZE = 42; SIZE }",
            "only allowed directly",
        ),
        (
            "trait Iter { derive fn helper(self) { assoc Item: Type = i64 } }\nfn main() { 42 }",
            "only allowed directly",
        ),
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "accepted {source}");
        assert!(result.codegen_output.functions.is_empty(), "{source}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|error| error.message.contains(message)),
            "{source}: {:?}",
            result.diagnostics
        );
        assert!(result.into_artifact().is_err(), "archived {source}");
    }
}

fn archive_value(source: &str) -> i64 {
    let result = driver::Driver::new().compile(source);
    assert!(!result.has_errors, "{:?}", result.diagnostics);
    let artifact = result.into_artifact().unwrap();
    let bytes = nsbc_io::write_artifact(&artifact).unwrap();
    let artifact = nsbc_io::read_artifact(&bytes).unwrap();
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .unwrap();
    let task = engine.vm_mut().spawn_root(entry);
    assert!(matches!(
        engine.vm_mut().run(),
        interpreter::VmResult::Finished
    ));
    engine.vm_mut().task_result_i64(task).unwrap()
}

fn assert_value(source: &str, expected: i64) {
    assert_eq!(common::run_value(source).unwrap(), expected);
    assert_eq!(archive_value(source), expected);
}

#[test]
fn multiple_impls_and_forward_aliases_resolve_one_type_namespace() {
    assert_value(
        "fn main() -> i64 { let f = Alias.answer; f() }\n\
         struct Point { x: i64 }\n\
         impl Alias { fn answer() -> i64 { helper() } }\n\
         impl Point { fn helper() -> i64 { 42 } }\n\
         typealias Alias = Point",
        42,
    );
}

#[test]
fn static_type_calls_and_qualified_calls_share_constructor_binding() {
    assert_value(
        "fn main() -> i64 { let first = Alias(x = 20); let second = Point.new(22); first.x + second.x }\n\
         struct Point { x: i64 }\n\
         impl Point { pub fn new(x: i64) -> Point { Point { x: x } } }\n\
         typealias Alias = Point",
        42,
    );
}

#[test]
fn selected_imports_and_nested_items_keep_the_defining_scope() {
    assert_value(
        "mod types { pub struct Point {} }\n\
         use types.Point\n\
         use Point.answer\n\
         impl Point { const BASE: i64 = 40; pub mod helpers { pub fn two() -> i64 { 2 } }; pub fn answer() -> i64 { BASE + Point.helpers.two() } }\n\
         fn main() -> i64 { answer() }",
        42,
    );
}

#[test]
fn impl_constants_globals_and_init_hooks_use_shared_storage() {
    assert_value(
        "struct State {}\n\
         impl State { const BASE: i64 = 20; global count: i64 = BASE; fn __init__() { count = count + 1 }; fn bump() -> i64 { count = count + 1; count } }\n\
         fn main() -> i64 { State.bump() + State.BASE }",
        42,
    );
}

#[test]
fn private_members_are_accessible_from_other_impls_of_the_same_type() {
    assert_value(
        "struct Point { private fn base() -> i64 { 40 } }\n\
         impl Point { private fn two() -> i64 { 2 } }\n\
         impl Point { fn answer() -> i64 { Point.base() + two() } }\n\
         fn main() -> i64 { Point.answer() }",
        42,
    );
}

#[test]
fn impl_instance_methods_retain_the_receiver_abi_after_roundtrip() {
    assert_value(
        "struct Point {}\n\
         impl Point { pub fn answer(self) -> i64 { 42 } }\n\
         fn main() -> i64 { let point = Point {}; point.answer() }",
        42,
    );
}

#[test]
fn invalid_or_inaccessible_associated_calls_fail_before_execution() {
    for (source, message) in [
        (
            "struct P {}\nimpl P { private fn hidden() -> i64 { 42 } }\nfn main() -> i64 { P.hidden() }",
            "not visible",
        ),
        (
            "struct P {}\nimpl P { private fn hidden(self) -> i64 { 42 } }\nfn main() -> i64 { let p = P {}; p.hidden() }",
            "not visible",
        ),
        (
            "struct P {}\nimpl P { private fn new() -> P { P {} } }\nfn main() { P() }",
            "not visible",
        ),
        (
            "struct P {}\nimpl P { fn new(x: i64) -> P { P {} } }\nfn main() { P(true) }",
            "type mismatch",
        ),
        (
            "struct P {}\nimpl P { fn new(x: i64) -> P { P {} } }\nfn main() { P() }",
            "missing required parameter",
        ),
        (
            "struct P {}\nimpl P { fn answer() {} }\nimpl P { fn answer() {} }",
            "duplicate associated definition",
        ),
        (
            "struct P {}\nfn main() { let dynamic_type: Type = P; dynamic_type() }",
            "Type values are not callable",
        ),
    ] {
        let result = driver::Driver::new().compile(source);
        assert!(result.has_errors, "accepted invalid source: {source}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains(message)),
            "{source}: {:?}",
            result.diagnostics
        );
        assert!(result.codegen_output.functions.is_empty());
    }
}
