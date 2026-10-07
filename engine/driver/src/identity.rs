//! Explicit compilation provenance, separate from diagnostic source names.

use pkg_manager::ResolvedPackageGraph;
use resolution::SourcePackageIdentity;
use type_pool::{IdentityPathSegment, PackageTypeContext};

/// Real package provenance and optional source-type version overrides.
#[derive(Debug, Clone)]
pub struct CompilationIdentityContext {
    pub package: PackageTypeContext,
    /// Defaults to the package version; applies to this package only.
    pub last_stable_version: Option<String>,
    /// Original declaration paths; duplicates and unused paths are errors.
    pub type_versions: Vec<(Vec<IdentityPathSegment>, String)>,
}

impl CompilationIdentityContext {
    /// Obtain provenance from the validated graph, including selected dependency identities.
    pub fn from_graph(graph: &ResolvedPackageGraph) -> Result<Self, String> {
        let manifest = graph.packages().last().ok_or("package graph has no root")?;
        let name = manifest.qualified_name();
        if graph.identity(&name) != Some(graph.root_identity()) {
            return Err("package graph root is inconsistent with dependency order".into());
        }
        Self::for_package(graph, &name)
    }

    /// Obtain the selected package's provenance without substituting the root's identity.
    pub fn for_package(graph: &ResolvedPackageGraph, name: &str) -> Result<Self, String> {
        let manifest = graph
            .packages()
            .iter()
            .find(|manifest| manifest.qualified_name() == name)
            .ok_or_else(|| format!("package `{name}` is not in the selected graph"))?;
        let identity = graph
            .identity(name)
            .ok_or_else(|| format!("selected package `{name}` has no identity"))?;
        Ok(Self {
            package: PackageTypeContext {
                identity_schema: 1,
                identity: *identity.as_bytes(),
                qualified_name: name.to_owned(),
                version: manifest.version.to_string(),
            },
            last_stable_version: None,
            type_versions: Vec::new(),
        })
    }

    pub(crate) fn into_source(self) -> SourcePackageIdentity {
        SourcePackageIdentity {
            package: self.package,
            last_stable_version: self.last_stable_version,
            type_versions: self.type_versions,
        }
    }
}

pub(crate) fn standard_library_identity() -> Result<SourcePackageIdentity, String> {
    let document =
        pkg_manager::ManifestDocument::parse(include_str!("../../../library/std/package.toml"))
            .map_err(|error| error.to_string())?;
    let graph = pkg_manager::PackageResolver::new()
        .resolve_document(&document)
        .map_err(|error| error.to_string())?;
    Ok(CompilationIdentityContext::from_graph(&graph)?.into_source())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_context_carries_real_manifest_identity_and_full_version() {
        let document = pkg_manager::ManifestDocument::parse(
            "[package]\nname='app'\ndomain='org.test'\nversion='2.3.4-beta.1+build.5'\ntype='lib'\ncustom=42\n",
        ).unwrap();
        let graph = pkg_manager::PackageResolver::new()
            .resolve_document(&document)
            .unwrap();
        let context = CompilationIdentityContext::from_graph(&graph).unwrap();
        assert_eq!(context.package.identity, *graph.root_identity().as_bytes());
        assert_eq!(context.package.qualified_name, "org.test/app");
        assert_eq!(context.package.version, "2.3.4-beta.1+build.5");
        assert!(context.last_stable_version.is_none());
        assert!(context.type_versions.is_empty());
    }

    #[test]
    fn standard_library_has_separate_checked_package_context() {
        let context = standard_library_identity().unwrap();
        assert_eq!(context.package.identity_schema, 1);
        assert_eq!(context.package.qualified_name, "org.nessa/std");
        assert_eq!(context.package.version, "0.1.0");
        assert_ne!(
            context.package.identity,
            resolution::scratch_package_identity("").unwrap().identity
        );
    }
}
