//! Content-addressed `.nsbc` compilation cache for single-file scripts and package graphs.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use pkg_manager::{PackageSources, PackageType, compute_package_sha256};
use sha2::{Digest, Sha256};
use type_pool::IdentityPathSegment;

use crate::CompilationIdentityContext;
use crate::standard_library::std_library_sha256;

const CACHE_DOMAIN: &[u8] = b"nessa.driver.nsbc_cache.v1\0";
static NEXT_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Resolve the active `.nsbc` cache directory from an optional `Driver` override
/// or standard process environment variables (`NESSA_NO_CACHE`, `NESSA_CACHE_DIR`,
/// `XDG_CACHE_HOME`, `HOME`).
pub(crate) fn active_cache_dir(override_dir: Option<&Option<PathBuf>>) -> Option<PathBuf> {
    if let Some(explicit) = override_dir {
        return explicit.clone();
    }
    resolve_cache_dir_from_vars(
        std::env::var_os("NESSA_NO_CACHE").as_deref(),
        std::env::var_os("NESSA_CACHE_DIR").as_deref(),
        std::env::var_os("XDG_CACHE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

pub(crate) fn resolve_cache_dir_from_vars(
    no_cache: Option<&OsStr>,
    cache_dir: Option<&OsStr>,
    xdg_cache_home: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Option<PathBuf> {
    if no_cache.is_some_and(is_truthy_disable) {
        return None;
    }
    if let Some(dir) = cache_dir.filter(|value| !value.is_empty()) {
        if is_disabled_cache_dir(dir) {
            return None;
        }
        return Some(PathBuf::from(dir));
    }
    if let Some(xdg) = xdg_cache_home.filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(xdg).join("nessa").join("nsbc"));
    }
    if let Some(home_dir) = home.filter(|value| !value.is_empty()) {
        return Some(
            PathBuf::from(home_dir)
                .join(".cache")
                .join("nessa")
                .join("nsbc"),
        );
    }
    Some(std::env::temp_dir().join("nessa-nsbc-cache"))
}

fn is_truthy_disable(val: &OsStr) -> bool {
    let text = val.to_string_lossy();
    matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn is_disabled_cache_dir(val: &OsStr) -> bool {
    let text = val.to_string_lossy();
    matches!(
        text.trim().to_ascii_lowercase().as_str(),
        "0" | "off" | "none" | "false" | "disable" | "disabled"
    )
}

fn new_seeded_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(CACHE_DOMAIN);
    hasher.update(nsbc::VERSION.to_be_bytes());
    hasher.update(runtime::BUILTIN_ABI_VERSION.to_be_bytes());
    hasher.update(std_library_sha256());
    hasher.update(pkg_manager::bundled_packages_sha256());
    hasher
}

fn feed_bytes(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn feed_str(hasher: &mut Sha256, text: &str) {
    feed_bytes(hasher, text.as_bytes());
}

fn finalize_hex(hasher: Sha256) -> String {
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

pub(crate) fn single_source_key(
    source: &str,
    identity: Option<&CompilationIdentityContext>,
) -> String {
    let mut hasher = new_seeded_hasher();
    hasher.update(b"single_source\0");
    feed_str(&mut hasher, source);
    match identity {
        None => hasher.update([0u8]),
        Some(ctx) => {
            hasher.update([1u8]);
            hasher.update(ctx.package.identity_schema.to_be_bytes());
            hasher.update(ctx.package.identity);
            feed_str(&mut hasher, &ctx.package.qualified_name);
            feed_str(&mut hasher, &ctx.package.version);
            match &ctx.last_stable_version {
                None => hasher.update([0u8]),
                Some(ver) => {
                    hasher.update([1u8]);
                    feed_str(&mut hasher, ver);
                }
            }
            hasher.update((ctx.type_versions.len() as u64).to_be_bytes());
            for (segments, ver) in &ctx.type_versions {
                hasher.update((segments.len() as u64).to_be_bytes());
                for seg in segments {
                    match seg {
                        IdentityPathSegment::Named(name) => {
                            hasher.update([0u8]);
                            feed_str(&mut hasher, name);
                        }
                        IdentityPathSegment::Lexical { kind, ordinal } => {
                            hasher.update([1u8, *kind]);
                            hasher.update(ordinal.to_be_bytes());
                        }
                    }
                }
                feed_str(&mut hasher, ver);
            }
        }
    }
    finalize_hex(hasher)
}

fn package_type_tag(package_type: PackageType) -> u8 {
    match package_type {
        PackageType::Exe => 1,
        PackageType::Lib => 2,
        PackageType::Tmp => 3,
    }
}

fn feed_package_sources(hasher: &mut Sha256, pkg: &PackageSources) {
    let manifest = pkg.manifest.manifest();
    feed_str(hasher, &manifest.qualified_name());
    feed_str(hasher, &manifest.version.to_string());
    hasher.update([package_type_tag(manifest.package_type)]);
    feed_str(hasher, &compute_package_sha256(pkg));
}

pub(crate) fn package_graph_key(
    root: &PackageSources,
    catalog: &[PackageSources],
    privileged_packages: &BTreeSet<String>,
) -> String {
    let mut hasher = new_seeded_hasher();
    hasher.update(b"package_graph\0");
    feed_package_sources(&mut hasher, root);
    hasher.update((catalog.len() as u64).to_be_bytes());
    for pkg in catalog {
        feed_package_sources(&mut hasher, pkg);
    }
    hasher.update((privileged_packages.len() as u64).to_be_bytes());
    for name in privileged_packages {
        feed_str(&mut hasher, name);
    }
    finalize_hex(hasher)
}

pub(crate) fn load_cached_artifact(
    dir: Option<&Path>,
    key: &str,
) -> Option<nsbc::CompiledArtifact> {
    let dir = dir?;
    let path = dir.join(format!("{key}.nsbc"));
    let file = File::open(&path).ok()?;
    let mut bytes = Vec::new();
    if file
        .take(nsbc_io::MAX_ARCHIVE_SIZE as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        let _ = fs::remove_file(&path);
        return None;
    }
    let Ok(artifact) = nsbc_io::read_artifact(&bytes) else {
        let _ = fs::remove_file(&path);
        return None;
    };
    if artifact.builtin_abi_version != runtime::BUILTIN_ABI_VERSION
        || artifact.builtins.iter().any(|import| {
            runtime::lookup_builtin_fn_meta(import.id).map(|meta| meta.name)
                != Some(import.name.as_str())
        })
    {
        let _ = fs::remove_file(&path);
        return None;
    }
    Some(artifact)
}

pub(crate) fn store_cached_artifact(
    dir: Option<&Path>,
    key: &str,
    artifact: &nsbc::CompiledArtifact,
) {
    let Some(dir) = dir else {
        return;
    };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let Ok(bytes) = nsbc_io::write_artifact(artifact) else {
        return;
    };
    let seq = NEXT_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_path = dir.join(format!(".{key}.{}.{seq}.tmp", std::process::id()));
    let final_path = dir.join(format!("{key}.nsbc"));
    if fs::write(&tmp_path, bytes).is_ok() && fs::rename(&tmp_path, &final_path).is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_dir_resolution_respects_disable_and_override_precedence() {
        assert_eq!(
            resolve_cache_dir_from_vars(
                Some(OsStr::new("1")),
                Some(OsStr::new("/tmp/custom")),
                None,
                None
            ),
            None
        );
        assert_eq!(
            resolve_cache_dir_from_vars(None, Some(OsStr::new("off")), None, None),
            None
        );
        assert_eq!(
            resolve_cache_dir_from_vars(
                None,
                Some(OsStr::new("/tmp/custom-nsbc")),
                Some(OsStr::new("/tmp/xdg")),
                Some(OsStr::new("/tmp/home"))
            ),
            Some(PathBuf::from("/tmp/custom-nsbc"))
        );
        assert_eq!(
            resolve_cache_dir_from_vars(
                None,
                None,
                Some(OsStr::new("/tmp/xdg")),
                Some(OsStr::new("/tmp/home"))
            ),
            Some(PathBuf::from("/tmp/xdg/nessa/nsbc"))
        );
        assert_eq!(
            resolve_cache_dir_from_vars(None, None, None, Some(OsStr::new("/tmp/home"))),
            Some(PathBuf::from("/tmp/home/.cache/nessa/nsbc"))
        );
    }
}
