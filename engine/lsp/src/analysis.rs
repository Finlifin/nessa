//! Live workspace analyzer, VFS package overlays, stdlib/bundled source materialization,
//! `last_good` snapshot fallback, and LSP 3.17 language intelligence providers.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use diagnostic::{Diagnostic, Level};
use driver::{CheckedSourceUnit, Driver, IdeCheckResult};
use lexer::TokenKind;
use pkg_manager::{ModuleSources, PackageSources};
use type_pool::{Intrinsic, TypeIndex};

use crate::index::{
    MemberEntry, Occurrence, ParamDoc, SemanticIndex, SymbolInfo, SymbolKey, build_semantic_index,
    build_semantic_tokens, lsp_completion_kind, lsp_symbol_kind,
};
use crate::json::JsonValue;
use crate::position::{
    LineIndex, LspPosition, LspRange, apply_content_changes, path_to_uri, uri_to_path,
};

/// Full-document formatter callback: `(path, source, tab_spaces) -> Result<formatted_source, error>`.
pub type FormatDocFn = fn(&Path, &str, u32) -> Result<String, String>;

/// Byte-range formatter callback: `(path, source, (start_byte, end_byte), tab_spaces) -> Result<formatted_source, error>`.
pub type FormatRangeFn = fn(&Path, &str, (u32, u32), u32) -> Result<String, String>;

const MAX_PACKAGE_WALK_DEPTH: usize = 64;
const COMPLETION_PROBE_IDENT: &str = "__nessa_lsp_probe__";

/// Configuration for [`WorkspaceAnalyzer`] and [`crate::LanguageServer`].
#[derive(Clone)]
pub struct ServerConfig {
    /// Optional explicit path to the Nessa `library/` directory (for `std` & `regex` sources).
    pub sysroot_override: Option<PathBuf>,
    /// Qualified package names (`domain/name`) granted privileged `'builtin` access.
    pub privileged_packages: BTreeSet<String>,
    /// Whether `textDocument/formatting` and `textDocument/rangeFormatting` are active.
    pub format_enabled: bool,
    /// Pluggable full-document formatter hook (defaults to [`default_format_doc`]).
    pub format_doc: Option<FormatDocFn>,
    /// Pluggable range formatter hook (defaults to [`default_format_range`]).
    pub format_range: Option<FormatRangeFn>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            sysroot_override: None,
            privileged_packages: BTreeSet::new(),
            format_enabled: true,
            format_doc: Some(default_format_doc),
            format_range: Some(default_format_range),
        }
    }
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("sysroot_override", &self.sysroot_override)
            .field("privileged_packages", &self.privileged_packages)
            .field("format_enabled", &self.format_enabled)
            .field("has_format_doc", &self.format_doc.is_some())
            .field("has_format_range", &self.format_range.is_some())
            .finish()
    }
}

impl ServerConfig {
    pub fn with_sysroot(mut self, sysroot: Option<PathBuf>) -> Self {
        self.sysroot_override = sysroot;
        self
    }

    pub fn with_formatter(
        mut self,
        format_doc: Option<FormatDocFn>,
        format_range: Option<FormatRangeFn>,
    ) -> Self {
        self.format_doc = format_doc;
        self.format_range = format_range;
        self
    }

    pub fn with_privileged_packages(
        mut self,
        packages: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.privileged_packages = packages.into_iter().map(Into::into).collect();
        self
    }
}

/// Default full-document formatter backed by [`nessa_fmt`].
pub fn default_format_doc(path: &Path, source: &str, tab_spaces: u32) -> Result<String, String> {
    let cfg = resolve_format_config_for_lsp(path, tab_spaces)?;
    nessa_fmt::format_source_with_path(source, Some(path), &cfg)
        .map(|res| res.formatted)
        .map_err(|err| err.to_string())
}

/// Default range formatter backed by [`nessa_fmt`].
pub fn default_format_range(
    path: &Path,
    source: &str,
    byte_range: (u32, u32),
    tab_spaces: u32,
) -> Result<String, String> {
    let cfg = resolve_format_config_for_lsp(path, tab_spaces)?;
    let line_idx = LineIndex::new(source);
    let lo = (byte_range.0 as usize).min(source.len());
    let hi = (byte_range.1 as usize).max(lo).min(source.len());
    let effective_hi = if hi > lo && source.as_bytes().get(hi - 1) == Some(&b'\n') {
        hi - 1
    } else {
        hi
    };
    let start_line = line_idx.offset_to_position(source, lo).line;
    let end_line = line_idx
        .offset_to_position(source, effective_hi)
        .line
        .max(start_line);
    nessa_fmt::format_range(source, (start_line, end_line), &cfg)
        .map(|res| res.formatted)
        .map_err(|err| err.to_string())
}

fn resolve_format_config_for_lsp(
    path: &Path,
    tab_spaces: u32,
) -> Result<nessa_fmt::FormatConfig, String> {
    let start_dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut cfg = if start_dir.is_dir() {
        nessa_fmt::FormatConfig::discover(start_dir, None).map_err(|e| e.to_string())?
    } else {
        nessa_fmt::FormatConfig::default()
    };
    if cfg.config_dir.is_none() && tab_spaces >= 1 {
        cfg.tab_spaces = tab_spaces as usize;
    }
    Ok(cfg)
}

/// LSP Diagnostic severity codes.
pub mod lsp_severity {
    pub const ERROR: u32 = 1;
    pub const WARNING: u32 = 2;
    pub const INFORMATION: u32 = 3;
    pub const HINT: u32 = 4;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedDiagInfo {
    pub file: PathBuf,
    pub range: LspRange,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDiagnostic {
    pub range: LspRange,
    pub severity: u32,
    pub code: Option<String>,
    pub message: String,
    pub related: Vec<RelatedDiagInfo>,
}

impl LspDiagnostic {
    pub fn to_json(&self) -> JsonValue {
        let mut fields = vec![
            ("range", self.range.to_json()),
            ("severity", JsonValue::from(self.severity)),
            ("source", JsonValue::from("nessa-lsp")),
            ("message", JsonValue::from(self.message.as_str())),
        ];
        if let Some(ref code) = self.code {
            fields.push(("code", JsonValue::from(code.as_str())));
        }
        if !self.related.is_empty() {
            let rel: Vec<JsonValue> = self
                .related
                .iter()
                .map(|r| {
                    JsonValue::object([
                        (
                            "location",
                            JsonValue::object([
                                ("uri", JsonValue::from(path_to_uri(&r.file))),
                                ("range", r.range.to_json()),
                            ]),
                        ),
                        ("message", JsonValue::from(r.message.as_str())),
                    ])
                })
                .collect();
            fields.push(("relatedInformation", JsonValue::Array(rel)));
        }
        JsonValue::object(fields)
    }
}

/// Live open buffer state tracked by the server.
#[derive(Debug, Clone)]
pub struct DocumentState {
    pub uri: String,
    pub path: PathBuf,
    pub version: i64,
    pub text: String,
    pub line_index: LineIndex,
}

impl DocumentState {
    pub fn new(uri: String, path: PathBuf, version: i64, text: String) -> Self {
        let line_index = LineIndex::new(&text);
        Self {
            uri,
            path,
            version,
            text,
            line_index,
        }
    }

    pub fn apply_changes(&mut self, version: i64, changes: &[JsonValue]) {
        apply_content_changes(&mut self.text, changes);
        self.version = version;
        self.line_index = LineIndex::new(&self.text);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum AnalysisTarget {
    Package(PathBuf),
    Standalone(PathBuf),
}

impl AnalysisTarget {
    fn root_dir(&self) -> PathBuf {
        match self {
            AnalysisTarget::Package(root) => root.clone(),
            AnalysisTarget::Standalone(file) => {
                file.parent().unwrap_or(Path::new(".")).to_path_buf()
            }
        }
    }
}

/// Cached analysis result for a package workspace or standalone script.
#[derive(Clone)]
pub struct PackageSnapshot {
    pub root_path: PathBuf,
    pub is_package: bool,
    pub index: SemanticIndex,
    pub diagnostics: HashMap<PathBuf, Vec<LspDiagnostic>>,
    pub member_files: HashSet<PathBuf>,
    pub dependency_names: Vec<String>,
    pub has_errors: bool,
}

/// Central stateful analyzer managing open buffers, package workspaces, stdlib mapping, and `last_good` caches.
pub struct WorkspaceAnalyzer {
    pub config: ServerConfig,
    pub workspace_roots: Vec<PathBuf>,
    pub open_docs: HashMap<PathBuf, DocumentState>,
    driver: Driver,
    snapshots: HashMap<AnalysisTarget, PackageSnapshot>,
    last_good_index: HashMap<AnalysisTarget, SemanticIndex>,
}

impl Default for WorkspaceAnalyzer {
    fn default() -> Self {
        Self::new(ServerConfig::default())
    }
}

impl WorkspaceAnalyzer {
    pub fn new(config: ServerConfig) -> Self {
        Self {
            config,
            workspace_roots: Vec::new(),
            open_docs: HashMap::new(),
            driver: Driver::new().without_cache(),
            snapshots: HashMap::new(),
            last_good_index: HashMap::new(),
        }
    }

    pub fn set_workspace_roots(&mut self, roots: Vec<PathBuf>) {
        self.workspace_roots = roots.into_iter().map(|p| normalize_path(&p)).collect();
    }

    pub fn set_sysroot_override(&mut self, sysroot: Option<PathBuf>) {
        self.config.sysroot_override = sysroot.map(|p| normalize_path(&p));
        self.snapshots.clear();
    }

    pub fn open_document(&mut self, uri: &str, version: i64, text: String) -> Option<PathBuf> {
        let raw_path = uri_to_path(uri)?;
        let path = normalize_path(&raw_path);
        let canonical_uri = path_to_uri(&path);
        self.open_docs.insert(
            path.clone(),
            DocumentState::new(canonical_uri, path.clone(), version, text),
        );
        self.reanalyze_for_file(&path);
        Some(path)
    }

    pub fn change_document(
        &mut self,
        uri: &str,
        version: i64,
        changes: &[JsonValue],
    ) -> Option<PathBuf> {
        let raw_path = uri_to_path(uri)?;
        let path = normalize_path(&raw_path);
        if let Some(doc) = self.open_docs.get_mut(&path) {
            doc.apply_changes(version, changes);
        } else if let Some(last_text) = changes.last().and_then(|c| c.get_str("text")) {
            let canonical_uri = path_to_uri(&path);
            self.open_docs.insert(
                path.clone(),
                DocumentState::new(canonical_uri, path.clone(), version, last_text.to_owned()),
            );
        }
        self.reanalyze_for_file(&path);
        Some(path)
    }

    pub fn save_document(&mut self, uri: &str, text: Option<String>) -> Option<PathBuf> {
        let raw_path = uri_to_path(uri)?;
        let path = normalize_path(&raw_path);
        if let Some(new_text) = text
            && let Some(doc) = self.open_docs.get_mut(&path)
        {
            doc.text = new_text;
            doc.line_index = LineIndex::new(&doc.text);
        }
        self.reanalyze_for_file(&path);
        Some(path)
    }

    pub fn close_document(&mut self, uri: &str) -> Option<PathBuf> {
        let raw_path = uri_to_path(uri)?;
        let path = normalize_path(&raw_path);
        self.open_docs.remove(&path);
        if path.exists() {
            self.reanalyze_for_file(&path);
        } else {
            let target = self.classify_target(&path);
            self.snapshots.remove(&target);
        }
        Some(path)
    }

    pub fn invalidate_all(&mut self) {
        self.snapshots.clear();
        let open_paths: Vec<PathBuf> = self.open_docs.keys().cloned().collect();
        for p in open_paths {
            self.reanalyze_for_file(&p);
        }
    }

    pub fn file_text(&self, path: &Path) -> Option<(String, LineIndex)> {
        let norm = normalize_path(path);
        if let Some(doc) = self.open_docs.get(&norm) {
            return Some((doc.text.clone(), doc.line_index.clone()));
        }
        let disk = fs::read_to_string(&norm).ok()?;
        let idx = LineIndex::new(&disk);
        Some((disk, idx))
    }

    /// Walk upward (bounded depth) looking for `package.toml`.
    pub fn find_package_manifest_dir(&self, file_or_dir: &Path) -> Option<PathBuf> {
        let start = if file_or_dir.is_dir() {
            file_or_dir.to_path_buf()
        } else {
            file_or_dir.parent()?.to_path_buf()
        };
        let mut cur = start.as_path();
        for _ in 0..MAX_PACKAGE_WALK_DEPTH {
            if cur.join("package.toml").is_file() {
                return Some(normalize_path(cur));
            }
            let Some(parent) = cur.parent() else {
                break;
            };
            cur = parent;
        }
        None
    }

    fn classify_target(&self, file_or_dir: &Path) -> AnalysisTarget {
        let norm = normalize_path(file_or_dir);
        if norm.is_dir() && norm.join("package.toml").is_file() {
            return AnalysisTarget::Package(norm);
        }
        if let Some(pkg_root) = self.find_package_manifest_dir(&norm) {
            if norm == pkg_root || norm == pkg_root.join("package.toml") {
                return AnalysisTarget::Package(pkg_root);
            }
            // Verify `norm` belongs to the package's `src/` tree (or loaded modules/dependencies)
            // rather than being a standalone scratch/test script outside `src/`.
            if norm.starts_with(pkg_root.join("src")) {
                return AnalysisTarget::Package(pkg_root);
            }
            if let Ok(ws) = pkg_manager::load_package_workspace(&pkg_root)
                && (package_sources_contains(&ws.root, &norm)
                    || ws
                        .dependencies
                        .iter()
                        .any(|d| package_sources_contains(d, &norm)))
            {
                return AnalysisTarget::Package(pkg_root);
            }
        }
        AnalysisTarget::Standalone(norm)
    }

    pub fn ensure_snapshot(&mut self, file_or_dir: &Path) -> &PackageSnapshot {
        let target = self.classify_target(file_or_dir);
        if !self.snapshots.contains_key(&target) {
            let snap = self.analyze_target(&target, None);
            self.snapshots.insert(target.clone(), snap);
        }
        self.snapshots.get(&target).unwrap()
    }

    pub fn reanalyze_for_file(&mut self, file: &Path) -> PackageSnapshot {
        let norm = normalize_path(file);
        let target = self.classify_target(&norm);
        let snap = self.analyze_target(&target, None);
        self.snapshots.insert(target.clone(), snap.clone());

        // Also refresh any other cached package snapshots that depend on `norm` (cross-package live workspace!).
        let dependent_targets: Vec<AnalysisTarget> = self
            .snapshots
            .iter()
            .filter(|&(k, s)| k != &target && s.member_files.contains(&norm))
            .map(|(k, _)| k.clone())
            .collect();
        for dep_target in dependent_targets {
            let refreshed = self.analyze_target(&dep_target, None);
            self.snapshots.insert(dep_target, refreshed);
        }

        snap
    }

    fn last_good_for(&self, file: &Path) -> Option<&SemanticIndex> {
        let target = self.classify_target(file);
        self.last_good_index.get(&target)
    }

    /// Discover on-disk `library/` directory (containing `std/mod.ns` or `regex/src/lib.ns`) if available.
    pub fn discover_library_dir(&self, from_dir: &Path) -> Option<PathBuf> {
        if let Some(ref ov) = self.config.sysroot_override {
            if ov.join("std/mod.ns").is_file() {
                return Some(ov.clone());
            }
            let cand = ov.join("library");
            if cand.join("std/mod.ns").is_file() {
                return Some(cand);
            }
        }
        if let Ok(env_sr) = std::env::var("NESSA_SYSROOT") {
            let p = normalize_path(Path::new(&env_sr));
            if p.join("std/mod.ns").is_file() {
                return Some(p);
            }
            let cand = p.join("library");
            if cand.join("std/mod.ns").is_file() {
                return Some(cand);
            }
        }
        for ws in &self.workspace_roots {
            if ws.join("std/mod.ns").is_file() {
                return Some(ws.clone());
            }
            let cand = ws.join("library");
            if cand.join("std/mod.ns").is_file() {
                return Some(cand);
            }
        }
        let mut cur = Some(from_dir);
        for _ in 0..MAX_PACKAGE_WALK_DEPTH {
            let Some(dir) = cur else { break };
            if dir.join("std/mod.ns").is_file() {
                return Some(normalize_path(dir));
            }
            let cand = dir.join("library");
            if cand.join("std/mod.ns").is_file() {
                return Some(normalize_path(&cand));
            }
            cur = dir.parent();
        }
        if let Ok(exe) = std::env::current_exe() {
            let mut cur = exe.parent();
            for _ in 0..16 {
                let Some(dir) = cur else { break };
                let cand = dir.join("library");
                if cand.join("std/mod.ns").is_file() {
                    return Some(normalize_path(&cand));
                }
                cur = dir.parent();
            }
        }
        None
    }

    fn resolve_unit_path(&self, unit: &CheckedSourceUnit, from_dir: &Path) -> PathBuf {
        if !unit.is_std && !unit.is_bundled {
            return normalize_path(&unit.path);
        }
        let rel_path = if unit.is_std {
            unit.logical_name.as_str()
        } else {
            let stripped = unit
                .logical_name
                .strip_prefix("bundled:")
                .unwrap_or(&unit.logical_name);
            stripped
                .split_once('/')
                .map(|(_, rest)| rest)
                .unwrap_or(stripped)
        };

        if let Some(lib_dir) = self.discover_library_dir(from_dir) {
            let real_cand = lib_dir.join(rel_path);
            if real_cand.is_file() {
                return normalize_path(&real_cand);
            }
        }

        materialize_stdlib_cache_file(rel_path, &unit.source)
    }

    fn analyze_target(
        &mut self,
        target: &AnalysisTarget,
        ephemeral_override: Option<(&Path, &str)>,
    ) -> PackageSnapshot {
        let from_dir = target.root_dir();
        match target {
            AnalysisTarget::Package(pkg_root) => {
                self.analyze_package_workspace(pkg_root, ephemeral_override)
            }
            AnalysisTarget::Standalone(file_path) => {
                let source = if let Some((ov_path, ov_src)) = ephemeral_override
                    && ov_path == file_path
                {
                    ov_src.to_owned()
                } else if let Some((txt, _)) = self.file_text(file_path) {
                    txt
                } else {
                    String::new()
                };

                let check = self.driver.check_source_ide(file_path, &source);
                let mut member_files = HashSet::new();
                member_files.insert(file_path.clone());
                let dep_names: Vec<String> = pkg_manager::bundled_packages()
                    .iter()
                    .map(|p| p.manifest.manifest().name.clone())
                    .collect();
                let mut snap = self.finish_snapshot(
                    target,
                    file_path.clone(),
                    false,
                    check,
                    member_files,
                    dep_names,
                    HashSet::new(),
                    &from_dir,
                    ephemeral_override.is_none(),
                );
                snap.diagnostics.entry(file_path.clone()).or_default();
                snap
            }
        }
    }

    fn analyze_package_workspace(
        &mut self,
        pkg_root: &Path,
        ephemeral_override: Option<(&Path, &str)>,
    ) -> PackageSnapshot {
        let target = AnalysisTarget::Package(pkg_root.to_path_buf());
        let mut workspace = match pkg_manager::load_package_workspace(pkg_root) {
            Ok(ws) => ws,
            Err(error) => {
                let manifest_file = pkg_root.join("package.toml");
                let target_file = if manifest_file.is_file() {
                    normalize_path(&manifest_file)
                } else {
                    pkg_root.to_path_buf()
                };
                let mut diagnostics = HashMap::new();
                diagnostics.insert(
                    target_file.clone(),
                    vec![LspDiagnostic {
                        range: LspRange::default(),
                        severity: lsp_severity::ERROR,
                        code: None,
                        message: error.to_string(),
                        related: Vec::new(),
                    }],
                );
                let fallback_index = self
                    .last_good_index
                    .get(&target)
                    .cloned()
                    .unwrap_or_default();
                let mut member_files = HashSet::new();
                member_files.insert(target_file);
                return PackageSnapshot {
                    root_path: pkg_root.to_path_buf(),
                    is_package: true,
                    index: fallback_index,
                    diagnostics,
                    member_files,
                    dependency_names: Vec::new(),
                    has_errors: true,
                };
            }
        };

        let mut member_files = HashSet::new();
        let mut privileged = self.config.privileged_packages.clone();
        let mut privileged_files = HashSet::new();

        self.overlay_package_sources(&mut workspace.root, ephemeral_override, &mut member_files);
        let root_manifest = workspace.root.manifest.manifest();
        if matches!(root_manifest.domain.as_str(), "std" | "core" | "alloc") {
            privileged.insert(root_manifest.qualified_name());
        }
        let root_has_view_priv = privileged.contains(&root_manifest.qualified_name())
            || workspace.root.manifest.ffi().allow;
        if root_has_view_priv {
            collect_package_file_paths(&workspace.root, &mut privileged_files);
        }

        for dep in &mut workspace.dependencies {
            if !pkg_manager::is_bundled_package_root(&dep.root) {
                self.overlay_package_sources(dep, ephemeral_override, &mut member_files);
            }
            let dm = dep.manifest.manifest();
            if matches!(dm.domain.as_str(), "std" | "core" | "alloc") {
                privileged.insert(dm.qualified_name());
            }
            if privileged.contains(&dm.qualified_name()) || dep.manifest.ffi().allow {
                collect_package_file_paths(dep, &mut privileged_files);
            }
        }

        let dep_names: Vec<String> = root_manifest
            .dependencies
            .iter()
            .map(|d| d.name.clone())
            .collect();

        let check = self.driver.check_package_sources_with_privileged_ide(
            &workspace.root,
            &workspace.dependencies,
            &privileged,
        );

        self.finish_snapshot(
            &target,
            pkg_root.to_path_buf(),
            true,
            check,
            member_files,
            dep_names,
            privileged_files,
            pkg_root,
            ephemeral_override.is_none(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_snapshot(
        &mut self,
        target: &AnalysisTarget,
        root_path: PathBuf,
        is_package: bool,
        check: IdeCheckResult,
        member_files: HashSet<PathBuf>,
        dependency_names: Vec<String>,
        privileged_files: HashSet<PathBuf>,
        from_dir: &Path,
        update_last_good: bool,
    ) -> PackageSnapshot {
        let index = build_semantic_index(
            &check,
            |unit| self.resolve_unit_path(unit, from_dir),
            privileged_files,
        );
        let diagnostics = self.map_diagnostics(&check, &root_path, from_dir);
        let has_errors = check.has_errors;

        if update_last_good && !has_errors && !index.symbols.is_empty() {
            self.last_good_index.insert(target.clone(), index.clone());
        }

        PackageSnapshot {
            root_path,
            is_package,
            index,
            diagnostics,
            member_files,
            dependency_names,
            has_errors,
        }
    }

    fn overlay_package_sources(
        &self,
        pkg: &mut PackageSources,
        ephemeral_override: Option<(&Path, &str)>,
        member_files: &mut HashSet<PathBuf>,
    ) {
        self.overlay_source_file(&mut pkg.entry, ephemeral_override, member_files);
        for module in &mut pkg.modules {
            self.overlay_module_sources(module, ephemeral_override, member_files);
        }
    }

    fn overlay_module_sources(
        &self,
        module: &mut ModuleSources,
        ephemeral_override: Option<(&Path, &str)>,
        member_files: &mut HashSet<PathBuf>,
    ) {
        self.overlay_source_file(&mut module.entry, ephemeral_override, member_files);
        for child in &mut module.children {
            self.overlay_module_sources(child, ephemeral_override, member_files);
        }
    }

    fn overlay_source_file(
        &self,
        sf: &mut pkg_manager::SourceFile,
        ephemeral_override: Option<(&Path, &str)>,
        member_files: &mut HashSet<PathBuf>,
    ) {
        let norm = normalize_path(&sf.file_path);
        sf.file_path = norm.clone();
        member_files.insert(norm.clone());
        if let Some((ov_path, ov_text)) = ephemeral_override
            && ov_path == norm
        {
            sf.source = ov_text.to_owned();
        } else if let Some(doc) = self.open_docs.get(&norm) {
            sf.source = doc.text.clone();
        }
    }

    fn map_diagnostics(
        &self,
        check: &IdeCheckResult,
        default_root: &Path,
        from_dir: &Path,
    ) -> HashMap<PathBuf, Vec<LspDiagnostic>> {
        let mut out: HashMap<PathBuf, Vec<LspDiagnostic>> = HashMap::new();
        let line_indices: Vec<(PathBuf, bool, LineIndex)> = check
            .units
            .iter()
            .map(|u| {
                let p = self.resolve_unit_path(u, from_dir);
                (p, u.is_std || u.is_bundled, LineIndex::new(&u.source))
            })
            .collect();

        let locate_diag_span = |span: rustc_span::Span| -> Option<(PathBuf, bool, LspRange)> {
            if span.is_dummy() {
                return None;
            }
            let part = check.units.partition_point(|u| u.start_pos <= span.lo());
            for cand in [part.checked_sub(1), Some(part)].into_iter().flatten() {
                if let Some(unit) = check.units.get(cand)
                    && let Some(raw) = unit.original_range(span)
                {
                    let (ref path, is_ext, ref lidx) = line_indices[cand];
                    let range =
                        lidx.span_to_range(&unit.source, raw.start as usize, raw.end as usize);
                    return Some((path.clone(), is_ext, range));
                }
            }
            None
        };

        let fallback_file = if default_root.is_file() {
            default_root.to_path_buf()
        } else if default_root.join("package.toml").is_file() {
            normalize_path(&default_root.join("package.toml"))
        } else {
            check
                .units
                .iter()
                .find(|u| !u.is_std && !u.is_bundled)
                .map(|u| normalize_path(&u.path))
                .unwrap_or_else(|| default_root.to_path_buf())
        };

        for diag in &check.diagnostics {
            self.push_mapped_diag(&mut out, diag, &locate_diag_span, &fallback_file);
        }
        out
    }

    fn push_mapped_diag<L>(
        &self,
        out: &mut HashMap<PathBuf, Vec<LspDiagnostic>>,
        diag: &Diagnostic,
        locate: &L,
        fallback_file: &Path,
    ) where
        L: Fn(rustc_span::Span) -> Option<(PathBuf, bool, LspRange)>,
    {
        let primary = diag
            .primary_span
            .and_then(locate)
            .or_else(|| diag.labels.iter().find_map(|l| locate(l.span)));

        let (file, is_ext, range) = match primary {
            Some(p) => p,
            None => (fallback_file.to_path_buf(), false, LspRange::default()),
        };
        if is_ext {
            return;
        }

        let severity = match diag.level {
            Level::Error => lsp_severity::ERROR,
            Level::Warning => lsp_severity::WARNING,
            Level::Note => lsp_severity::INFORMATION,
            Level::Help => lsp_severity::HINT,
        };
        let code = diag.code.map(|c| format!("E{c:04}"));

        let mut message = diag.message.clone();
        let mut related = Vec::new();
        for label in &diag.labels {
            if let Some((lfile, _, lrange)) = locate(label.span) {
                if lfile == file && lrange == range && label.message == diag.message {
                    continue;
                }
                if !label.message.is_empty() {
                    related.push(RelatedDiagInfo {
                        file: lfile,
                        range: lrange,
                        message: label.message.clone(),
                    });
                }
            }
        }
        for note in &diag.notes {
            message.push_str(&format!("\nnote: {note}"));
        }
        for help in &diag.helps {
            message.push_str(&format!("\nhelp: {help}"));
        }

        let entry = LspDiagnostic {
            range,
            severity,
            code,
            message,
            related,
        };
        let list = out.entry(file).or_default();
        if !list.iter().any(|existing| {
            existing.range == entry.range
                && existing.severity == entry.severity
                && existing.message == entry.message
        }) {
            list.push(entry);
        }
    }

    // =========================================================================
    // LSP Feature Queries
    // =========================================================================

    fn resolve_symbol_at<'a>(
        &'a self,
        snap: &'a PackageSnapshot,
        file: &Path,
        offset: usize,
        text: &str,
    ) -> Option<(&'a SymbolInfo, Option<&'a Occurrence>, &'a SemanticIndex)> {
        if let Some(occ) = snap.index.occurrence_at(file, offset)
            && let Some(sym) = snap.index.symbol(occ.target)
        {
            return Some((sym, Some(occ), &snap.index));
        }
        if let Some(good) = self.last_good_for(file) {
            if let Some(occ) = good.occurrence_at(file, offset)
                && let Some(sym) = good.symbol(occ.target)
            {
                return Some((sym, Some(occ), good));
            }
            if let Some((word, _)) = word_or_backtick_at(text, offset)
                && let Some(sym) = good.lookup_named_symbol_in_file(file, &word, offset)
            {
                return Some((sym, None, good));
            }
        }
        if let Some((word, _)) = word_or_backtick_at(text, offset)
            && let Some(sym) = snap.index.lookup_named_symbol_in_file(file, &word, offset)
        {
            return Some((sym, None, &snap.index));
        }
        None
    }

    pub fn hover(&mut self, file: &Path, pos: LspPosition) -> Option<JsonValue> {
        let norm = normalize_path(file);
        let (text, line_idx) = self.file_text(&norm)?;
        let offset = line_idx.position_to_offset(&text, pos);
        let snap = self.ensure_snapshot(&norm).clone();

        let (sym, occ, _) = self.resolve_symbol_at(&snap, &norm, offset, &text)?;
        let range = occ
            .map(|o| line_idx.span_to_range(&text, o.span.0, o.span.1))
            .or_else(|| {
                word_or_backtick_at(&text, offset)
                    .map(|(_, (lo, hi))| line_idx.span_to_range(&text, lo, hi))
            });

        let mut sig = sym.detail.clone();
        // If flow-narrowing refined a local variable's type at this exact use site, show it!
        if matches!(sym.kind, lsp_symbol_kind::VARIABLE)
            && let Some(occ) = occ
            && !occ.is_decl
            && let Some(ref narrowed) = occ.site_type_detail
            && !sig.ends_with(narrowed)
        {
            sig = format!("{}: {narrowed}", sym.name);
        }

        let mut md = format!("```nessa\n{sig}\n```");
        if let Some(ref container) = sym.container_name
            && !container.is_empty()
        {
            md.push_str(&format!("\n*in `{container}`*"));
        }
        if let Some(ref doc) = sym.doc
            && !doc.is_empty()
        {
            md.push_str("\n\n---\n\n");
            md.push_str(doc);
        }

        let mut fields = vec![(
            "contents",
            JsonValue::object([
                ("kind", JsonValue::from("markdown")),
                ("value", JsonValue::from(md)),
            ]),
        )];
        if let Some(r) = range {
            fields.push(("range", r.to_json()));
        }
        Some(JsonValue::object(fields))
    }

    pub fn goto_definition(&mut self, file: &Path, pos: LspPosition) -> Vec<JsonValue> {
        let norm = normalize_path(file);
        let Some((text, line_idx)) = self.file_text(&norm) else {
            return Vec::new();
        };
        let offset = line_idx.position_to_offset(&text, pos);
        let snap = self.ensure_snapshot(&norm).clone();
        let Some((sym, _, _)) = self.resolve_symbol_at(&snap, &norm, offset, &text) else {
            return Vec::new();
        };
        self.symbol_location_json(sym).into_iter().collect()
    }

    pub fn goto_type_definition(&mut self, file: &Path, pos: LspPosition) -> Vec<JsonValue> {
        let norm = normalize_path(file);
        let Some((text, line_idx)) = self.file_text(&norm) else {
            return Vec::new();
        };
        let offset = line_idx.position_to_offset(&text, pos);
        let snap = self.ensure_snapshot(&norm).clone();
        let Some((sym, occ, idx)) = self.resolve_symbol_at(&snap, &norm, offset, &text) else {
            return Vec::new();
        };
        let target_key = occ
            .and_then(|o| o.site_type_def)
            .or(sym.type_def_target)
            .unwrap_or(sym.key);
        let Some(target_sym) = idx.symbol(target_key) else {
            return Vec::new();
        };
        self.symbol_location_json(target_sym).into_iter().collect()
    }

    pub fn find_implementations(&mut self, file: &Path, pos: LspPosition) -> Vec<JsonValue> {
        let norm = normalize_path(file);
        let Some((text, line_idx)) = self.file_text(&norm) else {
            return Vec::new();
        };
        let offset = line_idx.position_to_offset(&text, pos);
        let snap = self.ensure_snapshot(&norm).clone();
        let Some((sym, _, idx)) = self.resolve_symbol_at(&snap, &norm, offset, &text) else {
            return Vec::new();
        };

        let mut locs = Vec::new();
        // Case 1: Cursor is on a Trait method -> jump to concrete method implementations!
        if let Some(tr_ty) = sym.trait_owner
            && let Some(impls) = idx.trait_impls.get(&tr_ty)
        {
            for imp in impls {
                if let Some(&mkey) = imp.methods.get(&sym.name)
                    && let Some(msym) = idx.symbol(mkey)
                    && let Some(loc) = self.symbol_location_json(msym)
                {
                    locs.push(loc);
                } else if let Some(loc) = self.span_location_json(&imp.file, imp.span) {
                    locs.push(loc);
                }
            }
            return locs;
        }

        // Case 2: Cursor is on a Trait name (`trait Drawable` or `Display`) -> jump to all `impl`/`extend`/`derive` blocks!
        if let Some(raw_ty) = sym.type_index {
            for (&cand_tr_ty, impls) in &idx.trait_impls {
                let matches_trait = cand_tr_ty == raw_ty
                    || idx
                        .type_names
                        .get(&cand_tr_ty)
                        .is_some_and(|n| n == &sym.name);
                if matches_trait {
                    for imp in impls {
                        if let Some(loc) = self.span_location_json(&imp.file, imp.span) {
                            locs.push(loc);
                        }
                    }
                }
            }
        }
        locs
    }

    pub fn find_references(
        &mut self,
        file: &Path,
        pos: LspPosition,
        include_decl: bool,
    ) -> Vec<JsonValue> {
        let norm = normalize_path(file);
        let Some((text, line_idx)) = self.file_text(&norm) else {
            return Vec::new();
        };
        let offset = line_idx.position_to_offset(&text, pos);
        let snap = self.ensure_snapshot(&norm).clone();
        let Some((sym, _, idx)) = self.resolve_symbol_at(&snap, &norm, offset, &text) else {
            return Vec::new();
        };
        idx.references_to(sym.key, include_decl)
            .into_iter()
            .filter_map(|(fpath, span)| self.span_location_json(&fpath, span))
            .collect()
    }

    pub fn document_highlights(&mut self, file: &Path, pos: LspPosition) -> Vec<JsonValue> {
        let norm = normalize_path(file);
        let Some((text, line_idx)) = self.file_text(&norm) else {
            return Vec::new();
        };
        let offset = line_idx.position_to_offset(&text, pos);
        let snap = self.ensure_snapshot(&norm).clone();
        let Some((sym, _, idx)) = self.resolve_symbol_at(&snap, &norm, offset, &text) else {
            return Vec::new();
        };
        idx.highlights_in_file(&norm, sym.key)
            .into_iter()
            .map(|occ| {
                // LSP DocumentHighlightKind: 1 = Text, 2 = Read, 3 = Write
                let kind = if occ.is_write || occ.is_decl {
                    3u32
                } else {
                    2u32
                };
                JsonValue::object([
                    (
                        "range",
                        line_idx
                            .span_to_range(&text, occ.span.0, occ.span.1)
                            .to_json(),
                    ),
                    ("kind", JsonValue::from(kind)),
                ])
            })
            .collect()
    }

    pub fn prepare_rename(&mut self, file: &Path, pos: LspPosition) -> Option<JsonValue> {
        let norm = normalize_path(file);
        let (text, line_idx) = self.file_text(&norm)?;
        let offset = line_idx.position_to_offset(&text, pos);
        let snap = self.ensure_snapshot(&norm).clone();
        let occ = snap.index.occurrence_at(&norm, offset)?;
        let sym = snap.index.symbol(occ.target)?;
        if sym.is_external || sym.is_synthesized_module || sym.name == "self" || sym.name == "Self"
        {
            return None;
        }
        Some(JsonValue::object([
            (
                "range",
                line_idx
                    .span_to_range(&text, occ.span.0, occ.span.1)
                    .to_json(),
            ),
            ("placeholder", JsonValue::from(sym.name.as_str())),
        ]))
    }

    pub fn rename(
        &mut self,
        file: &Path,
        pos: LspPosition,
        new_name: &str,
    ) -> Result<JsonValue, String> {
        let formatted_ident = format_nessa_rename_ident(new_name)?;
        let norm = normalize_path(file);
        let Some((text, line_idx)) = self.file_text(&norm) else {
            return Err("document is not available".into());
        };
        let offset = line_idx.position_to_offset(&text, pos);
        let snap = self.ensure_snapshot(&norm).clone();
        let Some(occ) = snap.index.occurrence_at(&norm, offset) else {
            return Err("no symbol at cursor position".into());
        };
        let Some(sym) = snap.index.symbol(occ.target) else {
            return Err("symbol cannot be resolved".into());
        };
        if sym.is_external {
            return Err("cannot rename external standard library or bundled symbol".into());
        }
        if sym.is_synthesized_module {
            return Err("cannot rename filesystem module declaration without moving file".into());
        }
        if sym.name == "self" || sym.name == "Self" {
            return Err("cannot rename `self` / `Self` keyword".into());
        }

        let refs = snap.index.references_to(sym.key, true);
        let mut by_uri: BTreeMap<String, Vec<JsonValue>> = BTreeMap::new();
        for (fpath, (lo, hi)) in refs {
            if let Some((ftext, flines)) = self.file_text(&fpath) {
                let uri = path_to_uri(&fpath);
                by_uri.entry(uri).or_default().push(JsonValue::object([
                    ("range", flines.span_to_range(&ftext, lo, hi).to_json()),
                    ("newText", JsonValue::from(formatted_ident.as_str())),
                ]));
            }
        }
        let changes = JsonValue::Object(
            by_uri
                .into_iter()
                .map(|(k, v)| (k, JsonValue::Array(v)))
                .collect(),
        );
        Ok(JsonValue::object([("changes", changes)]))
    }

    pub fn complete(&mut self, file: &Path, pos: LspPosition) -> Vec<JsonValue> {
        let norm = normalize_path(file);
        let Some((text, line_idx)) = self.file_text(&norm) else {
            return Vec::new();
        };
        let offset = line_idx.position_to_offset(&text, pos);
        let line_start = text[..offset].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_prefix = &text[line_start..offset];
        let trimmed_line = line_prefix.trim_start();

        // 1. `--%` formatter directive completions!
        if let Some(after_dir) = trimmed_line.strip_prefix("--%") {
            return complete_fmt_directives(after_dir);
        }
        // Inside an ordinary `--` comment on this line: suppress code completions.
        if trimmed_line.starts_with("--") {
            return Vec::new();
        }

        let snap = self.ensure_snapshot(&norm).clone();

        // 2. `'` view completions (`.print'builtin`, `expr'type`)!
        if let Some(partial) = quote_view_context(line_prefix) {
            let allow_builtin = snap.index.privileged_files.contains(&norm);
            return complete_views(partial, allow_builtin, &snap.index, &norm, offset);
        }

        // 3. `@` package-root completions (`use @...` or `@mod`)!
        if let Some(partial) = at_package_context(line_prefix) {
            return complete_at_package(&snap, self.last_good_for(&norm), partial);
        }

        // 4. Bare `use <pkg>` completions (`std`, `@`, manifest dependencies)!
        if let Some(partial) = bare_use_context(trimmed_line) {
            return complete_use_roots(&snap, partial);
        }

        // 5. Dot `.` completions (`expr.`, `expr.par`, `Enum.`, `std.`, `mod.`)!
        if let Some((dot_rel_in_line, partial)) = dot_completion_offset(line_prefix) {
            let dot_offset = line_start + dot_rel_in_line;
            let items = self.complete_dot_member(&norm, &text, dot_offset, &partial, &snap);
            if !items.is_empty() {
                return items;
            }
        }

        // 6. Lexical / scope + prelude + keyword completions!
        let partial = identifier_prefix(line_prefix);
        self.complete_lexical_scope(&norm, offset, partial, &snap)
    }

    fn complete_dot_member(
        &mut self,
        file: &Path,
        text: &str,
        dot_offset: usize,
        partial: &str,
        snap: &PackageSnapshot,
    ) -> Vec<JsonValue> {
        // Tier 1: Direct lookup in `snap.index.file_projections` (works when `expr.par` already has an identifier!).
        if let Some(proj) = snap
            .index
            .file_projections
            .iter()
            .flat_map(|_| snap.index.file_projections.get(file).into_iter().flatten())
            .find(|p| p.dot_offset == dot_offset)
        {
            let items = build_dot_items_from_projection(
                &snap.index,
                proj.receiver_type,
                proj.receiver_sym,
                proj.is_type_or_module_receiver,
                partial,
            );
            if !items.is_empty() {
                return items;
            }
        }

        // Tier 2: Speculative probe! Replace `.partial` with `.__nessa_lsp_probe__` so `expr.` becomes valid Nessa syntax!
        let replace_end = dot_offset + 1 + partial.len();
        if replace_end <= text.len() {
            let mut probed = String::with_capacity(text.len() + COMPLETION_PROBE_IDENT.len() + 1);
            probed.push_str(&text[..dot_offset + 1]);
            probed.push_str(COMPLETION_PROBE_IDENT);
            probed.push_str(&text[replace_end..]);

            let target = self.classify_target(file);
            let probe_snap = self.analyze_target(&target, Some((file, &probed)));
            if let Some(projs) = probe_snap.index.file_projections.get(file)
                && let Some(proj) = projs.iter().find(|p| p.dot_offset == dot_offset)
            {
                let items = build_dot_items_from_projection(
                    &probe_snap.index,
                    proj.receiver_type,
                    proj.receiver_sym,
                    proj.is_type_or_module_receiver,
                    partial,
                );
                if !items.is_empty() {
                    return items;
                }
            }
        }

        // Tier 3: Resilient chain walk across `snap.index` and `last_good_index`!
        let recv_chain = extract_trailing_chain(&text[..dot_offset]);
        if !recv_chain.is_empty() {
            for candidate_idx in [Some(&snap.index), self.last_good_for(file)]
                .into_iter()
                .flatten()
            {
                let items =
                    resolve_chain_dot_items(candidate_idx, file, dot_offset, &recv_chain, partial);
                if !items.is_empty() {
                    return items;
                }
            }
        }
        Vec::new()
    }

    fn complete_lexical_scope(
        &self,
        file: &Path,
        offset: usize,
        partial: &str,
        snap: &PackageSnapshot,
    ) -> Vec<JsonValue> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();

        for (priority, idx) in [("0", Some(&snap.index)), ("1", self.last_good_for(file))] {
            let Some(idx) = idx else { continue };
            for (name, key) in idx.visible_bindings_at(file, offset) {
                if !name.starts_with(partial)
                    || name == COMPLETION_PROBE_IDENT
                    || !seen.insert(name)
                {
                    continue;
                }
                if let Some(sym) = idx.symbol(key) {
                    out.push(symbol_to_completion_item(sym, priority));
                }
            }
            if let Some(roots) = idx.file_roots.get(file) {
                for &rkey in roots {
                    if let Some(sym) = idx.symbol(rkey)
                        && !matches!(sym.key, SymbolKey::ImplBlock(_))
                        && sym.name.starts_with(partial)
                        && seen.insert(sym.name.clone())
                    {
                        out.push(symbol_to_completion_item(sym, priority));
                    }
                }
            }
        }

        if seen.insert("std".to_owned()) && "std".starts_with(partial) {
            out.push(JsonValue::object([
                ("label", JsonValue::from("std")),
                ("kind", JsonValue::from(lsp_completion_kind::MODULE)),
                ("detail", JsonValue::from("Nessa standard library")),
                ("sortText", JsonValue::from("2_std")),
            ]));
        }

        for &kw in NESSA_KEYWORDS {
            if kw.starts_with(partial) && seen.insert(kw.to_owned()) {
                out.push(JsonValue::object([
                    ("label", JsonValue::from(kw)),
                    ("kind", JsonValue::from(lsp_completion_kind::KEYWORD)),
                    ("detail", JsonValue::from("keyword")),
                    ("sortText", JsonValue::from(format!("9_{kw}"))),
                ]));
            }
        }
        out
    }

    pub fn signature_help(&mut self, file: &Path, pos: LspPosition) -> Option<JsonValue> {
        let norm = normalize_path(file);
        let (text, line_idx) = self.file_text(&norm)?;
        let offset = line_idx.position_to_offset(&text, pos);
        let call_ctx = enclosing_call_context(&text, offset)?;
        let snap = self.ensure_snapshot(&norm).clone();

        // Resolve callee at the end of `call_ctx.callee_span` (the identifier before `(`).
        let callee_probe = call_ctx.callee_ident_span.1.saturating_sub(1);
        let sym_and_idx = self
            .resolve_symbol_at(&snap, &norm, callee_probe, &text)
            .or_else(|| {
                // If unclosed `(` broke parsing of this call expression, resolve callee via dot chain or lexical name!
                let callee_name = &text[call_ctx.callee_ident_span.0..call_ctx.callee_ident_span.1];
                if let Some(recv) = call_ctx.receiver_chain.as_deref() {
                    for idx in [Some(&snap.index), self.last_good_for(&norm)]
                        .into_iter()
                        .flatten()
                    {
                        if let Some(msym) = resolve_chain_member_symbol(
                            idx,
                            &norm,
                            call_ctx.callee_ident_span.0,
                            recv,
                            callee_name,
                        ) {
                            return Some((msym, None, idx));
                        }
                    }
                }
                None
            })?;

        let (sym, _, _) = sym_and_idx;
        if sym.params.is_empty()
            && !matches!(
                sym.kind,
                lsp_symbol_kind::FUNCTION
                    | lsp_symbol_kind::METHOD
                    | lsp_symbol_kind::ENUM_MEMBER
                    | lsp_symbol_kind::EVENT
            )
        {
            return None;
        }

        // Strip `self` parameter when called via dot syntax `receiver.method(...)` on an instance!
        let drop_self = call_ctx.is_dot_call
            && sym.params.first().is_some_and(|p| p.is_self)
            && !call_ctx.receiver_looks_like_type;

        let visible_params: Vec<&ParamDoc> =
            sym.params.iter().skip(usize::from(drop_self)).collect();

        let active_param = if let Some(ref named) = call_ctx.active_named_arg {
            visible_params
                .iter()
                .position(|p| p.name.as_deref() == Some(named.as_str()))
                .unwrap_or(call_ctx.comma_index)
        } else {
            let max_idx = visible_params.len().saturating_sub(1);
            if visible_params.last().is_some_and(|p| p.is_varargs) {
                call_ctx.comma_index.min(max_idx)
            } else {
                call_ctx.comma_index
            }
        };

        let param_jsons: Vec<JsonValue> = visible_params
            .iter()
            .map(|p| JsonValue::object([("label", JsonValue::from(p.label.as_str()))]))
            .collect();

        let mut sig_fields = vec![
            ("label", JsonValue::from(sym.detail.as_str())),
            ("parameters", JsonValue::Array(param_jsons)),
            ("activeParameter", JsonValue::from(active_param as u32)),
        ];
        if let Some(ref doc) = sym.doc
            && !doc.is_empty()
        {
            sig_fields.push((
                "documentation",
                JsonValue::object([
                    ("kind", JsonValue::from("markdown")),
                    ("value", JsonValue::from(doc.as_str())),
                ]),
            ));
        }

        Some(JsonValue::object([
            (
                "signatures",
                JsonValue::Array(vec![JsonValue::object(sig_fields)]),
            ),
            ("activeSignature", JsonValue::from(0u32)),
            ("activeParameter", JsonValue::from(active_param as u32)),
        ]))
    }

    pub fn document_symbols(&mut self, file: &Path) -> Vec<JsonValue> {
        let norm = normalize_path(file);
        let Some((text, line_idx)) = self.file_text(&norm) else {
            return Vec::new();
        };
        let snap = self.ensure_snapshot(&norm).clone();
        let idx = if snap
            .index
            .file_roots
            .get(&norm)
            .is_some_and(|r| !r.is_empty())
        {
            &snap.index
        } else if let Some(good) = self.last_good_for(&norm)
            && good.file_roots.get(&norm).is_some_and(|r| !r.is_empty())
        {
            good
        } else {
            &snap.index
        };

        let Some(roots) = idx.file_roots.get(&norm) else {
            return Vec::new();
        };
        roots
            .iter()
            .filter_map(|&k| build_document_symbol_json(k, idx, &text, &line_idx))
            .collect()
    }

    pub fn workspace_symbols(&mut self, query: &str) -> Vec<JsonValue> {
        // Ensure open docs & workspace root packages have snapshots.
        let seeds: Vec<PathBuf> = self
            .open_docs
            .keys()
            .cloned()
            .chain(self.workspace_roots.iter().cloned())
            .collect();
        for seed in seeds {
            if seed.is_file() || seed.join("package.toml").is_file() {
                let _ = self.ensure_snapshot(&seed);
            }
        }

        let q_lower = query.to_ascii_lowercase();
        let mut seen = HashSet::new();
        let mut results = Vec::new();

        for snap in self.snapshots.values() {
            for sym in snap.index.symbols.values() {
                if sym.is_external
                    || sym.is_synthesized_module
                    || matches!(sym.key, SymbolKey::ImplBlock(_))
                    || matches!(sym.kind, lsp_symbol_kind::VARIABLE)
                {
                    continue;
                }
                if !q_lower.is_empty()
                    && !fuzzy_subsequence_match(&sym.name.to_ascii_lowercase(), &q_lower)
                {
                    continue;
                }
                let dedup_key = (sym.file.clone(), sym.selection_span, sym.name.clone());
                if !seen.insert(dedup_key) {
                    continue;
                }
                let Some(loc) = self.symbol_location_json(sym) else {
                    continue;
                };
                let mut fields = vec![
                    ("name", JsonValue::from(sym.name.as_str())),
                    ("kind", JsonValue::from(sym.kind)),
                    ("location", loc),
                ];
                if let Some(ref c) = sym.container_name {
                    fields.push(("containerName", JsonValue::from(c.as_str())));
                }
                results.push(JsonValue::object(fields));
            }
        }
        results.sort_by(|a, b| {
            a.get_str("name")
                .unwrap_or("")
                .cmp(b.get_str("name").unwrap_or(""))
        });
        results.truncate(250);
        results
    }

    pub fn semantic_tokens(&mut self, file: &Path) -> Vec<u32> {
        let norm = normalize_path(file);
        let Some((text, line_idx)) = self.file_text(&norm) else {
            return Vec::new();
        };
        let snap = self.ensure_snapshot(&norm).clone();
        let active_idx = if snap.index.file_occurrences.contains_key(&norm) {
            Some(&snap.index)
        } else {
            self.last_good_for(&norm)
        };
        build_semantic_tokens(&text, &line_idx, &norm, active_idx)
    }

    pub fn folding_ranges(&self, file: &Path) -> Vec<JsonValue> {
        let norm = normalize_path(file);
        let Some((text, line_idx)) = self.file_text(&norm) else {
            return Vec::new();
        };
        let (tokens, _) = lexer::tokenize(&text);
        let mut ranges = Vec::new();
        let mut brace_stack: Vec<(TokenKind, u32)> = Vec::new();

        for tok in &tokens {
            let lo = tok.from as usize;
            let hi = tok.to as usize;
            if hi <= lo || hi > text.len() {
                continue;
            }
            let start_line = line_idx.offset_to_position(&text, lo).line;
            let end_line = line_idx
                .offset_to_position(&text, hi.saturating_sub(1))
                .line;
            match tok.kind {
                TokenKind::Comment if end_line > start_line => {
                    ranges.push(JsonValue::object([
                        ("startLine", JsonValue::from(start_line)),
                        ("endLine", JsonValue::from(end_line)),
                        ("kind", JsonValue::from("comment")),
                    ]));
                }
                TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket => {
                    brace_stack.push((tok.kind, start_line));
                }
                TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket => {
                    let expected = match tok.kind {
                        TokenKind::RBrace => TokenKind::LBrace,
                        TokenKind::RParen => TokenKind::LParen,
                        _ => TokenKind::LBracket,
                    };
                    if let Some(pos) = brace_stack.iter().rposition(|(k, _)| *k == expected) {
                        let (_, open_line) = brace_stack.remove(pos);
                        if end_line > open_line {
                            ranges.push(JsonValue::object([
                                ("startLine", JsonValue::from(open_line)),
                                (
                                    "endLine",
                                    JsonValue::from(end_line.saturating_sub(1).max(open_line)),
                                ),
                                ("kind", JsonValue::from("region")),
                            ]));
                        }
                    }
                }
                _ => {}
            }
        }

        // Line-based groups: consecutive `--` comment banners, `use` blocks, and `--% .off` .. `--% .on`.
        let lines: Vec<&str> = text.lines().collect();
        let mut comment_run_start: Option<u32> = None;
        let mut import_run_start: Option<u32> = None;
        let mut fmt_off_start: Option<u32> = None;

        for (i, raw_line) in lines.iter().enumerate() {
            let line_no = i as u32;
            let trimmed = raw_line.trim();
            if let Some(dir) = trimmed.strip_prefix("--%") {
                let d = dir.trim();
                if d.starts_with(".off") && fmt_off_start.is_none() {
                    fmt_off_start = Some(line_no);
                } else if d.starts_with(".on")
                    && let Some(start) = fmt_off_start.take()
                    && line_no > start
                {
                    ranges.push(JsonValue::object([
                        ("startLine", JsonValue::from(start)),
                        ("endLine", JsonValue::from(line_no)),
                        ("kind", JsonValue::from("region")),
                    ]));
                }
            }

            let is_line_comment = trimmed.starts_with("--") && !trimmed.starts_with("--%");
            if is_line_comment {
                comment_run_start.get_or_insert(line_no);
            } else if let Some(start) = comment_run_start.take()
                && line_no.saturating_sub(1) > start
            {
                ranges.push(JsonValue::object([
                    ("startLine", JsonValue::from(start)),
                    ("endLine", JsonValue::from(line_no - 1)),
                    ("kind", JsonValue::from("comment")),
                ]));
            }

            let is_use_line = trimmed.starts_with("use ") || trimmed.starts_with("pub use ");
            if is_use_line {
                import_run_start.get_or_insert(line_no);
            } else if !trimmed.is_empty()
                && let Some(start) = import_run_start.take()
                && line_no.saturating_sub(1) > start
            {
                ranges.push(JsonValue::object([
                    ("startLine", JsonValue::from(start)),
                    ("endLine", JsonValue::from(line_no - 1)),
                    ("kind", JsonValue::from("imports")),
                ]));
            }
        }
        let last_line = lines.len().saturating_sub(1) as u32;
        if let Some(start) = comment_run_start
            && last_line > start
        {
            ranges.push(JsonValue::object([
                ("startLine", JsonValue::from(start)),
                ("endLine", JsonValue::from(last_line)),
                ("kind", JsonValue::from("comment")),
            ]));
        }
        if let Some(start) = import_run_start
            && last_line > start
        {
            ranges.push(JsonValue::object([
                ("startLine", JsonValue::from(start)),
                ("endLine", JsonValue::from(last_line)),
                ("kind", JsonValue::from("imports")),
            ]));
        }
        if let Some(start) = fmt_off_start
            && last_line > start
        {
            ranges.push(JsonValue::object([
                ("startLine", JsonValue::from(start)),
                ("endLine", JsonValue::from(last_line)),
                ("kind", JsonValue::from("region")),
            ]));
        }

        ranges
    }

    pub fn format_document(&self, file: &Path, tab_spaces: u32) -> Result<Vec<JsonValue>, String> {
        let Some(hook) = self.config.format_doc else {
            return Ok(Vec::new());
        };
        let norm = normalize_path(file);
        let Some((text, line_idx)) = self.file_text(&norm) else {
            return Ok(Vec::new());
        };
        let formatted = hook(&norm, &text, tab_spaces)?;
        if formatted == text {
            return Ok(Vec::new());
        }
        Ok(vec![JsonValue::object([
            ("range", line_idx.full_range(&text).to_json()),
            ("newText", JsonValue::from(formatted)),
        ])])
    }

    pub fn format_range(
        &self,
        file: &Path,
        range: LspRange,
        tab_spaces: u32,
    ) -> Result<Vec<JsonValue>, String> {
        let norm = normalize_path(file);
        let Some((text, line_idx)) = self.file_text(&norm) else {
            return Ok(Vec::new());
        };
        let lo = line_idx.position_to_offset(&text, range.start) as u32;
        let hi = (line_idx.position_to_offset(&text, range.end) as u32).max(lo);
        let formatted = if let Some(hook) = self.config.format_range {
            hook(&norm, &text, (lo, hi), tab_spaces)?
        } else if let Some(doc_hook) = self.config.format_doc {
            doc_hook(&norm, &text, tab_spaces)?
        } else {
            return Ok(Vec::new());
        };
        if formatted == text {
            return Ok(Vec::new());
        }
        Ok(vec![JsonValue::object([
            ("range", line_idx.full_range(&text).to_json()),
            ("newText", JsonValue::from(formatted)),
        ])])
    }

    pub fn code_actions(&mut self, file: &Path, range: LspRange) -> Vec<JsonValue> {
        let norm = normalize_path(file);
        let snap = self.ensure_snapshot(&norm).clone();
        let Some(diags) = snap.diagnostics.get(&norm) else {
            return Vec::new();
        };
        let uri = path_to_uri(&norm);
        let mut actions = Vec::new();
        for d in diags {
            if d.range.end < range.start || d.range.start > range.end {
                continue;
            }
            if let Some(suggestion) = extract_did_you_mean(&d.message) {
                let edit = JsonValue::object([(
                    "changes",
                    JsonValue::object([(
                        uri.as_str(),
                        JsonValue::Array(vec![JsonValue::object([
                            ("range", d.range.to_json()),
                            ("newText", JsonValue::from(suggestion.as_str())),
                        ])]),
                    )]),
                )]);
                actions.push(JsonValue::object([
                    (
                        "title",
                        JsonValue::from(format!("Replace with `{suggestion}`")),
                    ),
                    ("kind", JsonValue::from("quickfix")),
                    ("edit", edit),
                ]));
            }
        }
        actions
    }

    fn symbol_location_json(&self, sym: &SymbolInfo) -> Option<JsonValue> {
        self.span_location_json(&sym.file, sym.selection_span)
    }

    fn span_location_json(&self, file: &Path, span: (usize, usize)) -> Option<JsonValue> {
        let (ftext, flines) = self.file_text(file)?;
        let range = flines.span_to_range(&ftext, span.0, span.1);
        Some(JsonValue::object([
            ("uri", JsonValue::from(path_to_uri(file))),
            ("range", range.to_json()),
        ]))
    }
}

// =============================================================================
// Internal Helpers
// =============================================================================

pub fn normalize_path(path: &Path) -> PathBuf {
    if let Ok(canon) = fs::canonicalize(path) {
        return canon;
    }
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut out = PathBuf::new();
    for comp in abs.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if let Some(parent) = out.parent()
        && let Some(fname) = out.file_name()
        && let Ok(canon_parent) = fs::canonicalize(parent)
    {
        return canon_parent.join(fname);
    }
    out
}

fn package_sources_contains(pkg: &PackageSources, norm_file: &Path) -> bool {
    if normalize_path(&pkg.entry.file_path) == norm_file {
        return true;
    }
    fn module_contains(m: &ModuleSources, target: &Path) -> bool {
        normalize_path(&m.entry.file_path) == target
            || m.children.iter().any(|c| module_contains(c, target))
    }
    pkg.modules.iter().any(|m| module_contains(m, norm_file))
}

fn collect_package_file_paths(pkg: &PackageSources, out: &mut HashSet<PathBuf>) {
    out.insert(normalize_path(&pkg.entry.file_path));
    fn rec(m: &ModuleSources, out: &mut HashSet<PathBuf>) {
        out.insert(normalize_path(&m.entry.file_path));
        for c in &m.children {
            rec(c, out);
        }
    }
    for m in &pkg.modules {
        rec(m, out);
    }
}

fn materialize_stdlib_cache_file(rel_path: &str, content: &str) -> PathBuf {
    let base_dir = std::env::var_os("NESSA_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("nessa-lsp-stdlib");
    let full = base_dir.join(rel_path);
    if fs::read_to_string(&full).ok().as_deref() == Some(content) {
        return normalize_path(&full);
    }
    if let Some(parent) = full.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(meta) = fs::metadata(&full)
        && meta.permissions().readonly()
    {
        let mut perms = meta.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        let _ = fs::set_permissions(&full, perms);
    }
    if fs::write(&full, content).is_ok()
        && let Ok(meta) = fs::metadata(&full)
    {
        let mut perms = meta.permissions();
        perms.set_readonly(true);
        let _ = fs::set_permissions(&full, perms);
    }
    normalize_path(&full)
}

fn word_or_backtick_at(src: &str, offset: usize) -> Option<(String, (usize, usize))> {
    let bytes = src.as_bytes();
    let clamped = offset.min(bytes.len());
    // Check if inside a backtick identifier `` `...` `` on the same line.
    let line_start = src[..clamped].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let line_end = src[clamped..]
        .find('\n')
        .map(|i| clamped + i)
        .unwrap_or(bytes.len());
    let line = &src[line_start..line_end];
    let rel = clamped - line_start;
    let mut scan = 0usize;
    while let Some(open_rel) = line[scan..].find('`') {
        let open = scan + open_rel;
        if let Some(close_rel) = line[open + 1..].find('`') {
            let close = open + 1 + close_rel + 1;
            if open <= rel && rel <= close {
                let inner = &line[open + 1..close - 1];
                return Some((inner.to_owned(), (line_start + open, line_start + close)));
            }
            scan = close;
        } else {
            break;
        }
    }

    let is_id = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut start = clamped;
    if start == bytes.len() || !is_id(bytes[start]) {
        if start > 0 && is_id(bytes[start - 1]) {
            start -= 1;
        } else {
            return None;
        }
    }
    while start > 0 && is_id(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = start;
    while end < bytes.len() && is_id(bytes[end]) {
        end += 1;
    }
    (end > start).then(|| (src[start..end].to_owned(), (start, end)))
}

fn format_nessa_rename_ident(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("identifier cannot be empty".into());
    }
    if let Some(inner) = trimmed.strip_prefix('`').and_then(|s| s.strip_suffix('`')) {
        if inner.is_empty() || inner.contains('`') || inner.contains(['\n', '\r']) {
            return Err("invalid backtick identifier".into());
        }
        return Ok(trimmed.to_owned());
    }
    if is_valid_plain_nessa_ident(trimmed) {
        return Ok(trimmed.to_owned());
    }
    if trimmed.contains('`')
        || trimmed.contains(['\n', '\r'])
        || trimmed.chars().any(char::is_control)
    {
        return Err(format!("invalid Nessa identifier `{raw}`"));
    }
    Ok(format!("`{trimmed}`"))
}

fn is_valid_plain_nessa_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return false;
    }
    lexer::token::lookup_keyword(name).is_none()
}

fn complete_fmt_directives(after_prefix: &str) -> Vec<JsonValue> {
    let trimmed = after_prefix.trim_start();
    let has_leading_dot = trimmed.starts_with('.');
    let partial = if has_leading_dot { trimmed } else { "" };

    nessa_fmt::DIRECTIVE_SPECS
        .iter()
        .filter(|spec| partial.is_empty() || spec.name.starts_with(partial))
        .enumerate()
        .map(|(idx, spec)| {
            let insert_full = match spec.name {
                ".max_width" => ".max_width 100",
                ".tab_spaces" => ".tab_spaces 4",
                ".newline_style" => ".newline_style .lf",
                ".use_small_heuristics" => ".use_small_heuristics .default",
                ".reorder_imports" => ".reorder_imports true",
                ".normalize_comments" => ".normalize_comments true",
                other => other,
            };
            let insert_text = if has_leading_dot {
                insert_full.strip_prefix('.').unwrap_or(insert_full)
            } else {
                insert_full
            };
            let detail = match spec.value_hint {
                Some(hint) => format!("{} — {}", hint, spec.description),
                None => spec.description.to_owned(),
            };
            JsonValue::object([
                ("label", JsonValue::from(spec.name)),
                ("insertText", JsonValue::from(insert_text)),
                (
                    "filterText",
                    JsonValue::from(spec.name.strip_prefix('.').unwrap_or(spec.name)),
                ),
                ("kind", JsonValue::from(lsp_completion_kind::PROPERTY)),
                ("detail", JsonValue::from(detail)),
                ("sortText", JsonValue::from(format!("0_{idx:02}"))),
            ])
        })
        .collect()
}

fn quote_view_context(line_prefix: &str) -> Option<&str> {
    let ident_len = line_prefix
        .bytes()
        .rev()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    let partial = &line_prefix[line_prefix.len() - ident_len..];
    let before = &line_prefix[..line_prefix.len() - ident_len];
    let before_quote = before.strip_suffix('\'')?;
    // Ignore char literals like `'a` when preceded by space / `=` / `(`.
    let prev = before_quote.bytes().last()?;
    if prev.is_ascii_alphanumeric() || matches!(prev, b'_' | b')' | b']' | b'}' | b'`') {
        Some(partial)
    } else {
        None
    }
}

fn complete_views(
    partial: &str,
    allow_builtin: bool,
    index: &SemanticIndex,
    file: &Path,
    offset: usize,
) -> Vec<JsonValue> {
    let mut items = Vec::new();
    if allow_builtin && "builtin".starts_with(partial) {
        items.push(JsonValue::object([
            ("label", JsonValue::from("builtin")),
            ("kind", JsonValue::from(lsp_completion_kind::KEYWORD)),
            (
                "detail",
                JsonValue::from("Engine builtin / FFI symbol view"),
            ),
            ("sortText", JsonValue::from("0_builtin")),
        ]));
    }
    if "type".starts_with(partial) {
        items.push(JsonValue::object([
            ("label", JsonValue::from("type")),
            ("kind", JsonValue::from(lsp_completion_kind::KEYWORD)),
            ("detail", JsonValue::from("First-class runtime Type view")),
            ("sortText", JsonValue::from("0_type")),
        ]));
    }
    for (name, key) in index.visible_bindings_at(file, offset) {
        if name.starts_with(partial)
            && let Some(sym) = index.symbol(key)
            && matches!(
                sym.kind,
                lsp_symbol_kind::FUNCTION | lsp_symbol_kind::INTERFACE
            )
        {
            items.push(symbol_to_completion_item(sym, "1"));
        }
    }
    items
}

fn at_package_context(line_prefix: &str) -> Option<&str> {
    let ident_len = line_prefix
        .bytes()
        .rev()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    let partial = &line_prefix[line_prefix.len() - ident_len..];
    let before = &line_prefix[..line_prefix.len() - ident_len];
    before.strip_suffix('@').map(|_| partial)
}

fn complete_at_package<'a>(
    snap: &'a PackageSnapshot,
    last_good: Option<&'a SemanticIndex>,
    partial: &str,
) -> Vec<JsonValue> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for idx in [Some(&snap.index), last_good].into_iter().flatten() {
        for (mname, key) in &idx.package_modules {
            if mname.starts_with(partial)
                && seen.insert(mname.clone())
                && let Some(sym) = idx.symbol(*key)
            {
                out.push(symbol_to_completion_item(sym, "0"));
            }
        }
    }
    for dep in &snap.dependency_names {
        if dep.starts_with(partial) && seen.insert(dep.clone()) {
            out.push(JsonValue::object([
                ("label", JsonValue::from(dep.as_str())),
                ("kind", JsonValue::from(lsp_completion_kind::MODULE)),
                ("detail", JsonValue::from("package dependency")),
                ("sortText", JsonValue::from(format!("1_{dep}"))),
            ]));
        }
    }
    out
}

fn bare_use_context(trimmed_line: &str) -> Option<&str> {
    let rest = trimmed_line
        .strip_prefix("pub use ")
        .or_else(|| trimmed_line.strip_prefix("use "))?;
    let trimmed = rest.trim_start();
    if trimmed.contains(['.', '@', '{']) {
        return None;
    }
    Some(trimmed)
}

fn complete_use_roots(snap: &PackageSnapshot, partial: &str) -> Vec<JsonValue> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    if "std".starts_with(partial) && seen.insert("std".to_owned()) {
        out.push(JsonValue::object([
            ("label", JsonValue::from("std")),
            ("kind", JsonValue::from(lsp_completion_kind::MODULE)),
            ("detail", JsonValue::from("Nessa standard library")),
            ("sortText", JsonValue::from("0_std")),
        ]));
    }
    for (mname, key) in &snap.index.package_modules {
        if mname.starts_with(partial)
            && seen.insert(mname.clone())
            && let Some(sym) = snap.index.symbol(*key)
        {
            out.push(symbol_to_completion_item(sym, "0"));
        }
    }
    for dep in &snap.dependency_names {
        if dep.starts_with(partial) && seen.insert(dep.clone()) {
            out.push(JsonValue::object([
                ("label", JsonValue::from(dep.as_str())),
                ("kind", JsonValue::from(lsp_completion_kind::MODULE)),
                ("detail", JsonValue::from("package dependency")),
                ("sortText", JsonValue::from(format!("1_{dep}"))),
            ]));
        }
    }
    out
}

fn dot_completion_offset(line_prefix: &str) -> Option<(usize, String)> {
    let ident_len = line_prefix
        .bytes()
        .rev()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    let partial = &line_prefix[line_prefix.len() - ident_len..];
    let before_partial = &line_prefix[..line_prefix.len() - ident_len];
    let before_dot = before_partial.strip_suffix('.')?;
    // Ignore `..` range operator!
    if before_dot.ends_with('.') {
        return None;
    }
    Some((before_dot.len(), partial.to_owned()))
}

fn build_dot_items_from_projection(
    index: &SemanticIndex,
    receiver_ty: Option<TypeIndex>,
    receiver_sym: Option<SymbolKey>,
    is_type_or_module: bool,
    partial: &str,
) -> Vec<JsonValue> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    let sym_ty = receiver_sym
        .and_then(|k| index.symbol(k))
        .and_then(|s| s.type_index);
    let ty = receiver_ty.or(sym_ty);

    if let Some(raw_ty) = ty {
        // Resolve canonical type_members entry (either exact `raw_ty` or matching canonical `type_members`).
        if let Some(members) = find_members_for_type(index, raw_ty) {
            for m in members {
                if m.is_static != is_type_or_module {
                    continue;
                }
                if m.name.starts_with(partial) && seen.insert(m.name.clone()) {
                    out.push(member_to_completion_item(m, "0"));
                }
            }
        }

        if !is_type_or_module {
            // Tuple `.0`, `.1` completions
            if let Some(elems) = index.tuple_elements.get(&raw_ty) {
                for (i, (_, el_ty_name)) in elems.iter().enumerate() {
                    let label = i.to_string();
                    if label.starts_with(partial) && seen.insert(label.clone()) {
                        out.push(JsonValue::object([
                            ("label", JsonValue::from(label.as_str())),
                            ("kind", JsonValue::from(lsp_completion_kind::FIELD)),
                            ("detail", JsonValue::from(el_ty_name.as_str())),
                            ("sortText", JsonValue::from(format!("0_{i:02}"))),
                        ]));
                    }
                }
            }
            // `?T` / `!E T` `.unwrap()`
            if let Some((_, inner_str)) = index
                .optional_inners
                .get(&raw_ty)
                .or_else(|| index.error_inners.get(&raw_ty))
                && "unwrap".starts_with(partial)
                && seen.insert("unwrap".to_owned())
            {
                out.push(JsonValue::object([
                    ("label", JsonValue::from("unwrap")),
                    ("kind", JsonValue::from(lsp_completion_kind::METHOD)),
                    (
                        "detail",
                        JsonValue::from(format!("fn unwrap(self) -> {inner_str}")),
                    ),
                    ("sortText", JsonValue::from("0_unwrap")),
                ]));
            }
            // `Future(T)` `.await`
            if let Some((_, inner_str)) = index.future_inners.get(&raw_ty)
                && "await".starts_with(partial)
                && seen.insert("await".to_owned())
            {
                out.push(JsonValue::object([
                    ("label", JsonValue::from("await")),
                    ("kind", JsonValue::from(lsp_completion_kind::KEYWORD)),
                    ("detail", JsonValue::from(inner_str.as_str())),
                    ("sortText", JsonValue::from("0_await")),
                ]));
            }
            // `Continuation` `.clone()`
            if raw_ty == Intrinsic::Continuation.type_index()
                && "clone".starts_with(partial)
                && seen.insert("clone".to_owned())
            {
                out.push(JsonValue::object([
                    ("label", JsonValue::from("clone")),
                    ("kind", JsonValue::from(lsp_completion_kind::METHOD)),
                    ("detail", JsonValue::from("fn clone(self) -> Continuation")),
                    ("sortText", JsonValue::from("0_clone")),
                ]));
            }
        }
    }

    // Special-case `std.` if `std_module_sym` was matched by name.
    if out.is_empty()
        && let Some(skey) = receiver_sym
        && Some(skey) == index.std_module_sym
        && let Some(std_sym) = index.symbol(skey)
        && let Some(sty) = std_sym.type_index
        && let Some(members) = find_members_for_type(index, sty)
    {
        for m in members {
            if m.name.starts_with(partial) && seen.insert(m.name.clone()) {
                out.push(member_to_completion_item(m, "0"));
            }
        }
    }

    out
}

fn find_members_for_type(index: &SemanticIndex, ty: TypeIndex) -> Option<&[MemberEntry]> {
    if let Some(m) = index.type_members.get(&ty) {
        return Some(m.as_slice());
    }
    let tname = index.type_names.get(&ty)?;
    for (&cand_ty, members) in &index.type_members {
        if index.type_names.get(&cand_ty) == Some(tname) {
            return Some(members.as_slice());
        }
    }
    None
}

fn resolve_chain_dot_items(
    index: &SemanticIndex,
    file: &Path,
    offset: usize,
    chain: &str,
    partial: &str,
) -> Vec<JsonValue> {
    let clean_chain = chain.strip_prefix('@').unwrap_or(chain);
    let segments: Vec<&str> = clean_chain.split('.').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return Vec::new();
    }

    // Special check for `std` root
    let mut cur_sym: Option<&SymbolInfo> = if segments[0] == "std" {
        index.std_module_sym.and_then(|k| index.symbol(k))
    } else {
        index.lookup_named_symbol_in_file(file, segments[0], offset)
    };

    let Some(first_sym) = cur_sym else {
        return Vec::new();
    };
    let mut is_static = matches!(
        first_sym.kind,
        lsp_symbol_kind::MODULE
            | lsp_symbol_kind::STRUCT
            | lsp_symbol_kind::ENUM
            | lsp_symbol_kind::INTERFACE
            | lsp_symbol_kind::CLASS
    );
    let mut cur_ty = first_sym.return_type.or(first_sym.type_index);

    for &seg in &segments[1..] {
        let seg_name = seg.split('(').next().unwrap_or(seg);
        let Some(ty) = cur_ty else {
            return Vec::new();
        };
        let Some(members) = find_members_for_type(index, ty) else {
            return Vec::new();
        };
        let Some(found) = members
            .iter()
            .find(|m| m.is_static == is_static && m.name == seg_name)
        else {
            return Vec::new();
        };
        cur_sym = found.key.and_then(|k| index.symbol(k));
        is_static = matches!(
            found.kind,
            lsp_symbol_kind::MODULE
                | lsp_symbol_kind::STRUCT
                | lsp_symbol_kind::ENUM
                | lsp_symbol_kind::INTERFACE
                | lsp_symbol_kind::CLASS
        );
        cur_ty = found.result_type;
    }

    build_dot_items_from_projection(index, cur_ty, cur_sym.map(|s| s.key), is_static, partial)
}

fn resolve_chain_member_symbol<'a>(
    index: &'a SemanticIndex,
    file: &Path,
    offset: usize,
    recv_chain: &str,
    member_name: &str,
) -> Option<&'a SymbolInfo> {
    let segments: Vec<&str> = recv_chain.split('.').filter(|s| !s.is_empty()).collect();
    let first = *segments.first()?;
    let first_sym = if first == "std" {
        index.std_module_sym.and_then(|k| index.symbol(k))?
    } else {
        index.lookup_named_symbol_in_file(file, first, offset)?
    };
    let mut is_static = matches!(
        first_sym.kind,
        lsp_symbol_kind::MODULE
            | lsp_symbol_kind::STRUCT
            | lsp_symbol_kind::ENUM
            | lsp_symbol_kind::INTERFACE
            | lsp_symbol_kind::CLASS
    );
    let mut cur_ty = first_sym.return_type.or(first_sym.type_index)?;
    for &seg in &segments[1..] {
        let members = find_members_for_type(index, cur_ty)?;
        let found = members
            .iter()
            .find(|m| m.is_static == is_static && m.name == seg)?;
        is_static = found.kind == lsp_symbol_kind::MODULE;
        cur_ty = found.result_type?;
    }
    let members = find_members_for_type(index, cur_ty)?;
    let target = members
        .iter()
        .find(|m| m.is_static == is_static && m.name == member_name)?;
    index.symbol(target.key?)
}

fn extract_trailing_chain(s: &str) -> String {
    let trimmed = s.trim_end();
    let len = trimmed
        .bytes()
        .rev()
        .take_while(|b| b.is_ascii_alphanumeric() || matches!(*b, b'_' | b'.' | b'@'))
        .count();
    trimmed[trimmed.len() - len..].trim_matches('.').to_owned()
}

fn identifier_prefix(line_prefix: &str) -> &str {
    let len = line_prefix
        .bytes()
        .rev()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
        .count();
    &line_prefix[line_prefix.len() - len..]
}

fn symbol_to_completion_item(sym: &SymbolInfo, priority_group: &str) -> JsonValue {
    let mut fields = vec![
        ("label", JsonValue::from(sym.name.as_str())),
        ("kind", JsonValue::from(sym.completion_kind)),
        ("detail", JsonValue::from(sym.detail.as_str())),
        (
            "sortText",
            JsonValue::from(format!("{priority_group}_{}", sym.name)),
        ),
    ];
    if let Some(ref doc) = sym.doc
        && !doc.is_empty()
    {
        fields.push((
            "documentation",
            JsonValue::object([
                ("kind", JsonValue::from("markdown")),
                ("value", JsonValue::from(doc.as_str())),
            ]),
        ));
    }
    JsonValue::object(fields)
}

fn member_to_completion_item(m: &MemberEntry, priority_group: &str) -> JsonValue {
    let mut fields = vec![
        ("label", JsonValue::from(m.name.as_str())),
        ("kind", JsonValue::from(m.completion_kind)),
        ("detail", JsonValue::from(m.detail.as_str())),
        (
            "sortText",
            JsonValue::from(format!("{priority_group}_{}", m.name)),
        ),
    ];
    if let Some(ref doc) = m.doc
        && !doc.is_empty()
    {
        fields.push((
            "documentation",
            JsonValue::object([
                ("kind", JsonValue::from("markdown")),
                ("value", JsonValue::from(doc.as_str())),
            ]),
        ));
    }
    JsonValue::object(fields)
}

struct CallContext {
    callee_ident_span: (usize, usize),
    comma_index: usize,
    active_named_arg: Option<String>,
    is_dot_call: bool,
    receiver_looks_like_type: bool,
    receiver_chain: Option<String>,
}

fn enclosing_call_context(src: &str, offset: usize) -> Option<CallContext> {
    let prefix = &src[..offset.min(src.len())];
    let (raw_tokens, _) = lexer::tokenize(prefix);
    let tokens: Vec<_> = raw_tokens
        .into_iter()
        .filter(|t| {
            !matches!(
                t.kind,
                TokenKind::Sof
                    | TokenKind::Eof
                    | TokenKind::Newline
                    | TokenKind::Indent
                    | TokenKind::Outdent
                    | TokenKind::Comment
            )
        })
        .collect();

    // Stack of `(lparen_idx_in_tokens, comma_count, current_arg_start_tok_idx, inner_brackets, inner_braces)`
    let mut stack: Vec<(usize, usize, usize, usize, usize)> = Vec::new();

    for (idx, tok) in tokens.iter().enumerate() {
        match tok.kind {
            TokenKind::LParen => {
                stack.push((idx, 0, idx + 1, 0, 0));
            }
            TokenKind::RParen => {
                stack.pop();
            }
            TokenKind::LBracket => {
                if let Some(top) = stack.last_mut() {
                    top.3 += 1;
                }
            }
            TokenKind::RBracket => {
                if let Some(top) = stack.last_mut() {
                    top.3 = top.3.saturating_sub(1);
                }
            }
            TokenKind::LBrace => {
                if let Some(top) = stack.last_mut() {
                    top.4 += 1;
                }
            }
            TokenKind::RBrace => {
                if let Some(top) = stack.last_mut() {
                    top.4 = top.4.saturating_sub(1);
                }
            }
            TokenKind::Comma => {
                if let Some(top) = stack.last_mut()
                    && top.3 == 0
                    && top.4 == 0
                {
                    top.1 += 1;
                    top.2 = idx + 1;
                }
            }
            _ => {}
        }
    }

    let (lparen_idx, comma_index, arg_start_idx, _, _) = *stack.last()?;
    let callee_tok_idx = lparen_idx.checked_sub(1)?;
    let callee_tok = tokens[callee_tok_idx];
    if !matches!(callee_tok.kind, TokenKind::Id | TokenKind::ArbitraryId) {
        return None;
    }
    let callee_ident_span = (callee_tok.from as usize, callee_tok.to as usize);

    // Check if preceded by `.` (`receiver.method(...)`)
    let is_dot_call = callee_tok_idx >= 1 && tokens[callee_tok_idx - 1].kind == TokenKind::Dot;
    let mut receiver_looks_like_type = false;
    let mut receiver_chain = None;
    if is_dot_call && callee_tok_idx >= 2 {
        let recv_tok = tokens[callee_tok_idx - 2];
        let recv_slice = &prefix[recv_tok.from as usize..recv_tok.to as usize];
        receiver_looks_like_type = recv_slice
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase());
        receiver_chain = Some(extract_trailing_chain(
            &prefix[..tokens[callee_tok_idx - 1].from as usize],
        ));
    }

    // Detect named argument `param = ...` (or `.param = ...`) at the start of the active argument!
    let mut active_named_arg = None;
    let arg_tokens = &tokens[arg_start_idx..];
    if arg_tokens.len() >= 2
        && matches!(arg_tokens[0].kind, TokenKind::Id | TokenKind::ArbitraryId)
        && arg_tokens[1].kind == TokenKind::Eq
    {
        let raw_arg_name = &prefix[arg_tokens[0].from as usize..arg_tokens[0].to as usize];
        active_named_arg = Some(raw_arg_name.trim_matches('`').to_owned());
    } else if arg_tokens.len() >= 3
        && arg_tokens[0].kind == TokenKind::Dot
        && matches!(arg_tokens[1].kind, TokenKind::Id | TokenKind::ArbitraryId)
        && arg_tokens[2].kind == TokenKind::Eq
    {
        let raw_arg_name = &prefix[arg_tokens[1].from as usize..arg_tokens[1].to as usize];
        active_named_arg = Some(raw_arg_name.trim_matches('`').to_owned());
    }

    Some(CallContext {
        callee_ident_span,
        comma_index,
        active_named_arg,
        is_dot_call,
        receiver_looks_like_type,
        receiver_chain,
    })
}

fn build_document_symbol_json(
    key: SymbolKey,
    index: &SemanticIndex,
    text: &str,
    line_idx: &LineIndex,
) -> Option<JsonValue> {
    let sym = index.symbol(key)?;
    let full_range = line_idx.span_to_range(text, sym.full_span.0, sym.full_span.1);
    let sel_lo = sym
        .selection_span
        .0
        .max(sym.full_span.0)
        .min(sym.full_span.1);
    let sel_hi = sym.selection_span.1.max(sel_lo).min(sym.full_span.1);
    let sel_range = line_idx.span_to_range(text, sel_lo, sel_hi);

    let children: Vec<JsonValue> = sym
        .children
        .iter()
        .filter_map(|&c| {
            let csym = index.symbol(c)?;
            if csym.file != sym.file {
                return None;
            }
            build_document_symbol_json(c, index, text, line_idx)
        })
        .collect();

    let mut fields = vec![
        ("name", JsonValue::from(sym.name.as_str())),
        ("detail", JsonValue::from(sym.detail.as_str())),
        ("kind", JsonValue::from(sym.kind)),
        ("range", full_range.to_json()),
        ("selectionRange", sel_range.to_json()),
    ];
    if !children.is_empty() {
        fields.push(("children", JsonValue::Array(children)));
    }
    Some(JsonValue::object(fields))
}

fn fuzzy_subsequence_match(haystack: &str, needle: &str) -> bool {
    if haystack.contains(needle) {
        return true;
    }
    let mut h_chars = haystack.chars();
    for nc in needle.chars() {
        if !h_chars.any(|hc| hc == nc) {
            return false;
        }
    }
    true
}

fn extract_did_you_mean(msg: &str) -> Option<String> {
    let marker = "did you mean `";
    let start = msg.find(marker)? + marker.len();
    let end = msg[start..].find('`')? + start;
    let cand = &msg[start..end];
    (!cand.is_empty()).then(|| cand.to_owned())
}

pub const NESSA_KEYWORDS: &[&str] = &[
    "and",
    "as",
    "assoc",
    "async",
    "atomic",
    "await",
    "break",
    "case",
    "catch",
    "const",
    "continue",
    "defer",
    "derive",
    "do",
    "effect",
    "else",
    "enum",
    "error",
    "extend",
    "extern",
    "false",
    "fn",
    "for",
    "global",
    "handles",
    "if",
    "impl",
    "in",
    "is",
    "itself",
    "lambda",
    "let",
    "match",
    "matches",
    "mod",
    "newtype",
    "not",
    "null",
    "or",
    "private",
    "pub",
    "resume",
    "return",
    "self",
    "Self",
    "static",
    "struct",
    "test",
    "trait",
    "true",
    "typealias",
    "use",
    "var",
    "when",
    "where",
    "while",
];
