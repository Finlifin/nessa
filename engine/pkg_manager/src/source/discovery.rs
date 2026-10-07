use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use lexer::TokenKind;

use crate::{DiscoveredModule, PackageType};

use super::error::io_error;
use super::{PackageSourceError, SourceLoadLimits};

/// Discover a package's source modules, returning errors instead of a partial tree.
pub fn discover_modules(
    src_dir: &Path,
    package_type: PackageType,
) -> Result<Vec<DiscoveredModule>, PackageSourceError> {
    discover_modules_with_limits(src_dir, package_type, SourceLoadLimits::default())
}

/// Follow logical directory aliases, rejecting canonical ancestor cycles.
/// Only the selected root entry and each directory's own `mod.ns` are excluded.
pub fn discover_modules_with_limits(
    src_dir: &Path,
    package_type: PackageType,
    limits: SourceLoadLimits,
) -> Result<Vec<DiscoveredModule>, PackageSourceError> {
    limits.validate()?;
    let entry = Path::new(package_type.entry_file())
        .file_name()
        .ok_or_else(|| PackageSourceError::InvalidEntry {
            path: src_dir.to_owned(),
        })?;
    let canonical = fs::canonicalize(src_dir)
        .map_err(|error| io_error("resolve source directory", src_dir, error))?;
    let mut ancestors = HashSet::from([canonical]);
    let mut discovery = Discovery {
        limits,
        modules: 1,
        entries: 0,
    };
    discovery.walk(src_dir, &[], entry, &mut ancestors)
}

pub(super) fn require_entry(path: &Path) -> Result<(), PackageSourceError> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(PackageSourceError::InvalidEntry {
            path: path.to_owned(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(PackageSourceError::MissingEntry {
                path: path.to_owned(),
            })
        }
        Err(error) => Err(io_error("inspect source entry", path, error)),
    }
}

struct Candidate {
    name: String,
    entry: PathBuf,
    directory: Option<PathBuf>,
}

struct Discovery {
    limits: SourceLoadLimits,
    modules: usize,
    entries: usize,
}

impl Discovery {
    fn walk(
        &mut self,
        directory: &Path,
        prefix: &[String],
        entry_name: &OsStr,
        ancestors: &mut HashSet<PathBuf>,
    ) -> Result<Vec<DiscoveredModule>, PackageSourceError> {
        require_entry(&directory.join(entry_name))?;
        let entries = fs::read_dir(directory)
            .map_err(|error| io_error("read source directory", directory, error))?;
        let mut ordered = Vec::new();
        for entry in entries {
            if self.entries >= self.limits.max_entries {
                return Err(PackageSourceError::Capacity {
                    resource: "entries",
                    limit: self.limits.max_entries,
                });
            }
            self.entries += 1;
            ordered
                .push(entry.map_err(|error| io_error("read directory entry", directory, error))?);
        }
        ordered.sort_by_key(|entry| entry.file_name());
        let mut candidates = BTreeMap::new();
        for entry in ordered {
            let file_name = entry.file_name();
            if file_name == entry_name {
                continue;
            }
            let path = entry.path();
            let metadata = fs::metadata(&path)
                .map_err(|error| io_error("inspect module path", &path, error))?;
            let (name, entry_path, directory_path) = if metadata.is_dir() {
                (file_name.to_str(), path.join("mod.ns"), Some(path.clone()))
            } else if file_name.as_encoded_bytes().ends_with(b".ns") {
                require_entry(&path)?;
                (
                    file_name.to_str().and_then(|name| name.strip_suffix(".ns")),
                    path.clone(),
                    None,
                )
            } else {
                continue;
            };
            let name = name
                .filter(|name| valid_module_name(name))
                .ok_or_else(|| PackageSourceError::InvalidModuleName { path: path.clone() })?;
            if prefix.len() >= self.limits.max_depth {
                return Err(PackageSourceError::Capacity {
                    resource: "depth",
                    limit: self.limits.max_depth,
                });
            }
            if self.modules >= self.limits.max_modules {
                return Err(PackageSourceError::Capacity {
                    resource: "modules",
                    limit: self.limits.max_modules,
                });
            }
            self.modules += 1;
            let candidate = Candidate {
                name: name.to_owned(),
                entry: entry_path,
                directory: directory_path,
            };
            if let Some(previous) = candidates.insert(name.to_owned(), candidate) {
                let mut module_path = prefix.to_vec();
                module_path.push(name.to_owned());
                return Err(PackageSourceError::DuplicateModule {
                    module_path,
                    first: previous.entry,
                    second: candidates[name].entry.clone(),
                });
            }
        }
        let mut modules = Vec::with_capacity(candidates.len());
        for candidate in candidates.into_values() {
            let mut path = prefix.to_vec();
            path.push(candidate.name);
            let children = if let Some(directory) = candidate.directory {
                let canonical = fs::canonicalize(&directory)
                    .map_err(|error| io_error("resolve module directory", &directory, error))?;
                if !ancestors.insert(canonical.clone()) {
                    return Err(PackageSourceError::SymlinkCycle {
                        path: directory,
                        target: canonical,
                    });
                }
                let result = self.walk(&directory, &path, OsStr::new("mod.ns"), ancestors);
                ancestors.remove(&canonical);
                result?
            } else {
                Vec::new()
            };
            modules.push(DiscoveredModule {
                path,
                file_path: candidate.entry,
                children,
            });
        }
        Ok(modules)
    }
}

fn valid_module_name(name: &str) -> bool {
    let (tokens, errors) = lexer::tokenize(name);
    errors.is_empty()
        && tokens.len() == 2
        && tokens[0].kind == TokenKind::Id
        && tokens[0].from == 0
        && tokens[0].to as usize == name.len()
        && tokens[1].kind == TokenKind::Eof
}
