use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

use crate::{DiscoveredModule, ManifestDocument};

use super::discovery::discover_modules_with_limits;
use super::error::io_error;
use super::{ModuleSources, PackageSourceError, PackageSources, SourceFile, SourceLoadLimits};

/// Read the complete package manifest and source tree into owned UTF-8 inputs.
pub fn load_package_sources(root: &Path) -> Result<PackageSources, PackageSourceError> {
    load_package_sources_with_limits(root, SourceLoadLimits::default())
}

/// Bound reads and traversal before returning any successful package source set.
pub fn load_package_sources_with_limits(
    root: &Path,
    limits: SourceLoadLimits,
) -> Result<PackageSources, PackageSourceError> {
    limits.validate()?;
    let root = fs::canonicalize(root)
        .map_err(|error| io_error("resolve package directory", root, error))?;
    let manifest_path = root.join("package.toml");
    let text = read_bounded(
        &manifest_path,
        limits.max_manifest_bytes,
        "manifest_bytes",
        limits.max_manifest_bytes,
    )?;
    let manifest =
        ManifestDocument::parse(&text).map_err(|source| PackageSourceError::InvalidManifest {
            path: manifest_path,
            source,
        })?;
    let discovered =
        discover_modules_with_limits(&root.join("src"), manifest.manifest().package_type, limits)?;
    let mut used_bytes = 0;
    let entry = read_source(
        &root.join(manifest.manifest().entry_file()),
        limits,
        &mut used_bytes,
    )?;
    let modules = discovered
        .into_iter()
        .map(|module| read_module(module, limits, &mut used_bytes))
        .collect::<Result<_, _>>()?;
    Ok(PackageSources {
        root,
        manifest,
        entry,
        modules,
    })
}

fn read_module(
    module: DiscoveredModule,
    limits: SourceLoadLimits,
    used_bytes: &mut usize,
) -> Result<ModuleSources, PackageSourceError> {
    let entry = read_source(&module.file_path, limits, used_bytes)?;
    let children = module
        .children
        .into_iter()
        .map(|child| read_module(child, limits, used_bytes))
        .collect::<Result<_, _>>()?;
    Ok(ModuleSources {
        path: module.path,
        entry,
        children,
    })
}

fn read_source(
    path: &Path,
    limits: SourceLoadLimits,
    used_bytes: &mut usize,
) -> Result<SourceFile, PackageSourceError> {
    let remaining = limits.max_source_bytes - *used_bytes;
    let (bound, resource, limit) = if remaining < limits.max_file_bytes {
        (remaining, "source_bytes", limits.max_source_bytes)
    } else {
        (limits.max_file_bytes, "file_bytes", limits.max_file_bytes)
    };
    let source = read_bounded(path, bound, resource, limit)?;
    // read_bounded guarantees len <= remaining, including files that grow while read.
    *used_bytes += source.len();
    Ok(SourceFile {
        file_path: path.to_owned(),
        source,
    })
}

fn read_bounded(
    path: &Path,
    bound: usize,
    resource: &'static str,
    limit: usize,
) -> Result<String, PackageSourceError> {
    // Reject special files before open: opening a FIFO could wait indefinitely.
    let metadata =
        fs::metadata(path).map_err(|error| io_error("inspect source file", path, error))?;
    if !metadata.is_file() {
        return Err(PackageSourceError::InvalidEntry {
            path: path.to_owned(),
        });
    }
    let file = File::open(path).map_err(|error| io_error("open source file", path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| io_error("inspect opened source file", path, error))?;
    if !metadata.is_file() {
        return Err(PackageSourceError::InvalidEntry {
            path: path.to_owned(),
        });
    }
    if metadata.len() > bound as u64 {
        return Err(PackageSourceError::Capacity { resource, limit });
    }
    let mut bytes = Vec::new();
    file.take((bound as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("read source file", path, error))?;
    if bytes.len() > bound {
        return Err(PackageSourceError::Capacity { resource, limit });
    }
    String::from_utf8(bytes).map_err(|error| {
        io_error(
            "decode source file",
            path,
            io::Error::new(io::ErrorKind::InvalidData, error),
        )
    })
}
