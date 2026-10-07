use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::{ManifestDocument, PackageError, PackageIdentity, PackageManifest, VersionConstraint};

/// Legacy dependency-order projection, including the root package.
#[derive(Debug)]
pub struct ResolvedPackages {
    pub packages: Vec<PackageManifest>,
}

/// An immutable, validated DAG with exact version selections and Merkle identities.
#[derive(Debug, Clone)]
pub struct ResolvedPackageGraph {
    pub(crate) root: String,
    pub(crate) documents: BTreeMap<String, ManifestDocument>,
    pub(crate) identities: BTreeMap<String, PackageIdentity>,
    packages: Vec<PackageManifest>,
}

impl ResolvedPackageGraph {
    pub fn root_identity(&self) -> PackageIdentity {
        self.identities[&self.root]
    }
    pub fn packages(&self) -> &[PackageManifest] {
        &self.packages
    }
    pub fn identity(&self, name: &str) -> Option<PackageIdentity> {
        self.identities.get(name).copied()
    }
    pub fn to_lock(&self) -> String {
        crate::lock::write(self)
    }
}

/// In-memory registry. Duplicate exact versions must describe identical content.
#[derive(Default)]
pub struct PackageResolver {
    pub(crate) registry: BTreeMap<String, BTreeMap<String, Arc<ManifestDocument>>>,
    legacy_error: Option<PackageError>,
    registry_bytes: usize,
    registry_versions: usize,
}

impl PackageResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Legacy registration cannot return errors; invalid registry input is
    /// retained as an error and reported by subsequent resolution.
    pub fn register(&mut self, manifest: PackageManifest) {
        if let Err(error) = ManifestDocument::from_manifest(manifest)
            .and_then(|document| self.register_document(document))
        {
            self.legacy_error = Some(error);
        }
    }

    pub fn register_document(&mut self, document: ManifestDocument) -> Result<(), PackageError> {
        let name = document.manifest.qualified_name();
        let version = document.manifest.version.to_string();
        if let Some(previous) = self
            .registry
            .get(&name)
            .and_then(|versions| versions.get(&version))
        {
            if previous.local_identity() != document.local_identity() {
                return Err(PackageError::Registry(format!(
                    "inconsistent duplicate {name}@{version}"
                )));
            }
            return Ok(());
        }
        if self.registry_versions >= 65_536 {
            return Err(PackageError::Capacity("registered versions (65536)"));
        }
        if self.registry_bytes + document.source_bytes > 128 * 1024 * 1024 {
            return Err(PackageError::Capacity("registry manifest bytes (128 MiB)"));
        }
        self.registry_versions += 1;
        self.registry_bytes += document.source_bytes;
        self.registry
            .entry(name)
            .or_default()
            .insert(version, Arc::new(document));
        Ok(())
    }

    pub fn resolve(&self, root: &PackageManifest) -> Result<ResolvedPackages, String> {
        let root =
            ManifestDocument::from_manifest(root.clone()).map_err(|error| error.to_string())?;
        self.resolve_document(&root)
            .map(|graph| ResolvedPackages {
                packages: graph.packages,
            })
            .map_err(|error| error.to_string())
    }

    pub fn resolve_document(
        &self,
        root: &ManifestDocument,
    ) -> Result<ResolvedPackageGraph, PackageError> {
        self.check_root(root)?;
        let root_name = root.manifest.qualified_name();
        let mut pending = vec![BTreeMap::from([(
            root_name.clone(),
            Arc::new(root.clone()),
        )])];
        let mut failure = None;
        let mut pending_entries = 1usize;
        let mut steps = 0usize;
        // Explicit search states avoid Rust call-stack growth on deep package
        // chains. Earlier qualified names get first choice of highest versions.
        while let Some(selected) = pending.pop() {
            pending_entries -= selected.len();
            steps += 1;
            if steps > 1_000_000 {
                return Err(PackageError::Capacity("dependency search states (1000000)"));
            }
            let requirements = match requirements(&root_name, &selected) {
                Ok(requirements) => requirements,
                Err(error) => {
                    failure = Some(error);
                    continue;
                }
            };
            if let Some((name, _)) = requirements.iter().find(|(name, constraints)| {
                selected.get(*name).is_some_and(|document| {
                    !constraints
                        .iter()
                        .all(|constraint| constraint.matches(&document.manifest.version))
                })
            }) {
                failure = Some(PackageError::Conflict(name.clone()));
                continue;
            }
            let unresolved = requirements
                .keys()
                .find(|name| !selected.contains_key(*name));
            let Some(name) = unresolved else {
                match build_graph(
                    root_name.clone(),
                    selected
                        .into_iter()
                        .map(|(name, document)| (name, document.as_ref().clone()))
                        .collect(),
                ) {
                    Ok(graph) => return Ok(graph),
                    Err(error) => {
                        failure = Some(error);
                        continue;
                    }
                }
            };
            let Some(versions) = self.registry.get(name) else {
                failure = Some(PackageError::MissingPackage(name.clone()));
                continue;
            };
            let mut candidates = versions
                .values()
                .filter(|document| {
                    requirements[name]
                        .iter()
                        .all(|constraint| constraint.matches(&document.manifest.version))
                })
                .collect::<Vec<_>>();
            // Build metadata breaks precedence ties deterministically. It does
            // not change whether a constraint accepts the candidate.
            candidates.sort_by(|left, right| {
                left.manifest
                    .version
                    .semantic()
                    .cmp_precedence(&right.manifest.version.semantic())
                    .then_with(|| {
                        left.manifest
                            .version
                            .to_string()
                            .cmp(&right.manifest.version.to_string())
                    })
            });
            if candidates.is_empty() {
                failure = Some(PackageError::Conflict(name.clone()));
            }
            for candidate in candidates {
                if selected.len() >= 65_536 {
                    return Err(PackageError::Capacity("selected packages (65536)"));
                }
                let branch_entries = selected.len() + 1;
                if pending_entries + branch_entries > 1_000_000 {
                    return Err(PackageError::Capacity(
                        "pending dependency selections (1000000)",
                    ));
                }
                let mut branch = selected.clone();
                branch.insert(name.clone(), Arc::clone(candidate));
                pending_entries += branch_entries;
                pending.push(branch);
            }
        }
        Err(failure.unwrap_or(PackageError::Conflict(root_name)))
    }

    pub fn resolve_document_locked(
        &self,
        root: &ManifestDocument,
        source: &str,
    ) -> Result<ResolvedPackageGraph, PackageError> {
        self.check_root(root)?;
        crate::lock::resolve(self, root, source)
    }

    fn check_root(&self, root: &ManifestDocument) -> Result<(), PackageError> {
        if let Some(error) = &self.legacy_error {
            return Err(error.clone());
        }
        if let Some(previous) = self
            .registry
            .get(&root.manifest.qualified_name())
            .and_then(|versions| versions.get(&root.manifest.version.to_string()))
            && previous.local_identity() != root.local_identity()
        {
            return Err(PackageError::Registry(format!(
                "root conflicts with registered {}@{}",
                root.manifest.qualified_name(),
                root.manifest.version
            )));
        }
        Ok(())
    }
}

fn requirements(
    root: &str,
    selected: &BTreeMap<String, Arc<ManifestDocument>>,
) -> Result<BTreeMap<String, Vec<VersionConstraint>>, PackageError> {
    let mut requirements = BTreeMap::<String, Vec<VersionConstraint>>::new();
    let mut pending = vec![root.to_owned()];
    let mut visited = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if !visited.insert(name.clone()) {
            continue;
        }
        let Some(document) = selected.get(&name) else {
            continue;
        };
        let mut names = BTreeSet::new();
        for dependency in &document.manifest.dependencies {
            let name = format!("{}/{}", dependency.domain, dependency.name);
            if !names.insert(name.clone()) {
                return Err(PackageError::Manifest(format!(
                    "duplicate dependency: {name}"
                )));
            }
            requirements
                .entry(name.clone())
                .or_default()
                .push(dependency.constraint.clone());
            pending.push(name);
        }
    }
    Ok(requirements)
}

pub(crate) fn dependency_names(document: &ManifestDocument) -> Vec<String> {
    let mut names = document
        .manifest
        .dependencies
        .iter()
        .map(|dependency| format!("{}/{}", dependency.domain, dependency.name))
        .collect::<Vec<_>>();
    names.sort();
    names
}

pub(crate) fn build_graph(
    root: String,
    documents: BTreeMap<String, ManifestDocument>,
) -> Result<ResolvedPackageGraph, PackageError> {
    // Iterative DFS checks cycles while producing a dependency-first order.
    let mut order = Vec::new();
    let mut complete = BTreeSet::new();
    let mut active = vec![(root.clone(), 0usize)];
    let mut positions = BTreeMap::from([(root.clone(), 0usize)]);
    while let Some((name, index)) = active.last_mut() {
        let document = documents
            .get(name)
            .ok_or_else(|| PackageError::MissingPackage(name.clone()))?;
        let children = dependency_names(document);
        if *index < children.len() {
            let child = children[*index].clone();
            *index += 1;
            if let Some(position) = positions.get(&child) {
                let mut path = active[*position..]
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>();
                path.push(child);
                return Err(PackageError::Cycle(path));
            }
            if !complete.contains(&child) {
                positions.insert(child.clone(), active.len());
                active.push((child, 0));
            }
        } else {
            let name = name.clone();
            complete.insert(name.clone());
            positions.remove(&name);
            order.push(name);
            active.pop();
        }
    }
    let mut identities = BTreeMap::new();
    let mut packages = Vec::new();
    for name in order {
        let document = &documents[&name];
        let children = dependency_names(document)
            .into_iter()
            .map(|child| {
                let identity = identities[&child];
                (child, identity)
            })
            .collect();
        identities.insert(name, crate::identity::identity(document, &children));
        packages.push(document.manifest.clone());
    }
    let documents = documents
        .into_iter()
        .filter(|(name, _)| complete.contains(name))
        .collect();
    Ok(ResolvedPackageGraph {
        root,
        documents,
        identities,
        packages,
    })
}
