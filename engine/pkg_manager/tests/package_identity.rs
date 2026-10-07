//! Canonical manifest identity, jointly selected dependency DAGs, and strict locks.

use pkg_manager::{
    Dependency, ManifestDocument, PackageManifest, PackageResolver, PackageType,
    ResolvedPackageGraph, Version, VersionConstraint,
};

fn document(source: &str) -> ManifestDocument {
    ManifestDocument::parse(source).unwrap_or_else(|error| panic!("{source}: {error:?}"))
}

fn package(name: &str, version: &str, metadata: &str, dependencies: &str) -> String {
    format!(
        "[package]\nname={name:?}\ndomain=\"example.org\"\nversion={version:?}\ntype=\"lib\"\n{metadata}\n[dependencies]\n{dependencies}\n"
    )
}

fn root(dependencies: &str) -> String {
    package("root", "9.0.0", "", dependencies)
}

fn registry(sources: &[String]) -> PackageResolver {
    let mut resolver = PackageResolver::new();
    for source in sources {
        resolver
            .register_document(document(source))
            .unwrap_or_else(|error| panic!("cannot register {source}: {error:?}"));
    }
    resolver
}

fn solve(sources: &[String], root: &str) -> ResolvedPackageGraph {
    registry(sources)
        .resolve_document(&document(root))
        .unwrap_or_else(|error| panic!("cannot resolve {root}: {error:?}"))
}

fn assert_same_selection(sources: &[String], root: &str, selected: &[String]) {
    let expected = solve(selected, root);
    for reverse in [false, true] {
        let mut sources = sources.to_vec();
        if reverse {
            sources.reverse();
        }
        let actual = solve(&sources, root);
        assert!(actual.root_identity() == expected.root_identity());
        let versions = |graph: &ResolvedPackageGraph| {
            let mut selected: Vec<_> = graph
                .packages()
                .iter()
                .map(|package| (package.qualified_name(), package.version.to_string()))
                .collect();
            selected.sort();
            selected
        };
        assert_eq!(versions(&actual), versions(&expected));
        assert_eq!(actual.packages().len(), selected.len() + 1);
        for package in actual.packages() {
            assert!(actual.identity(&package.qualified_name()).is_some());
        }
        assert_eq!(
            actual.identity("example.org/root"),
            Some(actual.root_identity())
        );
        assert_eq!(actual.to_lock(), expected.to_lock());
    }
}

fn invalid_manifest(source: &str) {
    let result = ManifestDocument::parse(source);
    assert!(result.is_err(), "accepted {source}");
}

fn assert_bad_lock(resolver: &PackageResolver, source: &str, lock: &str) {
    assert!(
        resolver
            .resolve_document_locked(&document(source), lock)
            .is_err(),
        "accepted invalid lock: {lock}"
    );
}

// Schema 1 emits sorted [[packages]] records; inspect those exact record bounds
// when removing or duplicating entries, without changing unrelated root fields.
fn lock_record_ranges(lock: &str) -> Vec<std::ops::Range<usize>> {
    let starts: Vec<_> = lock
        .match_indices("[[packages]]")
        .map(|(offset, _)| offset)
        .collect();
    assert!(
        starts.len() >= 2,
        "expected root and dependency records: {lock}"
    );
    starts
        .iter()
        .enumerate()
        .map(|(index, &start)| start..starts.get(index + 1).copied().unwrap_or(lock.len()))
        .collect()
}

fn corrupt_one_identity(lock: &str) -> String {
    for (offset, _) in lock.match_indices('"') {
        let start = offset + 1;
        if let Some(value) = lock.get(start..start + 32)
            && value.bytes().all(|byte| byte.is_ascii_hexdigit())
            && lock.as_bytes().get(start + 32) == Some(&b'"')
        {
            let mut changed = lock.to_owned();
            changed.replace_range(start..start + 1, if &value[..1] == "0" { "1" } else { "0" });
            return changed;
        }
    }
    panic!("lock must contain a serialized 128-bit identity: {lock}")
}

#[test]
fn manifest_formatting_key_order_and_escaping_do_not_change_identity() {
    let first = r#"
        # formatting is not content
        [package]
        name = "root"
        domain = "example.org"
        version = "1.0.0"
        type = "lib"
        description = "line\nquote\""
        authors = ["alpha", "beta"]
        [metadata]
        enabled = true
        weight = 42
    "#;
    let second = r#"
        [metadata]
        weight=0x2a
        enabled=true
        [package]
        authors=["\u0061lpha","beta"]
        description="line\u000aquote\u0022"
        type='lib'
        version='7.8.9'
        domain='example.org'
        name='root'
    "#;
    let a = document(first);
    let b = document(second);
    assert!(a.local_identity() == b.local_identity());
    assert_eq!(a.manifest().qualified_name(), "example.org/root");
    assert_eq!(a.manifest().version, Version::new(1, 0, 0));
    assert_eq!(b.manifest().version, Version::new(7, 8, 9));
    assert_eq!(a.manifest().entry_file(), "src/lib.ns");
    assert_eq!(a.metadata()["package"]["version"].as_str(), Some("1.0.0"));
    assert_eq!(a.metadata()["metadata"]["weight"].as_integer(), Some(42));
    assert_eq!(a.metadata()["metadata"]["enabled"].as_bool(), Some(true));
    assert_eq!(
        a.metadata()["package"]["authors"].as_array().unwrap().len(),
        2
    );
}

#[test]
fn every_metadata_field_and_unknown_nested_value_contributes() {
    let plain = package("root", "1.0.0", "", "");
    let base = document(&plain).local_identity();
    for metadata in [
        "description=\"description\"",
        "license=\"MIT\"",
        "authors=[\"one\"]",
        "repository=\"https://example.org/repo\"",
        "readme=\"README.md\"",
        "keywords=[\"compiler\"]",
        "min_nessa_version=\"1.0.0\"",
        "unknown={version=\"1.0.0\",value=42}",
    ] {
        assert!(document(&package("root", "1.0.0", metadata, "")).local_identity() != base);
    }
    for changed in [
        plain.replace("example.org", "other.org"),
        plain.replace("name=\"root\"", "name=\"other\""),
        plain.replace("type=\"lib\"", "type=\"exe\""),
        format!("{plain}\n[extra]\nunknown=42\n"),
    ] {
        assert!(document(&changed).local_identity() != base);
    }
}

#[test]
fn only_the_own_package_version_is_excluded() {
    let a = package("root", "1.0.0", "nested={version=\"one\"}", "");
    let b = a.replace("version=\"1.0.0\"", "version=\"2.0.0-beta.2\"");
    assert!(document(&a).local_identity() == document(&b).local_identity());
    let c = a.replace("version=\"one\"", "version=\"two\"");
    assert!(document(&a).local_identity() != document(&c).local_identity());
    let d = format!("version=\"outside-package\"\n{a}");
    let e = d.replace("outside-package", "changed-outside-package");
    assert!(document(&d).local_identity() != document(&e).local_identity());
}

#[test]
fn toml_value_types_array_order_and_boundaries_remain_distinct() {
    let identity = |value: &str| {
        document(&package("root", "1.0.0", &format!("value={value}"), "")).local_identity()
    };
    for (left, right) in [
        ("1", "1.0"),
        ("1", "\"1\""),
        ("true", "\"true\""),
        ("[1,2]", "[2,1]"),
        ("[]", "{}"),
        ("{a=1,b=2}", "{ab=1}"),
        ("9223372036854775807", "9223372036854775806"),
        ("1979-05-27", "\"1979-05-27\""),
    ] {
        assert!(
            identity(left) != identity(right),
            "{left} collided with {right}"
        );
    }
    assert!(identity("{a=1,b=2}") == identity("{b=2,a=1}"));
    assert!(identity("42") == identity("0x2a"));
    assert!(identity("1.0") == identity("1e0"));
    assert!(identity("nan") == identity("+nan"));
    assert!(identity("nan") == identity("-nan"));
    assert!(identity("0.0") != identity("-0.0"));
    assert!(identity("1979-05-27T07:32:00Z") == identity("1979-05-27T07:32:00+00:00"));
}

#[test]
fn equivalent_constraints_have_canonical_semantic_identity() {
    let identity = |constraint: &str| {
        document(&root(&format!("\"example.org/leaf\"={constraint:?}"))).local_identity()
    };
    assert!(identity(">=1.0, <3.0") == identity(" < 3.0.0 , >= 1.0.0 "));
    assert!(identity(">=1.0.0, <3.0.0") == identity("<3.0.0, >=1.0.0"));
    assert!(identity("^1.2.3") == identity(" ^ 1.2.3 "));
    assert!(identity(">=1.0.0, <3.0.0") == identity("<3.0.0, >=1.0.0, >=1.0.0"));
    for other in ["^1.2.4", "~1.2.3", "1.2.3"] {
        assert!(identity("^1.2.3") != identity(other));
    }
}

#[test]
fn exact_caret_tilde_ranges_and_zero_major_use_real_semver() {
    for (constraint, yes, no) in [
        ("1.2.3", "1.2.3", "1.2.4"),
        ("^1.2.3", "1.9.0", "2.0.0"),
        ("~1.2.3", "1.2.99", "1.3.0"),
        (">=1.0, <3.0", "2.9.0", "3.0.0"),
        ("^0.2.3", "0.2.9", "0.3.0"),
        ("^0.0.3", "0.0.3", "0.0.4"),
    ] {
        let parsed = VersionConstraint::parse(constraint).unwrap();
        assert!(
            parsed.matches(&Version::parse(yes).unwrap()),
            "{constraint}: {yes}"
        );
        assert!(
            !parsed.matches(&Version::parse(no).unwrap()),
            "{constraint}: {no}"
        );
    }
    for bad in ["", "1.2.3.4", "01.2.3", "1.2.3-", "1.2.3+", "1.2.3-01"] {
        assert!(
            Version::parse(bad).is_none(),
            "accepted invalid version {bad}"
        );
    }
    for bad in ["", "^", ">=wat", ">=1.0, <"] {
        assert!(
            VersionConstraint::parse(bad).is_none(),
            "accepted invalid constraint {bad}"
        );
    }
}

#[test]
fn prereleases_require_explicit_permission_and_have_semantic_precedence() {
    let stable = VersionConstraint::parse("^1.2.3").unwrap();
    assert!(!stable.matches(&Version::parse("1.3.0-beta.1").unwrap()));
    let prerelease = VersionConstraint::parse("^1.2.3-beta.2").unwrap();
    assert!(prerelease.matches(&Version::parse("1.2.3-beta.10").unwrap()));
    assert!(!prerelease.matches(&Version::parse("1.2.3-beta.1").unwrap()));
    assert!(!prerelease.matches(&Version::parse("1.3.0-beta.1").unwrap()));
    let older = package("leaf", "1.2.3-beta.2", "flavor=2", "");
    let newer = package("leaf", "1.2.3-beta.10", "flavor=10", "");
    assert_same_selection(
        &[older, newer.clone()],
        &root("\"example.org/leaf\"=\"^1.2.3-beta.2\""),
        &[newer],
    );
}

#[test]
fn dependency_and_registry_order_do_not_change_merkle_identity_or_lock() {
    let a = package("a", "1.0.0", "", "");
    let b = package("b", "2.0.0", "", "");
    let first = root("\"example.org/a\"=\"1.0.0\"\n\"example.org/b\"=\"2.0.0\"");
    let second = root("\"example.org/b\"=\"2.0.0\"\n\"example.org/a\"=\"1.0.0\"");
    let left = solve(&[a.clone(), b.clone()], &first);
    let right = solve(&[b, a], &second);
    assert!(left.root_identity() == right.root_identity());
    assert_eq!(left.to_lock(), right.to_lock());
}

#[test]
fn transitive_content_and_constraints_change_identity_but_concrete_versions_do_not() {
    let source = root("\"example.org/middle\"=\"^1.0.0\"");
    let middle = package("middle", "1.0.0", "", "\"example.org/leaf\"=\"^1.0.0\"");
    let leaf = package("leaf", "1.0.0", "flavor=42", "");
    let baseline = solve(&[middle.clone(), leaf.clone()], &source);
    let changed = solve(
        &[middle.clone(), leaf.replace("flavor=42", "flavor=43")],
        &source,
    );
    assert!(baseline.root_identity() != changed.root_identity());
    let version_only = solve(
        &[
            middle.clone(),
            leaf.replace("version=\"1.0.0\"", "version=\"1.9.0\""),
        ],
        &source,
    );
    assert!(baseline.root_identity() == version_only.root_identity());
    assert_ne!(baseline.to_lock(), version_only.to_lock());
    let constraint_only = solve(&[middle.replace("^1.0.0", "~1.0.0"), leaf], &source);
    assert!(baseline.root_identity() != constraint_only.root_identity());
}

#[test]
fn highest_compatible_selection_is_independent_of_registration_order() {
    let old = package("leaf", "1.0.0", "flavor=10", "");
    let latest = package("leaf", "1.9.0", "flavor=19", "");
    let incompatible = package("leaf", "2.0.0", "flavor=20", "");
    assert_same_selection(
        &[old, incompatible, latest.clone()],
        &root("\"example.org/leaf\"=\"^1.0.0\""),
        &[latest],
    );
}

#[test]
fn diamonds_apply_all_constraints_to_one_selected_package() {
    let a = package(
        "a",
        "1.0.0",
        "",
        "\"example.org/shared\"=\">=1.0.0, <3.0.0\"",
    );
    let b = package(
        "b",
        "1.0.0",
        "",
        "\"example.org/shared\"=\"<2.0.0, >=1.0.0\"",
    );
    let shared_old = package("shared", "1.0.0", "flavor=10", "");
    let shared_best = package("shared", "1.8.0", "flavor=18", "");
    let shared_incompatible = package("shared", "2.5.0", "flavor=25", "");
    let source = root("\"example.org/a\"=\"1.0.0\"\n\"example.org/b\"=\"1.0.0\"");
    assert_same_selection(
        &[
            a.clone(),
            b.clone(),
            shared_old,
            shared_incompatible,
            shared_best.clone(),
        ],
        &source,
        &[a, b, shared_best],
    );
}

#[test]
fn resolution_backtracks_earlier_highest_choices_when_later_dependencies_conflict() {
    let a_old = package(
        "a",
        "1.0.0",
        "flavor=10",
        "\"example.org/shared\"=\"1.0.0\"",
    );
    let a_new = package(
        "a",
        "2.0.0",
        "flavor=20",
        "\"example.org/shared\"=\"2.0.0\"",
    );
    let b = package("b", "1.0.0", "", "\"example.org/shared\"=\"1.0.0\"");
    let s_old = package("shared", "1.0.0", "flavor=10", "");
    let s_new = package("shared", "2.0.0", "flavor=20", "");
    let source = root("\"example.org/a\"=\">=1.0.0, <3.0.0\"\n\"example.org/b\"=\"1.0.0\"");
    assert_same_selection(
        &[a_old.clone(), a_new, b.clone(), s_new, s_old.clone()],
        &source,
        &[a_old, b, s_old],
    );
}

#[test]
fn missing_conflicting_cyclic_and_ambiguous_graphs_are_rejected() {
    let missing = root("\"example.org/missing\"=\"1.0.0\"");
    assert!(registry(&[]).resolve_document(&document(&missing)).is_err());
    let a = package("a", "1.0.0", "", "\"example.org/shared\"=\"1.0.0\"");
    let b = package("b", "1.0.0", "", "\"example.org/shared\"=\"2.0.0\"");
    let shared = [
        package("shared", "1.0.0", "", ""),
        package("shared", "2.0.0", "", ""),
    ];
    let source = root("\"example.org/a\"=\"1.0.0\"\n\"example.org/b\"=\"1.0.0\"");
    assert!(
        registry(&[a, b, shared[0].clone(), shared[1].clone()])
            .resolve_document(&document(&source))
            .is_err()
    );
    let a = package("a", "1.0.0", "", "\"example.org/b\"=\"1.0.0\"");
    let b = package("b", "1.0.0", "", "\"example.org/a\"=\"1.0.0\"");
    assert!(
        registry(&[a, b])
            .resolve_document(&document(&root("\"example.org/a\"=\"1.0.0\"")))
            .is_err()
    );
    let one = package("leaf", "1.0.0", "flavor=1", "");
    let two = package("leaf", "1.0.0", "flavor=2", "");
    let mut resolver = registry(&[one]);
    assert!(resolver.register_document(document(&two)).is_err());
    assert!(
        resolver
            .register_document(document(&package("leaf", "1.0.0", "flavor=1", "")))
            .is_ok()
    );
}

#[test]
fn locks_pin_exact_versions_even_when_a_new_higher_version_has_the_same_identity() {
    let source = root("\"example.org/leaf\"=\"^1.0.0\"");
    let old = package("leaf", "1.0.0", "flavor=42", "");
    let higher = old.replace("version=\"1.0.0\"", "version=\"1.9.0\"");
    let original = solve(std::slice::from_ref(&old), &source);
    let lock = original.to_lock();
    let resolver = registry(&[old, higher]);
    let fresh = resolver.resolve_document(&document(&source)).unwrap();
    assert!(fresh.root_identity() == original.root_identity());
    assert_ne!(fresh.to_lock(), lock);
    let pinned = resolver
        .resolve_document_locked(&document(&source), &lock)
        .unwrap();
    assert!(pinned.root_identity() == original.root_identity());
    assert_eq!(pinned.to_lock(), lock);
    assert_eq!(
        pinned
            .packages()
            .iter()
            .find(|package| package.name == "leaf")
            .unwrap()
            .version,
        Version::new(1, 0, 0)
    );
}

#[test]
fn locks_preserve_old_content_and_reject_stale_root_dependency_or_registry() {
    let source = root("\"example.org/leaf\"=\"^1.0.0\"");
    let old = package("leaf", "1.0.0", "flavor=1", "");
    let higher = package("leaf", "1.9.0", "flavor=19", "");
    let original = solve(std::slice::from_ref(&old), &source);
    let lock = original.to_lock();
    let resolver = registry(&[old.clone(), higher.clone()]);
    let pinned = resolver
        .resolve_document_locked(&document(&source), &lock)
        .unwrap();
    assert!(pinned.root_identity() == original.root_identity());
    assert!(
        pinned.root_identity()
            != resolver
                .resolve_document(&document(&source))
                .unwrap()
                .root_identity()
    );
    assert_bad_lock(
        &resolver,
        &source.replace("type=\"lib\"", "type=\"exe\""),
        &lock,
    );
    assert_bad_lock(
        &resolver,
        &source.replace("version=\"9.0.0\"", "version=\"9.1.0\""),
        &lock,
    );
    assert_bad_lock(&resolver, &source.replace("^1.0.0", "~1.0.0"), &lock);
    assert_bad_lock(
        &registry(&[old.replace("flavor=1", "flavor=2"), higher.clone()]),
        &source,
        &lock,
    );
    assert_bad_lock(&registry(&[higher]), &source, &lock);
}

#[test]
fn malformed_missing_extra_duplicate_and_corrupt_lock_records_are_rejected() {
    let source = root("\"example.org/leaf\"=\"1.2.3\"");
    let leaf = package("leaf", "1.2.3", "flavor=42", "");
    let resolver = registry(&[leaf]);
    let lock = resolver
        .resolve_document(&document(&source))
        .unwrap()
        .to_lock();
    assert_bad_lock(&resolver, &source, "");
    assert_bad_lock(&resolver, &source, "not a package lock");
    assert_bad_lock(&resolver, &source, &lock[..lock.len() / 2]);
    assert_bad_lock(&resolver, &source, &corrupt_one_identity(&lock));
    for (from, to) in [
        ("schema_version = 1", "schema_version = 999"),
        ("identity_schema = 1", "identity_schema = 999"),
        ("schema_version = 1", ""),
        ("identity_schema = 1", ""),
        ("schema_version = 1", "schema_version = \"1\""),
        ("[root]", "[root]\nunknown_root_field = 42"),
        ("[[packages]]", "[[packages]]\nunknown_package_field = 42"),
        ("dependencies = []", ""),
    ] {
        assert!(lock.contains(from), "missing schema binding {from}: {lock}");
        assert_bad_lock(&resolver, &source, &lock.replace(from, to));
    }
    assert_bad_lock(&resolver, &source, &format!("unknown_header = 42\n{lock}"));
    let mut root_removed = lock.clone();
    let root_start = root_removed.find("[root]").unwrap();
    let package_start = root_removed.find("[[packages]]").unwrap();
    root_removed.replace_range(root_start..package_start, "");
    assert_bad_lock(&resolver, &source, &root_removed);
    assert!(
        lock.contains("1.2.3"),
        "selected version absent from lock: {lock}"
    );
    assert_bad_lock(&resolver, &source, &lock.replace("1.2.3", "1.2.4"));
    assert_bad_lock(
        &resolver,
        &source,
        &lock.replace("1.2.3", "invalid-version"),
    );
    let ranges = lock_record_ranges(&lock);
    let last = ranges.last().unwrap();
    let mut missing = lock.clone();
    missing.replace_range(last.clone(), "");
    assert_bad_lock(&resolver, &source, &missing);
    let duplicate = format!("{lock}\n{}", &lock[last.clone()]);
    assert_bad_lock(&resolver, &source, &duplicate);
    let extra = lock[last.clone()].replace("example.org", "other.org");
    assert_ne!(extra, lock[last.clone()]);
    assert_bad_lock(&resolver, &source, &format!("{lock}\n{extra}"));
}

#[test]
fn invalid_and_duplicate_manifest_keys_never_make_documents() {
    let valid = package("root", "1.0.0", "", "");
    for source in [
        valid.replace("domain=\"example.org\"", ""),
        valid.replace("name=\"root\"", ""),
        valid.replace("version=\"1.0.0\"", ""),
        valid.replace("type=\"lib\"", ""),
        valid.replace("domain=\"example.org\"", "domain=\"\""),
        valid.replace("name=\"root\"", "name=\"\""),
        valid.replace("version=\"1.0.0\"", "version=42"),
        valid.replace("type=\"lib\"", "type=\"unknown\""),
        package("root", "1.0.0", "name=\"again\"", ""),
        package("root", "1.0.0", "", "\"leaf\"=\"1.0.0\""),
        package("root", "1.0.0", "", "\"example.org/leaf\"=42"),
        package("root", "1.0.0", "", "\"example.org/leaf\"=\"^\""),
        package(
            "root",
            "1.0.0",
            "",
            "\"example.org/leaf\"=\"1.0.0\"\n\"example.org/leaf\"=\"2.0.0\"",
        ),
    ] {
        invalid_manifest(&source);
    }
}

#[test]
fn deep_dags_are_checked_in_a_child_process_instead_of_aborting_the_suite() {
    const MARKER: &str = "NESSA_IDENTITY_DEEP_TEST_CHILD";
    if std::env::var_os(MARKER).is_some() {
        let mut sources = Vec::new();
        for index in 0..4096 {
            let dependencies = if index == 4095 {
                String::new()
            } else {
                format!("\"example.org/node_{}\"=\"1.0.0\"", index + 1)
            };
            sources.push(package(
                &format!("node_{index}"),
                "1.0.0",
                "",
                &dependencies,
            ));
        }
        let resolver = registry(&sources);
        let source = root("\"example.org/node_0\"=\"1.0.0\"");
        match resolver.resolve_document(&document(&source)) {
            Ok(graph) => {
                assert_eq!(graph.packages().len(), 4097);
                eprintln!(
                    "deep DAG 4096 nodes: resolved {} packages",
                    graph.packages().len()
                );
                let lock = graph.to_lock();
                let replay = resolver
                    .resolve_document_locked(&document(&source), &lock)
                    .unwrap();
                assert!(graph.root_identity() == replay.root_identity());
                assert_eq!(lock, replay.to_lock());
            }
            Err(error) => {
                let message = error.to_string().to_lowercase();
                eprintln!("deep DAG 4096 nodes: checked error: {message}");
                assert!(
                    ["limit", "depth", "size", "capacity"]
                        .iter()
                        .any(|term| message.contains(term)),
                    "unexpected deep-DAG error: {message}"
                );
            }
        }
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "deep_dags_are_checked_in_a_child_process_instead_of_aborting_the_suite",
            "--nocapture",
        ])
        .env(MARKER, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "deep solver crashed or failed: {output:?}"
    );
    eprintln!(
        "deep child stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    eprintln!(
        "deep child stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn legacy_manifest_version_and_resolver_apis_remain_usable() {
    let leaf = PackageManifest {
        name: "leaf".into(),
        domain: "example.org".into(),
        version: Version::new(1, 0, 0),
        package_type: PackageType::Lib,
        dependencies: vec![],
    };
    let root = PackageManifest {
        name: "root".into(),
        domain: "example.org".into(),
        version: Version::parse("1.0.0").unwrap(),
        package_type: PackageType::Exe,
        dependencies: vec![Dependency {
            domain: "example.org".into(),
            name: "leaf".into(),
            constraint: VersionConstraint::parse("^1.0.0").unwrap(),
        }],
    };
    let mut resolver = PackageResolver::new();
    resolver.register(leaf);
    let graph = resolver.resolve(&root).unwrap();
    assert_eq!(graph.packages.len(), 2);
    assert_eq!(graph.packages[0].qualified_name(), "example.org/leaf");
    assert_eq!(graph.packages[1].qualified_name(), "example.org/root");
    assert_eq!(root.entry_file(), "src/main.ns");
    let mut temporary = root.clone();
    temporary.package_type = PackageType::Tmp;
    assert_eq!(temporary.entry_file(), "src/lib.ns");
}

#[test]
fn schema_one_fixed_golden_covers_all_128_bits_and_leaf_graph_identity() {
    // Independently encoded with Python struct/hashlib from the frozen public
    // schema; no production encoder or result was used to generate this value.
    let source = package("root", "1.0.0", "", "");
    let doc = document(&source);
    assert_eq!(
        doc.local_identity().to_string(),
        "e902866d7a93e474b9f634b76df12ed5"
    );
    assert_eq!(
        doc.local_identity().as_bytes(),
        &[
            0xe9, 0x02, 0x86, 0x6d, 0x7a, 0x93, 0xe4, 0x74, 0xb9, 0xf6, 0x34, 0xb7, 0x6d, 0xf1,
            0x2e, 0xd5
        ]
    );
    let graph = registry(&[]).resolve_document(&doc).unwrap();
    assert!(graph.root_identity() == doc.local_identity());
    assert_eq!(
        graph.identity("example.org/root"),
        Some(doc.local_identity())
    );
    assert_eq!(graph.packages().len(), 1);
    let lock = graph.to_lock();
    let replay = registry(&[]).resolve_document_locked(&doc, &lock).unwrap();
    assert_eq!(
        replay.root_identity().to_string(),
        "e902866d7a93e474b9f634b76df12ed5"
    );
}

#[test]
fn five_hundred_twelve_dependency_edges_resolve_and_replay_exactly() {
    let mut sources = Vec::new();
    for index in 0..512 {
        let dependencies = if index == 511 {
            String::new()
        } else {
            format!("\"example.org/node_{}\"=\"1.0.0\"", index + 1)
        };
        sources.push(package(
            &format!("node_{index}"),
            "1.0.0",
            "",
            &dependencies,
        ));
    }
    let source = root("\"example.org/node_0\"=\"1.0.0\"");
    let resolver = registry(&sources);
    let graph = resolver.resolve_document(&document(&source)).unwrap();
    assert_eq!(graph.packages().len(), 513);
    assert_eq!(graph.packages()[0].qualified_name(), "example.org/node_511");
    assert_eq!(graph.packages()[511].qualified_name(), "example.org/node_0");
    assert_eq!(graph.packages()[512].qualified_name(), "example.org/root");
    for index in 0..512 {
        let name = format!("node_{index}");
        let selected = graph
            .packages()
            .iter()
            .find(|package| package.name == name)
            .unwrap();
        assert_eq!(selected.version, Version::new(1, 0, 0));
        assert!(graph.identity(&selected.qualified_name()).is_some());
    }
    let lock = graph.to_lock();
    let replay = resolver
        .resolve_document_locked(&document(&source), &lock)
        .unwrap();
    assert_eq!(replay.packages().len(), 513);
    eprintln!(
        "512 dependency edges: resolved {} packages; lock replay {} packages",
        graph.packages().len(),
        replay.packages().len()
    );
    assert!(replay.root_identity() == graph.root_identity());
    assert_eq!(replay.to_lock(), lock);
    for selected in graph.packages() {
        let name = selected.qualified_name();
        assert_eq!(replay.identity(&name), graph.identity(&name));
        assert_eq!(
            replay
                .packages()
                .iter()
                .find(|package| package.qualified_name() == name)
                .unwrap()
                .version,
            selected.version
        );
    }
}

#[test]
fn inline_dependency_metadata_is_retained_and_only_the_constraint_is_normalized() {
    let string = root("\"example.org/leaf\"=\"^1.0.0\"");
    let inline = root("\"example.org/leaf\"={version=\"^1.0.0\"}");
    assert_eq!(
        document(&string).local_identity(),
        document(&inline).local_identity()
    );
    let source = root(
        "\"example.org/leaf\"={version=\"^1.0.0\",features=[\"io\",\"net\"],unknown={version=\"one\"}}",
    );
    let doc = document(&source);
    assert_eq!(
        doc.metadata()["dependencies"]["example.org/leaf"]["features"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        doc.metadata()["dependencies"]["example.org/leaf"]["unknown"]["version"].as_str(),
        Some("one")
    );
    assert_ne!(doc.local_identity(), document(&string).local_identity());
    let leaf = package("leaf", "1.9.0", "", "");
    let baseline = solve(std::slice::from_ref(&leaf), &source);
    for changed in [
        source.replace("version=\"one\"", "version=\"two\""),
        source.replace("[\"io\",\"net\"]", "[\"net\",\"io\"]"),
        source.replace("^1.0.0", "~1.9.0"),
    ] {
        assert_ne!(doc.local_identity(), document(&changed).local_identity());
        let graph = solve(std::slice::from_ref(&leaf), &changed);
        assert_ne!(baseline.root_identity(), graph.root_identity());
        assert_eq!(
            graph
                .packages()
                .iter()
                .find(|package| package.name == "leaf")
                .unwrap()
                .version,
            Version::new(1, 9, 0)
        );
    }
}

#[test]
fn highest_candidates_with_missing_or_cyclic_transitive_edges_backtrack_to_valid_dags() {
    let old = package("a", "1.0.0", "flavor=10", "");
    let missing = package(
        "a",
        "2.0.0",
        "flavor=20",
        "\"example.org/unavailable\"=\"1.0.0\"",
    );
    let cyclic = package("a", "2.0.0", "flavor=20", "\"example.org/b\"=\"1.0.0\"");
    let b = package("b", "1.0.0", "", "\"example.org/a\"=\">=1.0.0, <3.0.0\"");
    let source = root("\"example.org/a\"=\">=1.0.0, <3.0.0\"");
    for sources in [vec![old.clone(), missing], vec![old.clone(), cyclic, b]] {
        assert_same_selection(&sources, &source, std::slice::from_ref(&old));
        let graph = solve(&sources, &source);
        assert_eq!(graph.packages().len(), 2);
        assert_eq!(
            graph
                .packages()
                .iter()
                .find(|package| package.name == "a")
                .unwrap()
                .version,
            Version::new(1, 0, 0)
        );
        let lock = graph.to_lock();
        let pinned = registry(&sources)
            .resolve_document_locked(&document(&source), &lock)
            .unwrap();
        assert_eq!(pinned.to_lock(), lock);
    }
}

#[test]
fn admitted_prereleases_keep_partial_comparator_precision_in_matching_and_identity() {
    for (partial, full, admission, candidate, partial_matches, full_matches) in [
        (
            "<3.0",
            "<3.0.0",
            ">=3.0.0-alpha",
            "3.0.0-alpha",
            false,
            true,
        ),
        (
            ">=1.0",
            ">=1.0.0",
            ">=1.0.1-alpha",
            "1.0.1-alpha",
            false,
            true,
        ),
        (
            "^1.0",
            "^1.0.0",
            ">=1.0.0-alpha",
            "1.0.0-alpha",
            true,
            false,
        ),
        (
            "~1.0",
            "~1.0.0",
            ">=1.0.1-alpha",
            "1.0.1-alpha",
            false,
            true,
        ),
    ] {
        let version = Version::parse(candidate).unwrap();
        let source = |constraint: &str| root(&format!("\"example.org/leaf\"={constraint:?}"));
        // Without admission, both constraints exclude prereleases and their
        // stable-only canonical identities remain equivalent.
        assert!(!VersionConstraint::parse(partial).unwrap().matches(&version));
        assert!(!VersionConstraint::parse(full).unwrap().matches(&version));
        assert_eq!(
            document(&source(partial)).local_identity(),
            document(&source(full)).local_identity()
        );
        let partial_request = format!("{partial}, {admission}");
        let full_request = format!("{full}, {admission}");
        assert_eq!(
            VersionConstraint::parse(&partial_request)
                .unwrap()
                .matches(&version),
            partial_matches,
            "{partial_request}: {candidate}"
        );
        assert_eq!(
            VersionConstraint::parse(&full_request)
                .unwrap()
                .matches(&version),
            full_matches,
            "{full_request}: {candidate}"
        );
        assert_ne!(
            document(&source(&partial_request)).local_identity(),
            document(&source(&full_request)).local_identity()
        );
        let leaf = package("leaf", candidate, "flavor=42", "");
        let resolver = registry(&[leaf]);
        for (request, expected) in [
            (&partial_request, partial_matches),
            (&full_request, full_matches),
        ] {
            let doc = document(&source(request));
            let result = resolver.resolve_document(&doc);
            assert_eq!(
                result.is_ok(),
                expected,
                "{request}: {candidate}: {result:?}"
            );
            if let Ok(graph) = result {
                assert_eq!(graph.packages().len(), 2);
                assert_eq!(
                    graph
                        .packages()
                        .iter()
                        .find(|package| package.name == "leaf")
                        .unwrap()
                        .version,
                    version
                );
                let lock = graph.to_lock();
                let replay = resolver.resolve_document_locked(&doc, &lock).unwrap();
                assert_eq!(replay.root_identity(), graph.root_identity());
                assert_eq!(replay.to_lock(), lock);
            }
        }
    }
}
