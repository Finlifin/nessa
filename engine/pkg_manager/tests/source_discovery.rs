//! Real filesystem contracts for checked discovery and owned package source loading.
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use pkg_manager::{
    DiscoveredModule, ManifestDocument, ModuleSources, PackageSourceError, PackageType,
    SourceLoadLimits, discover_modules, discover_modules_with_limits, load_package_sources,
    load_package_sources_with_limits,
};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "nessa-package-source-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path.canonicalize().unwrap()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("create fixture {path:?}: {error}"),
            }
        }
    }
    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }
    fn write(&self, relative: &str, bytes: impl AsRef<[u8]>) {
        let path = self.path(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    fn package(&self, kind: &str) -> String {
        let manifest = format!(
            "[package]\nname=\"sample\"\ndomain=\"example.org\"\nversion=\"2.3.4-rc.1+review\"\ntype={kind:?}\n[metadata]\nweight=42\nunknown={{version=\"private\",enabled=true}}\n"
        );
        self.write("package.toml", &manifest);
        self.write(
            if kind == "exe" {
                "src/main.ns"
            } else {
                "src/lib.ns"
            },
            "root",
        );
        manifest
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
fn flattened(modules: &[DiscoveredModule]) -> Vec<(Vec<String>, PathBuf)> {
    let mut result = Vec::new();
    for module in modules {
        result.push((module.path.clone(), module.file_path.clone()));
        result.extend(flattened(&module.children));
    }
    result
}
fn loaded(modules: &[ModuleSources]) -> Vec<(Vec<String>, PathBuf, String)> {
    let mut result = Vec::new();
    for module in modules {
        result.push((
            module.path.clone(),
            module.entry.file_path.clone(),
            module.entry.source.clone(),
        ));
        result.extend(loaded(&module.children));
    }
    result
}
fn expected(d: &Directory, entries: &[(&[&str], &str)]) -> Vec<(Vec<String>, PathBuf)> {
    let root = d.0.canonicalize().unwrap();
    entries
        .iter()
        .map(|(path, file)| {
            (
                path.iter().map(|s| (*s).to_owned()).collect(),
                root.join(file),
            )
        })
        .collect()
}
fn capacity(error: PackageSourceError, resource: &str, limit: usize) {
    assert!(
        matches!(&error, PackageSourceError::Capacity { resource: actual, limit: bound } if *actual == resource && *bound == limit),
        "{error:?}"
    );
    assert!(error.to_string().contains(resource));
}
fn missing(error: PackageSourceError, expected: &Path) {
    assert!(
        matches!(&error, PackageSourceError::MissingEntry { path } if path == expected),
        "{error:?}"
    );
    assert!(
        error
            .to_string()
            .contains(expected.file_name().unwrap().to_str().unwrap())
    );
}

#[test]
fn exact_root_entry_is_selected_by_exe_lib_and_tmp_without_dropping_other_names() {
    for (kind, package_type, root_entry, other) in [
        ("exe", PackageType::Exe, "main.ns", "lib.ns"),
        ("lib", PackageType::Lib, "lib.ns", "main.ns"),
        ("tmp", PackageType::Tmp, "lib.ns", "main.ns"),
    ] {
        let d = Directory::new();
        d.package(kind);
        d.write(&format!("src/{other}"), "other source");
        let modules = discover_modules(&d.path("src"), package_type).unwrap();
        let name = other.strip_suffix(".ns").unwrap();
        assert_eq!(
            flattened(&modules),
            expected(&d, &[(&[name], &format!("src/{other}"))])
        );
        let sources = load_package_sources(&d.0).unwrap();
        assert_eq!(
            sources.entry.file_path,
            d.0.canonicalize().unwrap().join("src").join(root_entry)
        );
        assert_eq!(sources.entry.source, "root");
        assert_eq!(sources.manifest.manifest().package_type, package_type);
        assert_eq!(sources.modules.len(), 1);
        assert_eq!(sources.modules[0].entry.source, "other source");
    }
}

#[test]
fn nested_main_and_lib_are_ordinary_children_and_siblings_are_unicode_sorted() {
    let d = Directory::new();
    d.package("exe");
    for (path, source) in [
        ("src/中文.ns", "chinese"),
        ("src/β.ns", "greek"),
        ("src/z.ns", "z"),
        ("src/alpha/mod.ns", "module alpha"),
        ("src/alpha/main.ns", "nested main"),
        ("src/alpha/lib.ns", "nested lib"),
        ("src/alpha/child/mod.ns", "module child"),
        ("src/alpha/child/end.ns", "end"),
    ] {
        d.write(path, source);
    }
    for file in [
        "src/readme.txt",
        "src/fn.txt",
        "src/ignored.ns.bak",
        "src/unused.NS",
    ] {
        d.write(file, "not a module");
    }
    let expected = expected(
        &d,
        &[
            (&["alpha"], "src/alpha/mod.ns"),
            (&["alpha", "child"], "src/alpha/child/mod.ns"),
            (&["alpha", "child", "end"], "src/alpha/child/end.ns"),
            (&["alpha", "lib"], "src/alpha/lib.ns"),
            (&["alpha", "main"], "src/alpha/main.ns"),
            (&["z"], "src/z.ns"),
            (&["β"], "src/β.ns"),
            (&["中文"], "src/中文.ns"),
        ],
    );
    let first = discover_modules(&d.path("src"), PackageType::Exe).unwrap();
    assert_eq!(flattened(&first), expected);
    assert_eq!(
        first,
        discover_modules(&d.path("src"), PackageType::Exe).unwrap()
    );
    let sources = load_package_sources(&d.0).unwrap();
    let actual = loaded(&sources.modules);
    assert_eq!(
        actual
            .iter()
            .map(|(name, path, _)| (name.clone(), path.clone()))
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        actual
            .iter()
            .map(|(_, _, text)| text.as_str())
            .collect::<Vec<_>>(),
        [
            "module alpha",
            "module child",
            "end",
            "nested lib",
            "nested main",
            "z",
            "greek",
            "chinese"
        ]
    );
}

#[test]
fn collisions_report_the_whole_logical_path_and_both_real_entries() {
    let d = Directory::new();
    d.package("exe");
    d.write("src/outer/mod.ns", "outer");
    d.write("src/outer/item.ns", "file");
    d.write("src/outer/item/mod.ns", "directory");
    let error = discover_modules(&d.path("src"), PackageType::Exe).unwrap_err();
    let PackageSourceError::DuplicateModule {
        module_path,
        first,
        second,
    } = &error
    else {
        panic!("{error:?}")
    };
    assert_eq!(module_path, &["outer", "item"]);
    let mut paths = [first.clone(), second.clone()];
    paths.sort();
    let mut expected = [d.path("src/outer/item.ns"), d.path("src/outer/item/mod.ns")];
    expected.sort();
    assert_eq!(paths, expected);
    assert!(error.to_string().contains("item"));
    assert!(matches!(
        load_package_sources(&d.0),
        Err(PackageSourceError::DuplicateModule { .. })
    ));
}

#[test]
fn source_names_require_exact_complete_lexer_identifiers() {
    for name in [
        "fn",
        "if",
        "null",
        "true",
        "not",
        "and",
        "mod",
        "9bad",
        "with space",
        "dash-name",
        "name ",
        "name\n",
        "name#note",
        "",
    ] {
        let d = Directory::new();
        d.package("exe");
        let relative = format!("src/{name}.ns");
        d.write(&relative, "42");
        let error = discover_modules(&d.path("src"), PackageType::Exe).unwrap_err();
        assert!(
            matches!(&error, PackageSourceError::InvalidModuleName { path } if path == &d.path(&relative)),
            "{name:?}: {error:?}"
        );
    }
    let d = Directory::new();
    d.package("exe");
    for name in ["CamelCase", "_private", "m2", "café", "变量"] {
        d.write(&format!("src/{name}.ns"), name);
    }
    let paths = flattened(&discover_modules(&d.path("src"), PackageType::Exe).unwrap());
    assert_eq!(
        paths.iter().map(|(p, _)| p[0].as_str()).collect::<Vec<_>>(),
        ["CamelCase", "_private", "café", "m2", "变量"]
    );
}

#[test]
fn invalid_directory_names_are_checked_before_their_source_is_exposed() {
    for name in ["if", "bad-name", " has_space"] {
        let d = Directory::new();
        d.package("exe");
        d.write(&format!("src/{name}/mod.ns"), "42");
        assert!(
            matches!(discover_modules(&d.path("src"),PackageType::Exe), Err(PackageSourceError::InvalidModuleName { path }) if path == d.path(&format!("src/{name}")))
        );
    }
}

#[cfg(unix)]
#[test]
fn non_utf8_source_file_and_directory_names_are_rejected_without_lossy_conversion() {
    use std::os::unix::ffi::OsStringExt;
    for directory in [false, true] {
        let d = Directory::new();
        d.package("exe");
        let name = std::ffi::OsString::from_vec(if directory {
            vec![b'x', 0xff]
        } else {
            vec![b'x', 0xff, b'.', b'n', b's']
        });
        let path = d.path("src").join(name);
        if directory {
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("mod.ns"), "42").unwrap();
        } else {
            std::fs::write(&path, "42").unwrap();
        }
        assert!(
            matches!(discover_modules(&d.path("src"),PackageType::Exe), Err(PackageSourceError::InvalidModuleName { path: actual }) if actual == path)
        );
    }
}

#[test]
fn missing_or_directory_entries_are_precise_errors_at_root_and_nested_modules() {
    for kind in [PackageType::Exe, PackageType::Lib, PackageType::Tmp] {
        let d = Directory::new();
        d.package(match kind {
            PackageType::Exe => "exe",
            PackageType::Lib => "lib",
            PackageType::Tmp => "tmp",
        });
        let name = if kind == PackageType::Exe {
            "main.ns"
        } else {
            "lib.ns"
        };
        std::fs::remove_file(d.path(&format!("src/{name}"))).unwrap();
        missing(
            load_package_sources(&d.0).unwrap_err(),
            &d.path(&format!("src/{name}")),
        );
        missing(
            discover_modules(&d.path("src"), kind).unwrap_err(),
            &d.path(&format!("src/{name}")),
        );
        std::fs::create_dir(d.path(&format!("src/{name}"))).unwrap();
        assert!(
            matches!(discover_modules(&d.path("src"),kind),Err(PackageSourceError::InvalidEntry{path}) if path==d.path(&format!("src/{name}")))
        );
    }
    let d = Directory::new();
    d.package("exe");
    std::fs::create_dir(d.path("src/net")).unwrap();
    missing(
        discover_modules(&d.path("src"), PackageType::Exe).unwrap_err(),
        &d.path("src/net/mod.ns"),
    );
    missing(
        load_package_sources(&d.0).unwrap_err(),
        &d.path("src/net/mod.ns"),
    );
    std::fs::create_dir(d.path("src/net/mod.ns")).unwrap();
    assert!(
        matches!(load_package_sources(&d.0),Err(PackageSourceError::InvalidEntry{path}) if path==d.path("src/net/mod.ns"))
    );
}

#[test]
fn missing_manifest_and_invalid_manifest_preserve_io_and_parse_error_chains() {
    let d = Directory::new();
    let error = load_package_sources(&d.0).unwrap_err();
    assert!(
        matches!(&error,PackageSourceError::Io{path,source,..} if path==&d.path("package.toml") && source.kind()==std::io::ErrorKind::NotFound)
    );
    assert!(error.source().is_some());
    for source in [
        "[package]\nname='missing fields'",
        "[package]\nname='x'\nname='y'",
        "not TOML",
    ] {
        d.write("package.toml", source);
        let error = load_package_sources(&d.0).unwrap_err();
        assert!(
            matches!(&error,PackageSourceError::InvalidManifest{path,..} if path==&d.path("package.toml")),
            "{error:?}"
        );
        assert!(error.source().is_some());
        assert!(error.to_string().contains("package.toml"));
    }
}

#[test]
fn nonexistent_source_directory_and_non_directory_package_root_do_not_succeed_empty() {
    let d = Directory::new();
    let path = d.path("absent");
    let error = discover_modules(&path, PackageType::Exe).unwrap_err();
    assert!(
        matches!(&error,PackageSourceError::Io{source,..} if source.kind()==std::io::ErrorKind::NotFound)
    );
    assert!(error.source().is_some());
    d.write("regular-file", "not a directory");
    assert!(matches!(
        load_package_sources(&d.path("regular-file")),
        Err(PackageSourceError::Io { .. })
    ));
}

#[test]
fn loading_preserves_exact_utf8_text_complete_manifest_and_owned_data_after_source_deletion() {
    let d = Directory::new();
    let manifest = d.package("lib");
    let root = "\u{feff}// 保留\r\nfn value(){\"a\\nb\"}\r\n\0";
    let module = "// café\n\t42\n";
    d.write("src/lib.ns", root);
    d.write("src/net/mod.ns", module);
    d.write("src/net/end.ns", "");
    let loaded = load_package_sources(&d.0).unwrap();
    assert_eq!(loaded.root, d.0.canonicalize().unwrap());
    assert_eq!(loaded.entry.source, root);
    assert_eq!(loaded.modules[0].entry.source, module);
    assert_eq!(loaded.modules[0].children[0].entry.source, "");
    assert_eq!(
        loaded.manifest.metadata(),
        ManifestDocument::parse(&manifest).unwrap().metadata()
    );
    assert_eq!(
        loaded.manifest.manifest().version.to_string(),
        "2.3.4-rc.1+review"
    );
    assert_eq!(
        loaded.manifest.metadata()["metadata"]["weight"].as_integer(),
        Some(42)
    );
    assert_eq!(
        loaded.manifest.metadata()["metadata"]["unknown"]["version"].as_str(),
        Some("private")
    );
    std::fs::remove_file(d.path("src/lib.ns")).unwrap();
    std::fs::remove_file(d.path("src/net/mod.ns")).unwrap();
    assert_eq!(loaded.clone().entry.source, root);
    assert_eq!(loaded.modules[0].entry.source, module);
}

#[test]
fn moving_a_package_changes_diagnostic_root_but_not_manifest_identity_or_logical_source_tree() {
    let a = Directory::new();
    let b = Directory::new();
    let manifest = a.package("exe");
    a.write("src/child.ns", "child exact");
    b.write("package.toml", manifest);
    b.write("src/main.ns", "root");
    b.write("src/child.ns", "child exact");
    let a = load_package_sources(&a.0).unwrap();
    let b = load_package_sources(&b.0).unwrap();
    assert_ne!(a.root, b.root);
    assert_eq!(a.manifest.local_identity(), b.manifest.local_identity());
    assert_eq!(a.entry.source, b.entry.source);
    assert_eq!(a.modules[0].path, b.modules[0].path);
    assert_eq!(a.modules[0].entry.source, b.modules[0].entry.source);
    assert_ne!(a.modules[0].entry.file_path, b.modules[0].entry.file_path);
}

#[test]
fn invalid_utf8_manifest_root_and_child_sources_report_invalid_data_instead_of_truncation() {
    for relative in ["package.toml", "src/main.ns", "src/child.ns"] {
        let d = Directory::new();
        d.package("exe");
        d.write(relative, [b'a', 0xff, b'b']);
        if relative != "package.toml" {
            assert_eq!(
                discover_modules(&d.path("src"), PackageType::Exe)
                    .unwrap()
                    .len(),
                usize::from(relative == "src/child.ns")
            );
        }
        let error = load_package_sources(&d.0).unwrap_err();
        assert!(
            matches!(&error,PackageSourceError::Io{path,source,..} if path==&d.path(relative) && source.kind()==std::io::ErrorKind::InvalidData),
            "{error:?}"
        );
        assert!(error.source().is_some());
    }
}

fn bounded_tree(d: &Directory) -> usize {
    let manifest = d.package("exe");
    d.write("src/main.ns", "root");
    d.write("src/a/mod.ns", "entry");
    d.write("src/a/leaf.ns", "123456");
    d.write("src/z.ns", "1234567");
    manifest.len()
}
fn exact_limits(manifest: usize) -> SourceLoadLimits {
    SourceLoadLimits {
        max_depth: 2,
        max_modules: 4,
        max_entries: 5,
        max_source_bytes: 22,
        max_file_bytes: 7,
        max_manifest_bytes: manifest,
    }
}
#[test]
fn simultaneous_exact_limits_accept_the_entire_tree_and_one_below_each_bound_rejects() {
    let d = Directory::new();
    let manifest = bounded_tree(&d);
    let sources = load_package_sources_with_limits(&d.0, exact_limits(manifest)).unwrap();
    assert_eq!(sources.entry.source, "root");
    assert_eq!(loaded(&sources.modules).len(), 3);
    assert_eq!(
        flattened(
            &discover_modules_with_limits(&d.path("src"), PackageType::Exe, exact_limits(manifest))
                .unwrap()
        ),
        expected(
            &d,
            &[
                (&["a"], "src/a/mod.ns"),
                (&["a", "leaf"], "src/a/leaf.ns"),
                (&["z"], "src/z.ns")
            ]
        )
    );
    for resource in [
        "depth",
        "modules",
        "entries",
        "source_bytes",
        "file_bytes",
        "manifest_bytes",
    ] {
        let mut limits = exact_limits(manifest);
        let bound = match resource {
            "depth" => {
                limits.max_depth = 1;
                1
            }
            "modules" => {
                limits.max_modules = 3;
                3
            }
            "entries" => {
                limits.max_entries = 4;
                4
            }
            "source_bytes" => {
                limits.max_source_bytes = 21;
                21
            }
            "file_bytes" => {
                limits.max_file_bytes = 6;
                6
            }
            "manifest_bytes" => {
                limits.max_manifest_bytes = manifest - 1;
                manifest - 1
            }
            _ => unreachable!(),
        };
        capacity(
            load_package_sources_with_limits(&d.0, limits).unwrap_err(),
            resource,
            bound,
        );
    }
}

#[test]
fn every_zero_limit_and_unsupported_depth_reject_even_an_empty_valid_tree() {
    let d = Directory::new();
    d.package("exe");
    d.write("src/main.ns", "");
    for field in [
        "max_depth",
        "max_modules",
        "max_entries",
        "max_source_bytes",
        "max_file_bytes",
        "max_manifest_bytes",
    ] {
        let make = || {
            let mut l = SourceLoadLimits::default();
            match field {
                "max_depth" => l.max_depth = 0,
                "max_modules" => l.max_modules = 0,
                "max_entries" => l.max_entries = 0,
                "max_source_bytes" => l.max_source_bytes = 0,
                "max_file_bytes" => l.max_file_bytes = 0,
                "max_manifest_bytes" => l.max_manifest_bytes = 0,
                _ => unreachable!(),
            };
            l
        };
        assert!(
            matches!(load_package_sources_with_limits(&d.0,make()),Err(PackageSourceError::InvalidLimits{field:actual}) if actual==field)
        );
        assert!(
            matches!(discover_modules_with_limits(&d.path("src"),PackageType::Exe,make()),Err(PackageSourceError::InvalidLimits{field:actual}) if actual==field)
        );
    }
    let limits = SourceLoadLimits {
        max_depth: 257,
        ..SourceLoadLimits::default()
    };
    assert!(matches!(
        load_package_sources_with_limits(&d.0, limits),
        Err(PackageSourceError::InvalidLimits { field: "max_depth" })
    ));
    let defaults = SourceLoadLimits::default();
    assert_eq!(
        (
            defaults.max_depth,
            defaults.max_modules,
            defaults.max_entries
        ),
        (256, 65_536, 262_144)
    );
    assert_eq!(
        (
            defaults.max_source_bytes,
            defaults.max_file_bytes,
            defaults.max_manifest_bytes
        ),
        (64 * 1024 * 1024, 16 * 1024 * 1024, 1024 * 1024)
    );
}

#[test]
fn ignored_entries_still_consume_directory_capacity_and_the_root_counts_as_a_module() {
    let d = Directory::new();
    d.package("exe");
    d.write("src/notes.txt", "ignored");
    d.write("src/old.ns.bak", "ignored");
    let limits = SourceLoadLimits {
        max_entries: 3,
        max_modules: 1,
        ..SourceLoadLimits::default()
    };
    assert!(
        discover_modules_with_limits(&d.path("src"), PackageType::Exe, limits)
            .unwrap()
            .is_empty()
    );
    let limits = SourceLoadLimits {
        max_entries: 2,
        ..SourceLoadLimits::default()
    };
    capacity(
        discover_modules_with_limits(&d.path("src"), PackageType::Exe, limits).unwrap_err(),
        "entries",
        2,
    );
    d.write("src/first.ns", "");
    let limits = SourceLoadLimits {
        max_modules: 1,
        ..SourceLoadLimits::default()
    };
    capacity(
        discover_modules_with_limits(&d.path("src"), PackageType::Exe, limits).unwrap_err(),
        "modules",
        1,
    );
}

#[test]
fn byte_limits_use_utf8_bytes_not_character_counts_and_bound_before_decoding() {
    let d = Directory::new();
    d.package("exe");
    d.write("src/main.ns", "é");
    let limits = SourceLoadLimits {
        max_file_bytes: 2,
        max_source_bytes: 2,
        ..SourceLoadLimits::default()
    };
    assert_eq!(
        load_package_sources_with_limits(&d.0, limits)
            .unwrap()
            .entry
            .source,
        "é"
    );
    let limits = SourceLoadLimits {
        max_file_bytes: 1,
        ..SourceLoadLimits::default()
    };
    capacity(
        load_package_sources_with_limits(&d.0, limits).unwrap_err(),
        "file_bytes",
        1,
    );
    d.write("src/main.ns", [0xff, 0xff]);
    let limits = SourceLoadLimits {
        max_file_bytes: 1,
        ..SourceLoadLimits::default()
    };
    capacity(
        load_package_sources_with_limits(&d.0, limits).unwrap_err(),
        "file_bytes",
        1,
    );
    let limits = SourceLoadLimits {
        max_file_bytes: 2,
        ..SourceLoadLimits::default()
    };
    assert!(
        matches!(load_package_sources_with_limits(&d.0,limits),Err(PackageSourceError::Io{source,..}) if source.kind()==std::io::ErrorKind::InvalidData)
    );
}

#[test]
fn small_deep_tree_obeys_exact_depth_and_does_not_return_a_partial_prefix() {
    let d = Directory::new();
    d.package("exe");
    let mut relative = "src".to_owned();
    for _ in 0..8 {
        relative.push_str("/a");
        d.write(&format!("{relative}/mod.ns"), "x");
    }
    let limits = SourceLoadLimits {
        max_depth: 8,
        max_modules: 9,
        ..SourceLoadLimits::default()
    };
    let modules = discover_modules_with_limits(&d.path("src"), PackageType::Exe, limits).unwrap();
    let flat = flattened(&modules);
    assert_eq!(flat.len(), 8);
    assert_eq!(flat[7].0, vec!["a"; 8]);
    assert_eq!(flat[7].1, d.path(&format!("{relative}/mod.ns")));
    let limits = SourceLoadLimits {
        max_depth: 7,
        ..SourceLoadLimits::default()
    };
    capacity(
        discover_modules_with_limits(&d.path("src"), PackageType::Exe, limits).unwrap_err(),
        "depth",
        7,
    );
}

#[cfg(unix)]
#[test]
fn symlink_ancestor_cycles_reject_but_distinct_sibling_aliases_are_logical_modules() {
    use std::os::unix::fs::symlink;
    let d = Directory::new();
    d.package("exe");
    d.write("shared/mod.ns", "shared");
    d.write("shared/end.ns", "child");
    symlink(d.path("shared"), d.path("src/b")).unwrap();
    symlink(d.path("shared"), d.path("src/a")).unwrap();
    let modules = discover_modules(&d.path("src"), PackageType::Exe).unwrap();
    assert_eq!(
        flattened(&modules),
        expected(
            &d,
            &[
                (&["a"], "src/a/mod.ns"),
                (&["a", "end"], "src/a/end.ns"),
                (&["b"], "src/b/mod.ns"),
                (&["b", "end"], "src/b/end.ns")
            ]
        )
    );
    let sources = load_package_sources(&d.0).unwrap();
    assert_eq!(
        loaded(&sources.modules)
            .iter()
            .map(|(_, _, s)| s.as_str())
            .collect::<Vec<_>>(),
        ["shared", "child", "shared", "child"]
    );
    // Each logical alias contributes its source bytes; no canonical-target dedup.
    let limits = SourceLoadLimits {
        max_source_bytes: 26,
        ..SourceLoadLimits::default()
    };
    assert_eq!(
        load_package_sources_with_limits(&d.0, limits)
            .unwrap()
            .modules
            .len(),
        2
    );
    let limits = SourceLoadLimits {
        max_source_bytes: 25,
        ..SourceLoadLimits::default()
    };
    capacity(
        load_package_sources_with_limits(&d.0, limits).unwrap_err(),
        "source_bytes",
        25,
    );
    symlink(d.path("shared"), d.path("shared/loop")).unwrap();
    let error = discover_modules(&d.path("src"), PackageType::Exe).unwrap_err();
    assert!(
        matches!(&error,PackageSourceError::SymlinkCycle{path,target} if path==&d.path("src/a/loop") && target==&d.path("shared").canonicalize().unwrap()),
        "{error:?}"
    );
}

#[cfg(unix)]
#[test]
fn canonical_package_root_and_logical_file_alias_paths_are_both_preserved() {
    use std::os::unix::fs::symlink;
    let d = Directory::new();
    d.package("lib");
    d.write("outside.ns", "aliased file");
    symlink(d.path("outside.ns"), d.path("src/leaf.ns")).unwrap();
    symlink(d.0.clone(), d.path("root_alias")).unwrap();
    let sources = load_package_sources(&d.path("root_alias")).unwrap();
    assert_eq!(sources.root, d.0.canonicalize().unwrap());
    assert_eq!(sources.modules[0].entry.file_path, d.path("src/leaf.ns"));
    assert_eq!(sources.modules[0].entry.source, "aliased file");
}

#[cfg(unix)]
#[test]
fn dangling_links_and_entry_link_loops_have_chained_real_io_errors() {
    use std::os::unix::fs::symlink;
    let d = Directory::new();
    d.package("exe");
    symlink(d.path("missing-target"), d.path("src/dangling")).unwrap();
    let error = discover_modules(&d.path("src"), PackageType::Exe).unwrap_err();
    assert!(
        matches!(&error,PackageSourceError::Io{source,..} if source.kind()==std::io::ErrorKind::NotFound),
        "{error:?}"
    );
    assert!(error.source().is_some());
    std::fs::remove_file(d.path("src/dangling")).unwrap();
    std::fs::remove_file(d.path("src/main.ns")).unwrap();
    symlink("main.ns", d.path("src/main.ns")).unwrap();
    let error = discover_modules(&d.path("src"), PackageType::Exe).unwrap_err();
    assert!(
        matches!(&error,PackageSourceError::Io{path,..} if path==&d.path("src/main.ns")),
        "{error:?}"
    );
    assert!(error.source().is_some());
}

#[test]
fn manifest_read_limit_is_exact_and_precedes_invalid_utf8_decoding() {
    let d = Directory::new();
    d.package("exe");
    d.write("package.toml", [0xff, 0xff]);
    let limits = SourceLoadLimits {
        max_manifest_bytes: 1,
        ..SourceLoadLimits::default()
    };
    capacity(
        load_package_sources_with_limits(&d.0, limits).unwrap_err(),
        "manifest_bytes",
        1,
    );
    let limits = SourceLoadLimits {
        max_manifest_bytes: 2,
        ..SourceLoadLimits::default()
    };
    assert!(matches!(load_package_sources_with_limits(&d.0, limits),
        Err(PackageSourceError::Io { source, .. }) if source.kind() == std::io::ErrorKind::InvalidData));
}

#[cfg(unix)]
#[test]
fn nonregular_directory_and_fifo_manifests_are_rejected_before_open_can_block() {
    const CHILD_ROOT: &str = "NESSA_NONREGULAR_MANIFEST_CHILD_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let root = PathBuf::from(root);
        assert!(matches!(load_package_sources(&root),
            Err(PackageSourceError::InvalidEntry { path }) if path == root.join("package.toml")));
        return;
    }
    let directory = Directory::new();
    std::fs::create_dir(directory.path("package.toml")).unwrap();
    assert!(matches!(load_package_sources(&directory.0),
        Err(PackageSourceError::InvalidEntry { path }) if path == directory.path("package.toml")));

    let fifo = Directory::new();
    let made = std::process::Command::new("mkfifo")
        .arg(fifo.path("package.toml"))
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "mkfifo: {}",
        String::from_utf8_lossy(&made.stderr)
    );
    // Regression protection must not hang the whole suite if a future reader
    // opens the FIFO before checking its file type. No writer opens this pipe.
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "nonregular_directory_and_fifo_manifests_are_rejected_before_open_can_block",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(CHILD_ROOT, &fifo.0)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let timed_out = loop {
        if child.try_wait().unwrap().is_some() {
            break false;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            break true;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    };
    let output = child.wait_with_output().unwrap();
    assert!(!timed_out, "manifest reader blocked opening a FIFO");
    assert!(
        output.status.success(),
        "FIFO rejection child: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
}
