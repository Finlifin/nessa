//! Compile a selected manifest DAG from caller-owned package source trees.

use std::collections::{BTreeMap, HashMap};

use ast::{NodeIndex, NodeKind};
use pkg_manager::{PackageResolver, PackageSources, PackageType, SourceLoadLimits};
use resolution::ResolveOptions;
use rustc_span::DUMMY_SP;

use crate::package::source_units;
use crate::{CompilationIdentityContext, CompileResult, Driver};

type SourceKey = (String, String);

impl Driver {
    /// Resolve and compile a root and an in-memory source catalog in one pipeline.
    /// Only selected, reachable versions are parsed. Paths are diagnostic labels;
    /// compilation never reopens them. Conflicting duplicate source trees fail.
    pub fn compile_package_sources_with_dependencies(
        &self,
        root: &PackageSources,
        catalog: &[PackageSources],
    ) -> CompileResult {
        let selected = match select_sources(root, catalog) {
            Ok(selected) => selected,
            Err(error) => return self.package_failure(error),
        };
        let (graph, sources) = selected;
        let identity = match CompilationIdentityContext::from_graph(&graph) {
            Ok(identity) => identity,
            Err(error) => return self.package_failure(error),
        };
        let (mut ast, mut diagnostics) = match self.package_ast(root) {
            Ok(parsed) => parsed,
            Err(errors) => {
                return CompileResult::failed(errors, type_pool::TypePool::with_intrinsics());
            }
        };
        let mut options = ResolveOptions::default();
        let mut nodes = BTreeMap::from([(root.manifest.manifest().qualified_name(), ast.root)]);
        let mut dependencies = Vec::new();
        for source in &sources {
            if std::ptr::eq(*source, root) {
                continue;
            }
            let (dependency, warnings) = match self.package_ast(source) {
                Ok(parsed) => parsed,
                Err(errors) => {
                    return CompileResult::failed(errors, type_pool::TypePool::with_intrinsics());
                }
            };
            diagnostics.extend(warnings);
            dependencies.push(dependency);
        }
        let bodies = match self.append_source_units(&mut ast, dependencies) {
            Ok(bodies) => bodies,
            Err(errors) => {
                return CompileResult::failed(errors, type_pool::TypePool::with_intrinsics());
            }
        };
        let mut members = ast.multi_children(ast.root).to_vec();
        for (source, body) in sources
            .iter()
            .filter(|source| !std::ptr::eq(**source, root))
            .zip(bodies)
        {
            let manifest = source.manifest.manifest();
            let result = (|| {
                let (prelude, warnings) =
                    self.parse_source_named("use std.prelude.*", "implicit-package-prelude")?;
                diagnostics.extend(warnings);
                let prelude = self.append_source_units(&mut ast, vec![prelude])?[0];
                let imports = ast.multi_children(prelude).to_vec();
                options.implicit_imports.extend(imports.iter().copied());
                let mut items = imports;
                items.extend_from_slice(ast.multi_children(body));
                let name = self.package_node(
                    &mut ast,
                    NodeKind::Id,
                    DUMMY_SP,
                    str_interner::intern(&manifest.name),
                    NodeIndex::NULL,
                    &[],
                )?;
                let span = ast.node(body).span;
                self.package_node(
                    &mut ast,
                    NodeKind::ModuleDef,
                    span,
                    str_interner::StrId::from_raw(0),
                    name,
                    &items,
                )
            })();
            let node = match result {
                Ok(node) => node,
                Err(errors) => {
                    return CompileResult::failed(errors, type_pool::TypePool::with_intrinsics());
                }
            };
            nodes.insert(manifest.qualified_name(), node);
            options.package_roots.insert(node);
            options.detached_package_roots.insert(node);
            let context =
                match CompilationIdentityContext::for_package(&graph, &manifest.qualified_name()) {
                    Ok(context) => context,
                    Err(error) => return self.package_failure(error),
                };
            options
                .package_identities
                .insert(node, context.into_source());
            members.push(node);
        }
        let original_root = ast.root;
        let span = ast.node(original_root).span;
        ast.root = match self.package_node(
            &mut ast,
            NodeKind::FileScope,
            span,
            str_interner::StrId::from_raw(0),
            NodeIndex::NULL,
            &members,
        ) {
            Ok(node) => node,
            Err(errors) => {
                return CompileResult::failed(errors, type_pool::TypePool::with_intrinsics());
            }
        };
        nodes.insert(root.manifest.manifest().qualified_name(), ast.root);
        for manifest in graph.packages() {
            let mut imports = HashMap::new();
            for dependency in &manifest.dependencies {
                let name = format!("{}/{}", dependency.domain, dependency.name);
                let Some(&target) = nodes.get(&name) else {
                    return self.package_failure(format!(
                        "selected dependency `{name}` has no source root"
                    ));
                };
                imports
                    .entry(str_interner::intern(&dependency.name))
                    .or_insert_with(Vec::new)
                    .push(target);
            }
            options
                .package_dependencies
                .insert(nodes[&manifest.qualified_name()], imports);
        }
        let startup = match root.manifest.manifest().package_type {
            PackageType::Exe => nir::StartupMode::RequiredMain,
            PackageType::Lib | PackageType::Tmp => nir::StartupMode::InitializationOnly,
        };
        self.compile_ast_with_options(ast, diagnostics, Some(identity), startup, Some(options))
    }
}

fn key(sources: &PackageSources) -> SourceKey {
    let manifest = sources.manifest.manifest();
    (manifest.qualified_name(), manifest.version.to_string())
}

fn same_sources(left: &PackageSources, right: &PackageSources) -> Result<bool, String> {
    let left = source_units(left)?;
    let right = source_units(right)?;
    Ok(left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.path == right.path && left.file.source == right.file.source))
}

fn select_sources<'a>(
    root: &'a PackageSources,
    catalog: &'a [PackageSources],
) -> Result<(pkg_manager::ResolvedPackageGraph, Vec<&'a PackageSources>), String> {
    if catalog.len() > 65_536 {
        return Err("source catalog exceeds package count limit (65536)".into());
    }
    let mut resolver = PackageResolver::new();
    let mut registered = BTreeMap::from([(key(root), root)]);
    for source in catalog {
        resolver
            .register_document(source.manifest.clone())
            .map_err(|error| error.to_string())?;
        let key = key(source);
        if let Some(previous) = registered.get(&key) {
            if !same_sources(previous, source)? {
                return Err(format!(
                    "conflicting duplicate sources for {}@{}",
                    key.0, key.1
                ));
            }
        } else {
            registered.insert(key, source);
        }
    }
    let graph = resolver
        .resolve_document(&root.manifest)
        .map_err(|error| error.to_string())?;
    let limits = SourceLoadLimits::default();
    let mut units = 0usize;
    let mut bytes = 0usize;
    let mut selected = Vec::new();
    for manifest in graph.packages() {
        let key = (manifest.qualified_name(), manifest.version.to_string());
        let source = registered
            .get(&key)
            .copied()
            .ok_or_else(|| format!("missing selected sources for {}@{}", key.0, key.1))?;
        for unit in source_units(source)? {
            units = units
                .checked_add(1)
                .filter(|&n| n <= limits.max_modules)
                .ok_or("selected packages exceed combined module count limit")?;
            bytes = bytes
                .checked_add(unit.file.source.len())
                .filter(|&n| n <= limits.max_source_bytes)
                .ok_or("selected packages exceed combined source byte limit")?;
        }
        selected.push(source);
    }
    Ok((graph, selected))
}
