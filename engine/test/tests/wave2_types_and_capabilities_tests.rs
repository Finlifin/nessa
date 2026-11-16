//! End-to-end integration tests for Wave 2 type system and capabilities features:
//! 1. Nominal `newtype Name = InnerType` (distinct TypeId, layout, construction, `.as(T)`, reflection, patterns, scopes, cycles).
//! 2. `Hash` trait, scalar `Hash` impls, `derive Hash`, `Newtype` derivations (`Eq`, `PartialEq`, `Ord`, `PartialOrd`, `Display`, `Hash`), and custom `derive UserTrait`.
//! 3. Cross-package orphan rule (`impl T`, `impl Trait for T`, `derive Trait for T` vs `extend`) and extended `__init__` + shared state in `impl Trait for T`, `extend T`, and `extend Trait for T`.
//! 4. Effect sets (`#[e1, e2] T`, `#[] T`, `++`, `const` aliases), capability methods (`handles(eff)`), `.use(handler)`, `HandlesStatement`, `LetHandler`, and `.nsbc` archive roundtrips.

mod common;

use std::path::PathBuf;

use diagnostic::Level;
use driver::{Driver, ManifestDocument, PackageSources, SourceFile};
use type_pool::{Intrinsic, TypeId, TypeIndex, TypeKind};

fn assert_rejected_msg(source: &str, expected_substring: &str) {
    let result = Driver::new().compile(source);
    assert!(
        result.has_errors,
        "expected compilation error containing {expected_substring:?} for:\n{source}\ngot diagnostics: {:?}",
        result.diagnostics
    );
    assert!(
        result.codegen_output.functions.is_empty(),
        "invalid source emitted bytecode:\n{source}"
    );
    assert!(
        result.diagnostics.iter().any(|d| {
            d.level == Level::Error
                && d.message
                    .to_lowercase()
                    .contains(&expected_substring.to_lowercase())
        }),
        "expected error containing {expected_substring:?} for:\n{source}\ngot diagnostics: {:?}",
        result.diagnostics
    );
}

fn assert_rejected(source: &str) {
    let result = Driver::new().compile(source);
    assert!(
        result.has_errors,
        "expected compilation error for:\n{source}\ngot diagnostics: {:?}",
        result.diagnostics
    );
    assert!(
        result.codegen_output.functions.is_empty(),
        "invalid source emitted bytecode:\n{source}"
    );
}

fn run_archive_value(source: &str) -> Result<i64, String> {
    let compiled = Driver::new().compile(source);
    assert!(
        !compiled.has_errors,
        "unexpected compile errors:\n{source}\ndiagnostics: {:?}",
        compiled.diagnostics
    );
    let artifact = compiled.into_artifact().map_err(|e| e.to_string())?;
    let bytes = nsbc_io::write_artifact(&artifact).map_err(|e| e.to_string())?;
    let restored = nsbc_io::read_artifact(&bytes).map_err(|e| e.to_string())?;
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), restored)
        .map_err(|e| e.to_string())?
        .expect("fixture has main");
    let task = engine.vm_mut().spawn_root(entry);
    match engine.vm_mut().run() {
        interpreter::VmResult::Finished => {
            assert_eq!(engine.vm.active_stack_count(), 0);
            engine
                .vm_mut()
                .task_result_number(task)
                .map_err(|e| format!("{e:?}"))?
                .to_i64_checked()
                .ok_or_else(|| "result does not fit i64".to_owned())
        }
        interpreter::VmResult::Error(err) => Err(format!("{err:?}")),
    }
}

fn pkg(
    name: &str,
    version: &str,
    kind: &str,
    deps: &[(&str, &str)],
    source: &str,
) -> PackageSources {
    let (domain, short) = name.split_once('/').unwrap();
    let mut manifest = format!(
        "[package]\nname={short:?}\ndomain={domain:?}\nversion={version:?}\ntype={kind:?}\n[dependencies]\n"
    );
    for (dep_name, dep_ver) in deps {
        manifest.push_str(&format!("{dep_name:?}={dep_ver:?}\n"));
    }
    let root = PathBuf::from(format!("/nonexistent-wave2/{domain}/{short}/{version}"));
    PackageSources {
        entry: SourceFile {
            file_path: root.join("src/entry.ns"),
            source: source.into(),
        },
        root,
        manifest: ManifestDocument::parse(&manifest).unwrap(),
        modules: vec![],
        wasm_assets: vec![],
    }
}

fn run_pkg(root: &PackageSources, catalog: &[PackageSources]) -> i64 {
    let compiled = Driver::new().compile_package_sources_with_dependencies(root, catalog);
    assert!(
        !compiled.has_errors,
        "unexpected package compile errors: {:?}",
        compiled.diagnostics
    );
    compiled.type_pool.validate().unwrap();
    let artifact = compiled.into_artifact().unwrap();
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), artifact)
        .unwrap()
        .unwrap();
    let task = engine.vm_mut().spawn_root(entry);
    assert!(matches!(
        engine.vm_mut().run(),
        interpreter::VmResult::Finished
    ));
    assert_eq!(engine.vm.active_stack_count(), 0);
    engine
        .vm_mut()
        .task_result_number(task)
        .unwrap()
        .to_i64_checked()
        .unwrap()
}

fn reject_pkg(root: &PackageSources, catalog: &[PackageSources], expected_substring: &str) {
    let compiled = Driver::new().compile_package_sources_with_dependencies(root, catalog);
    assert!(
        compiled.has_errors,
        "expected package compile error for:\n{}",
        root.entry.source
    );
    assert!(compiled.codegen_output.functions.is_empty());
    assert!(
        compiled.diagnostics.iter().any(|d| {
            d.level == Level::Error
                && d.message
                    .to_lowercase()
                    .contains(&expected_substring.to_lowercase())
        }),
        "expected {expected_substring:?}, got: {:?}",
        compiled.diagnostics
    );
}

// ============================================================================
// 1. Nominal `newtype`
// ============================================================================

#[test]
fn newtype_nominal_identity_layout_construction_casts_and_reflection() {
    let src = r#"
        newtype Meters = f64
        newtype Seconds = f64
        newtype BigCount = i64
        newtype Tag = String

        fn classify_any(x: Any) -> i64 {
            x match {
                m: Meters => (m.as(f64) * 10.0).as(i64)
                s: Seconds => (s.as(f64) * 100.0).as(i64)
                raw: f64 => (raw * 1000.0).as(i64)
                _ => -1
            }
        }

        fn main() -> i64 {
            let m1 = Meters(2.5)
            let m2 = 1.5.as(Meters)
            let total_f = m1.as(f64) + m2.as(f64)
            let seven_i32: i32 = 7
            let widened = seven_i32.as(BigCount)
            let piped = "ok" |> Tag()

            let any_m: Any = m1
            let any_raw: Any = 2.5

            let checks_ok = (any_m matches _: Meters)
                and not (any_m matches _: f64)
                and not (any_m matches _: Seconds)
                and not (any_raw matches _: Meters)
                and (any_raw matches _: f64)
                and (type_of(m1) == Meters)
                and (type_of(m1) != f64)
                and (type_of(m1) != Seconds)
                and (any_m.as(Meters).as(f64) == 2.5)
                and (piped.as(String) == "ok")

            let c_sum = classify_any(Meters(1.2)) + classify_any(Seconds(0.3)) + classify_any(0.005)
            if checks_ok and total_f == 4.0 {
                c_sum + total_f.as(i64) + widened.as(i64)
            } else {
                -999
            }
        }
    "#;

    let compiled = Driver::new().compile(src);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);

    let find_nt = |target_name: &str| -> TypeIndex {
        (0..compiled.type_pool.len())
            .map(|i| TypeIndex::from_raw(i as u32))
            .find(|&idx| {
                matches!(
                    compiled.type_pool.get(idx).kind,
                    TypeKind::Newtype { name, .. } if str_interner::get(name) == target_name
                )
            })
            .unwrap_or_else(|| panic!("missing newtype {target_name}"))
    };

    let meters_idx = find_nt("Meters");
    let seconds_idx = find_nt("Seconds");
    let meters_info = compiled.type_pool.get(meters_idx);
    let seconds_info = compiled.type_pool.get(seconds_idx);
    let f64_info = compiled.type_pool.get(Intrinsic::F64.type_index());

    assert_eq!(meters_info.size, 8);
    assert_eq!(meters_info.align, 8);
    assert_ne!(meters_info.type_id, TypeId::ZERO);
    assert_ne!(meters_info.type_id, f64_info.type_id);
    assert_ne!(meters_info.type_id, seconds_info.type_id);

    assert_eq!(common::run_value(src).unwrap(), 58);
}

#[test]
fn newtype_pattern_destructuring_associated_scopes_and_indirect_recursion() {
    let src = r#"
        newtype Score = i64

        impl Score {
            pub fn bonus(self, extra: i64) -> Score {
                let Score(base) = self
                Score(base + extra)
            }
        }

        extend Score {
            fn doubled(self) -> Score {
                self match {
                    Score(v) => Score(v * 2)
                }
            }
        }

        trait Measurable {
            def fn measure(self) -> i64
        }

        impl Measurable for Score {
            pub fn measure(self) -> i64 {
                let Score(v) = self
                v
            }
        }

        -- Indirect recursion through nominal Struct is legal (heap indirection breaks value cycle).
        newtype NodeLink = ?ChainNode
        struct ChainNode {
            val: i64
            next: NodeLink
        }

        fn sum_chain(link: NodeLink) -> i64 {
            link match {
                NodeLink(null) => 0
                NodeLink(some_node) => some_node.as(ChainNode).val + sum_chain(some_node.as(ChainNode).next)
            }
        }

        fn unpack_param(Score(x): Score) -> i64 = x + 1

        fn main() -> i64 {
            let s = Score(10).bonus(5).doubled()
            let Score(unpacked) = s
            let f = |Score(n): Score| n - 3

            var loop_sum = 0
            for Score(item) in [Score(2), Score(4), Score(6)] {
                loop_sum += item
            }

            let any_s: Any = s
            let matched = if (any_s matches _: Score) and (s matches Score(30)) { 1 } else { 0 }
            let chain = NodeLink(ChainNode {
                val: 7,
                next: NodeLink(ChainNode { val: 8, next: NodeLink(null) })
            })

            let m: Measurable = s
            unpacked + m.measure() + unpack_param(Score(4)) + f(Score(10)) + loop_sum + matched + sum_chain(chain)
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 100);
}

#[test]
fn newtype_rejects_cycles_implicit_inheritance_and_invalid_casts() {
    // Direct, mutual, and transparent wrapper cycles must be diagnosed cleanly without panic.
    assert_rejected_msg("newtype Loop = Loop\nfn main() -> i64 { 0 }", "cyclic");
    assert_rejected_msg(
        "newtype A = B\nnewtype B = A\nfn main() -> i64 { 0 }",
        "cyclic",
    );
    assert_rejected_msg(
        "newtype OptLoop = ?OptLoop\nfn main() -> i64 { 0 }",
        "cyclic",
    );
    assert_rejected_msg(
        "typealias Pair = (Rec, i64)\nnewtype Rec = Pair\nfn main() -> i64 { 0 }",
        "cyclic",
    );

    // No implicit operator, method, assignability, or cross-newtype cast inheritance.
    assert_rejected("newtype Meters = f64\nfn main() -> f64 { Meters(1.0) + Meters(2.0) }");
    assert_rejected("newtype Meters = f64\nfn main() -> i64 { let x: f64 = Meters(1.0); 0 }");
    assert_rejected("newtype Meters = f64\nfn main() -> i64 { let m: Meters = 1.0; 0 }");
    assert_rejected("newtype Title = String\nfn main() -> usize { Title(\"abc\").len() }");
    assert_rejected(
        "newtype Meters = f64\nnewtype Seconds = f64\nfn main() -> Meters { Seconds(1.0).as(Meters) }",
    );
    assert_rejected("newtype Meters = f64\nfn main() -> i64 { Meters(1.0).as(i64) }");
}

// ============================================================================
// 2. `Hash` Trait, `derive Hash`, `Newtype` Derivations & Custom `derive`
// ============================================================================

#[test]
fn scalar_hash_and_ordering_hash_preserve_equality_invariants() {
    for ty in [
        "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
    ] {
        let src = format!(
            "fn hash_via_trait(x: Hash) -> u64 = x.hash()\n\
             fn main() -> i64 {{\n\
                 let a: {ty} = 11\n\
                 let b: {ty} = 12\n\
                 if (a.hash() == hash_via_trait(a)) and (a.hash() != b.hash()) {{ 42 }} else {{ 0 }}\n\
             }}"
        );
        assert_eq!(common::run_value(&src).unwrap(), 42, "failed for {ty}");
    }

    let float_src = r#"
        use std.math
        fn hash_via_trait(x: Hash) -> u64 = x.hash()
        fn main() -> i64 {
            let neg_zero: f64 = -0.0
            let pos_zero: f64 = 0.0
            let nan1: f64 = math.nan()
            let nan2: f64 = -math.nan()
            let neg_zero32: f32 = -0.0
            let pos_zero32: f32 = 0.0
            if (neg_zero.hash() == pos_zero.hash())
                and (nan1.hash() == nan2.hash())
                and (neg_zero32.hash() == pos_zero32.hash())
                and (pos_zero32.hash() == hash_via_trait(pos_zero32))
                and (pos_zero.hash() == hash_via_trait(pos_zero)) {
                42
            } else {
                0
            }
        }
    "#;
    assert_eq!(common::run_value(float_src).unwrap(), 42, "failed floats");

    for (label, body) in [
        (
            "bool",
            "if (true.hash() != false.hash()) and (true.hash() == hash_via_trait(true)) { 42 } else { 0 }",
        ),
        (
            "char",
            "if ('a'.hash() != 'b'.hash()) and ('a'.hash() == hash_via_trait('a')) { 42 } else { 0 }",
        ),
        (
            "str",
            "if (\"nessa\".hash() == (\"ne\" ++ \"ssa\").hash()) and (\"nessa\".hash() != \"other\".hash()) and (\"nessa\".hash() == hash_via_trait(\"nessa\")) { 42 } else { 0 }",
        ),
        (
            "unit",
            "if (().hash() == ().hash()) and (().hash() == hash_via_trait(())) { 42 } else { 0 }",
        ),
        (
            "type",
            "let ty_i64: Type = i64; let ty_str: Type = String; if (ty_i64.hash() == type_of(42).hash()) and (ty_i64.hash() != ty_str.hash()) and (ty_i64.hash() == hash_via_trait(ty_i64)) { 42 } else { 0 }",
        ),
        (
            "ordering",
            "use std.ordering.Ordering; if (Ordering.less.hash() != Ordering.equal.hash()) and (Ordering.equal.hash() != Ordering.greater.hash()) and (Ordering.less.hash() == hash_via_trait(Ordering.less)) { 42 } else { 0 }",
        ),
    ] {
        let src =
            format!("fn hash_via_trait(x: Hash) -> u64 = x.hash()\nfn main() -> i64 {{ {body} }}");
        assert_eq!(common::run_value(&src).expect(label), 42, "failed {label}");
    }
}

#[test]
fn derive_all_builtin_traits_for_newtype_struct_enum_tuple_and_recursive_types() {
    let src = r#"
        newtype Label = String
        derive Eq, PartialEq, Ord, PartialOrd, Display, Hash for Label

        newtype Weight = i64
        derive Eq, PartialEq, Ord, PartialOrd, Display, Hash for Weight

        struct Record {
            label: Label
            weight: Weight
            note: ?String
        }
        derive Eq, PartialEq, Ord, PartialOrd, Display, Hash for Record

        enum Tree {
            leaf(v: Weight)
            branch(left: Tree, right: Tree)
        }
        derive Eq, PartialEq, Ord, PartialOrd, Display, Hash for Tree

        typealias PairAlias = (Label, Weight)
        derive Eq, PartialEq, Ord, PartialOrd, Display, Hash for PairAlias

        fn check_hash_eq(a: Hash, b: Hash) -> bool = a.hash() == b.hash()
        fn show_dyn(x: Display) -> String = x.to_string()

        fn main() -> i64 {
            let l1 = Label("alpha")
            let l2 = Label("beta")
            let disp_ok = (l1.to_string() == "Label(\"alpha\")")
                and (show_dyn(Weight(9)) == "Weight(9)")

            let cmp_ok = (l1 == Label("alpha"))
                and (l1 != l2)
                and (l1 < l2)
                and (Weight(3) <= Weight(3))
                and (Weight(10) > Weight(4))

            let r1 = Record { label: Label("x"), weight: Weight(5), note: "hi" }
            let r2 = Record { label: Label("x"), weight: Weight(5), note: "hi" }
            let r3 = Record { label: Label("x"), weight: Weight(5), note: null }
            let rec_ok = (r1 == r2)
                and (r1 != r3)
                and (r1.hash() == r2.hash())
                and (r1.hash() != r3.hash())
                and check_hash_eq(r1, r2)

            let t1 = Tree.branch(Tree.leaf(Weight(1)), Tree.leaf(Weight(2)))
            let t2 = Tree.branch(Tree.leaf(Weight(1)), Tree.leaf(Weight(2)))
            let t3 = Tree.branch(Tree.leaf(Weight(2)), Tree.leaf(Weight(1)))
            let tree_ok = (t1 == t2)
                and (t1 < t3)
                and (t1.hash() == t2.hash())
                and (t1.hash() != t3.hash())

            let p1: PairAlias = (Label("k"), Weight(7))
            let p2: PairAlias = (Label("k"), Weight(7))
            let pair_ok = (p1.hash() == p2.hash()) and (p1.to_string() == "(Label(\"k\"), Weight(7))")

            if disp_ok and cmp_ok and rec_ok and tree_ok and pair_ok { 42 } else { 0 }
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 42);
}

#[test]
fn custom_user_trait_derivation_and_negative_derivation_diagnostics() {
    let src = r#"
        trait Tagged {
            def fn tag_num(self) -> i64
        }

        trait Summary(Tagged) {
            derive fn summary_score(self) -> i64 {
                self.tag_num() * 10 + 2
            }
        }

        trait Marker {}

        newtype ItemId = i64
        impl Tagged for ItemId {
            pub fn tag_num(self) -> i64 = self.as(i64)
        }
        derive Summary, Marker for ItemId

        fn use_summary(x: Summary) -> i64 = x.summary_score()
        fn use_marker(_m: Marker) -> i64 = 100

        fn main() -> i64 {
            let id = ItemId(4)
            id.summary_score() + use_summary(id) + use_marker(id)
        }
    "#;
    // 42 + 42 + 100 = 184
    assert_eq!(common::run_value(src).unwrap(), 184);

    // Custom derive with required `def fn` methods must be rejected.
    assert_rejected_msg(
        r#"
        trait NeedsImpl {
            def fn compute(self) -> i64
        }
        struct Boxed { x: i64 }
        derive NeedsImpl for Boxed
        fn main() -> i64 { 0 }
        "#,
        "required method",
    );

    // Custom derive missing parent trait implementation must be rejected.
    assert_rejected_msg(
        r#"
        trait Base {
            def fn base_val(self) -> i64
        }
        trait Child(Base) {
            derive fn child_val(self) -> i64 { self.base_val() + 1 }
        }
        struct Plain { x: i64 }
        derive Child for Plain
        fn main() -> i64 { 0 }
        "#,
        "Base",
    );

    // `derive Hash` must reject Any, List, Map, and structs without Hash.
    assert_rejected_msg(
        "struct BadAny { v: Any }\nderive Hash for BadAny\nfn main() -> i64 { 0 }",
        "Hash derivation requires a checked global Hash implementation",
    );
    assert_rejected_msg(
        "struct BadList { v: List }\nderive Hash for BadList\nfn main() -> i64 { 0 }",
        "Hash derivation requires a checked global Hash implementation",
    );
    assert_rejected_msg(
        "struct BadMap { v: Map }\nderive Hash for BadMap\nfn main() -> i64 { 0 }",
        "Hash derivation requires a checked global Hash implementation",
    );
    assert_rejected_msg(
        "struct Unhashed { x: i64 }\nnewtype Wrap = Unhashed\nderive Hash for Wrap\nfn main() -> i64 { 0 }",
        "Hash derivation requires a checked global Hash implementation",
    );
}

// ============================================================================
// 3. Cross-Package Orphan Rule & Extended `__init__`
// ============================================================================

#[test]
fn cross_package_orphan_rule_enforces_coherence_while_allowing_local_and_extend() {
    let dep = pkg(
        "example.org/dep",
        "1.0.0",
        "lib",
        &[],
        r#"
        pub struct ForeignBox {
            pub val: i64
        }
        pub newtype ForeignId = i64

        pub trait ForeignTrait {
            def fn run_foreign(self) -> i64
        }

        pub trait ForeignAuto {
            derive fn auto_val(self) -> i64 { 11 }
        }
        "#,
    );

    // 1. Orphan `impl ForeignType` is rejected.
    let bad_inherent = pkg(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/dep", "1.0.0")],
        r#"
        use dep.ForeignBox
        impl ForeignBox {
            pub fn extra(self) -> i64 = self.val
        }
        fn main() -> i64 { 0 }
        "#,
    );
    reject_pkg(&bad_inherent, std::slice::from_ref(&dep), "orphan");

    // 2. Orphan `impl ForeignTrait for ForeignType` and `impl Display for ForeignType` are rejected.
    let bad_foreign_trait = pkg(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/dep", "1.0.0")],
        r#"
        use dep.{ForeignBox, ForeignTrait}
        impl ForeignTrait for ForeignBox {
            pub fn run_foreign(self) -> i64 = self.val
        }
        fn main() -> i64 { 0 }
        "#,
    );
    reject_pkg(&bad_foreign_trait, std::slice::from_ref(&dep), "orphan");

    let bad_std_trait = pkg(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/dep", "1.0.0")],
        r#"
        use dep.ForeignBox
        impl Display for ForeignBox {
            pub fn to_string(self) -> String = "box"
        }
        fn main() -> i64 { 0 }
        "#,
    );
    reject_pkg(&bad_std_trait, std::slice::from_ref(&dep), "orphan");

    // 3. Orphan `derive Eq for ForeignType` and `derive ForeignAuto for ForeignType` are rejected.
    let bad_derive_builtin = pkg(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/dep", "1.0.0")],
        r#"
        use dep.ForeignId
        derive Eq for ForeignId
        fn main() -> i64 { 0 }
        "#,
    );
    reject_pkg(&bad_derive_builtin, std::slice::from_ref(&dep), "orphan");

    let bad_derive_custom = pkg(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/dep", "1.0.0")],
        r#"
        use dep.{ForeignBox, ForeignAuto}
        derive ForeignAuto for ForeignBox
        fn main() -> i64 { 0 }
        "#,
    );
    reject_pkg(&bad_derive_custom, std::slice::from_ref(&dep), "orphan");

    // 4. Allowed combinations:
    //    - `impl LocalTrait for ForeignBox`
    //    - `impl ForeignTrait for LocalStruct`
    //    - `derive LocalAuto for ForeignBox`
    //    - `extend ForeignBox`
    //    - `extend ForeignTrait for ForeignBox`
    let good_app = pkg(
        "example.org/app",
        "1.0.0",
        "exe",
        &[("example.org/dep", "1.0.0")],
        r#"
        use dep.{ForeignBox, ForeignTrait}

        trait LocalTrait {
            def fn local_score(self) -> i64
        }

        trait LocalAuto {
            derive fn bonus_score(self) -> i64 { 7 }
        }

        struct LocalItem {
            n: i64
        }

        impl LocalTrait for ForeignBox {
            pub fn local_score(self) -> i64 = self.val + 1
        }

        impl ForeignTrait for LocalItem {
            pub fn run_foreign(self) -> i64 = self.n * 2
        }

        derive LocalAuto for ForeignBox

        extend ForeignBox {
            fn bumped(self) -> i64 = self.val + 5
        }

        extend ForeignTrait for ForeignBox {
            pub fn run_foreign(self) -> i64 = self.val + 10
        }

        fn main() -> i64 {
            let fb = ForeignBox { val: 3 }
            let li = LocalItem { n: 4 }
            fb.local_score() + li.run_foreign() + fb.bonus_score() + fb.bumped() + fb.run_foreign() + 2
        }
        "#,
    );
    assert_eq!(run_pkg(&good_app, &[dep]), 42);
}

#[test]
fn extended_init_and_shared_bindings_in_impl_trait_and_extend_blocks() {
    let src = r#"
        global order_log: i64 = 0

        struct Counter {
            v: i64
        }

        trait Scale {
            def fn scaled(self) -> i64
        }

        impl Scale for Counter {
            const FACTOR: i64 = 3
            global hits: i64 = 0

            fn __init__() {
                order_log = order_log * 10 + 1
                hits = 10
            }

            pub fn scaled(self) -> i64 {
                hits += 1
                self.v * FACTOR + hits
            }
        }

        extend Counter {
            global ext_offset: i64 = 0

            fn __init__() {
                order_log = order_log * 10 + 2
                ext_offset = 5
            }

            fn with_offset(self) -> i64 = self.v + ext_offset
        }

        trait Extra {
            def fn extra(self) -> i64
        }

        extend Extra for Counter {
            let base_bonus: i64 = 4

            fn __init__() {
                order_log = order_log * 10 + 3
            }

            pub fn extra(self) -> i64 = self.v + base_bonus
        }

        fn main() -> i64 {
            let c = Counter { v: 2 }
            order_log + c.scaled() + c.with_offset() + c.extra()
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 153);

    // Invalid `__init__` signatures inside `impl Trait for T` or `extend T` must be rejected.
    assert_rejected_msg(
        r#"
        struct S { x: i64 }
        extend S {
            fn __init__(bad: i64) {}
        }
        fn main() -> i64 { 0 }
        "#,
        "__init__",
    );
    assert_rejected_msg(
        r#"
        struct S { x: i64 }
        trait T { def fn f(self) -> i64 }
        impl T for S {
            fn __init__() -> i64 { 1 }
            pub fn f(self) -> i64 = self.x
        }
        fn main() -> i64 { 0 }
        "#,
        "__init__",
    );
}

// ============================================================================
// 4. Effect Sets, Capabilities (`handles`), `.use()`, `HandlesStatement` & `LetHandler`
// ============================================================================

#[test]
fn effect_sets_aliases_concat_and_partial_handler_application() {
    let src = r#"
        effect read_num() -> i64
        effect emit_num(x: i64) -> i64

        const READ_SET = [read_num]
        const EMIT_SET = [emit_num]
        const IO_SET = READ_SET ++ EMIT_SET

        struct FixedReader {
            val: i64
        }
        impl FixedReader {
            pub fn read(self) -> i64 handles(read_num) = self.val
        }

        fn pure_empty(x: #[] i64) -> i64 = x

        fn nested_qual() -> #read_num #[emit_num, read_num] i64 {
            let base = read_num()#
            emit_num(base + 5)#
        }

        fn combined_io() -> #IO_SET i64 {
            nested_qual()#
        }

        -- Eliminates `read_num` via capability while leaving `#emit_num` in the residual effect set!
        fn handle_only_read() -> #emit_num i64 {
            combined_io().use(FixedReader { val: 10 }.read)#
        }

        fn main() -> i64 {
            let a = handle_only_read().use(|n: i64| n * 2)
            let b = read_num().use(|| 12)
            pure_empty(a + b)
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 42);

    // Passing a bare closure to `.use()` when the receiver has >1 effects must be rejected as ambiguous.
    assert_rejected_msg(
        r#"
        effect e1() -> i64
        effect e2() -> i64
        fn both() -> #[e1, e2] i64 { e1()# + e2()# }
        fn main() -> i64 {
            both().use(|| 1)#
        }
        "#,
        "ambiguous",
    );
}

#[test]
fn capabilities_dot_use_with_methods_traits_and_multi_effect_instances() {
    let src = r#"
        effect ask_seed() -> i64
        effect scale_by(v: i64) -> i64
        effect choose_branch(bias: i64, catch k) -> i64

        trait SeedProvider {
            def fn provide(self) -> i64 handles(ask_seed)
            derive fn scaled(self, v: i64) -> i64 handles(scale_by) {
                v * 3
            }
        }

        struct Env {
            seed: i64
        }

        impl SeedProvider for Env {
            pub fn provide(self) -> i64 handles(ask_seed) {
                self.seed
            }
        }

        newtype Brancher = i64
        impl Brancher {
            pub fn split(self, bias: i64, catch k) -> i64 handles(choose_branch) {
                let factor = self.as(i64)
                k.clone()(bias + factor) + k(bias + factor * 2)
            }
        }

        fn workflow() -> #[ask_seed, scale_by] i64 {
            let s = ask_seed()#
            scale_by(s + 2)#
        }

        fn branch_work() -> #choose_branch i64 {
            let picked = choose_branch(10)#
            picked + 1
        }

        fn run_with_dyn_provider(p: SeedProvider) -> i64 {
            ask_seed().use(p.provide)
        }

        fn main() -> i64 {
            let env = Env { seed: 5 }
            let r1 = workflow().use(env)
            let r2 = workflow().use(env.provide).use(env.scaled)
            let r3 = run_with_dyn_provider(env)
            let r4 = branch_work().use(Brancher(2))
            r1 + r2 + r3 + r4
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 75);
}

#[test]
fn handles_statement_both_syntaxes_continuations_resume_and_defer_cleanup() {
    let src = r#"
        effect get_offset() -> i64
        effect transform(x: i64) -> i64
        effect fork(base: i64, catch k) -> i64

        global cleanup_log: i64 = 0

        fn run_parens_syntax() -> i64 {
            defer cleanup_log = cleanup_log * 10 + 9
            handles(get_offset)() -> i64 {
                resume 7
            }
            handles transform(x: i64) -> i64 => {
                if x < 0 {
                    return 0
                }
                x * 2
            }
            let a = get_offset()#
            let b = transform(a)#
            let c = transform(-5)#
            b + c
        }

        fn run_catch_handles() -> i64 {
            handles(fork)(base: i64, catch k) -> i64 {
                k.clone()(base + 1) + k(base + 4)
            }
            let v = fork(10)#
            v * 2
        }

        fn main() -> i64 {
            let p = run_parens_syntax()
            let c = run_catch_handles()
            p + cleanup_log + c
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 73);
}

#[test]
fn let_handler_single_chained_trait_typed_and_ambiguity_diagnostics() {
    let src = r#"
        effect read_cfg() -> i64
        effect audit_step(delta: i64) -> i64
        effect pick_two(seed: i64, catch k) -> i64

        global init_calls: i64 = 0

        trait ConfigCap {
            def fn cfg(self) -> i64 handles(read_cfg)
        }

        struct RuntimeCtx {
            base: i64
            logged: i64
        }

        impl ConfigCap for RuntimeCtx {
            pub fn cfg(self) -> i64 handles(read_cfg) = self.base
        }

        impl RuntimeCtx {
            pub fn record(self, delta: i64) -> i64 handles(audit_step) {
                self.logged += delta
                self.logged
            }

            pub fn choose(self, seed: i64, catch k) -> i64 handles(pick_two) {
                k.clone()(seed + 1) + k(seed + 2)
            }
        }

        fn make_ctx(base: i64) -> RuntimeCtx {
            init_calls += 1
            RuntimeCtx { base: base, logged: 0 }
        }

        fn sub_task() -> #[read_cfg, audit_step] i64 {
            let b = read_cfg()#
            let a1 = audit_step(b)#
            let a2 = audit_step(3)#
            a1 + a2
        }

        fn run_chained() -> i64 {
            handles(read_cfg) handles(audit_step) let ctx = make_ctx(10)
            let res = sub_task()#
            res + ctx.logged
        }

        fn run_trait_typed_and_catch() -> i64 {
            handles(read_cfg) let cap: ConfigCap = RuntimeCtx { base: 6, logged: 0 }
            handles(pick_two) let chooser = RuntimeCtx { base: 0, logged: 0 }
            let x = read_cfg()#
            let y = pick_two(x)#
            y
        }

        fn main() -> i64 {
            let a = run_chained()
            let b = run_trait_typed_and_catch()
            if init_calls == 1 { a + b + init_calls } else { -1 }
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 52);

    // Negative: capability signature mismatch, missing capability, and ambiguous capability!
    assert_rejected_msg(
        r#"
        effect ask(x: i64) -> i64
        struct Bad {}
        impl Bad {
            pub fn wrong(self, x: String) -> i64 handles(ask) = 0
        }
        fn main() -> i64 { 0 }
        "#,
        "type mismatch",
    );

    assert_rejected_msg(
        r#"
        effect ask() -> i64
        struct Plain { x: i64 }
        fn main() -> i64 {
            handles(ask) let p = Plain { x: 1 }
            ask()#
        }
        "#,
        "handle",
    );

    assert_rejected_msg(
        r#"
        effect ask() -> i64
        struct Dup { x: i64 }
        impl Dup {
            pub fn first(self) -> i64 handles(ask) = self.x
            pub fn second(self) -> i64 handles(ask) = self.x + 1
        }
        fn main() -> i64 {
            handles(ask) let d = Dup { x: 1 }
            ask()#
        }
        "#,
        "ambiguous",
    );
}

// ============================================================================
// 5. Full `.nsbc` Archive Serialization & Execution Roundtrip
// ============================================================================

#[test]
fn wave2_combined_features_survive_nsbc_archive_roundtrip() {
    let src = r#"
        effect fetch_key() -> String
        effect adjust(n: i64, catch k) -> i64

        const WORK_EFFS = [fetch_key, adjust]

        trait Auditable {
            derive fn tag_prefix(self) -> String { "ok:" }
        }

        newtype UserId = String
        derive Eq, PartialEq, Ord, PartialOrd, Display, Hash, Auditable for UserId

        struct KeyService {
            key: String
        }

        impl KeyService {
            pub fn get_key(self) -> String handles(fetch_key) = self.key
            pub fn tweak(self, n: i64, catch k) -> i64 handles(adjust) {
                k.clone()(n + 2) + k(n + 5)
            }
        }

        fn compute_score() -> #WORK_EFFS i64 {
            let k = fetch_key()#
            let u = k.as(UserId)
            let same_hash = (u.hash() == UserId("alice").hash())
            let shown_ok = (u.to_string() == "UserId(\"alice\")") and (u.tag_prefix() == "ok:")
            if same_hash and shown_ok {
                adjust(10)#
            } else {
                -100
            }
        }

        fn run_via_let() -> i64 {
            handles(fetch_key) handles(adjust) let svc = KeyService { key: "alice" }
            compute_score()#
        }

        fn run_via_use() -> i64 {
            let svc = KeyService { key: "alice" }
            compute_score().use(svc)
        }

        fn run_nested() -> i64 {
            handles(fetch_key) handles(adjust) let svc = KeyService { key: "alice" }
            let via_let = compute_score()#
            let via_use = compute_score().use(svc)
            via_let + via_use
        }

        fn main() -> i64 {
            let via_let = run_via_let()
            let via_use = run_via_use()
            let nested = run_nested()
            if nested == 81 {
                via_let + via_use
            } else {
                nested
            }
        }
    "#;
    assert_eq!(common::run_value(src).unwrap(), 54);
    assert_eq!(run_archive_value(src).unwrap(), 54);
}
