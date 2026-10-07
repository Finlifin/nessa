//! Exact selections are validated against manifests, never silently re-solved.

use std::collections::{BTreeMap, BTreeSet};

use crate::resolver::{build_graph, dependency_names};
use crate::{
    ManifestDocument, PackageError, PackageIdentity, PackageResolver, ResolvedPackageGraph, Version,
};

const LOCK_SCHEMA: i64 = 1;

struct Entry {
    version: Version,
    identity: PackageIdentity,
    dependencies: Vec<String>,
}

pub(crate) fn write(graph: &ResolvedPackageGraph) -> String {
    let root = &graph.documents[&graph.root];
    let mut source = format!(
        "schema_version = {LOCK_SCHEMA}\nidentity_schema = {}\n\n[root]\nqualified_name = {}\nversion = {}\nidentity = {}\n",
        crate::identity::SCHEMA,
        quoted(&graph.root),
        quoted(&root.manifest.version.to_string()),
        quoted(&graph.root_identity().to_string())
    );
    for (name, document) in &graph.documents {
        let dependencies = dependency_names(document)
            .iter()
            .map(|name| quoted(name))
            .collect::<Vec<_>>()
            .join(", ");
        source.push_str(&format!("\n[[packages]]\nqualified_name = {}\nversion = {}\nidentity = {}\ndependencies = [{dependencies}]\n", quoted(name), quoted(&document.manifest.version.to_string()), quoted(&graph.identities[name].to_string())));
    }
    source
}

fn quoted(source: &str) -> String {
    toml::Value::String(source.to_owned()).to_string()
}

pub(crate) fn resolve(
    resolver: &PackageResolver,
    root: &ManifestDocument,
    source: &str,
) -> Result<ResolvedPackageGraph, PackageError> {
    if source.len() > 32 * 1024 * 1024 {
        return Err(PackageError::Capacity("lock bytes (32 MiB)"));
    }
    let table: toml::Table = toml::from_str(source).map_err(|error| invalid(error.to_string()))?;
    keys(
        &table,
        &["schema_version", "identity_schema", "root", "packages"],
    )?;
    if table
        .get("schema_version")
        .and_then(toml::Value::as_integer)
        != Some(LOCK_SCHEMA)
        || table
            .get("identity_schema")
            .and_then(toml::Value::as_integer)
            != Some(crate::identity::SCHEMA.into())
    {
        return Err(invalid("unsupported or missing lock/identity schema"));
    }
    let root_table = table
        .get("root")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| invalid("missing root table"))?;
    keys(root_table, &["qualified_name", "version", "identity"])?;
    let root_name = text(root_table, "qualified_name")?;
    let root_version = version(root_table)?;
    let root_identity = identity(root_table)?;
    if root_name != root.manifest.qualified_name() || root_version != root.manifest.version {
        return Err(invalid("root name or version changed"));
    }
    let packages = table
        .get("packages")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| invalid("missing packages array"))?;
    if packages.len() > 65_536 {
        return Err(PackageError::Capacity("locked packages (65536)"));
    }
    let mut entries = BTreeMap::new();
    for value in packages {
        let package = value
            .as_table()
            .ok_or_else(|| invalid("package entry must be a table"))?;
        keys(
            package,
            &["qualified_name", "version", "identity", "dependencies"],
        )?;
        let name = text(package, "qualified_name")?.to_owned();
        crate::manifest::split_name(&name).map_err(|error| invalid(error.to_string()))?;
        let dependencies = package
            .get("dependencies")
            .and_then(toml::Value::as_array)
            .ok_or_else(|| invalid("missing dependency array"))?;
        let mut names = BTreeSet::new();
        for dependency in dependencies {
            let name = dependency
                .as_str()
                .ok_or_else(|| invalid("dependency name must be a string"))?;
            crate::manifest::split_name(name).map_err(|error| invalid(error.to_string()))?;
            if !names.insert(name.to_owned()) {
                return Err(invalid("duplicate dependency edge"));
            }
        }
        let entry = Entry {
            version: version(package)?,
            identity: identity(package)?,
            dependencies: names.into_iter().collect(),
        };
        if entries.insert(name.clone(), entry).is_some() {
            return Err(invalid(format!("duplicate package: {name}")));
        }
    }
    let root_entry = entries
        .get(root_name)
        .ok_or_else(|| invalid("root package entry missing"))?;
    if root_entry.version != root_version || root_entry.identity != root_identity {
        return Err(invalid("root entry inconsistent"));
    }
    let mut documents = BTreeMap::new();
    for (name, entry) in &entries {
        let document = if name == root_name {
            root.clone()
        } else {
            resolver
                .registry
                .get(name)
                .and_then(|versions| versions.get(&entry.version.to_string()))
                .map(|document| document.as_ref().clone())
                .ok_or_else(|| {
                    invalid(format!(
                        "pinned package unavailable: {name}@{}",
                        entry.version
                    ))
                })?
        };
        if dependency_names(&document) != entry.dependencies {
            return Err(invalid(format!("stale dependency edges: {name}")));
        }
        documents.insert(name.clone(), document);
    }
    // Every pinned edge must satisfy its declared constraint. Missing entries
    // fail here; the graph constructor separately rejects cycles and extras.
    for (name, document) in &documents {
        for dependency in &document.manifest.dependencies {
            let child = format!("{}/{}", dependency.domain, dependency.name);
            let entry = entries
                .get(&child)
                .ok_or_else(|| invalid(format!("missing pinned dependency: {child}")))?;
            if !dependency.constraint.matches(&entry.version) {
                return Err(invalid(format!(
                    "incompatible pinned dependency {name} -> {child}"
                )));
            }
        }
    }
    let graph =
        build_graph(root_name.to_owned(), documents).map_err(|error| invalid(error.to_string()))?;
    if graph.documents.len() != entries.len() {
        return Err(invalid("extra package entries"));
    }
    for (name, entry) in entries {
        if graph.identities.get(&name) != Some(&entry.identity) {
            return Err(invalid(format!("identity mismatch: {name}")));
        }
    }
    if graph.root_identity() != root_identity {
        return Err(invalid("root identity mismatch"));
    }
    Ok(graph)
}

fn keys(table: &toml::Table, allowed: &[&str]) -> Result<(), PackageError> {
    if let Some(key) = table.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid(format!("unknown lock field: {key}")));
    }
    Ok(())
}
fn text<'a>(table: &'a toml::Table, key: &str) -> Result<&'a str, PackageError> {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .ok_or_else(|| invalid(format!("missing string: {key}")))
}
fn version(table: &toml::Table) -> Result<Version, PackageError> {
    Version::parse(text(table, "version")?).ok_or_else(|| invalid("invalid exact version"))
}
fn identity(table: &toml::Table) -> Result<PackageIdentity, PackageError> {
    PackageIdentity::parse(text(table, "identity")?).ok_or_else(|| invalid("invalid identity hex"))
}
fn invalid(message: impl Into<String>) -> PackageError {
    PackageError::Lock(message.into())
}
