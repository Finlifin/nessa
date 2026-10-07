use crate::{ManifestDocument, PackageError, PackageResolver, Version, VersionConstraint};

fn document(name: &str, version: &str, dependencies: &[(&str, &str)]) -> ManifestDocument {
    let mut source = format!(
        "[package]\nname={name:?}\ndomain=\"org.test\"\nversion={version:?}\ntype=\"lib\"\n[dependencies]\n"
    );
    for (name, constraint) in dependencies {
        source.push_str(&format!("\"org.test/{name}\"={constraint:?}\n"));
    }
    ManifestDocument::parse(&source).unwrap()
}

#[test]
fn identities_cover_typed_unknown_metadata_and_normalize_only_documented_inputs() {
    let left = ManifestDocument::parse("[package]\nname='p'\ndomain='d'\nversion='1.0.0'\ntype='lib'\nkeywords=['a','b']\n[extra]\nx=42\ndate=1979-05-27T07:32:00Z").unwrap();
    let right = ManifestDocument::parse("[extra]\ndate=1979-05-27T07:32:00+00:00\nx=42\n[package]\nkeywords=[ 'a', 'b' ]\ntype='lib'\nversion='9.0.0-alpha+build'\ndomain='d'\nname='p'").unwrap();
    assert_eq!(left.local_identity(), right.local_identity());
    assert_ne!(left.local_identity(), ManifestDocument::parse("[package]\nname='p'\ndomain='d'\nversion='1.0.0'\ntype='lib'\nkeywords=['b','a']\n[extra]\nx=42\ndate=1979-05-27T07:32:00Z").unwrap().local_identity());
    let integer = ManifestDocument::parse(
        "[package]\nname='p'\ndomain='d'\nversion='1.0.0'\ntype='lib'\nx=1",
    )
    .unwrap();
    let float = ManifestDocument::parse(
        "[package]\nname='p'\ndomain='d'\nversion='1.0.0'\ntype='lib'\nx=1.0",
    )
    .unwrap();
    assert_ne!(integer.local_identity(), float.local_identity());
    let inline = |flag| {
        ManifestDocument::parse(&format!("[package]\nname='p'\ndomain='d'\nversion='1.0.0'\ntype='lib'\n[dependencies]\n'd/q'={{version='^1.0',flag={flag}}}")).unwrap()
    };
    assert_ne!(
        inline(true).local_identity(),
        inline(false).local_identity()
    );
    assert!(
        inline(true).metadata()["dependencies"]["d/q"]["flag"]
            .as_bool()
            .unwrap()
    );
}

#[test]
fn constraint_adts_preserve_semantic_boundaries_prereleases_and_zero_major() {
    for (constraint, yes, no) in [
        ("^0.2.3", "0.2.9", "0.3.0"),
        ("^0.0.3", "0.0.3", "0.0.4"),
        ("~1.2", "1.2.9", "1.3.0"),
        (">=1.0, <3.0", "2.9.9", "3.0.0"),
        ("^1.2.3-alpha", "1.2.3-beta", "1.3.0-alpha"),
    ] {
        let constraint = VersionConstraint::parse(constraint).unwrap();
        assert!(constraint.matches(&Version::parse(yes).unwrap()));
        assert!(!constraint.matches(&Version::parse(no).unwrap()));
    }
    assert_eq!(
        VersionConstraint::parse(">=1.0,<3.0").unwrap().canonical(),
        VersionConstraint::parse("<3.0.0,>=1.0.0,>=1.0.0")
            .unwrap()
            .canonical()
    );
    assert_ne!(
        VersionConstraint::parse(">1.0").unwrap().canonical(),
        VersionConstraint::parse(">1.0.0").unwrap().canonical()
    );
    assert_eq!(
        VersionConstraint::parse("^ 1.2.3").unwrap().canonical(),
        VersionConstraint::parse("^1.2.3").unwrap().canonical()
    );
    let partial = VersionConstraint::parse(">=3.0.0-alpha,<3.0").unwrap();
    let full = VersionConstraint::parse(">=3.0.0-alpha,<3.0.0").unwrap();
    assert!(!partial.matches(&Version::parse("3.0.0-beta").unwrap()));
    assert!(full.matches(&Version::parse("3.0.0-beta").unwrap()));
    assert_ne!(partial.canonical(), full.canonical());
    assert!(
        VersionConstraint::parse("1.0.0+build")
            .unwrap()
            .matches(&Version::parse("1.0.0+other").unwrap())
    );
}

#[test]
fn deterministic_joint_solving_backtracks_and_lock_does_not_upgrade() {
    let root = document("root", "1.0.0", &[("a", "^1.0"), ("b", "1.0.0")]);
    let mut resolver = PackageResolver::new();
    for entry in [
        document("a", "1.2.0", &[("c", "2.0.0")]),
        document("a", "1.1.0", &[("c", "1.0.0")]),
        document("b", "1.0.0", &[("c", "1.0.0")]),
        document("c", "1.0.0", &[]),
        document("c", "2.0.0", &[]),
    ] {
        resolver.register_document(entry).unwrap();
    }
    let graph = resolver.resolve_document(&root).unwrap();
    assert_eq!(
        graph
            .packages()
            .iter()
            .find(|package| package.name == "a")
            .unwrap()
            .version,
        Version::new(1, 1, 0)
    );
    let lock = graph.to_lock();
    resolver
        .register_document(document("a", "1.3.0", &[("c", "1.0.0")]))
        .unwrap();
    let pinned = resolver.resolve_document_locked(&root, &lock).unwrap();
    assert_eq!(pinned.to_lock(), lock);
    assert_eq!(pinned.root_identity(), graph.root_identity());
    assert_eq!(
        resolver
            .resolve_document(&root)
            .unwrap()
            .packages()
            .iter()
            .find(|package| package.name == "a")
            .unwrap()
            .version,
        Version::new(1, 3, 0)
    );
}

#[test]
fn registry_cycle_missing_and_conflict_fail_without_partial_graphs() {
    let mut resolver = PackageResolver::new();
    let a = document("a", "1.0.0", &[("b", "1.0.0")]);
    resolver.register_document(a.clone()).unwrap();
    resolver.register_document(a.clone()).unwrap();
    assert!(matches!(
        resolver.resolve_document(&a),
        Err(PackageError::MissingPackage(_))
    ));
    resolver
        .register_document(document("b", "1.0.0", &[("a", "1.0.0")]))
        .unwrap();
    assert!(matches!(
        resolver.resolve_document(&a),
        Err(PackageError::Cycle(_))
    ));
    assert!(
        resolver
            .register_document(document("a", "1.0.0", &[]))
            .is_err()
    );
    let conflict = document("root", "1.0.0", &[("a", "2.0.0")]);
    assert!(matches!(
        resolver.resolve_document(&conflict),
        Err(PackageError::Conflict(_))
    ));
}

#[test]
fn lock_damage_missing_extra_and_root_changes_are_rejected() {
    let root = document("root", "1.0.0", &[("child", "^1.0")]);
    let mut resolver = PackageResolver::new();
    resolver
        .register_document(document("child", "1.0.0", &[]))
        .unwrap();
    let lock = resolver.resolve_document(&root).unwrap().to_lock();
    for damaged in [
        lock.replace("schema_version = 1", "schema_version = 2"),
        lock.replace("[root]", "unknown = 1\n[root]"),
        lock.replace("dependencies = [\"org.test/child\"]", "dependencies = []"),
        format!(
            "{lock}\n[[packages]]\nqualified_name='org.test/extra'\nversion='1.0.0'\nidentity='00000000000000000000000000000000'\ndependencies=[]\n"
        ),
        lock.replace("version = \"1.0.0\"", "version = \"2.0.0\""),
    ] {
        assert!(
            resolver.resolve_document_locked(&root, &damaged).is_err(),
            "{damaged}"
        );
    }
    let changed = document("root", "2.0.0", &[("child", "^1.0")]);
    assert_eq!(changed.local_identity(), root.local_identity());
    assert!(resolver.resolve_document_locked(&changed, &lock).is_err());
}

#[test]
fn deep_dags_are_resolved_without_recursive_search_or_hashing() {
    let mut resolver = PackageResolver::new();
    for index in 0..512 {
        let child = format!("p{}", index + 1);
        resolver
            .register_document(document(
                &format!("p{index}"),
                "1.0.0",
                &[(child.as_str(), "1.0.0")],
            ))
            .unwrap();
    }
    resolver
        .register_document(document("p512", "1.0.0", &[]))
        .unwrap();
    let root = document("root", "1.0.0", &[("p0", "1.0.0")]);
    let graph = resolver.resolve_document(&root).unwrap();
    assert_eq!(graph.packages().len(), 514);
    assert_eq!(graph.packages().first().unwrap().name, "p512");
    assert_eq!(
        resolver
            .resolve_document_locked(&root, &graph.to_lock())
            .unwrap()
            .root_identity(),
        graph.root_identity()
    );
}

#[test]
fn malformed_required_fields_and_duplicate_toml_keys_are_rejected() {
    for source in [
        "",
        "[package]\nname='p'",
        "[package]\nname='p'\nname='q'",
        "[package]\nname='p'\ndomain='d'\nversion='1.0'\ntype='lib'",
        "[package]\nname='p'\ndomain='d'\nversion='1.0.0'\ntype='bad'",
        "[package]\nname='p'\ndomain='d'\nversion='1.0.0'\ntype='lib'\n[dependencies]\ninvalid='1.0.0'",
    ] {
        assert!(ManifestDocument::parse(source).is_err());
    }
}
