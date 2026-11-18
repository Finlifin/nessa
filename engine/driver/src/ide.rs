//! Silent, fault-tolerant front-end + semantic resolution entry points for IDE / LSP tooling.
//!
//! Unlike full compilation (`Driver::compile*`), IDE checking:
//! 1. Never writes diagnostics to `stderr` (`DiagnosticContext::new_silent`).
//! 2. Continues parsing with statement-level error recovery even when lexical or syntax errors
//!    exist in one or more source files, and still runs semantic resolution across the file,
//!    multi-file package, dependencies, bundled packages, and embedded standard library.
//! 3. Retains exact `CheckedSourceUnit` coordinate mappings (`SourceMapping`, `Send + Sync`) so
//!    normalized `rustc_span::Span`s map back to `(file_path, RawByteRange)` and raw byte offsets
//!    map forward to normalized `Span`s.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use ast::{Ast, AstStorageLimits, Node, NodeIndex, NodeKind};
use diagnostic::{Diagnostic, DiagnosticContext, Level, NessaError};
use pkg_manager::PackageSources;
use resolution::{ResolveOptions, ResolvedAst};
use rustc_span::{BytePos, DUMMY_SP, FileName, Span};
use source_span::{RawByteRange, SourceMapping};

use crate::package::{binding_name, source_units, unwrap_visibility};
use crate::package_graph::select_sources;
use crate::standard_library::{MODULES, STD_ROOT_SOURCE, mark_ffi_permissions, mark_trusted};
use crate::{CompilationIdentityContext, Driver};

/// Embedded standard library root (`std/mod.ns`) and submodules (`(module_name, source)`).
pub fn standard_library_sources() -> (&'static str, &'static [(&'static str, &'static str)]) {
    (STD_ROOT_SOURCE, MODULES)
}

/// Metadata and bidirectional byte-offset mapping for one parsed source unit in an IDE check.
#[derive(Clone, Debug)]
pub struct CheckedSourceUnit {
    /// Physical path for user/package files, or virtual `<std>/...` / bundled path.
    pub path: PathBuf,
    /// Stable logical name such as `"std/io.ns"`, `"regex/src/lib.ns"`, or path display.
    pub logical_name: String,
    /// True when this unit belongs to the embedded `std` package.
    pub is_std: bool,
    /// True when this unit belongs to an embedded bundled package (e.g. `regex`).
    pub is_bundled: bool,
    /// Normalized `SourceMap` start position (inclusive).
    pub start_pos: BytePos,
    /// Normalized `SourceMap` end position (inclusive for empty/EOF zero-width spans).
    pub end_pos: BytePos,
    /// Thread-safe coordinate mapping between original UTF-8 bytes and normalized `Span`s.
    pub mapping: SourceMapping,
    /// Exact original UTF-8 source text that was lexed and parsed.
    pub source: String,
}

impl CheckedSourceUnit {
    /// Check whether a non-dummy normalized `Span` falls within this source unit.
    pub fn contains_span(&self, span: Span) -> bool {
        !span.is_dummy()
            && span.lo() >= self.start_pos
            && span.hi() <= self.end_pos
            && span.lo() <= span.hi()
    }

    /// Convert a normalized `Span` inside this unit back to original UTF-8 byte offsets.
    pub fn original_range(&self, span: Span) -> Option<RawByteRange> {
        if !self.contains_span(span) {
            return None;
        }
        let range = self.mapping.original_range(span).ok()?;
        self.source
            .get(range.start as usize..range.end as usize)
            .map(|_| range)
    }

    /// Convert an original UTF-8 byte range inside `self.source` to a normalized `Span`.
    pub fn span_for_raw_range(&self, raw: RawByteRange) -> Option<Span> {
        self.source.get(raw.start as usize..raw.end as usize)?;
        self.mapping.map_range(raw).ok().map(|loc| loc.span)
    }
}

/// Complete output of a silent, fault-tolerant IDE check pass.
pub struct IdeCheckResult {
    /// Semantic resolution output (`Some` whenever AST assembly succeeded, even if errors exist).
    pub resolved: Option<ResolvedAst>,
    /// All lexical, syntax, package, and semantic diagnostics collected silently in order.
    pub diagnostics: Vec<Diagnostic>,
    /// All real, bundled, and `std` source units sorted by `start_pos`.
    pub units: Vec<CheckedSourceUnit>,
    /// True if `diagnostics` contains at least one `Level::Error`.
    pub has_errors: bool,
}

impl IdeCheckResult {
    fn failed(diagnostics: Vec<Diagnostic>, mut units: Vec<CheckedSourceUnit>) -> Self {
        units.sort_by_key(|u| (u.start_pos, u.end_pos));
        Self {
            resolved: None,
            diagnostics,
            units,
            has_errors: true,
        }
    }

    /// Locate the owning [`CheckedSourceUnit`] and exact original UTF-8 [`RawByteRange`] for `span`.
    pub fn locate_span(&self, span: Span) -> Option<(&CheckedSourceUnit, RawByteRange)> {
        if span.is_dummy() {
            return None;
        }
        let idx = self.units.partition_point(|u| u.start_pos <= span.lo());
        for cand_idx in [idx.checked_sub(1), Some(idx)].into_iter().flatten() {
            if let Some(unit) = self.units.get(cand_idx)
                && let Some(range) = unit.original_range(span)
            {
                return Some((unit, range));
            }
        }
        None
    }

    /// Find the [`CheckedSourceUnit`] matching `path`.
    pub fn unit_for_path(&self, path: &Path) -> Option<&CheckedSourceUnit> {
        self.units.iter().find(|u| u.path == path)
    }
}

impl Driver {
    /// Silently lex, parse (with error recovery), load `std` + bundled script packages,
    /// and resolve a standalone `.ns` source buffer for IDE / LSP consumers.
    pub fn check_source_ide(&self, path: &Path, source: &str) -> IdeCheckResult {
        let mut units = Vec::new();
        let mut diagnostics = Vec::new();

        let ast = match self.parse_unit_ide(
            path.to_path_buf(),
            path.to_string_lossy().into_owned(),
            source,
            false,
            false,
            &mut units,
            &mut diagnostics,
        ) {
            Ok(ast) => ast,
            Err(errors) => {
                diagnostics.extend(errors);
                return IdeCheckResult::failed(diagnostics, units);
            }
        };

        let needs_bundled_script_packages = {
            let bundled_names: std::collections::HashSet<str_interner::StrId> =
                pkg_manager::bundled_packages()
                    .iter()
                    .map(|pkg| str_interner::intern(&pkg.manifest.manifest().name))
                    .collect();
            ast.nodes
                .iter()
                .any(|node| node.kind == NodeKind::Id && bundled_names.contains(&node.str_id))
        };

        let (mut ast, mut options) =
            match self.load_standard_library_ide(ast, &mut units, &mut diagnostics) {
                Ok(loaded) => loaded,
                Err(errors) => {
                    diagnostics.extend(errors);
                    return IdeCheckResult::failed(diagnostics, units);
                }
            };

        let standard = options.package_roots.iter().copied().next();
        if needs_bundled_script_packages
            && let Err(errors) = self.attach_bundled_script_packages_ide(
                &mut ast,
                &mut options,
                standard,
                &mut units,
                &mut diagnostics,
            )
        {
            diagnostics.extend(errors);
            return IdeCheckResult::failed(diagnostics, units);
        }

        let diag_ctx = DiagnosticContext::new_silent(&self.source_map);
        let resolved = resolution::resolve_with_options(ast, &diag_ctx, options);
        diagnostics.extend(diag_ctx.diagnostics().iter().cloned());
        diagnostics.extend(resolved.diagnostics.iter().cloned());
        units.sort_by_key(|u| (u.start_pos, u.end_pos));
        let has_errors = diagnostics.iter().any(|d| d.level == Level::Error);

        IdeCheckResult {
            resolved: Some(resolved),
            diagnostics,
            units,
            has_errors,
        }
    }

    /// Silently load a disk package workspace and run fault-tolerant IDE checking.
    pub fn check_package_ide(&self, root: &Path) -> IdeCheckResult {
        match pkg_manager::load_package_workspace(root) {
            Ok(workspace) => {
                self.check_package_sources_ide(&workspace.root, &workspace.dependencies)
            }
            Err(error) => IdeCheckResult::failed(self.silent_errors(error.to_string()), Vec::new()),
        }
    }

    /// Silently resolve a caller-owned `PackageSources` root + dependency catalog for IDE / LSP
    /// consumers (supporting unsaved in-memory buffer overlays on `SourceFile.source`).
    pub fn check_package_sources_ide(
        &self,
        root: &PackageSources,
        catalog: &[PackageSources],
    ) -> IdeCheckResult {
        self.check_package_sources_with_privileged_ide(root, catalog, &BTreeSet::new())
    }

    /// Silently resolve a caller-owned `PackageSources` root + dependency catalog with optional
    /// privileged host-bridge package names (`domain/name`).
    pub fn check_package_sources_with_privileged_ide(
        &self,
        root: &PackageSources,
        catalog: &[PackageSources],
        privileged_packages: &BTreeSet<String>,
    ) -> IdeCheckResult {
        let mut units = Vec::new();
        let mut diagnostics = Vec::new();

        let (graph, sources) = match select_sources(root, catalog) {
            Ok(selected) => selected,
            Err(error) => return IdeCheckResult::failed(self.silent_errors(error), units),
        };
        let identity = match CompilationIdentityContext::from_graph(&graph) {
            Ok(identity) => identity,
            Err(error) => return IdeCheckResult::failed(self.silent_errors(error), units),
        };

        let mut ast = match self.package_ast_ide(root, false, &mut units, &mut diagnostics) {
            Ok(ast) => ast,
            Err(errors) => {
                diagnostics.extend(errors);
                return IdeCheckResult::failed(diagnostics, units);
            }
        };

        let mut pkg_options = ResolveOptions::default();
        if privileged_packages.contains(&root.manifest.manifest().qualified_name()) {
            mark_trusted(&ast, ast.root, &mut pkg_options.privileged_nodes);
        }
        mark_ffi_permissions(&ast, ast.root, root.manifest.ffi(), &mut pkg_options);

        let mut nodes = BTreeMap::from([(root.manifest.manifest().qualified_name(), ast.root)]);
        let mut dep_asts = Vec::new();
        for source in &sources {
            if std::ptr::eq(*source, root) {
                continue;
            }
            let is_bundled = pkg_manager::is_bundled_package_root(&source.root);
            let dep_ast =
                match self.package_ast_ide(source, is_bundled, &mut units, &mut diagnostics) {
                    Ok(parsed) => parsed,
                    Err(errors) => {
                        diagnostics.extend(errors);
                        return IdeCheckResult::failed(diagnostics, units);
                    }
                };
            dep_asts.push(dep_ast);
        }

        let bodies = match self.append_source_units_silent(&mut ast, dep_asts) {
            Ok(bodies) => bodies,
            Err(errors) => {
                diagnostics.extend(errors);
                return IdeCheckResult::failed(diagnostics, units);
            }
        };

        let mut members = ast.multi_children(ast.root).to_vec();
        for (source, body) in sources
            .iter()
            .filter(|source| !std::ptr::eq(**source, root))
            .zip(bodies)
        {
            let manifest = source.manifest.manifest();
            let built = (|| -> Result<NodeIndex, Vec<Diagnostic>> {
                let prelude =
                    self.parse_synthetic_silent("use std.prelude.*", "implicit-package-prelude")?;
                let prelude = self.append_source_units_silent(&mut ast, vec![prelude])?[0];
                let imports = ast.multi_children(prelude).to_vec();
                pkg_options.implicit_imports.extend(imports.iter().copied());
                let mut items = imports;
                items.extend_from_slice(ast.multi_children(body));
                let name = self.package_node_silent(
                    &mut ast,
                    NodeKind::Id,
                    DUMMY_SP,
                    str_interner::intern(&manifest.name),
                    NodeIndex::NULL,
                    &[],
                )?;
                let span = ast.node(body).span;
                self.package_node_silent(
                    &mut ast,
                    NodeKind::ModuleDef,
                    span,
                    str_interner::StrId::from_raw(0),
                    name,
                    &items,
                )
            })();
            let node = match built {
                Ok(node) => node,
                Err(errors) => {
                    diagnostics.extend(errors);
                    return IdeCheckResult::failed(diagnostics, units);
                }
            };
            nodes.insert(manifest.qualified_name(), node);
            pkg_options.package_roots.insert(node);
            pkg_options.detached_package_roots.insert(node);
            if privileged_packages.contains(&manifest.qualified_name()) {
                mark_trusted(&ast, node, &mut pkg_options.privileged_nodes);
            }
            mark_ffi_permissions(&ast, node, source.manifest.ffi(), &mut pkg_options);
            let context =
                match CompilationIdentityContext::for_package(&graph, &manifest.qualified_name()) {
                    Ok(context) => context,
                    Err(error) => {
                        diagnostics.extend(self.silent_errors(error));
                        return IdeCheckResult::failed(diagnostics, units);
                    }
                };
            pkg_options
                .package_identities
                .insert(node, context.into_source());
            members.push(node);
        }

        let original_root = ast.root;
        let span = ast.node(original_root).span;
        ast.root = match self.package_node_silent(
            &mut ast,
            NodeKind::FileScope,
            span,
            str_interner::StrId::from_raw(0),
            NodeIndex::NULL,
            &members,
        ) {
            Ok(node) => node,
            Err(errors) => {
                diagnostics.extend(errors);
                return IdeCheckResult::failed(diagnostics, units);
            }
        };
        nodes.insert(root.manifest.manifest().qualified_name(), ast.root);
        for manifest in graph.packages() {
            let mut imports = HashMap::new();
            for dependency in &manifest.dependencies {
                let name = format!("{}/{}", dependency.domain, dependency.name);
                let Some(&target) = nodes.get(&name) else {
                    diagnostics.extend(
                        self.silent_errors(format!(
                            "selected dependency `{name}` has no source root"
                        )),
                    );
                    return IdeCheckResult::failed(diagnostics, units);
                };
                imports
                    .entry(str_interner::intern(&dependency.name))
                    .or_insert_with(Vec::new)
                    .push(target);
            }
            pkg_options
                .package_dependencies
                .insert(nodes[&manifest.qualified_name()], imports);
        }

        let pre_std_root = ast.root;
        let (ast, mut options) =
            match self.load_standard_library_ide(ast, &mut units, &mut diagnostics) {
                Ok(loaded) => loaded,
                Err(errors) => {
                    diagnostics.extend(errors);
                    return IdeCheckResult::failed(diagnostics, units);
                }
            };

        let standard = options.package_roots.iter().copied().next();
        if let Some(dependencies) = pkg_options.package_dependencies.remove(&pre_std_root) {
            pkg_options
                .package_dependencies
                .insert(ast.root, dependencies);
        }
        if let Some(standard) = standard {
            for dependencies in pkg_options.package_dependencies.values_mut() {
                dependencies
                    .entry(str_interner::intern("std"))
                    .or_default()
                    .push(standard);
            }
        }
        options.package_roots.extend(pkg_options.package_roots);
        options.detached_package_roots = pkg_options.detached_package_roots;
        options.package_dependencies = pkg_options.package_dependencies;
        options
            .package_identities
            .extend(pkg_options.package_identities);
        options
            .implicit_imports
            .extend(pkg_options.implicit_imports);
        options
            .privileged_nodes
            .extend(pkg_options.privileged_nodes);
        options.ffi_nodes.extend(pkg_options.ffi_nodes);
        options
            .ffi_library_allowlists
            .extend(pkg_options.ffi_library_allowlists);
        options
            .package_identities
            .insert(ast.root, identity.into_source());

        let diag_ctx = DiagnosticContext::new_silent(&self.source_map);
        let resolved = resolution::resolve_with_options(ast, &diag_ctx, options);
        diagnostics.extend(diag_ctx.diagnostics().iter().cloned());
        diagnostics.extend(resolved.diagnostics.iter().cloned());
        units.sort_by_key(|u| (u.start_pos, u.end_pos));
        let has_errors = diagnostics.iter().any(|d| d.level == Level::Error);

        IdeCheckResult {
            resolved: Some(resolved),
            diagnostics,
            units,
            has_errors,
        }
    }

    fn silent_errors(&self, message: String) -> Vec<Diagnostic> {
        let context = DiagnosticContext::new_silent(&self.source_map);
        context.error(message).emit(&context);
        context.diagnostics().to_vec()
    }

    fn append_source_units_silent(
        &self,
        destination: &mut Ast,
        sources: Vec<Ast>,
    ) -> Result<Vec<NodeIndex>, Vec<Diagnostic>> {
        destination
            .try_append_asts_with_limits(sources, AstStorageLimits::default())
            .map_err(|error| self.silent_errors(format!("cannot assemble source units: {error}")))
    }

    fn package_node_silent(
        &self,
        ast: &mut Ast,
        kind: NodeKind,
        span: Span,
        str_id: str_interner::StrId,
        name: NodeIndex,
        body: &[NodeIndex],
    ) -> Result<NodeIndex, Vec<Diagnostic>> {
        ast.try_add_node(
            Node {
                kind,
                span,
                str_id,
                children: [name, NodeIndex::NULL, NodeIndex::NULL, NodeIndex::NULL],
                multi_start: 0,
                multi_len: 0,
            },
            body,
        )
        .map_err(|error| self.silent_errors(error.to_string()))
    }

    fn parse_synthetic_silent(&self, source: &str, name: &str) -> Result<Ast, Vec<Diagnostic>> {
        let source_id = self.next_source.fetch_add(1, Ordering::Relaxed);
        let sf = self.source_map.new_source_file(
            FileName::Custom(format!("{name}-{source_id}")),
            source.to_string(),
        );
        let diag_ctx = DiagnosticContext::new_silent(&self.source_map);
        let (tokens, lex_errors) = lexer::tokenize(source);
        for error in &lex_errors {
            error.emit(&diag_ctx, sf.start_pos);
        }
        let parser = parser::Parser::new(&tokens, source, &diag_ctx, sf.start_pos);
        let mut ast = parser.parse();
        if ast.root.is_null() {
            let empty_span = Span::new(sf.start_pos, sf.start_pos);
            ast.root = ast.builder(NodeKind::FileScope, empty_span).build();
        }
        Ok(ast)
    }

    #[allow(clippy::too_many_arguments)]
    fn parse_unit_ide(
        &self,
        path: PathBuf,
        logical_name: String,
        source: &str,
        is_std: bool,
        is_bundled: bool,
        units: &mut Vec<CheckedSourceUnit>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<Ast, Vec<Diagnostic>> {
        let source_id = self.next_source.fetch_add(1, Ordering::Relaxed);
        let sf = self.source_map.new_source_file(
            FileName::Custom(format!("{logical_name}-{source_id}")),
            source.to_string(),
        );
        let diag_ctx = DiagnosticContext::new_silent(&self.source_map);
        let mapper = diag_ctx
            .source_span_mapper(sf.start_pos, source)
            .map_err(|err| self.silent_errors(format!("invalid source mapping: {err}")))?;
        let mapping = mapper.mapping();

        let (tokens, lex_errors) = lexer::tokenize(source);
        for error in &lex_errors {
            error.emit(&diag_ctx, sf.start_pos);
        }

        let parser = parser::Parser::new(&tokens, source, &diag_ctx, sf.start_pos);
        let mut ast = parser.parse();
        if ast.root.is_null() {
            let empty_span = Span::new(sf.start_pos, sf.start_pos);
            ast.root = ast.builder(NodeKind::FileScope, empty_span).build();
        }

        diagnostics.extend(diag_ctx.diagnostics().iter().cloned());
        units.push(CheckedSourceUnit {
            path,
            logical_name,
            is_std,
            is_bundled,
            start_pos: sf.start_pos,
            end_pos: sf.end_pos,
            mapping,
            source: source.to_owned(),
        });
        Ok(ast)
    }

    fn package_ast_ide(
        &self,
        sources: &PackageSources,
        is_bundled: bool,
        units_out: &mut Vec<CheckedSourceUnit>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<Ast, Vec<Diagnostic>> {
        let units = source_units(sources).map_err(|error| self.silent_errors(error))?;
        let mut asts = Vec::with_capacity(units.len());
        for unit in &units {
            let logical = unit.file.file_path.to_string_lossy().into_owned();
            let ast = self.parse_unit_ide(
                unit.file.file_path.clone(),
                logical,
                &unit.file.source,
                false,
                is_bundled,
                units_out,
                diagnostics,
            )?;
            asts.push(ast);
        }
        let mut parsed = asts.into_iter();
        let mut ast = parsed
            .next()
            .ok_or_else(|| self.silent_errors("package has no entry".into()))?;
        let root = ast.root;
        let mut roots = vec![root];
        roots.extend(self.append_source_units_silent(&mut ast, parsed.collect())?);
        let mut members: Vec<Vec<NodeIndex>> = roots
            .iter()
            .map(|&r| ast.multi_children(r).to_vec())
            .collect();
        let mut generated: Vec<Vec<NodeIndex>> = vec![Vec::new(); units.len()];
        for index in (1..units.len()).rev() {
            members[index].extend(generated[index].iter().rev().copied());
            let parent = units[index]
                .parent
                .ok_or_else(|| self.silent_errors("module has no parent".into()))?;
            let name = units[index]
                .path
                .last()
                .ok_or_else(|| self.silent_errors("module has no name".into()))?;
            let matching: Vec<_> = members[parent]
                .iter()
                .copied()
                .filter(|&item| {
                    binding_name(&ast, item).is_some_and(|id| str_interner::get(id) == *name)
                })
                .collect();
            if matching.len() > 1 {
                diagnostics
                    .extend(self.silent_errors(format!("duplicate module declaration `{name}`")));
                continue;
            }
            let body = &members[index];
            if let Some(&item) = matching.first() {
                let declaration = unwrap_visibility(&ast, item);
                if ast.node(declaration).kind != NodeKind::ModuleDef {
                    diagnostics.extend(self.silent_errors(format!(
                        "filesystem module `{name}` conflicts with a source binding"
                    )));
                    continue;
                }
                if !ast.multi_children(declaration).is_empty() {
                    diagnostics.extend(self.silent_errors(format!(
                        "inline module `{name}` conflicts with filesystem module"
                    )));
                    continue;
                }
                let start = u32::try_from(ast.extra_children.len()).map_err(|_| {
                    self.silent_errors("module child storage exceeds capacity".into())
                })?;
                let end = ast
                    .extra_children
                    .len()
                    .checked_add(body.len())
                    .filter(|&end| end <= u32::MAX as usize)
                    .ok_or_else(|| {
                        self.silent_errors("module child storage exceeds capacity".into())
                    })?;
                ast.extra_children.try_reserve(body.len()).map_err(|_| {
                    self.silent_errors("cannot allocate module child storage".into())
                })?;
                ast.extra_children.extend_from_slice(body);
                ast.nodes[declaration.0 as usize].multi_start = start;
                ast.nodes[declaration.0 as usize].multi_len = (end - start as usize) as u32;
            } else {
                let name_node = self.package_node_silent(
                    &mut ast,
                    NodeKind::Id,
                    DUMMY_SP,
                    str_interner::intern(name),
                    NodeIndex::NULL,
                    &[],
                )?;
                let span = ast.node(roots[index]).span;
                let module = self.package_node_silent(
                    &mut ast,
                    NodeKind::ModuleDef,
                    span,
                    str_interner::StrId::from_raw(0),
                    name_node,
                    body,
                )?;
                generated[parent].push(module);
            }
        }
        members[0].extend(generated[0].iter().rev().copied());
        if !sources.manifest.ffi().wasm.is_empty() {
            let qpkg = sources.manifest.manifest().qualified_name();
            let mut synth = String::new();
            for spec in &sources.manifest.ffi().wasm {
                let conflict = members[0].iter().any(|&item| {
                    binding_name(&ast, item).is_some_and(|id| str_interner::get(id) == spec.name)
                });
                if conflict {
                    diagnostics.extend(self.silent_errors(format!(
                        "[[ffi.wasm]] module `{}` conflicts with an existing declaration",
                        spec.name
                    )));
                    continue;
                }
                synth.push_str(&format!(
                    "pub mod {} {{\n    use std.prelude.*\n",
                    spec.name
                ));
                for exp in &spec.exports {
                    let params_src = exp
                        .params
                        .iter()
                        .enumerate()
                        .map(|(i, ty)| format!("_p{i}: {ty}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let body_src = if exp.ret == "Unit" {
                        format!(
                            "let _ = std.runtime.backtrace(message = \"__nessa_wasm_bind:{qpkg}::{}::{}\"); ()",
                            spec.name, exp.nessa_name
                        )
                    } else {
                        format!(
                            "let _m: Any = std.runtime.backtrace(message = \"__nessa_wasm_bind:{qpkg}::{}::{}\"); _m.as({})",
                            spec.name, exp.nessa_name, exp.ret
                        )
                    };
                    synth.push_str(&format!(
                        "    pub fn {}({}) -> {} {{ {body_src} }}\n",
                        exp.nessa_name, params_src, exp.ret
                    ));
                }
                synth.push_str("}\n");
            }
            if !synth.is_empty() {
                let synth_ast =
                    self.parse_synthetic_silent(&synth, "ffi-wasm-generated-wrappers")?;
                let appended = self.append_source_units_silent(&mut ast, vec![synth_ast])?[0];
                members[0].extend_from_slice(ast.multi_children(appended));
            }
        }
        let root_span = ast.node(roots[0]).span;
        ast.root = self.package_node_silent(
            &mut ast,
            NodeKind::FileScope,
            root_span,
            str_interner::StrId::from_raw(0),
            NodeIndex::NULL,
            &members[0],
        )?;
        ast.validate_storage()
            .map_err(|error| self.silent_errors(error.to_string()))?;
        Ok(ast)
    }

    fn load_standard_library_ide(
        &self,
        mut ast: Ast,
        units_out: &mut Vec<CheckedSourceUnit>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<(Ast, ResolveOptions), Vec<Diagnostic>> {
        let user_items = ast.multi_children(ast.root).to_vec();
        let mut options = ResolveOptions {
            expose_root_builtins: false,
            expose_root_types: false,
            ..ResolveOptions::default()
        };
        let std_mod_ast = self.parse_unit_ide(
            PathBuf::from("<std>/mod.ns"),
            "std/mod.ns".into(),
            STD_ROOT_SOURCE,
            true,
            false,
            units_out,
            diagnostics,
        )?;
        let package_body = self.append_source_units_silent(&mut ast, vec![std_mod_ast])?[0];
        mark_trusted(&ast, package_body, &mut options.privileged_nodes);
        let members = ast.multi_children(package_body).to_vec();
        let mut sources = Vec::with_capacity(MODULES.len());
        for &(name, source) in MODULES {
            let module = self.parse_unit_ide(
                PathBuf::from(format!("<std>/{name}.ns")),
                format!("std/{name}.ns"),
                source,
                true,
                false,
                units_out,
                diagnostics,
            )?;
            sources.push(module);
        }
        let bodies = self.append_source_units_silent(&mut ast, sources)?;
        for (&(name, _), body) in MODULES.iter().zip(bodies) {
            mark_trusted(&ast, body, &mut options.privileged_nodes);
            let items = ast.multi_children(body).to_vec();
            let declaration = members.iter().copied().find_map(|member| {
                let inner = if ast.node(member).kind == NodeKind::PubDef {
                    ast.fixed_children(member)[0]
                } else {
                    member
                };
                (ast.node(inner).kind == NodeKind::ModuleDef
                    && str_interner::get(ast.node(ast.fixed_children(inner)[0]).str_id) == name)
                    .then_some(inner)
            });
            let Some(declaration) = declaration else {
                return Err(self.silent_errors(format!("std/mod.ns must declare module `{name}`")));
            };
            let mut definitions = ast.multi_children(declaration).to_vec();
            definitions.extend(items);
            let start = ast.extra_children.len() as u32;
            ast.extra_children.extend_from_slice(&definitions);
            ast.nodes[declaration.0 as usize].multi_start = start;
            ast.nodes[declaration.0 as usize].multi_len = definitions.len() as u32;
        }
        let span = ast.node(package_body).span;
        let name = ast
            .builder(NodeKind::Id, span)
            .set_str_id(str_interner::intern("std"))
            .build();
        let package = ast
            .builder(NodeKind::ModuleDef, span)
            .add_child(name)
            .add_multi_children(&members)
            .build();
        options.package_roots.insert(package);
        let identity = crate::identity::standard_library_identity().map_err(|message| {
            self.silent_errors(format!(
                "invalid standard-library package identity: {message}"
            ))
        })?;
        options.package_identities.insert(package, identity);
        let package = ast
            .builder(NodeKind::PubDef, span)
            .add_child(package)
            .build();
        let prelude = self.parse_synthetic_silent("use std.prelude.*", "implicit-prelude")?;
        let prelude = self.append_source_units_silent(&mut ast, vec![prelude])?[0];
        let imports = ast.multi_children(prelude).to_vec();
        options.implicit_imports.extend(imports.iter().copied());
        let mut items = vec![package];
        items.extend(imports);
        items.extend(user_items);
        ast.root = ast
            .builder(NodeKind::FileScope, ast.node(ast.root).span)
            .add_multi_children(&items)
            .build();
        Ok((ast, options))
    }

    fn attach_bundled_script_packages_ide(
        &self,
        ast: &mut Ast,
        options: &mut ResolveOptions,
        std_root: Option<NodeIndex>,
        units_out: &mut Vec<CheckedSourceUnit>,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Result<(), Vec<Diagnostic>> {
        let mut bundled_mods = Vec::new();
        for pkg in pkg_manager::bundled_packages() {
            let manifest = pkg.manifest.manifest();
            let pkg_ast = self.package_ast_ide(pkg, true, units_out, diagnostics)?;
            let body = self.append_source_units_silent(ast, vec![pkg_ast])?[0];

            let prelude =
                self.parse_synthetic_silent("use std.prelude.*", "implicit-bundled-prelude")?;
            let prelude_node = self.append_source_units_silent(ast, vec![prelude])?[0];
            let imports = ast.multi_children(prelude_node).to_vec();
            options.implicit_imports.extend(imports.iter().copied());

            let mut items = imports;
            items.extend_from_slice(ast.multi_children(body));
            let span = ast.node(body).span;
            let name_id = str_interner::intern(&manifest.name);
            let name_node = ast.builder(NodeKind::Id, span).set_str_id(name_id).build();
            let mod_node = ast
                .builder(NodeKind::ModuleDef, span)
                .add_child(name_node)
                .add_multi_children(&items)
                .build();

            mark_ffi_permissions(ast, mod_node, pkg.manifest.ffi(), options);
            options.package_roots.insert(mod_node);
            options.detached_package_roots.insert(mod_node);
            let identity =
                crate::identity::bundled_package_identity(&pkg.manifest).map_err(|message| {
                    self.silent_errors(format!("invalid bundled package identity: {message}"))
                })?;
            options.package_identities.insert(mod_node, identity);
            if let Some(std_node) = std_root {
                options
                    .package_dependencies
                    .entry(mod_node)
                    .or_default()
                    .insert(str_interner::intern("std"), vec![std_node]);
            }
            bundled_mods.push((name_id, mod_node));
        }

        if bundled_mods.is_empty() {
            return Ok(());
        }

        let existing = ast.multi_children(ast.root).to_vec();
        let mut combined = Vec::with_capacity(existing.len() + bundled_mods.len());
        if let Some(&first) = existing.first() {
            combined.push(first);
            for &(_, mod_node) in &bundled_mods {
                combined.push(mod_node);
            }
            combined.extend_from_slice(&existing[1..]);
        } else {
            for &(_, mod_node) in &bundled_mods {
                combined.push(mod_node);
            }
        }
        let root_span = ast.node(ast.root).span;
        ast.root = ast
            .builder(NodeKind::FileScope, root_span)
            .add_multi_children(&combined)
            .build();
        let root_deps = options.package_dependencies.entry(ast.root).or_default();
        for (name_id, mod_node) in bundled_mods {
            root_deps.entry(name_id).or_default().push(mod_node);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_source_ide_resolves_healthy_symbols_despite_syntax_error_in_buffer() {
        let driver = Driver::new();
        let path = Path::new("/virtual/editor_buf.ns");
        let src = "\
struct Point {
    x: i64,
    y: i64,
}

enum Direction {
    north,
    south,
}

fn broken_fn() {
    let x =
}

fn compute(p: Point) -> i64 {
    p.x + p.y
}
";
        let result = driver.check_source_ide(path, src);
        assert!(result.has_errors, "expected syntax error from broken_fn");
        assert!(!result.diagnostics.is_empty());
        let resolved = result.resolved.as_ref().expect("resolved AST retained");

        let compute_id = str_interner::intern("compute");
        let compute_sym = resolved
            .symbols
            .iter()
            .find(|sym| sym.name == compute_id)
            .expect("compute symbol resolved despite earlier syntax error");
        let def_span = resolved.ast.node(compute_sym.def_node).span;
        let (unit, raw_range) = result.locate_span(def_span).expect("span located");
        assert_eq!(unit.path, path);
        assert!(!unit.is_std);
        let snippet = &unit.source[raw_range.start as usize..raw_range.end as usize];
        assert!(
            snippet.contains("fn compute"),
            "unexpected snippet: {snippet}"
        );
    }

    #[test]
    fn check_source_ide_locates_std_definitions_and_handles_crlf() {
        let driver = Driver::new();
        let path = Path::new("/virtual/crlf_buf.ns");
        let src = "use std.io\r\nfn run_app() -> i64 {\r\n    let v = 42\r\n    v\r\n}\r\n";
        let result = driver.check_source_ide(path, src);
        assert!(!result.has_errors, "{:?}", result.diagnostics);
        let resolved = result.resolved.as_ref().expect("resolved AST");
        let unit = result.unit_for_path(path).expect("user unit registered");
        let run_offset = src.find("run_app").unwrap() as u32;
        let span = unit
            .span_for_raw_range(RawByteRange {
                start: run_offset,
                end: run_offset + 7,
            })
            .expect("CRLF raw range mapped to normalized span");
        let (_, back) = result.locate_span(span).expect("mapped back");
        assert_eq!(&src[back.start as usize..back.end as usize], "run_app");
        assert!(
            result
                .units
                .iter()
                .any(|u| u.is_std && u.logical_name == "std/io.ns"),
            "std/io.ns unit should be indexed for go-to-definition"
        );
        assert!(!resolved.symbols.is_empty());
    }
}
