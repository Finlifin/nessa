//! Package manager — manifest parsing (`package.toml`), module discovery,
//! and dependency resolution.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Version
// ---------------------------------------------------------------------------

/// A semantic version.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self { major, minor, patch }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 3 {
            return None;
        }
        Some(Self {
            major: parts[0].parse().ok()?,
            minor: parts[1].parse().ok()?,
            patch: parts[2].parse().ok()?,
        })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

// ---------------------------------------------------------------------------
// VersionConstraint
// ---------------------------------------------------------------------------

/// A version constraint for dependency resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionConstraint {
    /// Exact match: "1.2.3"
    Exact(Version),
    /// Compatible updates: "^1.2.0" means >=1.2.0, <2.0.0
    Caret(Version),
    /// Patch-level updates: "~1.2.0" means >=1.2.0, <1.3.0
    Tilde(Version),
}

impl VersionConstraint {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some(rest) = s.strip_prefix('^') {
            Some(Self::Caret(Version::parse(rest)?))
        } else if let Some(rest) = s.strip_prefix('~') {
            Some(Self::Tilde(Version::parse(rest)?))
        } else {
            Some(Self::Exact(Version::parse(s)?))
        }
    }

    pub fn matches(&self, v: &Version) -> bool {
        match self {
            Self::Exact(req) => v == req,
            Self::Caret(req) => {
                v.major == req.major
                    && (v.major > 0
                        && (v.minor > req.minor
                            || (v.minor == req.minor && v.patch >= req.patch)))
                    || v == req
            }
            Self::Tilde(req) => {
                v.major == req.major
                    && v.minor == req.minor
                    && v.patch >= req.patch
            }
        }
    }
}

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
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "exe" => Some(Self::Exe),
            "lib" => Some(Self::Lib),
            "tmp" => Some(Self::Tmp),
            _ => None,
        }
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
        match self.package_type {
            PackageType::Exe => "src/main.ns",
            PackageType::Lib | PackageType::Tmp => "src/lib.ns",
        }
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
#[derive(Debug, Clone)]
pub struct DiscoveredModule {
    /// Module path segments (e.g. ["net", "http"]).
    pub path: Vec<String>,
    /// Full file path.
    pub file_path: PathBuf,
    /// Child modules.
    pub children: Vec<DiscoveredModule>,
}

/// Discover modules under a `src/` directory.
pub fn discover_modules(src_dir: &Path) -> Vec<DiscoveredModule> {
    let mut modules = Vec::new();
    if !src_dir.is_dir() {
        return modules;
    }
    discover_recursive(src_dir, &[], &mut modules);
    modules
}

fn discover_recursive(
    dir: &Path,
    prefix: &[String],
    modules: &mut Vec<DiscoveredModule>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        if path.is_file() && name_str.ends_with(".ns") {
            let mod_name = name_str.trim_end_matches(".ns").to_string();
            // Skip entry files.
            if mod_name == "main" || mod_name == "lib" || mod_name == "mod" {
                continue;
            }
            let mut mod_path = prefix.to_vec();
            mod_path.push(mod_name);
            modules.push(DiscoveredModule {
                path: mod_path,
                file_path: path,
                children: Vec::new(),
            });
        } else if path.is_dir() {
            let mod_name = name_str.to_string();
            let mod_entry = path.join("mod.ns");
            if mod_entry.exists() {
                let mut mod_path = prefix.to_vec();
                mod_path.push(mod_name);
                let mut children = Vec::new();
                discover_recursive(&path, &mod_path, &mut children);
                modules.push(DiscoveredModule {
                    path: mod_path,
                    file_path: mod_entry,
                    children,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PackageResolver — simple dependency resolution
// ---------------------------------------------------------------------------

/// A resolved dependency graph.
#[derive(Debug)]
pub struct ResolvedPackages {
    /// Packages in dependency order (leaves first).
    pub packages: Vec<PackageManifest>,
}

/// Simple package resolver.
pub struct PackageResolver {
    /// Known packages: qualified_name → available versions.
    registry: HashMap<String, Vec<PackageManifest>>,
}

impl PackageResolver {
    pub fn new() -> Self {
        Self {
            registry: HashMap::new(),
        }
    }

    /// Register a package as available.
    pub fn register(&mut self, manifest: PackageManifest) {
        let key = manifest.qualified_name();
        self.registry.entry(key).or_default().push(manifest);
    }

    /// Resolve dependencies for a root package.
    pub fn resolve(&self, root: &PackageManifest) -> Result<ResolvedPackages, String> {
        let mut resolved = Vec::new();
        let mut visited = std::collections::HashSet::new();
        self.resolve_inner(root, &mut resolved, &mut visited)?;
        Ok(ResolvedPackages { packages: resolved })
    }

    fn resolve_inner(
        &self,
        pkg: &PackageManifest,
        resolved: &mut Vec<PackageManifest>,
        visited: &mut std::collections::HashSet<String>,
    ) -> Result<(), String> {
        let qn = pkg.qualified_name();
        if visited.contains(&qn) {
            return Ok(());
        }
        visited.insert(qn);

        for dep in &pkg.dependencies {
            let dep_qn = format!("{}/{}", dep.domain, dep.name);
            let versions = self
                .registry
                .get(&dep_qn)
                .ok_or_else(|| format!("package not found: {dep_qn}"))?;

            let matched = versions
                .iter()
                .find(|v| dep.constraint.matches(&v.version))
                .ok_or_else(|| {
                    format!(
                        "no matching version for {dep_qn} (constraint: {:?})",
                        dep.constraint
                    )
                })?;

            self.resolve_inner(matched, resolved, visited)?;
        }

        resolved.push(pkg.clone());
        Ok(())
    }
}

impl Default for PackageResolver {
    fn default() -> Self {
        Self::new()
    }
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

        let dep = PackageManifest {
            name: "utils".into(),
            domain: "com.test".into(),
            version: Version::new(1, 0, 0),
            package_type: PackageType::Lib,
            dependencies: vec![],
        };
        resolver.register(dep);

        let root = PackageManifest {
            name: "app".into(),
            domain: "com.test".into(),
            version: Version::new(0, 1, 0),
            package_type: PackageType::Exe,
            dependencies: vec![Dependency {
                domain: "com.test".into(),
                name: "utils".into(),
                constraint: VersionConstraint::parse("1.0.0").unwrap(),
            }],
        };

        let resolved = resolver.resolve(&root).unwrap();
        assert_eq!(resolved.packages.len(), 2);
        // Dependency comes first.
        assert_eq!(resolved.packages[0].name, "utils");
        assert_eq!(resolved.packages[1].name, "app");
    }

    #[test]
    fn package_resolver_missing_dep() {
        let resolver = PackageResolver::new();
        let root = PackageManifest {
            name: "app".into(),
            domain: "com.test".into(),
            version: Version::new(0, 1, 0),
            package_type: PackageType::Exe,
            dependencies: vec![Dependency {
                domain: "com.test".into(),
                name: "missing".into(),
                constraint: VersionConstraint::parse("1.0.0").unwrap(),
            }],
        };
        assert!(resolver.resolve(&root).is_err());
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
