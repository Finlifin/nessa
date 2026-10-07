use std::fmt;
use std::path::PathBuf;

use crate::PackageError;

/// Failed source discovery/loading. No partial module tree or source set is returned.
#[derive(Debug)]
pub enum PackageSourceError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    InvalidManifest {
        path: PathBuf,
        source: PackageError,
    },
    InvalidModuleName {
        path: PathBuf,
    },
    DuplicateModule {
        module_path: Vec<String>,
        first: PathBuf,
        second: PathBuf,
    },
    MissingEntry {
        path: PathBuf,
    },
    InvalidEntry {
        path: PathBuf,
    },
    SymlinkCycle {
        path: PathBuf,
        target: PathBuf,
    },
    Capacity {
        resource: &'static str,
        limit: usize,
    },
    InvalidLimits {
        field: &'static str,
    },
}

impl fmt::Display for PackageSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(f, "cannot {operation} {}: {source}", path.display()),
            Self::InvalidManifest { path, source } => {
                write!(f, "{}: {source}", path.display())
            }
            Self::InvalidModuleName { path } => {
                write!(f, "invalid module name: {}", path.display())
            }
            Self::DuplicateModule {
                module_path,
                first,
                second,
            } => write!(
                f,
                "duplicate module {}: {} and {}",
                module_path.join("."),
                first.display(),
                second.display()
            ),
            Self::MissingEntry { path } => write!(f, "missing source entry: {}", path.display()),
            Self::InvalidEntry { path } => {
                write!(f, "source entry is not a regular file: {}", path.display())
            }
            Self::SymlinkCycle { path, target } => write!(
                f,
                "module directory cycle: {} resolves to ancestor {}",
                path.display(),
                target.display()
            ),
            Self::Capacity { resource, limit } => {
                write!(f, "package source limit exceeded: {resource} ({limit})")
            }
            Self::InvalidLimits { field } => write!(f, "invalid package source limit: {field}"),
        }
    }
}

impl std::error::Error for PackageSourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidManifest { source, .. } => Some(source),
            _ => None,
        }
    }
}

pub(super) fn io_error(
    operation: &'static str,
    path: &std::path::Path,
    source: std::io::Error,
) -> PackageSourceError {
    PackageSourceError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}
