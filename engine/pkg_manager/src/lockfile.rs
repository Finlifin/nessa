//! Deterministic `package.lock` schema (v1) for local package workspaces.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path};

use sha2::{Digest, Sha256};

use crate::manifest::validate_part;
use crate::workspace::{PackageLoadError, PackageWorkspace};
use crate::{ModuleSources, PackageError, PackageSources, Version};

pub const LOCKFILE_VERSION: u32 = 1;
const MAX_LOCKFILE_BYTES: usize = 16 * 1024 * 1024;
const CONTENT_HASH_DOMAIN: &[u8] = b"nessa.package.content.v1\0";

/// A single locked dependency record in `package.lock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedPackage {
    pub name: String,
    pub domain: Option<String>,
    pub version: Version,
    /// Normalized `/` relative path from the root package directory.
    pub path: String,
    /// Lowercase 64-character hex SHA-256 over `package.toml` + discovered `.ns` files.
    pub sha256: String,
}

/// Parsed and validated `package.lock` document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lockfile {
    pub version: u32,
    /// Sorted deterministically by `(name, domain)`.
    pub packages: Vec<LockedPackage>,
}

impl Lockfile {
    /// Construct a deterministic lockfile from a resolved [`PackageWorkspace`].
    pub fn from_workspace(workspace: &PackageWorkspace) -> Self {
        let mut packages: Vec<LockedPackage> = workspace
            .dependencies
            .iter()
            .filter(|dep| !crate::bundled::is_bundled_package_root(&dep.root))
            .map(|dep| {
                let manifest = dep.manifest.manifest();
                LockedPackage {
                    name: manifest.name.clone(),
                    domain: Some(manifest.domain.clone()),
                    version: manifest.version.clone(),
                    path: relative_normalized_path(&workspace.root.root, &dep.root),
                    sha256: compute_package_sha256(dep),
                }
            })
            .collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.domain.cmp(&b.domain)));
        Self {
            version: LOCKFILE_VERSION,
            packages,
        }
    }

    /// Parse and strictly validate a `package.lock` TOML string.
    pub fn parse(source: &str) -> Result<Self, PackageError> {
        if source.len() > MAX_LOCKFILE_BYTES {
            return Err(PackageError::Capacity("lockfile bytes (16 MiB)"));
        }
        let table: toml::Table = toml::from_str(source)
            .map_err(|error| PackageError::Lock(format!("malformed lockfile TOML: {error}")))?;
        ensure_allowed_keys(&table, &["version", "package"])?;

        let version_int = table
            .get("version")
            .and_then(toml::Value::as_integer)
            .ok_or_else(|| PackageError::Lock("missing integer `version` field".into()))?;
        if version_int != i64::from(LOCKFILE_VERSION) {
            return Err(PackageError::Lock(format!(
                "unsupported lockfile version {version_int}; expected {LOCKFILE_VERSION}"
            )));
        }

        let mut packages = Vec::new();
        if let Some(pkg_val) = table.get("package") {
            let arr = pkg_val
                .as_array()
                .ok_or_else(|| PackageError::Lock("`package` must be an array of tables".into()))?;
            if arr.len() > 65_536 {
                return Err(PackageError::Capacity("lockfile packages (65536)"));
            }
            let mut seen_packages = BTreeSet::new();
            for entry in arr {
                let pkg = entry
                    .as_table()
                    .ok_or_else(|| PackageError::Lock("`package` entry must be a table".into()))?;
                ensure_allowed_keys(pkg, &["name", "domain", "version", "path", "sha256"])?;

                let name = required_str(pkg, "name")?.to_owned();
                validate_part(&name).map_err(|e| PackageError::Lock(e.to_string()))?;

                let domain = match pkg.get("domain") {
                    Some(val) => {
                        let d = val.as_str().filter(|s| !s.is_empty()).ok_or_else(|| {
                            PackageError::Lock("invalid string field `domain`".into())
                        })?;
                        validate_part(d).map_err(|e| PackageError::Lock(e.to_string()))?;
                        Some(d.to_owned())
                    }
                    None => None,
                };

                if !seen_packages.insert((name.clone(), domain.clone())) {
                    let label = match &domain {
                        Some(d) => format!("{d}/{name}"),
                        None => name.clone(),
                    };
                    return Err(PackageError::Lock(format!(
                        "duplicate package entry `{label}` in lockfile"
                    )));
                }

                let version_str = required_str(pkg, "version")?;
                let version = Version::parse(version_str).ok_or_else(|| {
                    PackageError::Lock(format!("invalid semantic version `{version_str}`"))
                })?;

                let path = required_str(pkg, "path")?.to_owned();
                if path.contains('\0') || path.contains('\\') {
                    return Err(PackageError::Lock(format!(
                        "non-normalized or invalid path `{path}` in lockfile"
                    )));
                }

                let sha256 = required_str(pkg, "sha256")?.to_owned();
                if !is_valid_sha256_hex(&sha256) {
                    return Err(PackageError::Lock(format!(
                        "invalid sha256 digest `{sha256}` for package `{name}`"
                    )));
                }

                packages.push(LockedPackage {
                    name,
                    domain,
                    version,
                    path,
                    sha256,
                });
            }

            for pair in packages.windows(2) {
                let left = (&pair[0].name, &pair[0].domain);
                let right = (&pair[1].name, &pair[1].domain);
                if left > right {
                    return Err(PackageError::Lock(
                        "lockfile `[[package]]` entries are not deterministically sorted".into(),
                    ));
                }
            }
        }

        Ok(Self {
            version: LOCKFILE_VERSION,
            packages,
        })
    }

    /// Read and validate a `package.lock` file from disk with bounded allocation.
    pub fn read(path: &Path) -> Result<Self, PackageLoadError> {
        let metadata = fs::metadata(path).map_err(|error| PackageLoadError::InvalidLockfile {
            path: path.to_owned(),
            source: PackageError::Lock(format!("cannot inspect lockfile: {error}")),
        })?;
        if !metadata.is_file() {
            return Err(PackageLoadError::InvalidLockfile {
                path: path.to_owned(),
                source: PackageError::Lock("lockfile path is not a regular file".into()),
            });
        }
        if metadata.len() > MAX_LOCKFILE_BYTES as u64 {
            return Err(PackageLoadError::InvalidLockfile {
                path: path.to_owned(),
                source: PackageError::Capacity("lockfile bytes (16 MiB)"),
            });
        }
        let file = File::open(path).map_err(|error| PackageLoadError::InvalidLockfile {
            path: path.to_owned(),
            source: PackageError::Lock(format!("cannot open lockfile: {error}")),
        })?;
        let mut bytes = Vec::new();
        file.take((MAX_LOCKFILE_BYTES as u64) + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| PackageLoadError::InvalidLockfile {
                path: path.to_owned(),
                source: PackageError::Lock(format!("cannot read lockfile: {error}")),
            })?;
        if bytes.len() > MAX_LOCKFILE_BYTES {
            return Err(PackageLoadError::InvalidLockfile {
                path: path.to_owned(),
                source: PackageError::Capacity("lockfile bytes (16 MiB)"),
            });
        }
        let text = String::from_utf8(bytes).map_err(|error| PackageLoadError::InvalidLockfile {
            path: path.to_owned(),
            source: PackageError::Lock(format!("lockfile is not valid UTF-8: {error}")),
        })?;
        Self::parse(&text).map_err(|source| PackageLoadError::InvalidLockfile {
            path: path.to_owned(),
            source,
        })
    }

    /// Render the lockfile in canonical, deterministic TOML format.
    pub fn to_toml_string(&self) -> String {
        let mut out = format!("version = {}\n", self.version);
        for pkg in &self.packages {
            let _ = write!(out, "\n[[package]]\nname = {}\n", toml_quote(&pkg.name));
            if let Some(domain) = &pkg.domain {
                let _ = writeln!(out, "domain = {}", toml_quote(domain));
            }
            let _ = writeln!(out, "version = {}", toml_quote(&pkg.version.to_string()));
            let _ = writeln!(out, "path = {}", toml_quote(&pkg.path));
            let _ = writeln!(out, "sha256 = {}", toml_quote(&pkg.sha256));
        }
        out
    }

    /// Write this lockfile to disk.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        fs::write(path, self.to_toml_string())
    }

    /// Strictly verify that this lockfile matches every resolved dependency's
    /// name, version, normalized path, and SHA-256 content digest on disk.
    pub fn verify(&self, workspace: &PackageWorkspace) -> Result<(), PackageError> {
        if self.version != LOCKFILE_VERSION {
            return Err(PackageError::Lock(format!(
                "unsupported lockfile version {}; expected {LOCKFILE_VERSION}",
                self.version
            )));
        }
        let expected = Self::from_workspace(workspace);
        if self.packages.len() != expected.packages.len() {
            return Err(PackageError::Lock(format!(
                "lockfile package count mismatch: locked {}, workspace has {}",
                self.packages.len(),
                expected.packages.len()
            )));
        }
        for (actual, exp) in self.packages.iter().zip(&expected.packages) {
            if actual.name != exp.name {
                return Err(PackageError::Lock(format!(
                    "lockfile package mismatch: expected `{}`, found `{}`",
                    exp.name, actual.name
                )));
            }
            let same_name_count = expected
                .packages
                .iter()
                .filter(|p| p.name == exp.name)
                .count();
            if same_name_count > 1 && actual.domain.is_none() {
                return Err(PackageError::Lock(format!(
                    "lockfile entry `{}` must specify `domain` when multiple domains share the same package name",
                    actual.name
                )));
            }
            if let Some(actual_domain) = &actual.domain
                && Some(actual_domain) != exp.domain.as_ref()
            {
                return Err(PackageError::Lock(format!(
                    "lockfile domain mismatch for `{}`: locked `{actual_domain}`, found `{}`",
                    actual.name,
                    exp.domain.as_deref().unwrap_or("")
                )));
            }
            let label = match &exp.domain {
                Some(d) => format!("{d}/{}", exp.name),
                None => exp.name.clone(),
            };
            if actual.version != exp.version {
                return Err(PackageError::Lock(format!(
                    "lockfile version mismatch for `{label}`: locked {}, found {}",
                    actual.version, exp.version
                )));
            }
            if actual.path != exp.path {
                return Err(PackageError::Lock(format!(
                    "lockfile path mismatch for `{label}`: locked `{}`, found `{}`",
                    actual.path, exp.path
                )));
            }
            if actual.sha256 != exp.sha256 {
                return Err(PackageError::Lock(format!(
                    "lockfile sha256 mismatch for `{label}`: locked {}, computed {}",
                    actual.sha256, exp.sha256
                )));
            }
        }
        Ok(())
    }
}

/// Compute a deterministic SHA-256 content digest over `package.toml` and all
/// discovered `.ns` source files in `sources`.
pub fn compute_package_sha256(sources: &PackageSources) -> String {
    let mut hasher = Sha256::new();
    hasher.update(CONTENT_HASH_DOMAIN);

    // 1. package.toml digest + byte length
    hasher.update(b"package.toml\0");
    hasher.update((sources.manifest.source_bytes as u64).to_be_bytes());
    hasher.update(sources.manifest.source_sha256());

    // 2. Entry file + all discovered module `.ns` files in canonical sorted order
    let mut files: Vec<(String, &str)> = Vec::new();
    files.push((
        sources.manifest.manifest().entry_file().to_owned(),
        sources.entry.source.as_str(),
    ));
    collect_module_sources(&sources.root, &sources.modules, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));

    hasher.update((files.len() as u64).to_be_bytes());
    for (rel_path, content) in files {
        hasher.update((rel_path.len() as u64).to_be_bytes());
        hasher.update(rel_path.as_bytes());
        hasher.update((content.len() as u64).to_be_bytes());
        hasher.update(content.as_bytes());
    }

    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn collect_module_sources<'a>(
    pkg_root: &Path,
    modules: &'a [ModuleSources],
    out: &mut Vec<(String, &'a str)>,
) {
    for module in modules {
        let key = module
            .entry
            .file_path
            .strip_prefix(pkg_root)
            .ok()
            .map(normalize_separators)
            .unwrap_or_else(|| format!("module:{}", module.path.join("/")));
        out.push((key, module.entry.source.as_str()));
        collect_module_sources(pkg_root, &module.children, out);
    }
}

/// Compute a normalized POSIX-style (`/`-separated) lexical relative path from `base` to `target`.
pub fn relative_normalized_path(base: &Path, target: &Path) -> String {
    let base_components = lexical_normalize(base);
    let target_components = lexical_normalize(target);

    let common = base_components
        .iter()
        .zip(&target_components)
        .take_while(|(a, b)| a == b)
        .count();

    if common == 0 {
        return normalize_separators(target);
    }

    let mut parts: Vec<String> = Vec::new();
    for comp in &base_components[common..] {
        if matches!(comp, Component::Normal(_)) {
            parts.push("..".to_owned());
        }
    }
    for comp in &target_components[common..] {
        if let Component::Normal(seg) = comp {
            parts.push(seg.to_string_lossy().into_owned());
        }
    }

    if parts.is_empty() {
        ".".to_owned()
    } else {
        parts.join("/")
    }
}

fn lexical_normalize(path: &Path) -> Vec<Component<'_>> {
    let mut out = Vec::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.last(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push(comp);
                }
            }
            _ => out.push(comp),
        }
    }
    out
}

fn normalize_separators(path: &Path) -> String {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            Component::ParentDir => Some("..".to_owned()),
            Component::CurDir => None,
            Component::RootDir => Some(String::new()),
            Component::Prefix(p) => Some(p.as_os_str().to_string_lossy().into_owned()),
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn toml_quote(value: &str) -> String {
    toml::Value::String(value.to_owned()).to_string()
}

fn ensure_allowed_keys(table: &toml::Table, allowed: &[&str]) -> Result<(), PackageError> {
    if let Some(unknown) = table.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(PackageError::Lock(format!(
            "unknown lockfile field `{unknown}`"
        )));
    }
    Ok(())
}

fn required_str<'a>(table: &'a toml::Table, key: &str) -> Result<&'a str, PackageError> {
    table
        .get(key)
        .and_then(toml::Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| PackageError::Lock(format!("missing or invalid string field `{key}`")))
}

fn is_valid_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
