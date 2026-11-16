//! Engine-bundled extension packages (`nessa/regex`) compiled from embedded `.ns` sources.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sha2::{Digest, Sha256};

use crate::{ManifestDocument, PackageSources, SourceFile, compute_package_sha256};

const BUNDLED_PREFIX: &str = "bundled:";

/// Check whether `path` identifies an engine-bundled package root rather than a local disk directory.
pub fn is_bundled_package_root(path: &Path) -> bool {
    path.to_str().is_some_and(|s| s.starts_with(BUNDLED_PREFIX))
}

/// Return all engine-bundled extension packages (`nessa/regex`).
pub fn bundled_packages() -> &'static [PackageSources] {
    static PACKAGES: OnceLock<Vec<PackageSources>> = OnceLock::new();
    PACKAGES.get_or_init(|| {
        let regex_manifest =
            ManifestDocument::parse(include_str!("../../../library/regex/package.toml"))
                .expect("bundled library/regex/package.toml must be valid");
        let regex_pkg = PackageSources {
            root: PathBuf::from("bundled:nessa/regex"),
            manifest: regex_manifest,
            entry: SourceFile {
                file_path: PathBuf::from("bundled:nessa/regex/src/lib.ns"),
                source: include_str!("../../../library/regex/src/lib.ns").to_owned(),
            },
            modules: Vec::new(),
            wasm_assets: Vec::new(),
        };
        vec![regex_pkg]
    })
}

/// Deterministic SHA-256 digest over all bundled extension packages for `.nsbc` cache seeding.
pub fn bundled_packages_sha256() -> [u8; 32] {
    static DIGEST: OnceLock<[u8; 32]> = OnceLock::new();
    *DIGEST.get_or_init(|| {
        let mut hasher = Sha256::new();
        hasher.update(b"nessa.bundled.packages.v1\0");
        let pkgs = bundled_packages();
        hasher.update((pkgs.len() as u64).to_be_bytes());
        for pkg in pkgs {
            let qname = pkg.manifest.manifest().qualified_name();
            hasher.update((qname.len() as u64).to_be_bytes());
            hasher.update(qname.as_bytes());
            let sha = compute_package_sha256(pkg);
            hasher.update((sha.len() as u64).to_be_bytes());
            hasher.update(sha.as_bytes());
        }
        hasher.finalize().into()
    })
}
