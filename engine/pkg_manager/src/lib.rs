//! Package manager — manifest parsing (`package.toml`), module discovery,
//! and dependency resolution.

use std::path::PathBuf;

mod bundled;
mod error;
mod identity;
mod lock;
mod lockfile;
mod manifest;
mod resolver;
mod source;
mod version;
mod workspace;

#[cfg(test)]
mod foundation_tests;

pub use bundled::{bundled_packages, bundled_packages_sha256, is_bundled_package_root};
pub use error::PackageError;
pub use identity::PackageIdentity;
pub use lockfile::{
    LOCKFILE_VERSION, LockedPackage, Lockfile, compute_package_sha256, relative_normalized_path,
};
pub use manifest::{FfiConfig, ManifestDocument, PathDependency};
pub use resolver::{PackageResolver, ResolvedPackageGraph};
pub use source::{
    ModuleSources, PackageSourceError, PackageSources, SourceFile, SourceLoadLimits,
    discover_modules, discover_modules_with_limits, load_package_sources,
    load_package_sources_with_limits,
};
pub use version::{Version, VersionConstraint};
pub use workspace::{
    PackageLoadError, PackageWorkspace, load_package_workspace, load_package_workspace_with_limits,
};

// ---------------------------------------------------------------------------
// PackageType
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageType {
    Exe,
    Lib,
    /// Temporary / scratch package.
    Tmp,
}

impl PackageType {
    pub(crate) fn entry_file(self) -> &'static str {
        match self {
            Self::Exe => "src/main.ns",
            Self::Lib | Self::Tmp => "src/lib.ns",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "exe" => Some(Self::Exe),
            "lib" => Some(Self::Lib),
            "tmp" => Some(Self::Tmp),
            _ => None,
        }
    }
}

impl std::str::FromStr for PackageType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or(())
    }
}

// ---------------------------------------------------------------------------
// PackageManifest
// ---------------------------------------------------------------------------

/// A parsed `package.toml` manifest.
#[derive(Debug, Clone)]
pub struct PackageManifest {
    pub name: String,
    pub domain: String,
    pub version: Version,
    pub package_type: PackageType,
    pub dependencies: Vec<Dependency>,
}

impl PackageManifest {
    /// The entry file for this package.
    pub fn entry_file(&self) -> &str {
        self.package_type.entry_file()
    }

    /// Full qualified package identifier: domain/name
    pub fn qualified_name(&self) -> String {
        format!("{}/{}", self.domain, self.name)
    }
}

/// A dependency declaration.
#[derive(Debug, Clone)]
pub struct Dependency {
    pub domain: String,
    pub name: String,
    pub constraint: VersionConstraint,
}

// ---------------------------------------------------------------------------
// Module discovery — filesystem mapping
// ---------------------------------------------------------------------------

/// A discovered module in the file system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredModule {
    /// Module path segments (e.g. ["net", "http"]).
    pub path: Vec<String>,
    /// Full file path.
    pub file_path: PathBuf,
    /// Child modules.
    pub children: Vec<DiscoveredModule>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parse() {
        let v = Version::parse("1.2.3").unwrap();
        assert_eq!(v, Version::new(1, 2, 3));
        assert!(Version::parse("1.2").is_none());
    }

    #[test]
    fn version_constraint_exact() {
        let c = VersionConstraint::parse("1.0.0").unwrap();
        assert!(c.matches(&Version::new(1, 0, 0)));
        assert!(!c.matches(&Version::new(1, 0, 1)));
    }

    #[test]
    fn version_constraint_tilde() {
        let c = VersionConstraint::parse("~1.2.0").unwrap();
        assert!(c.matches(&Version::new(1, 2, 0)));
        assert!(c.matches(&Version::new(1, 2, 5)));
        assert!(!c.matches(&Version::new(1, 3, 0)));
    }

    #[test]
    fn package_resolver_simple() {
        let mut resolver = PackageResolver::new();

        let dep = ManifestDocument::parse(
            r#"[package]
name = "utils"
domain = "com.test"
version = "1.0.0"
type = "lib"
"#,
        )
        .unwrap();
        resolver.register_document(dep).unwrap();

        let root = ManifestDocument::parse(
            r#"[package]
name = "app"
domain = "com.test"
version = "0.1.0"
type = "exe"

[dependencies]
"com.test/utils" = "1.0.0"
"#,
        )
        .unwrap();

        let resolved = resolver.resolve_document(&root).unwrap();
        assert_eq!(resolved.packages().len(), 2);
        // Dependency comes first.
        assert_eq!(resolved.packages()[0].name, "utils");
        assert_eq!(resolved.packages()[1].name, "app");
    }

    #[test]
    fn package_resolver_missing_dep() {
        let resolver = PackageResolver::new();
        let root = ManifestDocument::parse(
            r#"[package]
name = "app"
domain = "com.test"
version = "0.1.0"
type = "exe"

[dependencies]
"com.test/missing" = "1.0.0"
"#,
        )
        .unwrap();
        assert!(resolver.resolve_document(&root).is_err());
    }

    #[test]
    fn entry_file_exe_vs_lib() {
        let exe = PackageManifest {
            name: "a".into(),
            domain: "d".into(),
            version: Version::new(0, 1, 0),
            package_type: PackageType::Exe,
            dependencies: vec![],
        };
        assert_eq!(exe.entry_file(), "src/main.ns");

        let lib = PackageManifest {
            name: "b".into(),
            domain: "d".into(),
            version: Version::new(0, 1, 0),
            package_type: PackageType::Lib,
            dependencies: vec![],
        };
        assert_eq!(lib.entry_file(), "src/lib.ns");
    }
}
