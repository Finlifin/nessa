use std::fmt;

/// Invalid manifest, registry graph, or lock data. No partial graph is returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageError {
    Capacity(&'static str),
    Manifest(String),
    Registry(String),
    MissingPackage(String),
    Conflict(String),
    Cycle(Vec<String>),
    Lock(String),
}

impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capacity(resource) => write!(f, "package resource limit exceeded: {resource}"),
            Self::Manifest(message) => write!(f, "invalid package manifest: {message}"),
            Self::Registry(message) => write!(f, "invalid package registry: {message}"),
            Self::MissingPackage(name) => write!(f, "package not found: {name}"),
            Self::Conflict(name) => write!(f, "no jointly compatible version for {name}"),
            Self::Cycle(path) => write!(f, "package dependency cycle: {}", path.join(" -> ")),
            Self::Lock(message) => write!(f, "invalid package lock: {message}"),
        }
    }
}

impl std::error::Error for PackageError {}
