use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use driver::{
    Driver, EngineConfig, ModuleLoadState, ModuleLoader, OptLevel, RunResult, disassemble_bytes,
    is_modular_archive, load_package_sources, load_package_workspace, read_modular_artifact,
    read_modular_exports_only, write_modular_artifact,
};
use initialization::Engine;
use interpreter::VmResult;
use nsbc::SymbolLinkSelector;

struct TempDirGuard(PathBuf);

impl TempDirGuard {
    fn new(prefix: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        loop {
            let candidate = std::env::temp_dir().join(format!(
                "nessa-mod-test-{prefix}-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => return Self(candidate),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("failed to create temp test dir: {e}"),
            }
        }
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_pkg_file(root: &Path, rel: &str, content: &str) {
    let full = root.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(full, content).expect("write package file");
}

#[test]
fn std_compiles_to_valid_modular_nsbc_and_roundtrips() {
    let tmp = TempDirGuard::new("std-roundtrip");
    let driver = Driver::new()
        .with_cache_dir(tmp.path().join("cache"))
        .with_opt_level(OptLevel::O2);
    let std_art = driver
        .compile_std_modular()
        .expect("std should compile to ModularArtifact");

    assert!(std_art.exports.is_privileged_std);
    assert!(
        std_art.exports.modules.len() >= 25,
        "expected per-mod descriptors for std submodules, got {}",
        std_art.exports.modules.len()
    );

    let bytes = write_modular_artifact(&std_art).expect("encode std.nsbc");
    assert!(is_modular_archive(&bytes));

    let exports_only = read_modular_exports_only(&bytes).expect("read header-only exports");
    assert_eq!(exports_only.interface_hash, std_art.exports.interface_hash);
    assert_eq!(exports_only.modules.len(), std_art.exports.modules.len());

    let restored = read_modular_artifact(&bytes).expect("round-trip std.nsbc");
    assert_eq!(
        restored.exports.qualified_name,
        std_art.exports.qualified_name
    );
    assert_eq!(
        restored.exports.interface_hash,
        std_art.exports.interface_hash
    );

    let disasm = disassemble_bytes(&bytes).expect("disassemble modular std.nsbc");
    assert!(disasm.contains("; NSBC Modular Disassembly"));
}

#[test]
fn separate_compilation_purely_against_nsbc_without_dependency_sources() {
    let tmp = TempDirGuard::new("no-dep-sources");
    let ws = tmp.path();

    let core_dir = ws.join("core_pkg");
    write_pkg_file(
        &core_dir,
        "package.toml",
        "[package]\nname = \"core_pkg\"\ndomain = \"org.test\"\nversion = \"0.1.0\"\ntype = \"lib\"\n",
    );
    write_pkg_file(
        &core_dir,
        "src/lib.ns",
        r#"
pub mod geom

pub enum CoreErr {
    pub InvalidInput(reason: String),
}

pub enum RouteKind {
    pub fast_path,
    pub weighted_path(factor: i64),
}

pub trait Scorable {
    fn base_score(self) -> i64
    derive fn boosted_score(self) -> i64 {
        self.base_score() * 2 + 1
    }
}

pub effect step_emit(amount: i64, catch k) -> i64
pub effect audit_emit(note: String) -> i64

const base_effects = [audit_emit]
pub const interactive_effects = base_effects ++ [step_emit]
pub const route_errors = [CoreErr]

pub var shared_counter: i64 = 10

pub fn bump_counter(.delta: i64 = 5) -> i64 {
    shared_counter = shared_counter + delta
    shared_counter
}

pub fn run_with_effect(seed: i64) -> #interactive_effects i64 {
    let bonus = audit_emit("step")#
    let a = step_emit(seed + bonus)#
    let b = step_emit(a + 3)#
    b
}
"#,
    );
    write_pkg_file(
        &core_dir,
        "src/geom.ns",
        r#"
use @Scorable
use @CoreErr
use @RouteKind

pub struct Point {
    pub x: i64,
    pub y: i64 = 7,
    private tag_secret: i64 = 3,
}
derive Eq, Hash, Display, Json for Point

impl Point {
    pub fn make(x: i64, .y: i64 = 7) -> Point {
        Point { x: x, y: y, tag_secret: 9 }
    }

    pub fn total(self) -> i64 {
        self.x + self.y + self.tag_secret
    }
}

impl Scorable for Point {
    pub fn base_score(self) -> i64 {
        self.x + self.y
    }
}

pub fn classify_point(pt: Point, mode: RouteKind) -> !CoreErr i64 {
    if pt.x < 0 {
        return error CoreErr.InvalidInput("negative x")
    }
    mode match {
        RouteKind.fast_path => pt.total(),
        RouteKind.weighted_path(w) => pt.total() * w,
    }
}
"#,
    );

    let driver = Driver::new()
        .with_cache_dir(ws.join("cache"))
        .with_opt_level(OptLevel::O2);
    let std_art = driver.compile_std_modular().expect("compile std");
    let core_sources = load_package_sources(&core_dir).expect("load core_pkg sources");
    let (core_art, _) = driver
        .compile_package_modular(&core_sources, &[], &BTreeSet::new())
        .expect("compile core_pkg to ModularArtifact");

    let core_nsbc_bytes = write_modular_artifact(&core_art).expect("serialize core_pkg.nsbc");

    // Delete `core_pkg` sources completely so `app_pkg` CANNOT touch any `.ns` file of `core_pkg`!
    fs::remove_dir_all(&core_dir).expect("delete core_pkg source directory");
    assert!(!core_dir.exists());

    let restored_core = Arc::new(
        read_modular_artifact(&core_nsbc_bytes).expect("restore core_pkg purely from .nsbc"),
    );

    let app_dir = ws.join("app_pkg");
    write_pkg_file(
        &app_dir,
        "package.toml",
        "[package]\nname = \"app_pkg\"\ndomain = \"org.test\"\nversion = \"0.1.0\"\ntype = \"exe\"\n\n[dependencies]\n\"org.test/core_pkg\" = \"0.1.0\"\n",
    );
    write_pkg_file(
        &app_dir,
        "src/main.ns",
        r#"
use core_pkg.{
    CoreErr, RouteKind, Scorable, audit_emit, bump_counter, interactive_effects, route_errors,
    run_with_effect, shared_counter, step_emit,
}
use core_pkg.geom.{Point, classify_point}

fn forward_effect(seed: i64) -> #interactive_effects !route_errors i64 {
    if seed < 0 {
        return error CoreErr.InvalidInput("neg seed")
    }
    run_with_effect(seed)#
}

fn measure_dyn(item: Scorable) -> i64 {
    item.boosted_score()
}

fn main() {
    let p1 = Point.make(4)
    let p2 = Point.make(4, y = 7)
    assert(p1 == p2)

    let encoded = p1.to_json()!
    let decoded = std.json.decode(Point, encoded)!
    assert(decoded.x == 4 and decoded.y == 7)

    let dyn_score = measure_dyn(p1)
    assert(dyn_score == 23)

    let route_val = classify_point(p1, RouteKind.weighted_path(2))!
    assert(route_val == 40)

    let err_probe = classify_point(Point.make(-1), RouteKind.fast_path) ! {
        v! => v,
        error CoreErr.InvalidInput(msg) => {
            assert(msg == "negative x")
            -99
        },
    }
    assert(err_probe == -99)

    let eff_res: !route_errors i64 = (forward_effect(5))# {
        audit_emit(note) => note.len().as(i64),
        step_emit(v, k) => k(v * 2),
    }
    let eff_val = eff_res!
    assert(eff_val == 42)

    let bumped1 = bump_counter()
    let bumped2 = bump_counter(delta = 3)
    assert(bumped1 == 15 and bumped2 == 18 and shared_counter == 18)

    let iter_sum = [1, 2, 3, 4].iter().map(|n| n * 3).filter(|n| n > 3).fold(0, |acc, x| acc + x)
    assert(iter_sum == 27)
}
"#,
    );

    let app_sources = load_package_sources(&app_dir).expect("load app_pkg sources");
    let (app_art, _) = driver
        .compile_package_modular(
            &app_sources,
            &[Arc::clone(&restored_core)],
            &BTreeSet::new(),
        )
        .expect("compile app_pkg purely against core_pkg.nsbc");
    let app_bytes = write_modular_artifact(&app_art).expect("encode app_pkg.nsbc");
    let restored_app = Arc::new(read_modular_artifact(&app_bytes).expect("restore app_pkg.nsbc"));

    let mut engine = Engine::new(EngineConfig::default());
    let mut loader = ModuleLoader::new();
    loader
        .register_artifacts(&[std_art, restored_core, restored_app])
        .expect("register modular artifacts");
    let prepared = loader
        .load_module(engine.vm_mut(), "org.test/app_pkg", &[])
        .expect("load & init app_pkg");
    let main_fid = prepared.main_func.expect("exe main FuncId");
    engine.vm_mut().spawn_root(main_fid);
    if let VmResult::Error(err) = engine.vm_mut().run() {
        panic!("modular VM execution failed: {err:?}");
    }
}

#[test]
fn editing_dependency_body_keeps_interface_hash_and_recompiles_zero_downstream_packages() {
    let tmp = TempDirGuard::new("incremental-dag");
    let ws = tmp.path();
    let cache_dir = ws.join("nsbc_cache");
    let out_file = ws.join("result.txt");

    let core_dir = ws.join("core");
    write_pkg_file(
        &core_dir,
        "package.toml",
        "[package]\nname = \"core\"\ndomain = \"org.test\"\nversion = \"0.1.0\"\ntype = \"lib\"\n",
    );
    write_pkg_file(
        &core_dir,
        "src/lib.ns",
        "pub fn base_unit() -> i64 {\n    100\n}\n",
    );

    let ext_a_dir = ws.join("ext_alpha");
    write_pkg_file(
        &ext_a_dir,
        "package.toml",
        "[package]\nname = \"ext_alpha\"\ndomain = \"org.test\"\nversion = \"0.1.0\"\ntype = \"lib\"\n\n[dependencies]\ncore = { path = \"../core\" }\n",
    );
    write_pkg_file(
        &ext_a_dir,
        "src/lib.ns",
        "use @core\npub fn alpha_val() -> i64 {\n    core.base_unit() + 1\n}\n",
    );

    let ext_b_dir = ws.join("ext_beta");
    write_pkg_file(
        &ext_b_dir,
        "package.toml",
        "[package]\nname = \"ext_beta\"\ndomain = \"org.test\"\nversion = \"0.1.0\"\ntype = \"lib\"\n\n[dependencies]\ncore = { path = \"../core\" }\n",
    );
    write_pkg_file(
        &ext_b_dir,
        "src/lib.ns",
        "use @core\npub fn beta_val() -> i64 {\n    core.base_unit() + 2\n}\n",
    );

    let boot_dir = ws.join("boot");
    write_pkg_file(
        &boot_dir,
        "package.toml",
        "[package]\nname = \"boot\"\ndomain = \"org.test\"\nversion = \"0.1.0\"\ntype = \"exe\"\n\n[dependencies]\next_alpha = { path = \"../ext_alpha\" }\next_beta = { path = \"../ext_beta\" }\n",
    );
    let out_escaped = out_file.to_string_lossy().replace('\\', "/");
    write_pkg_file(
        &boot_dir,
        "src/main.ns",
        &format!(
            "use @ext_alpha\nuse @ext_beta\nfn main() {{\n    let total = ext_alpha.alpha_val() + ext_beta.beta_val()\n    std.fs.write_string(\"{out_escaped}\", \"{{total}}\")!\n}}\n"
        ),
    );

    let driver = Driver::new()
        .with_cache_dir(cache_dir)
        .with_opt_level(OptLevel::O2);

    // 1. Cold build: compiles all 4 workspace packages.
    let loaded1 = load_package_workspace(&boot_dir).expect("load workspace 1");
    let build1 = driver
        .compile_modular_graph_cached(&loaded1.root, &loaded1.dependencies, &BTreeSet::new())
        .expect("build 1");
    assert_eq!(build1.compiled_packages.len(), 4);
    assert_eq!(build1.cached_packages.len(), 0);
    let core_hash_1 = build1
        .artifacts
        .iter()
        .find(|a| a.exports.qualified_name == "org.test/core")
        .unwrap()
        .exports
        .interface_hash;

    assert!(matches!(driver.run_modular_graph(&build1), RunResult::Ok));
    assert_eq!(fs::read_to_string(&out_file).unwrap(), "203");

    // 2. Edit ONLY `core` implementation body + add private helper (public signature unchanged!).
    write_pkg_file(
        &core_dir,
        "src/lib.ns",
        "private fn secret_offset() -> i64 { 50 }\npub fn base_unit() -> i64 {\n    200 + secret_offset()\n}\n",
    );
    let loaded2 = load_package_workspace(&boot_dir).expect("load workspace 2");
    let build2 = driver
        .compile_modular_graph_cached(&loaded2.root, &loaded2.dependencies, &BTreeSet::new())
        .expect("build 2");
    let core_hash_2 = build2
        .artifacts
        .iter()
        .find(|a| a.exports.qualified_name == "org.test/core")
        .unwrap()
        .exports
        .interface_hash;
    assert_eq!(
        core_hash_1, core_hash_2,
        "implementation-only change must keep core interface_hash identical"
    );
    assert_eq!(
        build2.compiled_packages,
        vec!["org.test/core".to_string()],
        "editing core body must recompile ONLY core and 0 downstream packages"
    );
    assert_eq!(build2.cached_packages.len(), 3);

    assert!(matches!(driver.run_modular_graph(&build2), RunResult::Ok));
    assert_eq!(fs::read_to_string(&out_file).unwrap(), "503");

    // 3. Edit ONLY `ext_alpha` body -> recompiles ONLY `ext_alpha` (not `core`, `ext_beta`, or `boot`!).
    write_pkg_file(
        &ext_a_dir,
        "src/lib.ns",
        "use @core\npub fn alpha_val() -> i64 {\n    core.base_unit() + 10\n}\n",
    );
    let loaded3 = load_package_workspace(&boot_dir).expect("load workspace 3");
    let build3 = driver
        .compile_modular_graph_cached(&loaded3.root, &loaded3.dependencies, &BTreeSet::new())
        .expect("build 3");
    assert_eq!(
        build3.compiled_packages,
        vec!["org.test/ext_alpha".to_string()],
        "editing 1 extension must recompile ONLY that 1 extension"
    );
    assert_eq!(build3.cached_packages.len(), 3);

    assert!(matches!(driver.run_modular_graph(&build3), RunResult::Ok));
    assert_eq!(fs::read_to_string(&out_file).unwrap(), "512");
}

#[test]
fn per_mod_lazy_loading_skips_unreferenced_modules_and_supports_live_incremental_load() {
    let tmp = TempDirGuard::new("lazy-mod-load");
    let ws = tmp.path();
    let log_file = ws.join("init_order.txt");
    let log_escaped = log_file.to_string_lossy().replace('\\', "/");

    let pkg_dir = ws.join("multi_pkg");
    write_pkg_file(
        &pkg_dir,
        "package.toml",
        "[package]\nname = \"multi_pkg\"\ndomain = \"org.test\"\nversion = \"0.1.0\"\ntype = \"exe\"\n",
    );
    write_pkg_file(
        &pkg_dir,
        "src/main.ns",
        &format!(
            r#"
pub mod used_leaf
pub mod used_mid
pub mod heavy_unused
pub mod plugin_late

fn append_log(tag: String) {{
    let prev = if std.fs.exists("{log_escaped}") {{
        std.fs.read_to_string("{log_escaped}") ! {{ s! => s, error _ => "" }}
    }} else {{
        ""
    }}
    let _ = std.fs.write_string("{log_escaped}", "{{prev}}{{tag}};") ! {{ u! => u, error _ => () }}
}}

fn __init__() {{
    append_log("root")
}}

fn main() {{
    assert(used_mid.mid_val == 42)
    append_log("main")
}}
"#
        ),
    );
    write_pkg_file(
        &pkg_dir,
        "src/used_leaf.ns",
        &format!(
            r#"
pub let leaf_seed: i64 = 20

fn __init__() {{
    let prev = if std.fs.exists("{log_escaped}") {{
        std.fs.read_to_string("{log_escaped}") ! {{ s! => s, error _ => "" }}
    }} else {{
        ""
    }}
    let _ = std.fs.write_string("{log_escaped}", "{{prev}}used_leaf;") ! {{ u! => u, error _ => () }}
}}
"#
        ),
    );
    write_pkg_file(
        &pkg_dir,
        "src/used_mid.ns",
        &format!(
            r#"
use @used_leaf

pub let mid_val: i64 = used_leaf.leaf_seed * 2 + 2

fn __init__() {{
    let prev = std.fs.read_to_string("{log_escaped}") ! {{ s! => s, error _ => "" }}
    let _ = std.fs.write_string("{log_escaped}", "{{prev}}used_mid;") ! {{ u! => u, error _ => () }}
}}
"#
        ),
    );
    write_pkg_file(
        &pkg_dir,
        "src/heavy_unused.ns",
        r#"
fn __init__() {
    panic("heavy_unused must never be loaded or initialized when unreferenced!")
}

pub fn never_called() -> i64 {
    999
}
"#,
    );
    write_pkg_file(
        &pkg_dir,
        "src/plugin_late.ns",
        &format!(
            r#"
use @used_leaf

pub let late_bonus: i64 = used_leaf.leaf_seed + 5

fn __init__() {{
    let prev = std.fs.read_to_string("{log_escaped}") ! {{ s! => s, error _ => "" }}
    let _ = std.fs.write_string("{log_escaped}", "{{prev}}plugin_late;") ! {{ u! => u, error _ => () }}
}}

pub fn run_plugin() -> i64 {{
    late_bonus * 2
}}
"#
        ),
    );

    let driver = Driver::new()
        .with_cache_dir(ws.join("cache"))
        .with_opt_level(OptLevel::O2);
    let workspace = load_package_workspace(&pkg_dir).expect("load multi_pkg workspace");
    let compiled = driver
        .compile_modular_graph_cached(&workspace.root, &workspace.dependencies, &BTreeSet::new())
        .expect("compile multi_pkg modular");

    let mut engine = Engine::new(EngineConfig::default());
    let mut loader = ModuleLoader::new();
    loader
        .register_artifacts(&compiled.artifacts)
        .expect("register artifacts");

    // Before loading: everything is Unloaded.
    assert_eq!(
        loader.module_state("org.test/multi_pkg", &[]),
        Some(ModuleLoadState::Unloaded)
    );

    // Load root `[]` -> loads ONLY `[]`, `used_mid`, and `used_leaf` (plus used `std` modules).
    let prep = loader
        .load_module(engine.vm_mut(), "org.test/multi_pkg", &[])
        .expect("load root module");
    assert_eq!(
        loader.module_state("org.test/multi_pkg", &["used_leaf".into()]),
        Some(ModuleLoadState::Initialized)
    );
    assert_eq!(
        loader.module_state("org.test/multi_pkg", &["used_mid".into()]),
        Some(ModuleLoadState::Initialized)
    );
    assert_eq!(
        loader.module_state("org.test/multi_pkg", &["heavy_unused".into()]),
        Some(ModuleLoadState::Unloaded),
        "unreferenced heavy_unused mod must remain Unloaded"
    );
    assert_eq!(
        loader.module_state("org.test/multi_pkg", &["plugin_late".into()]),
        Some(ModuleLoadState::Unloaded),
        "unreferenced plugin_late mod must remain Unloaded"
    );
    assert_eq!(
        loader.module_state("org.nessa/std", &["wasm".into()]),
        Some(ModuleLoadState::Unloaded),
        "unused std.wasm submodule must remain Unloaded"
    );
    assert_eq!(
        loader.module_state("org.nessa/std", &["net".into()]),
        Some(ModuleLoadState::Unloaded),
        "unused std.net submodule must remain Unloaded"
    );

    let main_fid = prep.main_func.expect("main FuncId");
    engine.vm_mut().spawn_root(main_fid);
    assert!(!matches!(engine.vm_mut().run(), VmResult::Error(_)));

    let log_after_main = fs::read_to_string(&log_file).unwrap();
    // `used_leaf` must initialize before `used_mid`, and both before `main`.
    let pos_leaf = log_after_main.find("used_leaf;").expect("used_leaf init");
    let pos_mid = log_after_main.find("used_mid;").expect("used_mid init");
    let pos_main = log_after_main.find("main;").expect("main ran");
    assert!(
        pos_leaf < pos_mid && pos_mid < pos_main,
        "log={log_after_main}"
    );
    assert!(!log_after_main.contains("plugin_late;"));

    // Now dynamically load `plugin_late` into the SAME live VM!
    loader
        .load_module(
            engine.vm_mut(),
            "org.test/multi_pkg",
            &["plugin_late".into()],
        )
        .expect("incrementally load plugin_late into live VM");
    assert_eq!(
        loader.module_state("org.test/multi_pkg", &["plugin_late".into()]),
        Some(ModuleLoadState::Initialized)
    );
    assert_eq!(
        loader.module_state("org.test/multi_pkg", &["heavy_unused".into()]),
        Some(ModuleLoadState::Unloaded),
        "heavy_unused must STILL remain Unloaded"
    );

    let log_after_plugin = fs::read_to_string(&log_file).unwrap();
    assert!(
        log_after_plugin.ends_with("plugin_late;"),
        "expected plugin_late __init__ to run once on live load, got {log_after_plugin}"
    );
    assert_eq!(
        log_after_plugin.matches("used_leaf;").count(),
        1,
        "already-initialized used_leaf must NOT re-run __init__"
    );

    // Invoke `plugin_late.run_plugin` on the live VM and verify it reads `used_leaf.leaf_seed`!
    let plugin_fn = loader
        .lookup_function(
            "org.test/multi_pkg",
            &SymbolLinkSelector::ScopedSymbol {
                module_path: vec!["plugin_late".into()],
                owner_type_id: None,
                name: "run_plugin".into(),
            },
        )
        .expect("lookup run_plugin FuncId");
    engine.vm_mut().spawn_root(plugin_fn);
    assert!(!matches!(engine.vm_mut().run(), VmResult::Error(_)));
}

#[test]
fn relocates_and_executes_type_indices_above_4096_across_packages() {
    let tmp = TempDirGuard::new("wide-type-reloc");
    let ws = tmp.path();
    let driver = Driver::new()
        .with_cache_dir(ws.join("cache"))
        .with_opt_level(OptLevel::O2);
    let std_art = driver.compile_std_modular().expect("compile std");

    let make_bulk_art = |name: &str, count: usize| -> Arc<nsbc::ModularArtifact> {
        let dir = ws.join(name);
        write_pkg_file(
            &dir,
            "package.toml",
            &format!(
                "[package]\nname = \"{name}\"\ndomain = \"org.test\"\nversion = \"0.1.0\"\ntype = \"lib\"\n"
            ),
        );
        let mut src = String::with_capacity(count * 48 + 256);
        for i in 0..count {
            src.push_str(&format!("pub struct BulkItem{i} {{ pub val: i64 }}\n"));
        }
        let last = count - 1;
        src.push_str(&format!(
            "pub let anchor_val: i64 = (BulkItem{last} {{ val: 7 }}).val\n"
        ));
        write_pkg_file(&dir, "src/lib.ns", &src);
        let sources = load_package_sources(&dir).expect("load bulk sources");
        let (art, _) = driver
            .compile_package_modular(&sources, &[], &BTreeSet::new())
            .expect("compile bulk modular artifact");
        Arc::new(art)
    };

    let bulk_a = make_bulk_art("bulk_a", 1600);
    let bulk_b = make_bulk_art("bulk_b", 1600);
    let bulk_c = make_bulk_art("bulk_c", 1600);

    let root_dir = ws.join("wide_app");
    write_pkg_file(
        &root_dir,
        "package.toml",
        "[package]\nname = \"wide_app\"\ndomain = \"org.test\"\nversion = \"0.1.0\"\ntype = \"exe\"\n",
    );
    write_pkg_file(
        &root_dir,
        "src/main.ns",
        r#"
pub trait Measurable {
    fn measure(self) -> i64
}

pub struct LateWidget {
    pub weight: i64,
}

impl Measurable for LateWidget {
    pub fn measure(self) -> i64 = self.weight * 3
}

fn read_via_trait(m: Measurable) -> i64 = m.measure()

fn box_any(v: Any) -> Any = v

fn main() {
    -- `LateWidget` and `Measurable` live well past TypeIndex 4800 in the merged VM TypePool!
    var sum: i64 = 0
    for i in 0..100 {
        let w = LateWidget { weight: i + 1 }
        let boxed = box_any(w)
        let matched = boxed match {
            _: LateWidget => true,
            _ => false,
        }
        assert(matched)
        let unboxed = boxed.as(LateWidget)
        sum = sum + read_via_trait(unboxed)
    }
    assert(sum == 15150)
}
"#,
    );

    let root_sources = load_package_sources(&root_dir).expect("load wide_app sources");
    let (wide_art, _) = driver
        .compile_package_modular(&root_sources, &[], &BTreeSet::new())
        .expect("compile wide_app modular");
    let wide_art = Arc::new(wide_art);

    let mut engine = Engine::new(EngineConfig::default());
    let mut loader = ModuleLoader::new();
    loader
        .register_artifacts(&[std_art, bulk_a, bulk_b, bulk_c, wide_art])
        .expect("register artifacts");
    loader
        .load_module(engine.vm_mut(), "org.test/bulk_a", &[])
        .expect("load bulk_a");
    loader
        .load_module(engine.vm_mut(), "org.test/bulk_b", &[])
        .expect("load bulk_b");
    loader
        .load_module(engine.vm_mut(), "org.test/bulk_c", &[])
        .expect("load bulk_c");
    let prep = loader
        .load_module(engine.vm_mut(), "org.test/wide_app", &[])
        .expect("load wide_app after >4800 merged types");
    let merged_pool_len = engine.vm_mut().type_pool().len();
    assert!(
        merged_pool_len > 4800,
        "expected merged TypePool > 4800 types, got {merged_pool_len}"
    );
    let main_fid = prep.main_func.expect("main FuncId");
    engine.vm_mut().spawn_root(main_fid);
    if let VmResult::Error(err) = engine.vm_mut().run() {
        panic!("wide TypeIndex VM execution failed: {err:?}");
    }
}
