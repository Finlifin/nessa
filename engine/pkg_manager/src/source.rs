//! Checked filesystem inputs for package compilation. No source is compiled here.

mod discovery;
mod error;
mod loading;

pub use discovery::{discover_modules, discover_modules_with_limits};
pub use error::PackageSourceError;
pub use loading::{load_package_sources, load_package_sources_with_limits};

use std::path::PathBuf;

use crate::ManifestDocument;

const MAX_DEPTH: usize = 256;

/// Bounds on logical module traversal and owned source contents.
/// Depth counts module path segments; the package root counts as one module.
/// Entries count every directory entry encountered, including ignored files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceLoadLimits {
    pub max_depth: usize,
    pub max_modules: usize,
    pub max_entries: usize,
    pub max_source_bytes: usize,
    pub max_file_bytes: usize,
    pub max_manifest_bytes: usize,
}

impl Default for SourceLoadLimits {
    fn default() -> Self {
        Self {
            max_depth: MAX_DEPTH,
            max_modules: 65536,
            max_entries: 262144,
            max_source_bytes: 64 * 1024 * 1024,
            max_file_bytes: 16 * 1024 * 1024,
            max_manifest_bytes: 1024 * 1024,
        }
    }
}

impl SourceLoadLimits {
    fn validate(self) -> Result<(), PackageSourceError> {
        for (field, value) in [
            ("max_depth", self.max_depth),
            ("max_modules", self.max_modules),
            ("max_entries", self.max_entries),
            ("max_source_bytes", self.max_source_bytes),
            ("max_file_bytes", self.max_file_bytes),
            ("max_manifest_bytes", self.max_manifest_bytes),
        ] {
            if value == 0 || (field == "max_depth" && value > MAX_DEPTH) {
                return Err(PackageSourceError::InvalidLimits { field });
            }
        }
        Ok(())
    }
}

/// Owned UTF-8 contents and the physical path used for diagnostics.
#[derive(Clone, Debug)]
pub struct SourceFile {
    pub file_path: PathBuf,
    pub source: String,
}

/// One logical module, whose entry may be a file or a directory's `mod.ns`.
#[derive(Clone, Debug)]
pub struct ModuleSources {
    pub path: Vec<String>,
    pub entry: SourceFile,
    pub children: Vec<ModuleSources>,
}

/// Complete filesystem inputs for one package. Dependency selection and compilation
/// consume this data separately; paths are not package or type identity inputs.
#[derive(Clone, Debug)]
pub struct PackageSources {
    pub root: PathBuf,
    pub manifest: ManifestDocument,
    pub entry: SourceFile,
    pub modules: Vec<ModuleSources>,
}
