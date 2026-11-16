//! Integration tests for local path dependencies, recursive workspace discovery,
//! cycle/conflict/version validation, and deterministic `package.lock` (v1).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use pkg_manager::{
    LOCKFILE_VERSION, Lockfile, ManifestDocument, PackageLoadError, compute_package_sha256,
    load_package_workspace, relative_normalized_path,
};

struct TempWorkspace(PathBuf);

impl TempWorkspace {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let dir = std::env::temp_dir().join(format!(
                "nessa-pkg-workspace-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&dir) {
                Ok(()) => return Self(dir),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("temp workspace creation failed: {e}"),
            }
        }
    }

    fn write(&self, rel: &str, content: &str) -> PathBuf {
        let path = self.0.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
    }

    fn pkg(
        &self,
        rel_dir: &str,
        id: &str,
        kind: &str,
        deps_toml: &str,
        entry_source: &str,
    ) -> PathBuf {
        let (qualified, version) = id.split_once('@').unwrap();
        let (domain, name) = qualified.split_once('/').unwrap();
        let manifest = format!(
            "[package]\nname = {name:?}\ndomain = {domain:?}\nversion = {version:?}\ntype = {kind:?}\n\n{deps_toml}\n"
        );
        self.write(&format!("{rel_dir}/package.toml"), &manifest);
        let entry = match kind {
            "exe" => "src/main.ns",
            "lib" => "src/lib.ns",
            "tmp" => "src/tmp.ns",
            other => panic!("unexpected package type {other}"),
        };
        self.write(&format!("{rel_dir}/{entry}"), entry_source);
        self.0.join(rel_dir)
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn manifest_parses_local_path_dependencies_with_optional_version_and_domain() {
    let doc = ManifestDocument::parse(
        r#"
[package]
name = "app"
domain = "acme"
version = "0.1.0"
type = "exe"

[dependencies]
util = { path = "../util" }
math = { path = "../math", version = "^1.2.0" }
"corp.io/codec" = { path = "../../codec", version = "0.4.1" }
"#,
    )
    .unwrap();

    assert_eq!(doc.manifest().name, "app");
    // Unqualified non-path dependency keys must still be rejected to preserve schema-1 rules.
    assert!(
        ManifestDocument::parse(
            r#"
[package]
name = "app"
domain = "acme"
version = "0.1.0"

[dependencies]
util = "1.0.0"
"#,
        )
        .is_err()
    );
    // Empty or NUL path must be rejected.
    for invalid_path in ["\"\"", "\"../bad\\u0000path\""] {
        let bad = format!(
            "[package]\nname = \"app\"\ndomain = \"acme\"\nversion = \"0.1.0\"\n[dependencies]\nutil = {{ path = {invalid_path} }}\n"
        );
        assert!(ManifestDocument::parse(&bad).is_err());
    }
}

#[test]
fn discovers_transitive_and_diamond_shared_local_dependencies_topologically() {
    let ws = TempWorkspace::new();
    ws.pkg(
        "shared",
        "org.demo/shared@0.2.3",
        "lib",
        "",
        "pub fn base() -> i64 { 10 }\n",
    );
    ws.pkg(
        "nested/left",
        "org.demo/left@0.1.0",
        "lib",
        "[dependencies]\nshared = { path = \"../../shared\", version = \"^0.2.0\" }",
        "use @shared.base\npub fn left_val() -> i64 { base() + 1 }\n",
    );
    ws.pkg(
        "right",
        "org.demo/right@0.1.0",
        "lib",
        "[dependencies]\n\"org.demo/shared\" = { path = \"../shared\", version = \">=0.2.0, <0.3.0\" }",
        "use @shared.base\npub fn right_val() -> i64 { base() + 2 }\n",
    );
    let app_dir = ws.pkg(
        "app",
        "org.demo/app@0.1.0",
        "exe",
        "[dependencies]\nleft = { path = \"../nested/left\" }\nright = { path = \"../right\", version = \"0.1.0\" }",
        "fn main() {}\n",
    );

    let loaded = load_package_workspace(&app_dir).unwrap();
    assert_eq!(loaded.root.manifest.manifest().name, "app");
    assert!(loaded.existing_lockfile.is_none());

    // `shared` appears once even though reached via two different relative paths,
    // and precedes both `left` and `right` in topological order.
    let names: Vec<&str> = loaded
        .dependencies
        .iter()
        .map(|p| p.manifest.manifest().name.as_str())
        .collect();
    assert_eq!(names.len(), 3);
    assert_eq!(names[0], "shared");
    assert!(names.contains(&"left"));
    assert!(names.contains(&"right"));
}

#[test]
fn rejects_missing_directory_missing_manifest_cycles_version_identity_and_path_conflicts() {
    // 1. Missing dependency directory
    {
        let ws = TempWorkspace::new();
        let app = ws.pkg(
            "app",
            "org.demo/app@0.1.0",
            "exe",
            "[dependencies]\nghost = { path = \"../does_not_exist\" }",
            "fn main() {}\n",
        );
        let err = load_package_workspace(&app).unwrap_err();
        assert!(
            matches!(err, PackageLoadError::MissingDependencyPath { ref dependency, .. } if dependency.as_ref() == "ghost"),
            "unexpected error: {err}"
        );
    }

    // 2. Directory exists, but `package.toml` is missing
    {
        let ws = TempWorkspace::new();
        fs::create_dir_all(ws.0.join("empty_dep")).unwrap();
        let app = ws.pkg(
            "app",
            "org.demo/app@0.1.0",
            "exe",
            "[dependencies]\nempty_dep = { path = \"../empty_dep\" }",
            "fn main() {}\n",
        );
        let err = load_package_workspace(&app).unwrap_err();
        assert!(
            matches!(err, PackageLoadError::Source(_)),
            "expected missing manifest source error, got: {err}"
        );
    }

    // 3. Direct self-cycle (`a -> a`) and 3-node transitive cycle (`a -> b -> c -> a`)
    {
        let ws = TempWorkspace::new();
        let self_cycle = ws.pkg(
            "self_pkg",
            "org.demo/self_pkg@0.1.0",
            "lib",
            "[dependencies]\nself_pkg = { path = \".\" }",
            "",
        );
        let err = load_package_workspace(&self_cycle).unwrap_err();
        assert!(matches!(err, PackageLoadError::Cycle { .. }), "{err}");
        assert!(err.to_string().contains("cycle"), "{err}");

        let a = ws.pkg(
            "a",
            "org.demo/a@0.1.0",
            "exe",
            "[dependencies]\nb = { path = \"../b\" }",
            "fn main() {}\n",
        );
        ws.pkg(
            "b",
            "org.demo/b@0.1.0",
            "lib",
            "[dependencies]\nc = { path = \"../c\" }",
            "",
        );
        ws.pkg(
            "c",
            "org.demo/c@0.1.0",
            "lib",
            "[dependencies]\na = { path = \"../a\" }",
            "",
        );
        let err = load_package_workspace(&a).unwrap_err();
        match &err {
            PackageLoadError::Cycle { chain } => {
                assert_eq!(
                    chain,
                    &[
                        "org.demo/a".to_owned(),
                        "org.demo/b".to_owned(),
                        "org.demo/c".to_owned(),
                        "org.demo/a".to_owned(),
                    ]
                );
            }
            other => panic!("expected Cycle, got {other}"),
        }
    }

    // 4. Semver version mismatch
    {
        let ws = TempWorkspace::new();
        ws.pkg("dep", "org.demo/dep@2.0.0", "lib", "", "");
        let app = ws.pkg(
            "app",
            "org.demo/app@0.1.0",
            "exe",
            "[dependencies]\ndep = { path = \"../dep\", version = \"^1.0.0\" }",
            "fn main() {}\n",
        );
        let err = load_package_workspace(&app).unwrap_err();
        assert!(
            matches!(err, PackageLoadError::VersionMismatch { .. }),
            "{err}"
        );
    }

    // 5. Package name / domain mismatch
    {
        let ws = TempWorkspace::new();
        ws.pkg("dep", "org.other/actual_name@0.1.0", "lib", "", "");
        let app = ws.pkg(
            "app",
            "org.demo/app@0.1.0",
            "exe",
            "[dependencies]\ndep = { path = \"../dep\" }",
            "fn main() {}\n",
        );
        let err = load_package_workspace(&app).unwrap_err();
        assert!(
            matches!(err, PackageLoadError::IdentityMismatch { .. }),
            "{err}"
        );

        let ws2 = TempWorkspace::new();
        ws2.pkg("dep", "org.other/dep@0.1.0", "lib", "", "");
        let app2 = ws2.pkg(
            "app",
            "org.demo/app@0.1.0",
            "exe",
            "[dependencies]\n\"org.expected/dep\" = { path = \"../dep\" }",
            "fn main() {}\n",
        );
        let err2 = load_package_workspace(&app2).unwrap_err();
        assert!(
            matches!(err2, PackageLoadError::IdentityMismatch { .. }),
            "{err2}"
        );
    }

    // 6. Conflicting definitions of the same dependency name pointing to different canonical paths
    {
        let ws = TempWorkspace::new();
        ws.pkg("v1/common", "org.demo/common@0.1.0", "lib", "", "");
        ws.pkg("v2/common", "org.demo/common@0.1.0", "lib", "", "");
        ws.pkg(
            "mid",
            "org.demo/mid@0.1.0",
            "lib",
            "[dependencies]\ncommon = { path = \"../v2/common\" }",
            "",
        );
        let app = ws.pkg(
            "app",
            "org.demo/app@0.1.0",
            "exe",
            "[dependencies]\ncommon = { path = \"../v1/common\" }\nmid = { path = \"../mid\" }",
            "fn main() {}\n",
        );
        let err = load_package_workspace(&app).unwrap_err();
        assert!(
            matches!(err, PackageLoadError::ConflictingDependencyPath { ref dependency, .. } if dependency.as_ref() == "org.demo/common"),
            "{err}"
        );
    }
}

#[test]
fn lockfile_roundtrip_content_hashing_and_corrupt_lockfile_rejection() {
    let ws = TempWorkspace::new();
    ws.pkg(
        "zeta",
        "org.demo/zeta@1.2.0",
        "lib",
        "",
        "pub fn z() -> i64 { 26 }\n",
    );
    ws.write("zeta/src/sub.ns", "pub fn helper() -> i64 { 1 }\n");
    ws.pkg(
        "alpha",
        "org.demo/alpha@0.3.1",
        "lib",
        "[dependencies]\nzeta = { path = \"../zeta\", version = \"^1.0.0\" }",
        "use @zeta.z\npub fn a() -> i64 { z() }\n",
    );
    let app_dir = ws.pkg(
        "app",
        "org.demo/app@0.1.0",
        "exe",
        "[dependencies]\nzeta = { path = \"../zeta\" }\nalpha = { path = \"../alpha\" }",
        "fn main() {}\n",
    );

    let workspace = load_package_workspace(&app_dir).unwrap();
    let lockfile = Lockfile::from_workspace(&workspace);
    assert_eq!(lockfile.version, LOCKFILE_VERSION);
    assert_eq!(lockfile.packages.len(), 2);
    // Deterministically sorted by name: `alpha`, then `zeta`.
    assert_eq!(lockfile.packages[0].name, "alpha");
    assert_eq!(lockfile.packages[0].path, "../alpha");
    assert_eq!(lockfile.packages[1].name, "zeta");
    assert_eq!(lockfile.packages[1].path, "../zeta");
    assert_eq!(lockfile.packages[0].sha256.len(), 64);
    assert_eq!(lockfile.packages[1].sha256.len(), 64);

    let lock_path = app_dir.join("package.lock");
    lockfile.write(&lock_path).unwrap();

    let reloaded_lock = Lockfile::read(&lock_path).unwrap();
    assert_eq!(reloaded_lock, lockfile);
    assert_eq!(reloaded_lock.to_toml_string(), lockfile.to_toml_string());
    reloaded_lock.verify(&workspace).unwrap();

    // Reloading workspace picks up existing valid `package.lock`.
    let workspace_with_lock = load_package_workspace(&app_dir).unwrap();
    assert_eq!(
        workspace_with_lock.existing_lockfile.as_ref(),
        Some(&lockfile)
    );

    // Modifying a nested `.ns` file inside `zeta` changes `compute_package_sha256` and fails `verify`.
    let old_zeta_sha = compute_package_sha256(&workspace.dependencies[0]);
    ws.write("zeta/src/sub.ns", "pub fn helper() -> i64 { 999 }\n");
    let modified_workspace = load_package_workspace(&app_dir).unwrap();
    let new_zeta_sha = compute_package_sha256(&modified_workspace.dependencies[0]);
    assert_ne!(old_zeta_sha, new_zeta_sha);
    let verify_err = lockfile.verify(&modified_workspace).unwrap_err();
    assert!(
        verify_err.to_string().contains("sha256 mismatch"),
        "{verify_err}"
    );

    // Corrupt or malformed `package.lock` variants must all be rejected cleanly without panicking.
    let valid_sha = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let invalid_cases = [
        "not = [valid toml",
        "version = 2\n",
        "extra = true\nversion = 1\n",
        "[[package]]\nname = \"alpha\"\nversion = \"0.3.1\"\npath = \"../alpha\"\nsha256 = \"0123\"\n",
        &format!(
            "version = 1\n\n[[package]]\nname = \"alpha\"\nversion = \"not-semver\"\npath = \"../alpha\"\nsha256 = \"{valid_sha}\"\n"
        ),
        &format!(
            "version = 1\n\n[[package]]\nname = \"alpha\"\nversion = \"0.3.1\"\npath = \"..\\\\alpha\"\nsha256 = \"{valid_sha}\"\n"
        ),
        &format!(
            "version = 1\n\n[[package]]\nname = \"alpha\"\nversion = \"0.3.1\"\npath = \"../alpha\"\nsha256 = \"{}\"\n",
            valid_sha.to_uppercase()
        ),
        // Duplicate package entries
        &format!(
            "version = 1\n\n[[package]]\nname = \"alpha\"\nversion = \"0.3.1\"\npath = \"../alpha\"\nsha256 = \"{valid_sha}\"\n\n[[package]]\nname = \"alpha\"\nversion = \"0.3.1\"\npath = \"../alpha\"\nsha256 = \"{valid_sha}\"\n"
        ),
        // Unsorted `[[package]]` entries (`zeta` before `alpha`)
        &format!(
            "version = 1\n\n[[package]]\nname = \"zeta\"\nversion = \"1.2.0\"\npath = \"../zeta\"\nsha256 = \"{valid_sha}\"\n\n[[package]]\nname = \"alpha\"\nversion = \"0.3.1\"\npath = \"../alpha\"\nsha256 = \"{valid_sha}\"\n"
        ),
    ];

    for bad_toml in invalid_cases {
        assert!(
            Lockfile::parse(bad_toml).is_err(),
            "unexpectedly accepted corrupt lockfile:\n{bad_toml}"
        );
        fs::write(&lock_path, bad_toml).unwrap();
        let err = load_package_workspace(&app_dir).unwrap_err();
        assert!(
            matches!(err, PackageLoadError::InvalidLockfile { .. }),
            "expected InvalidLockfile, got {err}"
        );
    }

    // Invalid UTF-8 bytes in `package.lock` are also rejected cleanly.
    fs::write(&lock_path, [0xFF, 0xFE, 0x00]).unwrap();
    assert!(matches!(
        load_package_workspace(&app_dir).unwrap_err(),
        PackageLoadError::InvalidLockfile { .. }
    ));

    // Relative path normalization uses `/` separators deterministically.
    assert_eq!(
        relative_normalized_path(Path::new("/repo/apps/cli"), Path::new("/repo/libs/core")),
        "../../libs/core"
    );
}

#[test]
fn distinct_domains_with_identical_short_package_name_coexist_in_workspace_and_lockfile() {
    let ws = TempWorkspace::new();
    ws.pkg(
        "alpha_utils",
        "org.alpha/utils@0.1.0",
        "lib",
        "",
        "pub fn alpha_val() -> i64 { 10 }\n",
    );
    ws.pkg(
        "beta_utils",
        "org.beta/utils@0.2.0",
        "lib",
        "",
        "pub fn beta_val() -> i64 { 32 }\n",
    );
    ws.pkg(
        "left",
        "org.demo/left@0.1.0",
        "lib",
        "[dependencies]\n\"org.alpha/utils\" = { path = \"../alpha_utils\" }",
        "use @utils.alpha_val\npub fn l() -> i64 { alpha_val() }\n",
    );
    ws.pkg(
        "right",
        "org.demo/right@0.1.0",
        "lib",
        "[dependencies]\n\"org.beta/utils\" = { path = \"../beta_utils\" }",
        "use @utils.beta_val\npub fn r() -> i64 { beta_val() }\n",
    );
    let app_dir = ws.pkg(
        "app",
        "org.demo/app@0.1.0",
        "exe",
        "[dependencies]\nleft = { path = \"../left\" }\nright = { path = \"../right\" }\n\"org.alpha/utils\" = { path = \"../alpha_utils\" }\n\"org.beta/utils\" = { path = \"../beta_utils\" }",
        "fn main() {}\n",
    );

    let workspace = load_package_workspace(&app_dir).unwrap();
    assert_eq!(workspace.dependencies.len(), 4);

    let lockfile = Lockfile::from_workspace(&workspace);
    let rendered = lockfile.to_toml_string();
    let reparsed = Lockfile::parse(&rendered).unwrap();
    assert_eq!(reparsed, lockfile);
    reparsed.verify(&workspace).unwrap();

    // Verify the two `utils` entries are sorted deterministically by `(name, domain)`.
    let utils_domains: Vec<Option<&str>> = reparsed
        .packages
        .iter()
        .filter(|p| p.name == "utils")
        .map(|p| p.domain.as_deref())
        .collect();
    assert_eq!(utils_domains, vec![Some("org.alpha"), Some("org.beta")]);
}
