//! Recursive local path dependency discovery and workspace graph validation.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::lockfile::Lockfile;
use crate::{
    PackageError, PackageResolver, PackageSourceError, PackageSources, SourceLoadLimits, Version,
    VersionConstraint, load_package_sources_with_limits,
};

const MAX_WORKSPACE_PACKAGES: usize = 65_536;

/// Structured errors reported while loading a disk package and its local path dependencies.
#[derive(Debug)]
pub enum PackageLoadError {
    Source(Box<PackageSourceError>),
    MissingDependencyPath {
        package: Box<str>,
        dependency: Box<str>,
        declared_path: Box<str>,
        resolved_path: PathBuf,
        source: io::Error,
    },
    Cycle {
        chain: Vec<String>,
    },
    VersionMismatch {
        package: Box<str>,
        dependency: Box<str>,
        constraint: Box<VersionConstraint>,
        found: Box<Version>,
        path: PathBuf,
    },
    IdentityMismatch {
        package: Box<str>,
        dependency_key: Box<str>,
        expected: Box<str>,
        found: Box<str>,
        path: PathBuf,
    },
    ConflictingDependencyPath {
        dependency: Box<str>,
        first_path: PathBuf,
        second_path: PathBuf,
    },
    InvalidLockfile {
        path: PathBuf,
        source: PackageError,
    },
    Resolution(PackageError),
}

impl fmt::Display for PackageLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(error) => error.fmt(f),
            Self::MissingDependencyPath {
                package,
                dependency,
                declared_path,
                resolved_path,
                source,
            } => write!(
                f,
                "cannot resolve dependency `{dependency}` (`{declared_path}` -> {}) declared by `{package}`: {source}",
                resolved_path.display()
            ),
            Self::Cycle { chain } => {
                write!(f, "package dependency cycle: {}", chain.join(" -> "))
            }
            Self::VersionMismatch {
                package,
                dependency,
                constraint,
                found,
                path,
            } => write!(
                f,
                "dependency `{dependency}` in `{package}` requires version `{constraint}`, but {} declares `{found}`",
                path.display()
            ),
            Self::IdentityMismatch {
                package,
                dependency_key,
                expected,
                found,
                path,
            } => write!(
                f,
                "dependency `{dependency_key}` in `{package}` expects `{expected}`, but {} declares `{found}`",
                path.display()
            ),
            Self::ConflictingDependencyPath {
                dependency,
                first_path,
                second_path,
            } => write!(
                f,
                "conflicting local path definitions for dependency `{dependency}`: {} vs {}",
                first_path.display(),
                second_path.display()
            ),
            Self::InvalidLockfile { path, source } => {
                write!(f, "{}: {source}", path.display())
            }
            Self::Resolution(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for PackageLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Source(error) => Some(error.as_ref()),
            Self::MissingDependencyPath { source, .. } => Some(source),
            Self::InvalidLockfile { source, .. } | Self::Resolution(source) => Some(source),
            _ => None,
        }
    }
}

/// Complete disk package workspace with recursively discovered local dependencies.
#[derive(Clone, Debug)]
pub struct PackageWorkspace {
    pub root: PackageSources,
    /// Transitively discovered local path dependencies in deterministic topological order.
    pub dependencies: Vec<PackageSources>,
    /// Parsed and validated existing `package.lock` if present in `root.root`.
    pub existing_lockfile: Option<Lockfile>,
}

/// Load a root disk package, validate any existing `package.lock`, and recursively
/// discover and validate all transitive local path dependencies.
pub fn load_package_workspace(root: &Path) -> Result<PackageWorkspace, PackageLoadError> {
    load_package_workspace_with_limits(root, SourceLoadLimits::default())
}

/// Bounded variant of [`load_package_workspace`].
pub fn load_package_workspace_with_limits(
    root: &Path,
    limits: SourceLoadLimits,
) -> Result<PackageWorkspace, PackageLoadError> {
    let mut root_sources = load_package_sources_with_limits(root, limits)
        .map_err(|error| PackageLoadError::Source(Box::new(error)))?;

    let lock_path = root_sources.root.join("package.lock");
    let existing_lockfile = match fs::symlink_metadata(&lock_path) {
        Ok(_) => Some(Lockfile::read(&lock_path)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(PackageLoadError::InvalidLockfile {
                path: lock_path,
                source: PackageError::Lock(format!("cannot inspect lockfile: {error}")),
            });
        }
    };

    let mut state = DiscoveryState {
        limits,
        loaded_by_path: BTreeMap::new(),
        path_by_qualified_name: BTreeMap::from([(
            root_sources.manifest.manifest().qualified_name(),
            root_sources.root.clone(),
        )]),
        active_stack: vec![(
            root_sources.root.clone(),
            root_sources.manifest.manifest().qualified_name(),
        )],
    };

    discover_from(&root_sources, &mut state)?;

    // Bind resolved `(domain, version)` onto root and every discovered dependency manifest.
    bind_manifest_paths(&mut root_sources, &state.loaded_by_path)?;
    let snapshot: BTreeMap<PathBuf, (String, Version)> = state
        .loaded_by_path
        .iter()
        .map(|(path, pkg)| {
            let m = pkg.manifest.manifest();
            (path.clone(), (m.domain.clone(), m.version.clone()))
        })
        .collect();
    for pkg in state.loaded_by_path.values_mut() {
        let pkg_root = pkg.root.clone();
        let path_deps = pkg.manifest.path_dependencies().to_vec();
        for dep in path_deps {
            let canonical = fs::canonicalize(pkg_root.join(&dep.path)).map_err(|source| {
                PackageLoadError::MissingDependencyPath {
                    package: pkg.manifest.manifest().qualified_name().into_boxed_str(),
                    dependency: dep.key.clone().into_boxed_str(),
                    declared_path: dep.path.clone().into_boxed_str(),
                    resolved_path: pkg_root.join(&dep.path),
                    source,
                }
            })?;
            let (domain, version) = &snapshot[&canonical];
            pkg.manifest
                .bind_path_dependency(&dep.key, domain, version)
                .map_err(PackageLoadError::Resolution)?;
        }
    }

    // Validate full dependency DAG via PackageResolver and produce topological order.
    let root_qname = root_sources.manifest.manifest().qualified_name();
    let mut resolver = PackageResolver::new();
    let mut by_qualified: BTreeMap<String, PackageSources> = BTreeMap::new();
    for pkg in state.loaded_by_path.into_values() {
        resolver
            .register_document(pkg.manifest.clone())
            .map_err(PackageLoadError::Resolution)?;
        by_qualified.insert(pkg.manifest.manifest().qualified_name(), pkg);
    }
    for bundled in crate::bundled_packages() {
        let qname = bundled.manifest.manifest().qualified_name();
        if qname != root_qname && !by_qualified.contains_key(&qname) {
            resolver
                .register_document(bundled.manifest.clone())
                .map_err(PackageLoadError::Resolution)?;
            by_qualified.insert(qname, bundled.clone());
        }
    }
    let graph = resolver
        .resolve_document(&root_sources.manifest)
        .map_err(PackageLoadError::Resolution)?;

    let mut dependencies = Vec::with_capacity(by_qualified.len());
    for manifest in graph.packages() {
        let qname = manifest.qualified_name();
        if qname == root_qname {
            continue;
        }
        if let Some(pkg) = by_qualified.remove(&qname) {
            dependencies.push(pkg);
        }
    }

    Ok(PackageWorkspace {
        root: root_sources,
        dependencies,
        existing_lockfile,
    })
}

struct DiscoveryState {
    limits: SourceLoadLimits,
    loaded_by_path: BTreeMap<PathBuf, PackageSources>,
    path_by_qualified_name: BTreeMap<String, PathBuf>,
    active_stack: Vec<(PathBuf, String)>,
}

fn discover_from(
    current: &PackageSources,
    state: &mut DiscoveryState,
) -> Result<(), PackageLoadError> {
    let current_qname = current.manifest.manifest().qualified_name();
    for path_dep in current.manifest.path_dependencies() {
        let candidate = current.root.join(&path_dep.path);
        let canonical = fs::canonicalize(&candidate).map_err(|source| {
            PackageLoadError::MissingDependencyPath {
                package: current_qname.clone().into_boxed_str(),
                dependency: path_dep.key.clone().into_boxed_str(),
                declared_path: path_dep.path.clone().into_boxed_str(),
                resolved_path: candidate.clone(),
                source,
            }
        })?;

        if let Some(pos) = state.active_stack.iter().position(|(p, _)| p == &canonical) {
            let mut chain: Vec<String> = state.active_stack[pos..]
                .iter()
                .map(|(_, name)| name.clone())
                .collect();
            let target_label = state
                .active_stack
                .get(pos)
                .map(|(_, name)| name.clone())
                .unwrap_or_else(|| path_dep.key.clone());
            chain.push(target_label);
            return Err(PackageLoadError::Cycle { chain });
        }

        if !state.loaded_by_path.contains_key(&canonical) {
            if state.loaded_by_path.len() >= MAX_WORKSPACE_PACKAGES {
                return Err(PackageLoadError::Resolution(PackageError::Capacity(
                    "workspace packages (65536)",
                )));
            }
            let dep_sources = load_package_sources_with_limits(&canonical, state.limits)
                .map_err(|error| PackageLoadError::Source(Box::new(error)))?;
            let dep_qname = dep_sources.manifest.manifest().qualified_name();

            if let Some(existing_path) = state.path_by_qualified_name.get(&dep_qname) {
                if existing_path != &canonical {
                    return Err(PackageLoadError::ConflictingDependencyPath {
                        dependency: dep_qname.into_boxed_str(),
                        first_path: existing_path.clone(),
                        second_path: canonical,
                    });
                }
            } else {
                state
                    .path_by_qualified_name
                    .insert(dep_qname.clone(), canonical.clone());
            }

            validate_edge(&current_qname, path_dep, &dep_sources, &canonical)?;

            state.active_stack.push((canonical.clone(), dep_qname));
            let recurse_result = discover_from(&dep_sources, state);
            state.active_stack.pop();
            recurse_result?;

            state.loaded_by_path.insert(canonical, dep_sources);
        } else {
            let dep_sources = &state.loaded_by_path[&canonical];
            validate_edge(&current_qname, path_dep, dep_sources, &canonical)?;
        }
    }
    Ok(())
}

fn validate_edge(
    current_qname: &str,
    path_dep: &crate::manifest::PathDependency,
    target: &PackageSources,
    canonical: &Path,
) -> Result<(), PackageLoadError> {
    let target_manifest = target.manifest.manifest();
    let domain_matches = path_dep
        .domain
        .as_deref()
        .is_none_or(|d| d == target_manifest.domain);
    if target_manifest.name != path_dep.name || !domain_matches {
        let (expected, found) = match &path_dep.domain {
            Some(domain) => (
                format!("{domain}/{}", path_dep.name),
                target_manifest.qualified_name(),
            ),
            None => (path_dep.name.clone(), target_manifest.name.clone()),
        };
        return Err(PackageLoadError::IdentityMismatch {
            package: current_qname.into(),
            dependency_key: path_dep.key.clone().into_boxed_str(),
            expected: expected.into_boxed_str(),
            found: found.into_boxed_str(),
            path: canonical.to_owned(),
        });
    }
    if let Some(constraint) = &path_dep.constraint
        && !constraint.matches(&target_manifest.version)
    {
        return Err(PackageLoadError::VersionMismatch {
            package: current_qname.into(),
            dependency: path_dep.key.clone().into_boxed_str(),
            constraint: Box::new(constraint.clone()),
            found: Box::new(target_manifest.version.clone()),
            path: canonical.to_owned(),
        });
    }
    Ok(())
}

fn bind_manifest_paths(
    pkg: &mut PackageSources,
    loaded_by_path: &BTreeMap<PathBuf, PackageSources>,
) -> Result<(), PackageLoadError> {
    let pkg_root = pkg.root.clone();
    let path_deps = pkg.manifest.path_dependencies().to_vec();
    for dep in path_deps {
        let canonical = fs::canonicalize(pkg_root.join(&dep.path)).map_err(|source| {
            PackageLoadError::MissingDependencyPath {
                package: pkg.manifest.manifest().qualified_name().into_boxed_str(),
                dependency: dep.key.clone().into_boxed_str(),
                declared_path: dep.path.clone().into_boxed_str(),
                resolved_path: pkg_root.join(&dep.path),
                source,
            }
        })?;
        let target = &loaded_by_path[&canonical];
        let m = target.manifest.manifest();
        pkg.manifest
            .bind_path_dependency(&dep.key, &m.domain, &m.version)
            .map_err(PackageLoadError::Resolution)?;
    }
    Ok(())
}
