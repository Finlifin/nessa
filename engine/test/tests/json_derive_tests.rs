//! Comprehensive integration tests for native RFC 8259 JSON, `Json` trait, and `derive Json`.

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use driver::{Driver, RunResult};

struct TempPackage(PathBuf);

impl TempPackage {
    fn new(kind: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = loop {
            let candidate = std::env::temp_dir().join(format!(
                "nessa-json-derive-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("failed to create temp package dir: {e}"),
            }
        };
        let pkg = Self(path);
        pkg.write(
            "package.toml",
            &format!(
                "[package]\nname=\"jsontest\"\ndomain=\"example.org\"\nversion=\"0.1.0\"\ntype={kind:?}\n"
            ),
        );
        pkg
    }

    fn write(&self, relative: &str, contents: &str) {
        let full = self.0.join(relative);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, contents).unwrap();
    }
}

impl Drop for TempPackage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn derive_json_structs_defaults_optionals_and_strict_unknown_fields() {
    let source = r#"
        use std.json

        fn next_default() -> i64 { 77 }

        struct Config {
            host: String,
            port: i64 = 8080,
            retries: i64 = next_default(),
            tag: ?String,
            override_port: ?i64 = 9090,
        }
        derive Json, Eq, PartialEq for Config

        struct Empty {}
        derive Json, Eq, PartialEq for Empty

        fn main() -> i64 {
            let c0 = Config {
                host: "localhost",
                port: 3000,
                retries: 5,
                tag: "prod",
                override_port: null,
            }
            let raw0 = c0.to_json().unwrap()
            if raw0 != "{{\"host\":\"localhost\",\"port\":3000,\"retries\":5,\"tag\":\"prod\",\"override_port\":null}}" {
                return 1
            }
            let back0 = Config.from_json(raw0).unwrap()
            if back0 != c0 { return 2 }

            -- Missing fields with defaults and missing `?T` defaulting to null
            let c1 = Config.from_json("{{\"host\":\"db.internal\",\"extra\":[1,2,3]}}").unwrap()
            if (c1.host != "db.internal" or c1.port != 8080 or c1.retries != 77 or c1.tag != null or c1.override_port != 9090) {
                return 3
            }

            -- Explicit null overrides `?i64 = 9090` default
            let c2 = Config.from_json("{{\"host\":\"cache\",\"override_port\":null}}").unwrap()
            if c2.override_port != null { return 4 }

            -- Duplicate keys in JSON object: last key wins per RFC 8259 / std.json
            let c3 = Config.from_json("{{\"host\":\"first\",\"port\":1,\"host\":\"second\",\"port\":2}}").unwrap()
            if c3.host != "second" or c3.port != 2 { return 5 }

            -- Strict mode rejects unknown fields with exact path and field name
            let unk_ok = Config.from_json("{{\"host\":\"ok\",\"bogus\":true}}", allow_unknown_fields = false) match {
                _! => false,
                error JsonError.UnknownField(path, field) => path == "$" and field == "bogus",
                error _ => false,
            }
            if not unk_ok { return 6 }

            -- Required field missing -> MissingField
            let miss_ok = Config.from_json("{{\"port\":80}}") match {
                _! => false,
                error JsonError.MissingField(path, field) => path == "$" and field == "host",
                error _ => false,
            }
            if not miss_ok { return 7 }

            -- Empty struct roundtrip and strict check
            let e = Empty {}
            if e.to_json().unwrap() != "{{}}" { return 8 }
            if Empty.from_json("{{}}").unwrap() != e { return 9 }
            let empty_unk = Empty.from_json("{{\"a\":1}}", allow_unknown_fields = false) match {
                _! => false,
                error JsonError.UnknownField(p, f) => p == "$" and f == "a",
                error _ => false,
            }
            if not empty_unk { return 10 }

            -- Pretty printing & std.json.encode
            let pretty = e.to_json_pretty().unwrap()
            if pretty != "{{}}" { return 11 }
            let c_pretty = json.encode(c0, indent = 2).unwrap()
            if not c_pretty.contains("\n  \"host\": \"localhost\"") { return 12 }
            if Config.from_json(c_pretty).unwrap() != c0 { return 13 }

            42
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn derive_json_large_object_hash_index_and_nested_paths() {
    let source = r#"
        struct Wide {
            f00: i64, f01: i64, f02: i64, f03: i64, f04: i64,
            f05: i64, f06: i64, f07: i64, f08: i64, f09: i64,
            f10: i64, f11: i64, f12: i64, f13: i64, f14: i64,
            f15: i64, f16: i64, f17: i64, f18: i64, f19: i64,
        }
        derive Json, Eq, PartialEq for Wide

        struct Wrapper {
            name: String,
            wide: Wide,
        }
        derive Json, Eq, PartialEq for Wrapper

        fn main() -> i64 {
            let w = Wide {
                f00: 0, f01: 1, f02: 2, f03: 3, f04: 4,
                f05: 5, f06: 6, f07: 7, f08: 8, f09: 9,
                f10: 10, f11: 11, f12: 12, f13: 13, f14: 14,
                f15: 15, f16: 16, f17: 17, f18: 18, f19: 19,
            }
            let wrap = Wrapper { name: "test", wide: w }
            let encoded = wrap.to_json().unwrap()
            let decoded = Wrapper.from_json(encoded, allow_unknown_fields = false).unwrap()
            if decoded != wrap { return 1 }

            -- Nested path in TypeMismatch
            let bad = Wrapper.from_json("{{\"name\":\"ok\",\"wide\":{{\"f00\":\"bad\"}}}}") match {
                _! => false,
                error JsonError.TypeMismatch(path, expected, actual) => path == "$.wide.f00" and expected == "i64" and actual == "string",
                error _ => false,
            }
            if not bad { return 2 }

            -- Nested UnknownField propagation when allow_unknown_fields = false
            let with_rogue = encoded.slice(0, encoded.len() - 2) ++ ",\"rogue\":1}}}}"
            let unk_nested = Wrapper.from_json(with_rogue, allow_unknown_fields = false) match {
                _! => false,
                error JsonError.UnknownField(path, field) => path == "$.wide" and field == "rogue",
                error _ => false,
            }
            if not unk_nested { return 3 }

            42
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn derive_json_enums_newtypes_tuples_and_trait_bounds() {
    let source = r#"
        newtype UserId = i64
        derive Json, Eq, PartialEq for UserId

        typealias Pair = (String, i64)
        derive Json for Pair

        enum Shape {
            Origin,
            Circle(radius: i64),
            Rect(w: i64, h: i64),
        }
        derive Json, Eq, PartialEq for Shape

        struct Canvas {
            owner: UserId,
            bounds: (i64, i64),
            shapes: (Shape, Shape, Shape),
        }
        derive Json, Eq, PartialEq for Canvas

        fn serialize_any_json(item: Json) -> !JsonError String {
            item.to_json()
        }

        fn main() -> i64 {
            let uid = UserId(99)
            if uid.to_json().unwrap() != "99" { return 1 }
            if UserId.from_json("99").unwrap() != uid { return 2 }

            let p: Pair = ("score", 100)
            if p.to_json().unwrap() != "[\"score\",100]" { return 3 }
            let p2 = Pair.from_json("[\"score\",100]").unwrap()
            if p2 != p { return 4 }

            let s0 = Shape.Origin
            let s1 = Shape.Circle(7)
            let s2 = Shape.Rect(10, 20)
            if s0.to_json().unwrap() != "\"Origin\"" { return 5 }
            if s1.to_json().unwrap() != "{{\"Circle\":7}}" { return 6 }
            if s2.to_json().unwrap() != "{{\"Rect\":[10,20]}}" { return 7 }

            if Shape.from_json("\"Origin\"").unwrap() != s0 { return 8 }
            if Shape.from_json("{{\"Circle\":7}}").unwrap() != s1 { return 9 }
            if Shape.from_json("{{\"Rect\":[10,20]}}").unwrap() != s2 { return 10 }

            let c = Canvas {
                owner: uid,
                bounds: (640, 480),
                shapes: (s0, s1, s2),
            }
            let c_json = serialize_any_json(c).unwrap()
            if Canvas.from_json(c_json).unwrap() != c { return 11 }

            -- Enum error cases: InvalidVariant (string and object), wrong arity, multi-key object
            let inv1 = Shape.from_json("\"Triangle\"") match {
                _! => false,
                error JsonError.InvalidVariant(path, ty, v) => path == "$" and ty == "Shape" and v == "Triangle",
                error _ => false,
            }
            if not inv1 { return 12 }

            let inv2 = Shape.from_json("{{\"Hexagon\":6}}") match {
                _! => false,
                error JsonError.InvalidVariant(path, ty, v) => path == "$" and ty == "Shape" and v == "Hexagon",
                error _ => false,
            }
            if not inv2 { return 13 }

            let bad_unit = Shape.from_json("{{\"Origin\":null}}") match {
                _! => false,
                error JsonError.TypeMismatch(path, expected, actual) => path == "$" and expected == "string" and actual == "object",
                error _ => false,
            }
            if not bad_unit { return 14 }

            let bad_payload = Shape.from_json("\"Circle\"") match {
                _! => false,
                error JsonError.TypeMismatch(path, expected, actual) => path == "$" and expected == "object" and actual == "string",
                error _ => false,
            }
            if not bad_payload { return 15 }

            let bad_multi_key = Shape.from_json("{{\"Circle\":1,\"Rect\":[2,3]}}") match {
                _! => false,
                error JsonError.TypeMismatch(path, _, actual) => path == "$" and actual == "object",
                error _ => false,
            }
            if not bad_multi_key { return 16 }

            let bad_arity = Shape.from_json("{{\"Rect\":[10]}}") match {
                _! => false,
                error JsonError.TypeMismatch(path, expected, actual) => path == "$.Rect" and expected == "array of length 2" and actual == "array",
                error _ => false,
            }
            if not bad_arity { return 17 }

            42
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn derive_json_recursive_types_forward_decls_and_cycle_detection() {
    let source = r#"
        use std.json

        -- Forward reference before `Node` is declared
        fn decode_list(raw: String) -> !JsonError Node {
            Node.from_json(raw)
        }

        struct Node {
            val: i64,
            next: ?Node,
        }
        derive Json, Eq, PartialEq for Node

        -- Custom `impl Json` + static `from_json` composed inside a derived struct
        struct HexByte {
            b: u8,
        }
        impl Json for HexByte {
            pub fn to_json(self) -> !JsonError String {
                self.b.to_json()
            }
        }
        impl HexByte {
            pub fn from_json(raw: String) -> !JsonError HexByte {
                HexByte { b: u8.from_json(raw)! }
            }
        }
        struct Packet {
            header: HexByte,
            chain: Node,
        }
        derive Json for Packet

        fn main() -> i64 {
            let list = decode_list("{{\"val\":1,\"next\":{{\"val\":2,\"next\":{{\"val\":3,\"next\":null}}}}}}").unwrap()
            if list.val != 1 { return 1 }
            let n2? = list.next else { return 2 }
            if n2.val != 2 { return 3 }
            let n3? = n2.next else { return 4 }
            if n3.val != 3 or n3.next != null { return 5 }

            if Node.from_json(list.to_json().unwrap()).unwrap() != list { return 6 }

            let pkt = Packet { header: HexByte { b: (255).as(u8) }, chain: list }
            let pkt2 = Packet.from_json(pkt.to_json().unwrap()).unwrap()
            if pkt2.header.b != (255).as(u8) or pkt2.chain.val != 1 { return 7 }

            -- Cyclic recursive struct -> CycleDetected (no stack overflow!)
            let cyc = Node { val: 10, next: null }
            cyc.next = cyc
            let cyc_ok = cyc.to_json() match {
                _! => false,
                error JsonError.CycleDetected => true,
                error _ => false,
            }
            if not cyc_ok { return 8 }

            -- Cyclic dynamic List -> CycleDetected in std.json.stringify
            let dyn_list = List()
            dyn_list.push(dyn_list)
            let dyn_cyc = json.stringify(dyn_list) match {
                _! => false,
                error JsonError.CycleDetected => true,
                error _ => false,
            }
            if not dyn_cyc { return 9 }

            42
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn derive_json_all_integer_widths_floats_surrogates_and_depth_limits() {
    let source = r#"
        use std.json
        use std.math

        struct AllScalars {
            a_i8: i8,
            a_i16: i16,
            a_i32: i32,
            a_i64: i64,
            a_i128: i128,
            a_isize: isize,
            a_u8: u8,
            a_u16: u16,
            a_u32: u32,
            a_u64: u64,
            a_u128: u128,
            a_usize: usize,
            a_f32: f32,
            a_f64: f64,
            a_bool: bool,
            a_char: char,
            a_str: String,
            a_unit: Unit,
        }
        derive Json, PartialEq for AllScalars

        fn main() -> i64 {
            let min128 = (1).as(i128).wrapping_shl(127)
            let max128 = (0).as(u128).wrapping_sub((1).as(u128))
            let max64 = (0).as(u64).wrapping_sub((1).as(u64))
            let s = AllScalars {
                a_i8: (-128).as(i8),
                a_i16: (-32768).as(i16),
                a_i32: (-2147483648).as(i32),
                a_i64: -9223372036854775807,
                a_i128: min128,
                a_isize: (-1024).as(isize),
                a_u8: (255).as(u8),
                a_u16: (65535).as(u16),
                a_u32: (4294967295).as(u32),
                a_u64: max64,
                a_u128: max128,
                a_usize: (2048).as(usize),
                a_f32: (1.5).as(f32),
                a_f64: -3.25,
                a_bool: true,
                a_char: 'λ',
                a_str: "hello\n\"world\"",
                a_unit: (),
            }
            let raw = s.to_json().unwrap()
            let s2 = AllScalars.from_json(raw).unwrap()
            if s2 != s { return 1 }

            -- Out-of-range integer checks
            let oor_u8 = u8.from_json("256") match {
                _! => false,
                error JsonError.IntegerOutOfRange(p, v, t) => p == "$" and v == "256" and t == "u8",
                error _ => false,
            }
            if not oor_u8 { return 2 }

            let oor_neg_u64 = u64.from_json("-1") match {
                _! => false,
                error JsonError.IntegerOutOfRange(p, v, t) => p == "$" and v == "-1" and t == "u64",
                error _ => false,
            }
            if not oor_neg_u64 { return 3 }

            let oor_huge = i128.from_json("999999999999999999999999999999999999999999999") match {
                _! => false,
                error JsonError.IntegerOutOfRange(_, _, t) => t == "i128",
                error _ => false,
            }
            if not oor_huge { return 4 }

            -- Fractional number rejected for integer target with TypeMismatch
            let frac_err = i64.from_json("1.5") match {
                _! => false,
                error JsonError.TypeMismatch(p, exp, act) => p == "$" and exp == "i64" and act == "float",
                error _ => false,
            }
            if not frac_err { return 5 }

            -- Non-finite float rejected on serialization (NaN, +Inf, -Inf)
            let nan_err = math.nan().to_json() match {
                _! => false,
                error JsonError.UnsupportedValue(_) => true,
                error _ => false,
            }
            let inf_err = math.infinity().to_json() match {
                _! => false,
                error JsonError.UnsupportedValue(_) => true,
                error _ => false,
            }
            let ninf_err = math.neg_infinity().to_json() match {
                _! => false,
                error JsonError.UnsupportedValue(_) => true,
                error _ => false,
            }
            if not nan_err or not inf_err or not ninf_err { return 6 }

            -- Valid UTF-16 surrogate pair vs lone surrogates
            let emoji = String.from_json("\"\\uD83D\\uDE00\"").unwrap()
            if emoji != "😀" { return 7 }
            let lone_hi = String.from_json("\"\\uD83D\"") match {
                _! => false,
                error JsonError.InvalidEscape(_, _) => true,
                error _ => false,
            }
            if not lone_hi { return 8 }
            let lone_lo = String.from_json("\"\\uDE00\"") match {
                _! => false,
                error JsonError.InvalidEscape(_, _) => true,
                error _ => false,
            }
            if not lone_lo { return 9 }

            -- Unescaped control char < 0x20 rejected
            let ctrl_err = String.from_json("\"a\tb\"") match {
                _! => false,
                error JsonError.InvalidString(_, _) => true,
                error _ => false,
            }
            if not ctrl_err { return 10 }

            -- Depth limit 128 ok, 129 rejected
            let ok_128 = "[".repeat(128) ++ "]".repeat(128)
            let _ = json.parse(ok_128).unwrap()
            let deep_129 = "[".repeat(129) ++ "]".repeat(129)
            let depth_err = json.parse(deep_129) match {
                _! => false,
                error JsonError.DepthLimitExceeded => true,
                error _ => false,
            }
            if not depth_err { return 11 }

            42
        }
    "#;
    assert_eq!(common::run_value(source).unwrap(), 42);
}

#[test]
fn derive_json_rejects_unsupported_targets_and_components_at_compile_time() {
    let cases = [
        (
            "struct Bad { x: Any }\nderive Json for Bad\nfn main() -> i64 { 0 }",
            "does not support field type `Any`",
        ),
        (
            "struct Bad { f: fn(i64) -> i64 }\nderive Json for Bad\nfn main() -> i64 { 0 }",
            "does not support field type",
        ),
        (
            "struct Bad { k: Continuation }\nderive Json for Bad\nfn main() -> i64 { 0 }",
            "does not support field type `Continuation`",
        ),
        (
            "struct Bad { items: List }\nderive Json for Bad\nfn main() -> i64 { 0 }",
            "untyped runtime field type `List`",
        ),
        (
            "struct Bad { entries: Map }\nderive Json for Bad\nfn main() -> i64 { 0 }",
            "untyped runtime field type `Map`",
        ),
        (
            "struct Bad { fut: Future }\nderive Json for Bad\nfn main() -> i64 { 0 }",
            "does not support field type `Future",
        ),
        (
            "struct Plain { x: i64 }\nstruct Holder { p: Plain }\nderive Json for Holder\nfn main() -> i64 { 0 }",
            "requires a checked global Json implementation",
        ),
        (
            "struct Half { x: i64 }\nimpl Json for Half { pub fn to_json(self) -> !JsonError String { \"0\" } }\nstruct Holder { h: Half }\nderive Json for Holder\nfn main() -> i64 { 0 }",
            "Half.from_json",
        ),
        (
            "struct Scoped { x: i64 }\nimpl Scoped { pub fn from_json(s: String) -> !JsonError Scoped { let _ = s; Scoped { x: 0 } } }\nmod m {\n    extend Json for Scoped { pub fn to_json(self) -> !JsonError String { \"0\" } }\n    struct Holder { s: Scoped }\n    derive Json for Holder\n}\nfn main() -> i64 { 0 }",
            "local `extend Json`",
        ),
    ];

    for (source, expected_substr) in cases {
        let result = Driver::new().compile(source);
        assert!(result.has_errors, "unexpectedly accepted:\n{source}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.message.contains(expected_substr)),
            "expected `{expected_substr}` in diagnostics for:\n{source}\ngot: {:?}",
            result.diagnostics
        );
        assert!(result.into_artifact().is_err());
    }
}

#[test]
fn derive_json_standalone_nsbc_archive_roundtrip_without_sources() {
    let pkg = TempPackage::new("exe");
    pkg.write(
        "src/main.ns",
        r#"
        use std.json

        newtype TagId = u16
        derive Json, Eq, PartialEq for TagId

        enum Role {
            Admin,
            Member(level: u8),
            Pair(a: String, b: i64),
        }
        derive Json, Eq, PartialEq for Role

        struct Account {
            id: TagId,
            role: Role,
            active: bool = true,
            note: ?String,
        }
        derive Json, Eq, PartialEq for Account

        fn roundtrip_via_trait(v: Json) -> !JsonError String {
            v.to_json_pretty(indent = 2)
        }

        fn main() -> i64 {
            let acc = Account {
                id: TagId((512).as(u16)),
                role: Role.Pair("team", 9),
                active: true,
                note: null,
            }
            let pretty = roundtrip_via_trait(acc).unwrap()
            let back = Account.from_json(pretty, allow_unknown_fields = false).unwrap()
            if back != acc {
                panic("archive roundtrip mismatch")
            }
            0
        }
        "#,
    );

    let archive_path = pkg.0.join("standalone_app.nsbc");
    Driver::new()
        .compile_package_to_archive(&pkg.0, &archive_path)
        .expect("compile_package_to_archive succeeded");
    assert!(archive_path.exists());

    // Delete all `.ns` sources and `package.toml` so execution relies strictly on `.nsbc`!
    std::fs::remove_dir_all(pkg.0.join("src")).unwrap();
    std::fs::remove_file(pkg.0.join("package.toml")).unwrap();

    let bytes = std::fs::read(&archive_path).unwrap();
    let restored = nsbc_io::read_artifact(&bytes).expect("read_artifact succeeded");
    assert_ne!(
        restored.type_pool.well_known.json,
        type_pool::TypeIndex::INVALID
    );
    let run_res = Driver::new().run_artifact(restored);
    assert!(matches!(run_res, RunResult::Ok), "{run_res:?}");
}

fn force_gc_at_print(ctx: &mut interpreter::BuiltinCtx<'_>) -> Result<(), interpreter::VmError> {
    ctx.require_arity(1)?;
    assert!(ctx.collect_garbage()?);
    ctx.return_unit();
    Ok(())
}

#[test]
fn active_json_writer_ancestors_and_tapes_survive_forced_moving_collections() {
    let source = r#"
        struct Leaf {
            text: String,
        }
        impl Json for Leaf {
            pub fn to_json(self) -> !JsonError String {
                print("")
                self.text.to_json()
            }
        }
        impl Leaf {
            pub fn from_json(raw: String) -> !JsonError Leaf {
                print("")
                Leaf { text: String.from_json(raw)! }
            }
        }

        struct Container {
            first: Leaf,
            second: Leaf,
            nums: (i64, i64),
        }
        derive Json for Container

        fn main() -> i64 {
            let c = Container {
                first: Leaf { text: "alpha_" ++ "beta" },
                second: Leaf { text: "gamma_" ++ "delta" },
                nums: (11, 31),
            }
            let encoded = c.to_json().unwrap()
            print("")
            let decoded = Container.from_json(encoded, allow_unknown_fields = false).unwrap()
            if (decoded.first.text == "alpha_beta" and decoded.second.text == "gamma_delta" and decoded.nums == (11, 31)) {
                42
            } else {
                0
            }
        }
    "#;

    let compiled = Driver::new().compile(source);
    assert!(!compiled.has_errors, "{:?}", compiled.diagnostics);
    let mut engine = initialization::Engine::with_defaults();
    let entry = driver::install_artifact(engine.vm_mut(), compiled.into_artifact().unwrap())
        .unwrap()
        .unwrap();
    engine
        .vm_mut()
        .register_builtin(runtime::ids::PRINT, force_gc_at_print);
    let before = engine.vm_mut().completed_collections();
    let task = engine.vm_mut().spawn_root(entry);
    assert!(matches!(
        engine.vm_mut().run(),
        interpreter::VmResult::Finished
    ));
    assert!(engine.vm_mut().completed_collections() >= before + 5);
    assert_eq!(engine.vm_mut().task_result_i64(task).unwrap(), 42);
    assert_eq!(engine.vm_mut().active_stack_count(), 0);
}
